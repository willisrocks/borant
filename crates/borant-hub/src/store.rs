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

/// A write of a key the backend cannot hold is a bug in the writer, so it
/// fails with the key named instead of being dropped.
fn refuse_key(key: &str) -> Result<(), ApiError> {
    if borant_core::keys::valid_key(key) {
        return Ok(());
    }
    Err(ApiError::internal(format!(
        "refusing to write the key {key:?}, which the store cannot hold"
    )))
}

impl Store {
    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<Doc<T>>, ApiError> {
        // A key that the backend cannot hold reads as absent, with a debug
        // line: a record with an empty or odd part must not fail a pass.
        if !borant_core::keys::valid_key(key) {
            tracing::debug!(key = %key, "skipping a key the store refuses");
            return Ok(None);
        }
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
        refuse_key(key)?;
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
        refuse_key(key)?;
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
        // A key the backend cannot hold has nothing to delete.
        if !borant_core::keys::valid_key(key) {
            tracing::debug!(key = %key, "skipping a delete of a key the store refuses");
            return Ok(());
        }
        match self.0.delete(key, None).await {
            Ok(()) => Ok(()),
            Err(e) if e.is(PortErrorKind::NotFound) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn list<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<Doc<T>>, ApiError> {
        // A prefix the backend cannot hold lists nothing.
        if !borant_core::keys::valid_key(prefix.trim_end_matches('.')) {
            tracing::debug!(prefix = %prefix, "skipping a list of a prefix the store refuses");
            return Ok(Vec::new());
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::ports::StateStore;
    use borant_memory::MemoryState;

    fn store() -> (Store, std::sync::Arc<MemoryState>) {
        let state = std::sync::Arc::new(MemoryState::new());
        (Store(state.clone()), state)
    }

    /// Rule: a key the backend cannot hold reads as absent and deletes as
    /// nothing, with a debug line, so a record with an empty or odd part
    /// cannot fail a pass that reads every key.
    #[tokio::test]
    async fn store_skips_keys_the_backend_refuses() {
        let (store, _state) = store();
        for key in ["", "alert.", ".alert", "a..b", "a b"] {
            assert!(
                store.get::<serde_json::Value>(key).await.unwrap().is_none(),
                "{key:?} reads as absent"
            );
            store
                .delete(key)
                .await
                .expect("a bad key has nothing to delete");
        }
        // A valid key still works.
        store
            .put("agent.a1", &serde_json::json!({ "a": 1 }), None)
            .await
            .expect("a valid key");
        assert!(
            store
                .get::<serde_json::Value>("agent.a1")
                .await
                .unwrap()
                .is_some()
        );
    }

    /// Rule: a write of a key the backend cannot hold names the key and fails,
    /// because a writer that builds one is a bug.
    #[tokio::test]
    async fn store_refuses_to_write_a_key_the_backend_refuses() {
        let (store, state) = store();
        let err = store
            .put("alert.", &serde_json::json!({}), None)
            .await
            .expect_err("a bad key");
        assert!(err.message.contains("alert."), "{err}");
        let err = store
            .upsert("spawn.", &serde_json::json!({}))
            .await
            .expect_err("a bad key");
        assert!(err.message.contains("spawn."), "{err}");
        assert!(
            state.list("").await.unwrap().is_empty(),
            "nothing was written"
        );
    }
}
