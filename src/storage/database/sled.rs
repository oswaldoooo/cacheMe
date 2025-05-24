use async_trait::async_trait;

use crate::ErrorKind;
use std::{str::FromStr, sync::Arc};
pub struct Sled {
    db: Arc<sled::Db>,
}

impl FromStr for Sled {
    type Err = ErrorKind;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let db = sled::open(s)
            .map_err(|err| ErrorKind::MachineError(format!("open sled {s} failed {err}")))?;
        Ok(Self { db: Arc::new(db) })
    }
}
#[async_trait]
impl super::Database for Sled {
    async fn add_objectmeta_with_hash(
        &self,
        hashkey: &crate::types::Hash,
        meta: &crate::types::metadata::ObjectMetaData,
    ) -> Result<(), ErrorKind> {
        let db = self.db.clone();
        let content = serde_json::to_vec(meta).map_err(|err| {
            ErrorKind::OperateError(format!("marshal metadata to json failed {err}"))
        })?;
        let hashkey = hashkey.clone();
        let _ = tokio::task::spawn_blocking(move || db.insert(hashkey, content))
            .await
            .map_err(|err| {
                ErrorKind::OperateError(format!("sled insert key value failed {err}"))
            })?;
        Ok(())
    }
    async fn get_objectmeta_with_hash(
        &self,
        hash: &crate::types::Hash,
    ) -> Result<Option<crate::types::metadata::ObjectMetaData>, ErrorKind> {
        let hashkey = hash.clone();
        let db = self.db.clone();
        let ret = tokio::task::spawn_blocking(move || db.get(hashkey))
            .await
            .map_err(|err| {
                ErrorKind::OperateError(format!("get hashkey {hash} from sled failed {err}"))
            })?
            .map_err(|err| {
                ErrorKind::OperateError(format!("get hashkey {hash} from sled failed {err}"))
            })?;
        Ok(match ret {
            Some(value) => Some(serde_json::from_slice(&value).map_err(|err| {
                ErrorKind::OperateError(format!("dirty data {hash}, parse to object failed {err}"))
            })?),
            None => None,
        })
    }
    async fn remove_objectmeta_with_hash(
        &self,
        hash: &crate::types::Hash,
    ) -> Result<(), ErrorKind> {
        let hashkey = hash.to_string();
        let db = self.db.clone();
        let _ = tokio::task::spawn_blocking(move || db.remove(&hashkey))
            .await
            .map_err(|err| ErrorKind::OperateError(format!("sled remove {hash} error {err}")))?;
        Ok(())
    }
    async fn run_gc(&self) -> Result<(), ErrorKind> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let timestamp = chrono::Utc::now().timestamp();
            let gc_objs = db
                .iter()
                .filter_map(|ret| match ret {
                    Ok((k, v)) => {
                        let data =
                            serde_json::from_slice::<crate::types::metadata::ObjectMetaData>(&v);
                        if let Err(_) = &data {
                            let key = unsafe { std::str::from_utf8_unchecked(&k) }.to_string();
                            log::error!("dirty data {key}");
                            return Some(key);
                        }
                        let data = data.unwrap();
                        if (data.first_time_ref + data.expire) > timestamp {
                            return None;
                        }
                        Some(unsafe { std::str::from_utf8_unchecked(&k) }.to_string())
                    }
                    Err(_) => None,
                })
                .collect::<Vec<String>>();
            if gc_objs.is_empty() {
                return Ok::<(), ErrorKind>(());
            }
            let mut batch = sled::Batch::default();
            gc_objs.into_iter().all(|k| {
                batch.remove(k.as_str());
                true
            });
            db.apply_batch(batch).map_err(|err| {
                ErrorKind::OperateError(format!("sled run batch delete in gc failed {err}"))
            })
        })
        .await
        .map_err(|err| ErrorKind::OperateError(format!("run blocking failed {err}")))?
    }
}
