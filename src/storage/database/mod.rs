use crate::types::metadata::ObjectMetaData;
use crate::ErrorKind;
use async_trait::async_trait;
pub mod sled;
#[async_trait]
pub trait Database {
    async fn add_objectmeta_with_hash(&self,hashkey:&crate::types::Hash, meta: &ObjectMetaData) -> Result<(), ErrorKind>;
    async fn get_objectmeta_with_hash(
        &self,
        hash: &crate::types::Hash,
    ) -> Result<Option<ObjectMetaData>, ErrorKind>;
    async fn remove_objectmeta_with_hash(&self, hash: &crate::types::Hash)
        -> Result<(), ErrorKind>;
    async fn run_gc(&self) -> Result<(), ErrorKind>;
}

