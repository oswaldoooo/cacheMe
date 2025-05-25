use std::{pin::Pin, sync::Arc};
pub mod disk;
use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::ErrorKind;
pub struct CacheState {
    pub avaiable_size: usize,
    pub total_size: usize,
    pub error_count: usize, /*磁盘错误次数 */
}
pub struct GetOption {
    pub rng: (i64, i64),
}
pub struct MetaData {
    pub size: u64,
}
/// 缓存区
#[async_trait]
pub trait CacheZone {
    ///zone是否可用
    fn is_ok(&self) -> bool;
    ///逻辑可用大小，非实际
    fn state(&self) -> CacheState;
    fn get_object_path(&self,path:&str)->String;
    async fn get_object_content(
        &self,
        path: &str,
        opt: &GetOption,
    ) -> Result<Pin<Box<dyn tokio::io::AsyncRead + Send + Sync>>, ErrorKind>;
    async fn get_object_meta(&self, path: &str) -> Result<MetaData, ErrorKind>;
    async fn link_temp_to_object(&self, tmp_path: &str) -> Result<String, ErrorKind>;
    async fn remove_object(&self, path: &str) -> Result<(), ErrorKind>;
}
