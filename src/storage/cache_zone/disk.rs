use crate::ErrorKind;
use async_trait::async_trait;
use md5::Digest;
use std::{io::Write, os::unix::fs::MetadataExt, path::Path, pin::Pin, str::FromStr, task::Poll};
use tokio::io::{AsyncReadExt, AsyncSeekExt, ReadBuf};
// 通用磁盘实现
pub struct CommonDisk {
    available: std::sync::atomic::AtomicBool,
    parent_dir: String,
    avaiable_size: usize,
    total_size: usize,
    error_count: usize,
}

impl FromStr for CommonDisk {
    type Err = ErrorKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let disks = sysinfo::Disks::new_with_refreshed_list();
        let _ = std::fs::create_dir_all(s);
        let sp = Path::new(s);
        let mut root_total_size = 0;
        let mut root_avaiable_size = 0;
        for disk in disks.iter() {
            let mount_path = disk.mount_point();
            if sp.starts_with(mount_path) {
                return Ok(Self {
                    available: std::sync::atomic::AtomicBool::new(true),
                    parent_dir: s.to_string(),
                    avaiable_size: disk.available_space() as usize,
                    total_size: disk.total_space() as usize,
                    error_count: 0,
                });
            } else if mount_path.to_str().unwrap() == "/" {
                root_avaiable_size = disk.available_space() as usize;
                root_total_size = disk.total_space() as usize;
            }
        }
        return Ok(Self {
            available: std::sync::atomic::AtomicBool::new(true),
            parent_dir: s.to_string(),
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
            avaiable_size: self.avaiable_size,
            total_size: self.total_size,
            error_count: self.error_count,
        };
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
            return Ok(Box::pin(fd));
        }
        Ok(Box::pin(fd))
    }
    async fn get_object_meta(&self, path: &str) -> Result<super::MetaData, ErrorKind> {
        let rpath = Path::new(self.parent_dir.as_str()).join(path);
        let metadata = tokio::fs::metadata(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("open {:?} error {err}", rpath)))?;
        Ok(super::MetaData { size: metadata.size() })
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
        Ok(new_path)
    }
    async fn remove_object(&self, path: &str) -> Result<(), ErrorKind> {
        let rpath = Path::new(self.parent_dir.as_str()).join(path);
        tokio::fs::remove_file(&rpath)
            .await
            .map_err(|err| ErrorKind::MachineError(format!("remove {:?} failed {err}", rpath)))?;
        Ok(())
    }
}
