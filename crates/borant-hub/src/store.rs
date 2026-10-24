//! Typed JSON access to the state store.

use std::sync::Arc;

use borant_core::ports::{PortErrorKind, StateStore};
use borant_core::proto::ApiError;
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Clone)]
pub(crate) struct Store(pub Arc<dyn StateStore>);

pub(crate) struct Doc<T> {
    pub key: String,
    pub value: T,
    pub revision: u64,
}

impl Store {
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<Doc<T>>, ApiError> {
        let Some(e) = self.0.get(key).await? else {
            return Ok(None);
        };
        let value = serde_json::from_slice(&e.value)
            .map_err(|err| ApiError::internal(format!("bad document {key}: {err}")))?;
        Ok(Some(Doc {
            key: e.key,
            value,
            revision: e.revision,
        }))
    }

    /// Creates the key when `revision` is `None`, else updates it with
    /// compare-and-set.
    pub async fn put<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        revision: Option<u64>,
    ) -> Result<u64, ApiError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|e| ApiError::internal(format!("encode {key}: {e}")))?;
        Ok(match revision {
            None => self.0.create(key, bytes.into()).await?,
            Some(r) => self.0.update(key, bytes.into(), r).await?,
        })
    }

    /// Writes the key whatever its current revision is. Only the hub writes,
    /// so a conflict here means a retry is safe.
    pub async fn upsert<T: Serialize>(&self, key: &str, value: &T) -> Result<u64, ApiError> {
        for _ in 0..3 {
            let rev = self.0.get(key).await?.map(|e| e.revision);
            match self.put(key, value, rev).await {
                Err(e) if e.code == borant_core::proto::ApiErrorCode::Conflict => continue,
                other => return other,
            }
        }
        Err(ApiError::internal(format!("could not write {key}")))
    }

    pub async fn delete(&self, key: &str) -> Result<(), ApiError> {
        match self.0.delete(key, None).await {
            Ok(()) => Ok(()),
            Err(e) if e.is(PortErrorKind::NotFound) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<Doc<T>>, ApiError> {
        let mut out = Vec::new();
        for e in self.0.list(prefix).await? {
            match serde_json::from_slice(&e.value) {
                Ok(value) => out.push(Doc {
                    key: e.key,
                    value,
                    revision: e.revision,
                }),
                Err(err) => tracing::warn!(key = %e.key, "skip bad document: {err}"),
            }
        }
        Ok(out)
    }
}
