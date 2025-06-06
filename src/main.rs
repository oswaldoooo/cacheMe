use std::{pin::Pin, str::FromStr, sync::Arc, time::Duration};

#[tokio::main]
async fn main() -> Result<(), String> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();
    let mut args = std::env::args();
    args.next();
    let cnf_path = if let Some(cnf_path) = args.next() {
        cnf_path
    } else {
        "config.json".to_string()
    };
    let cnf = load_config(&cnf_path).await?;
    drop(cnf_path);
    let _ = tokio::fs::create_dir_all(&cnf.temp_dir).await;
    let db = load_database(&cnf).await?;
    let mut cache_zone: Vec<Pin<Box<dyn cache_me::storage::cache_zone::CacheZone + Send + Sync>>> =
        Vec::new();
    for cz in &cnf.cache_zone {
        cache_zone.push(load_cache_zone(cz).await?);
    }
    if cache_zone.is_empty() {
        return Err("cache_zone not set".to_string());
    }
    let server = cache_me::ProxyServer::new(cnf.temp_dir, db, cache_zone);
    let server = Arc::new(server);
    let gc_interval = cache_me::parse_duration(cnf.gc_interval)?;

    tokio::spawn({
        let server = server.clone();
        async move { cache_me::run_daemon(gc_interval, server).await }
    });
    let l = tokio::net::TcpListener::bind(&cnf.http_bind)
        .await
        .map_err(|err| format!("bind {} error {err}", cnf.http_bind))?;
    if let Some(control_bind) = &cnf.http_control_bind {
        let control_bind = control_bind.clone();
        tokio::spawn(async move {
            while let Err(err) = cache_me::listen_control(&control_bind).await {
                log::error!("listen_control error {err}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
    tokio::spawn(run_resource_stats());
    cache_me::listen_and_server(l, server)
        .await
        .expect("listen_and_server failed");
    Ok(())
}

#[derive(serde::Deserialize)]
struct Config {
    http_bind: String,
    http_control_bind: Option<String>,
    temp_dir: String,
    gc_interval: String,
    cache_zone: Vec<CacheZoneConfig>,
    sled: Option<SledConfig>,
}
#[derive(serde::Deserialize)]
struct CacheZoneConfig {
    generic: Option<String>,
}
#[derive(serde::Deserialize)]
struct SledConfig {
    path: String,
}

async fn load_cache_zone(
    cnf: &CacheZoneConfig,
) -> Result<Pin<Box<dyn cache_me::storage::cache_zone::CacheZone + Send + Sync>>, String> {
    if let Some(generic) = &cnf.generic {
        let cd = cache_me::storage::cache_zone::disk::CommonDisk::from_str(&generic)
            .map_err(|err| format!("{err}"))?;
        return Ok(Box::pin(cd));
    }
    return Err("not set any driver".to_string());
}
async fn load_database(
    cnf: &Config,
) -> Result<Pin<Box<dyn cache_me::storage::database::Database + Send + Sync + 'static>>, String> {
    if let Some(sled) = &cnf.sled {
        let db = cache_me::storage::database::sled::Sled::from_str(&sled.path)
            .map_err(|err| format!("{err}"))?;
        return Ok(Box::pin(db));
    }
    return Err("not set any driver".to_string());
}

async fn load_config(rpath: &str) -> Result<Config, String> {
    let content = tokio::fs::read(rpath)
        .await
        .map_err(|err| format!("read config {rpath} error {err}"))?;
    serde_json::from_slice(&content).map_err(|err| format!("parse config error {err}"))
}
async fn run_resource_stats() {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    let cpu_usage = prometheus::register_gauge!("cpu_usage", "cpu usage").unwrap();
    let mem_stat = prometheus::register_gauge!("memory_stat", "memory stat").unwrap();
    let disk_iobps = prometheus::register_gauge!("disk_iobps", "disk iobps").unwrap();
    let mut system = sysinfo::System::new();
    let pid = sysinfo::Pid::from_u32(std::process::id());
    loop {
        tick.tick().await;
        system.refresh_all();
        if let Some(process) = system.process(pid) {
            cpu_usage.set(process.cpu_usage() as f64);
            mem_stat.set(process.memory() as f64);
            let du = process.disk_usage();
            disk_iobps.set(
                (du.read_bytes + du.written_bytes) as f64
                    / (du.total_read_bytes + du.total_written_bytes) as f64,
            );
        }
    }
}
