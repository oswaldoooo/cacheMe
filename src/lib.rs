use futures_util::TryStreamExt;
use http_body_util::Full;
use hyper::{
    body::{Body, Incoming},
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use lazy_static::lazy_static;
use md5::Digest;
use prometheus::Encoder;
use std::{
    convert::Infallible,
    fmt::{Debug, Display},
    future::Future,
    io::Write,
    os::unix::fs::MetadataExt,
    path::Path,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    time::Duration,
};
use tokio::{io::AsyncReadExt, sync::Mutex};
enum CacheEntry {
    MetaData(types::metadata::ObjectMetaData),
    Response(types::Response),
}
pub mod cache;
pub mod storage;
mod stream;
pub mod types;

macro_rules! insert_header_value {
    ($header:expr,$key:expr,$val:expr) => {
        match $header.get_mut(&$key) {
            Some(vlist) => {
                vlist.push($val);
            }
            None => {
                $header.insert($key, vec![$val]);
            }
        }
    };
}
macro_rules! set_header_value {
    ($header:expr,$key:expr,$val:expr) => {
        match $header.get_mut(&$key) {
            Some(vlist) => {
                vlist.clear();
                vlist.push($val);
            }
            None => {
                $header.insert($key, vec![$val]);
            }
        }
    };
}

lazy_static! {
    static ref COMPLETED_REQUEST_TOTAL: prometheus::Counter =
        prometheus::register_counter!("completed_request_total", "completed request total")
            .unwrap();
    static ref SERVER_INTERNAL_ERROR_TOTAL: prometheus::Counter =
        prometheus::register_counter!("server_internal_error_total", "server internal error total")
            .unwrap();
    static ref HIT_TOTAL: prometheus::Counter =
        prometheus::register_counter!("hit_total", "hit total").unwrap();
    static ref MISS_TOTAL: prometheus::Counter =
        prometheus::register_counter!("miss_total", "miss total").unwrap();
}
type ServiceHandleFunc = Box<
    dyn Send
        + Fn(
            Arc<ProxyServer>,
            types::Request,
        ) -> Pin<Box<dyn Future<Output = Result<types::Response, ErrorKind>> + Send>>,
>;
pub enum ErrorKind {
    MachineError(String), /*硬件错误，例如访问文件磁盘错误 */
    LogicalError(String), /*逻辑错误，比如不符合规则，鉴权失败 */
    OperateError(String), /*操作错误，例如回源失败 */
}
impl std::error::Error for ErrorKind {}
impl Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MachineError(arg0) => f.debug_tuple("MachineError").field(arg0).finish(),
            Self::LogicalError(arg0) => f.debug_tuple("LogicalError").field(arg0).finish(),
            Self::OperateError(arg0) => f.debug_tuple("OperateError").field(arg0).finish(),
        }
    }
}
impl Debug for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MachineError(arg0) => f.debug_tuple("MachineError").field(arg0).finish(),
            Self::LogicalError(arg0) => f.debug_tuple("LogicalError").field(arg0).finish(),
            Self::OperateError(arg0) => f.debug_tuple("OperateError").field(arg0).finish(),
        }
    }
}

pub struct ProxyServer {
    temp_dir: String,
    cache: cache::Cache,
    cache_zone: Vec<Pin<Box<dyn storage::cache_zone::CacheZone + Send + Sync>>>,
    acl: std::collections::HashMap<String, Vec<types::acl::Acl>>,
    share_stat: std::collections::HashMap<String, ShareState>,
}

impl ProxyServer {
    pub fn new(
        temp_dir: String,
        db: Pin<Box<dyn storage::database::Database + Send + Sync + 'static>>,
        cache_zone: Vec<Pin<Box<dyn storage::cache_zone::CacheZone + Send + Sync>>>,
        acl: std::collections::HashMap<String, Vec<types::acl::Acl>>,
    ) -> Self {
        let mut ss = std::collections::HashMap::default();
        acl.iter().all(|(k, _)| {
            ss.insert(k.to_string(), ShareState::default());
            true
        });
        Self {
            temp_dir: temp_dir,
            cache: cache::Cache::new(db),
            cache_zone: cache_zone,
            acl: acl,
            share_stat: ss,
        }
    }
    async fn run_gc(&self) {
        if let Err(err) = self.cache.run_gc().await {
            log::error!("run_gc failed {err}");
        }
    }
    async fn remove_object(&self, hash: &types::Hash, meta: &types::metadata::ObjectMetaData) {
        if let Err(err) = self.cache.remove_object(hash).await {
            log::error!("remove object metadata {hash} error {err}");
        }
        if let Some(cache_zone) = self.cache_zone.get(meta.cache_zone as usize) {
            if let Err(err) = cache_zone.remove_object(&meta.storage_path).await {
                log::error!(
                    "remove cache_zone {} path {} error {err}",
                    meta.cache_zone,
                    meta.storage_path
                );
            }
        }
    }
    async fn hanlde_cache_request(
        &self,
        req: types::Request,
    ) -> Result<types::Response, ErrorKind> {
        let hash = get_hash_v1(&req.method, &req.host, &req.url_path);
        let mut meta = self.cache.get_object(&hash).await?;
        let mut is_hit = meta.is_some();
        if let Some(meta) = &meta {
            if (meta.expire + meta.first_time_ref) <= req.timestamp {
                self.remove_object(&hash, meta).await;
                is_hit = false;
            }
            if let Some(if_none_match) = req.get_if_none_match() {
                if if_none_match != meta.etag.as_str() {
                    let resp = self.head_request(&req).await?;
                    if resp.0 == 304 {
                        is_hit = false;
                    } else if resp.0 == 200 {
                        if let Some(etag) = resp.1.get("etag") {
                            if let Ok(etag) = etag.to_str() {
                                if etag != meta.etag.as_str() {
                                    self.remove_object(&hash, meta).await;
                                    is_hit = false;
                                }
                            }
                        }
                    }
                }
            } else if let Some(if_modified_since) = req.get_if_modified_since() {
                if let Some(last_modified) = meta.last_modified() {
                    if last_modified <= if_modified_since {
                        let cursor = std::io::Cursor::new("".as_bytes());
                        let buffreader = tokio::io::BufReader::new(cursor);
                        return Ok(types::Response {
                            status_code: 304,
                            headers: http::HeaderMap::default(),
                            content: Box::pin(buffreader),
                        });
                    }
                }
            }
        }
        let mut rng = if let Some(rng) = req.headers.get("range") {
            if let Ok(rng) = rng.to_str() {
                parse_range(rng)
            } else {
                (-1, -1)
            }
        } else {
            (-1, -1)
        };
        let host_stat = self.share_stat.get(req.host.as_str()).unwrap();
        if !is_hit && host_stat.try_upstream(hash.clone()).await {
            //回源拿
            let resp = match self.sub_request(&req).await {
                Ok(resp) => resp,
                Err(err) => {
                    host_stat.release_upstream(&hash, true).await;
                    return Err(err);
                }
            };
            let ret = match self.build_object_meta(&req, &hash, resp).await {
                Ok(ret) => ret,
                Err(err) => {
                    host_stat.release_upstream(&hash, true).await;
                    return Err(err);
                }
            };
            match ret {
                CacheEntry::MetaData(mut object_meta_data) => {
                    let cache_zone = self.cache_zone.get(object_meta_data.cache_zone as usize);
                    match cache_zone {
                        Some(cache_zone) => {
                            if !cache_zone.is_ok() {
                                host_stat.release_upstream(&hash, true).await;
                                return Err(ErrorKind::MachineError(format!(
                                    "cache_zone {} is not ok",
                                    object_meta_data.cache_zone
                                )));
                            }
                            match cache_zone
                                .link_temp_to_object(&object_meta_data.storage_path)
                                .await
                            {
                                Ok(real_path) => {
                                    object_meta_data.storage_path = real_path;
                                }
                                Err(err) => {
                                    host_stat.release_upstream(&hash, true).await;
                                    return Err(ErrorKind::MachineError(format!(
                                        "cache_zone {} link_temp {} return error {err}",
                                        object_meta_data.cache_zone, object_meta_data.storage_path
                                    )));
                                }
                            }
                        }
                        None => {
                            host_stat.release_upstream(&hash, true).await;
                            // log::error!("cache_zone {} get failed", object_meta_data.cache_zone)
                            return Err(ErrorKind::MachineError(format!(
                                "cache_zone {} get failed",
                                object_meta_data.cache_zone
                            )));
                        }
                    }
                    host_stat.release_upstream(&hash, false).await;
                    meta = Some(object_meta_data);
                }
                CacheEntry::Response(response) => {
                    return Ok(response);
                }
            }
        }
        if is_hit {
            HIT_TOTAL.inc();
        } else {
            if host_stat.wait_upstream(&hash).await {
                return Err(ErrorKind::OperateError(format!(
                    "other upstream failed.hashkey={hash} use same result"
                )));
            }
        }
        let mut meta = meta.unwrap();
        let cache_zone = self.cache_zone.get(meta.cache_zone as usize);
        if cache_zone.is_none() {
            return Err(ErrorKind::OperateError(format!(
                "not found cache_zone {}",
                meta.cache_zone
            )));
        }
        let cache_zone = cache_zone.unwrap();
        if !cache_zone.is_ok() {
            return Err(ErrorKind::MachineError(format!(
                "cache_zone {} is not ok",
                meta.cache_zone,
            )));
        }
        let object_metadatab = cache_zone.get_object_meta(&meta.storage_path).await?;
        if rng.1 >= object_metadatab.size as i64 {
            rng.1 = (object_metadatab.size - 1) as i64;
        }
        if rng.0 >= object_metadatab.size as i64 {
            rng.0 = (object_metadatab.size - 1) as i64;
        }
        let mut content_stream = cache_zone
            .get_object_content(&meta.storage_path, &storage::cache_zone::GetOption { rng })
            .await?;
        meta.last_ref_time = req.timestamp;
        meta.ref_count = meta.ref_count + 1;
        if let Err(err) = self.cache.put_object(&hash, &meta).await {
            log::error!("put object metadata failed {err}");
        }
        let mut ok_status = meta.status_code;
        if rng.1 > 0 || rng.0 > 0 {
            let end = if rng.1 > 0 {
                (rng.1 + 1) as usize
            } else {
                meta.size
            };
            let start = if rng.0 < 0 { 0 } else { rng.0 as usize };
            if end > start {
                ok_status = 206;
                if let Some(cl) = meta.headers.get_mut("content-length") {
                    let cl = &mut cl[0];
                    *cl = (end - start).to_string();
                }
            } else {
                log::info!("{end} <= {start} {}", meta.size);
            }
        }
        meta.headers.insert(
            "x-cache".to_string(),
            vec![if is_hit { "HIT" } else { "MISS" }.to_string()],
        );
        if let Some(ae) = req.get_accept_encoding() {
            for ae in ae {
                if ae == "gzip" {
                    let mut cl = 0;
                    content_stream =
                        stream::build_gzip_encoder_stream(content_stream, Some(&mut cl)).await?;
                    meta.headers
                        .insert("content-encoding".to_string(), vec!["gzip".to_string()]);
                    let vary = "Vary".to_string();
                    insert_header_value!(meta.headers, vary, "accept-encoding".to_string());
                    let clstr = "content-length".to_string();
                    set_header_value!(meta.headers, clstr, cl.to_string());
                    break;
                }
            }
        }
        Ok(types::Response {
            status_code: ok_status,
            headers: conver_hashmap_to_header_map(meta.headers),
            content: content_stream,
        })
    }
    async fn head_request(
        &self,
        raw_req: &types::Request,
    ) -> Result<(u16, hyper::HeaderMap), ErrorKind> {
        let url = format!("http://{}{}", raw_req.host, raw_req.url_path);
        let mut req = reqwest::Request::new(
            http::Method::HEAD,
            url.parse()
                .map_err(|err| ErrorKind::LogicalError(format!("build logical error {err}")))?,
        );
        let headers = req.headers_mut();
        for (k, v) in &raw_req.headers {
            if k.as_str() == "range" {
                continue;
            }
            headers.insert(k, v.clone());
        }
        let resp = reqwest::Client::new()
            .execute(req)
            .await
            .map_err(|err| ErrorKind::OperateError(format!("upstream {url} failed {err}")))?;
        Ok((resp.status().as_u16(), resp.headers().to_owned()))
    }
    async fn sub_request(&self, raw_req: &types::Request) -> Result<types::Response, ErrorKind> {
        let url = format!("http://{}{}", raw_req.host, raw_req.url_path);
        let mut req = reqwest::Request::new(
            http::Method::from_str(&raw_req.method).map_err(|err| {
                ErrorKind::LogicalError(format!("build method {} error {err}", raw_req.method))
            })?,
            url.parse()
                .map_err(|err| ErrorKind::LogicalError(format!("build logical error {err}")))?,
        );
        let headers = req.headers_mut();
        for (k, v) in &raw_req.headers {
            if k.as_str() == "range" {
                continue;
            }
            headers.insert(k, v.clone());
        }
        let resp = reqwest::Client::new()
            .execute(req)
            .await
            .map_err(|err| ErrorKind::OperateError(format!("upstream {url} failed {err}")))?;
        let status = resp.status().as_u16();
        let header = resp.headers().to_owned();
        let stream = tokio_util::io::StreamReader::new(
            resp.bytes_stream()
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)),
        );
        Ok(types::Response {
            status_code: status,
            headers: header,
            content: Box::pin(stream),
        })
    }
    async fn get_suitable_cache_zone(
        &self,
        storage_level: i8,
        size: usize,
    ) -> Result<u8, ErrorKind> {
        let min_disk_usage = 95.0;
        let mut ret = None;
        let mut index = 0;
        for cache_zone in self.cache_zone.iter() {
            let state = cache_zone.state();
            if state.avaiable_size > size {
                let disk_usage = state.avaiable_size as f64 / state.total_size as f64;
                if disk_usage < min_disk_usage {
                    ret = Some(index);
                }
            }
            index = index + 1;
        }
        ret.map_or(
            Err(ErrorKind::MachineError(format!("not avaiable cache_zone"))),
            |index| Ok(index as u8),
        )
    }
    async fn store_temp_file(
        &self,
        mut r: Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>,
    ) -> Result<String, ErrorKind> {
        let mut buff = [0u8; 4];
        get_random(&mut buff).await?;
        let rpath = Path::new(self.temp_dir.as_str()).join(hex::encode(buff));
        let mut fd = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .read(true)
            .mode(0o640)
            .open(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("open {:?} failed {err}", rpath)))?;
        match tokio::io::copy(&mut r, &mut fd).await {
            Ok(_) => {
                return Ok(rpath.to_str().unwrap().to_string());
            }
            Err(err) => {
                let _ = tokio::fs::remove_file(&rpath).await;
                return Err(ErrorKind::MachineError(format!(
                    "copy stream to tempfile {:?} error {err}",
                    rpath
                )));
            }
        }
    }
    async fn build_object_meta(
        &self,
        req: &types::Request,
        hash: &types::Hash,
        resp: types::Response,
    ) -> Result<CacheEntry, ErrorKind> {
        let cc = if let Some(cache_control) = resp.headers.get("cache-control") {
            if let Ok(cache_control) = cache_control.to_str() {
                parse_cache_control(cache_control)
            } else {
                CacheControl::default()
            }
        } else {
            CacheControl::default()
        };
        if !cc.need_cache {
            return Ok(CacheEntry::Response(resp));
        }
        let temp_path = self.store_temp_file(resp.content).await?;

        let fsize = match tokio::fs::metadata(&temp_path).await.map_err(|err| {
            ErrorKind::MachineError(format!("get temp file {temp_path} metadata failed {err}"))
        }) {
            Ok(meta) => meta.size() as usize,
            Err(err) => {
                if let Err(err) = tokio::fs::remove_file(&temp_path).await {
                    log::error!("remove temp file {temp_path} failed {err}");
                }
                return Err(err);
            }
        };

        let cache_zone = match self.get_suitable_cache_zone(1, fsize).await {
            Ok(cache_zone) => cache_zone,
            Err(err) => {
                if let Err(err) = tokio::fs::remove_file(&temp_path).await {
                    log::error!("remove temp file {temp_path} failed {err}");
                }
                return Err(err);
            }
        };
        let etag = if let Some(etag) = resp.headers.get("etag") {
            etag.to_str().map_or("".to_string(), |v| v.to_string())
        } else {
            "".to_string()
        };
        let ret = types::metadata::ObjectMetaData {
            etag: etag,
            status_code: resp.status_code,
            headers: conver_header_map_to_hashmap(resp.headers)?,
            size: fsize,
            method: req.method.clone(),
            url_path: req.url_path.clone(),
            host: req.host.clone(),
            expire: cc.max_age as i64,
            last_ref_time: req.timestamp,
            ref_count: 0,
            first_time_ref: req.timestamp,
            storage_level: 1,
            storage_path: temp_path,
            cache_zone: cache_zone,
        };
        Ok(CacheEntry::MetaData(ret))
    }
}

fn get_hash_v1(method: &str, host: &str, url_path: &str) -> types::Hash {
    let mut hash = md5::Md5::new();
    let _ = hash.write_all(host.as_bytes());
    let _ = hash.write_all(method.as_bytes());
    let _ = hash.write_all(url_path.as_bytes());
    let buff: [u8; 16] = hash.finalize().into();
    hex::encode(buff)
}
pub async fn run_daemon(gc_interval: Duration, server: Arc<ProxyServer>) {
    tokio::time::interval(gc_interval);
    tokio::task::spawn({
        let server = server.clone();
        let mut gc_interval = tokio::time::interval(gc_interval.clone());
        async move {
            loop {
                gc_interval.tick().await;
                server.run_gc().await;
            }
        }
    });
}
pub async fn listen_control(address: &str) -> Result<(), String> {
    let l = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|err| format!("bind {address} failed {err}"))?;
    loop {
        let (conn, _) = l
            .accept()
            .await
            .map_err(|err| format!("tcp listener accpet error {err}"))?;
        let conn = TokioIo::new(conn);
        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(conn, service_fn(handle_metrics_request))
                .await
            {
                log::error!("http1 server error {err}");
            }
        });
    }
}
pub async fn listen_and_server(
    l: tokio::net::TcpListener,
    server: Arc<ProxyServer>,
) -> Result<(), ErrorKind> {
    loop {
        let (conn, _) = l
            .accept()
            .await
            .map_err(|err| ErrorKind::MachineError(format!("tcp listener accept error {err}")))?;
        let conn = hyper_util::rt::TokioIo::new(conn);
        let server = server.clone();
        tokio::spawn(async move {
            let handle = service_fn(move |req| handle_request(req, server.clone()));
            if let Err(err) = http1::Builder::new()
                .serve_connection(conn, handle.clone())
                .await
            {
                log::error!("serve connection error {err}");
            }
        });
    }
}

async fn handle_request(
    req: hyper::Request<Incoming>,
    server: Arc<ProxyServer>,
) -> Result<hyper::Response<Full<bytes::Bytes>>, Infallible> {
    let headers = req.headers();
    let uri = req.uri();
    let host = uri.host();
    if host.is_none() {
        log::info!("request not set host");
        return Ok(hyper::Response::builder()
            .status(400)
            .body(Full::new(bytes::Bytes::from("".as_bytes())))
            .unwrap());
    }
    let host = host.unwrap();
    let url_path = uri.path();
    let method = req.method().as_str();
    let acl = server.acl.get(host);
    if let None = &acl {
        return Ok(hyper::Response::builder()
            .status(404)
            .body(Full::new(bytes::Bytes::from("")))
            .unwrap());
    }
    let handle: Option<ServiceHandleFunc> = {
        let acl = acl.unwrap();
        let mut handle: Option<ServiceHandleFunc> = None;
        for acl in acl {
            if acl.method_match(method) {
                if regex::Regex::new(acl.path_match())
                    .unwrap()
                    .is_match(url_path)
                {
                    handle = Some(acl.clone().handle());
                    break;
                }
            }
        }
        handle
    };
    if handle.is_none() {
        return Ok(hyper::Response::builder()
            .status(404)
            .header("content-length", 0)
            .header("contention", "close")
            .body(Full::new(bytes::Bytes::from("")))
            .unwrap());
    }

    let current = chrono::Utc::now().timestamp();
    let ret = handle.unwrap()(
        server,
        types::Request {
            host: host.to_string(),
            method: method.to_string(),
            url_path: url_path.to_string(),
            headers: headers.to_owned(),
            timestamp: current,
        },
    )
    .await;
    COMPLETED_REQUEST_TOTAL.inc();
    match ret {
        Ok(mut resp) => {
            let mut ret = hyper::Response::builder().status(resp.status_code);
            for (k, v) in resp.headers.iter() {
                ret = ret.header(k, v);
            }
            let mut buff = Vec::new();
            if let Err(err) = resp.content.read_to_end(&mut buff).await {
                log::error!("{method} {host} {url_path} read buff error {err}");

                SERVER_INTERNAL_ERROR_TOTAL.inc();
                return Ok(hyper::Response::builder()
                    .status(501)
                    .body(Full::new(bytes::Bytes::from("")))
                    .unwrap());
            }
            return Ok(ret.body(Full::new(bytes::Bytes::from(buff))).unwrap());
        }
        Err(err) => match err {
            ErrorKind::MachineError(err) => {
                log::error!("{method} {host} {url_path} machine error {err}");
                SERVER_INTERNAL_ERROR_TOTAL.inc();
                return Ok(hyper::Response::builder()
                    .status(501)
                    .body(Full::new(bytes::Bytes::from("")))
                    .unwrap());
            }
            ErrorKind::LogicalError(err) => {
                log::info!("{method} {host} {url_path} logical error {err}");
                return Ok(hyper::Response::builder()
                    .status(403)
                    .body(Full::new(bytes::Bytes::from("")))
                    .unwrap());
            }
            ErrorKind::OperateError(err) => {
                log::warn!("{method} {host} {url_path} operate error {err}");
                SERVER_INTERNAL_ERROR_TOTAL.inc();
                return Ok(hyper::Response::builder()
                    .status(500)
                    .body(Full::new(bytes::Bytes::from("")))
                    .unwrap());
            }
        },
    }
}

async fn handle_metrics_request(
    req: hyper::Request<Incoming>,
) -> Result<hyper::Response<Full<bytes::Bytes>>, Infallible> {
    if req.uri().path() == "/metrics" {
        let encoder = prometheus::TextEncoder::new();
        let mut data = Vec::new();
        let ret = match encoder.encode(&prometheus::gather(), &mut data) {
            Ok(_) => hyper::Response::builder()
                .status(200)
                .body(Full::new(bytes::Bytes::from(data)))
                .unwrap(),
            Err(err) => {
                log::error!("prometheus textencoder encode error {err}");
                hyper::Response::builder()
                    .status(500)
                    .body(Full::new(bytes::Bytes::from(
                        "server internal error".as_bytes(),
                    )))
                    .unwrap()
            }
        };
        return Ok(ret);
    }
    Ok(hyper::Response::builder()
        .status(404)
        .body(Full::new(bytes::Bytes::from("Not Found".as_bytes())))
        .unwrap())
}

struct CacheControl {
    need_cache: bool,
    max_age: u32,
}
impl Default for CacheControl {
    fn default() -> Self {
        Self {
            need_cache: true,
            max_age: 86400,
        }
    }
}
fn parse_cache_control(src: &str) -> CacheControl {
    let commands = src.split(",").collect::<Vec<&str>>();
    let mut cc = CacheControl::default();
    for command in commands.into_iter() {
        if command == "private" || command == "no-store" || command == "no-cache" {
            cc.need_cache = false;
        } else if command.starts_with("max-age=") {
            let command = &command[8..];
            if let Ok(ma) = u32::from_str_radix(command, 10) {
                cc.max_age = ma;
            }
        }
    }
    todo!()
}

pub async fn get_random(b: &mut [u8]) -> Result<(), ErrorKind> {
    let mut fd = tokio::fs::OpenOptions::new()
        .read(true)
        .open("/dev/random")
        .await
        .map_err(|err| ErrorKind::MachineError(format!("open /dev/random failed {err}")))?;
    fd.read_exact(b)
        .await
        .map_err(|err| ErrorKind::MachineError(format!("read /dev/random error {err}")))?;
    Ok(())
}

pub fn conver_header_map_to_hashmap(
    src: hyper::HeaderMap,
) -> Result<std::collections::HashMap<String, Vec<String>>, ErrorKind> {
    let mut ret: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    src.into_iter().all(|(k, v)| {
        if k.is_none() {
            return true;
        }
        match v.to_str() {
            Ok(value) => {
                let value = value.to_string();
                let k = k.unwrap().to_string();
                match ret.get_mut(&k) {
                    Some(vlist) => {
                        vlist.push(value);
                    }
                    None => {
                        ret.insert(k, vec![value]);
                    }
                }
            }
            Err(_) => {}
        }
        true
    });
    Ok(ret)
}
pub fn conver_hashmap_to_header_map(
    src: std::collections::HashMap<String, Vec<String>>,
) -> hyper::HeaderMap {
    let mut ret = hyper::HeaderMap::new();
    src.into_iter().all(|(k, v)| {
        for v in v {
            ret.append(
                hyper::header::HeaderName::from_str(&k).unwrap(),
                hyper::header::HeaderValue::from_str(&v).unwrap(),
            );
        }
        true
    });
    ret
}

pub fn parse_duration<T: Into<String>>(src: T) -> Result<std::time::Duration, &'static str> {
    let src: String = src.into();
    let src = src.as_str();
    let mut index = 0;

    for k in src.as_bytes().iter() {
        if *k < b'0' || *k > b'9' {
            let number = &src[..index];
            let unit = &src[index..];
            let number = i64::from_str_radix(number, 10).unwrap();
            match unit {
                "s" => {
                    return Ok(std::time::Duration::from_secs(number as u64));
                }
                "m" => {
                    return Ok(std::time::Duration::from_secs(number as u64 * 60));
                }
                "h" => {
                    return Ok(std::time::Duration::from_secs(number as u64 * 60 * 60));
                }
                "d" => {
                    return Ok(std::time::Duration::from_secs(number as u64 * 60 * 60 * 24));
                }
                _ => return Err("unit only can be s,h,m,d"),
            }
        }
        index = index + 1;
    }
    return Err("not set unit");
}

fn parse_range(rng: &str) -> (i64, i64) {
    let mut start = -1;
    let mut end = -1;
    if let Some(pos) = rng.find("bytes=") {
        let rng = &rng[pos + 6..];
        let rng = rng.trim();
        if let Some(pos) = rng.find('-') {
            let s1 = (&rng[..pos]).trim();
            let s2 = (&rng[pos + 1..]).trim();
            if !s1.is_empty() {
                start = i64::from_str_radix(s1, 10).map_or(-1, |v| v);
            }
            if !s2.is_empty() {
                end = i64::from_str_radix(s2, 10).map_or(-1, |v| v);
            }
        }
    }

    (start, end)
}

pub trait RequestHandler {
    fn handle(self) -> ServiceHandleFunc;
}

impl RequestHandler for types::acl::Acl {
    fn handle(self) -> ServiceHandleFunc {
        match self {
            types::acl::Acl::Proxy(proxy) => proxy.handle(),
            types::acl::Acl::Cache(cache) => cache.handle(),
        }
    }
}

impl RequestHandler for types::acl::Cache {
    fn handle(self) -> ServiceHandleFunc {
        Box::new(move |server, req| Box::pin(async move { server.hanlde_cache_request(req).await }))
    }
}

impl RequestHandler for types::acl::Proxy {
    fn handle(self) -> ServiceHandleFunc {
        todo!()
    }
}
#[derive(Default)]
struct ShareState {
    upstream_lock: Mutex<std::collections::HashMap<types::Hash, UpstreamStat>>, /*正在回源的hashkey */
}
#[derive(Default)]
struct UpstreamStat {
    is_released: std::sync::atomic::AtomicBool,
    fail: std::sync::atomic::AtomicBool, /*回源是否失败 */
    notifier: tokio::sync::Notify,
}

impl UpstreamStat {
    async fn release(&self, failed: bool) {
        if self.is_released.load(std::sync::atomic::Ordering::Acquire) {
            panic!("upstream lock released by more than one time")
        }
        self.is_released
            .store(true, std::sync::atomic::Ordering::Release);
        self.fail
            .store(failed, std::sync::atomic::Ordering::Release);
        self.notifier.notify_waiters();
    }
    async fn wait(&self) -> bool {
        if !self.is_released.load(std::sync::atomic::Ordering::Acquire) {
            return self.fail.load(std::sync::atomic::Ordering::Acquire);
        }
        self.notifier.notified().await;
        if !self.is_released.load(std::sync::atomic::Ordering::Acquire) {
            panic!("upstream lock is not released")
        }
        self.fail.load(std::sync::atomic::Ordering::Acquire)
    }
}

struct ShareStateGuard<'a> {
    ss: &'a ShareState,
    key: String,
}

impl ShareState {
    async fn try_upstream(&self, key: types::Hash) -> bool {
        let mut l = self.upstream_lock.lock().await;
        if l.contains_key(&key) {
            return false;
        } else {
            l.insert(key, UpstreamStat::default());
            return true;
        }
    }
    async fn release_upstream(&self, key: &types::Hash, failed: bool) {
        let mut l = self.upstream_lock.lock().await;
        let val = l.remove(key);
        if val.is_none() {
            panic!("lock {key} is unsafely, lock release by other");
        }
        let val = val.unwrap();
        val.release(failed).await;
    }
    async fn wait_upstream(&self, key: &types::Hash) -> bool {
        let l = self.upstream_lock.lock().await;
        let stat = l.get(key);
        if stat.is_none() {
            return false;
        }
        let stat = stat.unwrap();
        stat.wait().await
    }
}
