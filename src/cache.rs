use crate::types::metadata::ObjectMetaData;
use crate::ErrorKind;
use std::pin::Pin;

pub struct Cache {
    database: Pin<Box<dyn crate::storage::database::Database + Sync + Send>>,
}
impl Cache {
    pub const fn new(
        db: Pin<Box<dyn crate::storage::database::Database + Sync + Send + 'static>>,
    ) -> Self {
        Self {
            database: db,
        }
    }
    pub async fn run_gc(&self)->Result<(),ErrorKind>{
        self.database.run_gc().await
    }
    pub async fn get_object(
        &self,
        hash: &crate::types::Hash,
    ) -> Result<Option<ObjectMetaData>, ErrorKind> {
        let ret = self.database.get_objectmeta_with_hash(hash).await?;
        if let Some(data) = ret {
            return Ok(Some(data));
        } else {
            return Ok(None);
        }
    }
    pub async fn put_object(&self,hashkey:&crate::types::Hash, meta: &ObjectMetaData) -> Result<(), ErrorKind> {
        self.database.add_objectmeta_with_hash(hashkey,meta).await
    }
    pub async fn remove_object(&self, hash: &crate::types::Hash) -> Result<(), ErrorKind> {
        self.database.remove_objectmeta_with_hash(hash).await
    }
}
