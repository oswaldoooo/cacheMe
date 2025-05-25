use crate::ErrorKind;
use async_trait::async_trait;
use lazy_static::lazy_static;
use md5::Digest;
use std::{io::Write, os::unix::fs::MetadataExt, path::Path, pin::Pin, str::FromStr, task::Poll};
use tokio::io::{AsyncReadExt, AsyncSeekExt, ReadBuf};
// 通用磁盘实现
pub struct CommonDisk {
    available: std::sync::atomic::AtomicBool,
    water_level: f64,
    parent_dir: String,
    avaiable_size: std::sync::atomic::AtomicUsize,
    total_size: usize,
    error_count: usize,
}
lazy_static! {
    static ref COMMONDISK_GET_CONTENT_TOTAL: prometheus::Counter = prometheus::register_counter!(
        "common_disk_get_content_total",
        "common disk get content total"
    )
    .unwrap();
}
impl TryFrom<(&str, f64)> for CommonDisk {
    type Error = ErrorKind;

    fn try_from(value: (&str, f64)) -> Result<Self, Self::Error> {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let _ = std::fs::create_dir_all(value.0);
        let sp = Path::new(value.0);
        let mut root_total_size = 0;
        let root_avaiable_size = std::sync::atomic::AtomicUsize::new(0);
        for disk in disks.iter() {
            let mount_path = disk.mount_point();
            if sp.starts_with(mount_path) {
                return Ok(Self {
                    available: std::sync::atomic::AtomicBool::new(true),
                    water_level: value.1,
                    parent_dir: value.0.to_string(),
                    avaiable_size: std::sync::atomic::AtomicUsize::new(
                        disk.available_space() as usize
                    ),
                    total_size: disk.total_space() as usize,
                    error_count: 0,
                });
            } else if mount_path.to_str().unwrap() == "/" {
                root_avaiable_size.store(
                    disk.available_space() as usize,
                    std::sync::atomic::Ordering::Release,
                );
                root_total_size = disk.total_space() as usize;
            }
        }
        return Ok(Self {
            available: std::sync::atomic::AtomicBool::new(true),
            water_level: value.1,
            parent_dir: value.0.to_string(),
            avaiable_size: root_avaiable_size,
            total_size: root_total_size,
            error_count: 0,
        });
    }
}

#[async_trait]
impl super::CacheZone for CommonDisk {
    ///zone是否可用
    fn is_ok(&self) -> bool {
        self.available.load(std::sync::atomic::Ordering::Acquire)
    }
    fn state(&self) -> super::CacheState {
        return super::CacheState {
            avaiable_size: self
                .avaiable_size
                .load(std::sync::atomic::Ordering::Acquire),
            total_size: self.total_size,
            error_count: self.error_count,
        };
    }
    fn get_object_path(&self, path: &str) -> String {
        Path::new(self.parent_dir.as_str())
            .join(path)
            .to_str()
            .unwrap()
            .to_string()
    }
    async fn get_object_content(
        &self,
        path: &str,
        opts: &super::GetOption,
    ) -> Result<Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>, ErrorKind> {
        let rpath = Path::new(self.parent_dir.as_str()).join(path);
        let mut fd = tokio::fs::OpenOptions::new()
            .read(true)
            .open(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("open {:?} error {err}", rpath)))?;
        if opts.rng.0 > 0 {
            let _ = fd.seek(std::io::SeekFrom::Start(opts.rng.0 as u64)).await;
        }
        if opts.rng.1 > 0 && opts.rng.1 > opts.rng.0 {
            let start = if opts.rng.0 < 0 { 0 } else { opts.rng.0 };
            let fd = fd.take((opts.rng.1 - start + 1) as u64);
            COMMONDISK_GET_CONTENT_TOTAL.inc();
            return Ok(Box::pin(fd));
        }
        COMMONDISK_GET_CONTENT_TOTAL.inc();
        Ok(Box::pin(fd))
    }
    async fn get_object_meta(&self, path: &str) -> Result<super::MetaData, ErrorKind> {
        let rpath = Path::new(self.parent_dir.as_str()).join(path);
        let metadata = tokio::fs::metadata(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("open {:?} error {err}", rpath)))?;
        Ok(super::MetaData {
            size: metadata.size(),
        })
    }
    async fn link_temp_to_object(&self, tmp_path: &str) -> Result<String, ErrorKind> {
        let mut hash = md5::Md5::new();
        let _ = hash.write_all(tmp_path.as_bytes());
        let buff: [u8; 16] = hash.finalize().into();
        let new_path = hex::encode(buff);
        let rpath = Path::new(self.parent_dir.as_str()).join(&new_path);
        tokio::fs::rename(tmp_path, &rpath).await.map_err(|err| {
            ErrorKind::MachineError(format!("rename {tmp_path} -> {:?} failed {err}", rpath))
        })?;
        match tokio::fs::metadata(&rpath).await {
            Ok(meta) => {
                let fsize = meta.size();
                let old_size = self
                    .avaiable_size
                    .fetch_add(fsize as usize, std::sync::atomic::Ordering::Release);
                if ((old_size as f64 + fsize as f64) * 100.0 / self.total_size as f64)
                    >= self.water_level
                {
                    self.available
                        .store(false, std::sync::atomic::Ordering::Release);
                }
            }
            Err(err) => {
                log::error!(
                    "get {:?} metadata for update logical size failed {err}",
                    rpath
                )
            }
        }
        Ok(new_path)
    }
    async fn remove_object(&self, path: &str) -> Result<(), ErrorKind> {
        let rpath = Path::new(self.parent_dir.as_str()).join(path);
        let mut fsize = 0;
        match tokio::fs::metadata(&rpath).await {
            Ok(meta) => {
                fsize = meta.size() as usize;
            }
            Err(err) => {
                log::error!("get metadata {:?} when remove_object error {err}", rpath);
            }
        }
        tokio::fs::remove_file(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("remove {:?} failed {err}", rpath)))?;
        if fsize > 0 {
            let old_size = self
                .avaiable_size
                .fetch_sub(fsize, std::sync::atomic::Ordering::Release);
            if ((old_size - fsize) as f64 * 100.0 / self.total_size as f64) < self.water_level {
                self.available
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        Ok(())
    }
}
