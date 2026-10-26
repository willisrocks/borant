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

/// The error of a conditional delete that lost: another writer took the key,
/// or the backend reports a missing key instead of a stale revision.
fn taken(key: &str) -> ApiError {
    ApiError::new(
        borant_core::proto::ApiErrorCode::Conflict,
        format!("the key {key:?} was already taken"),
    )
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

    /// Deletes the key only when it is still at `revision`, so one of two
    /// callers that read the same document wins and the other gets
    /// `Conflict`.
    ///
    /// A key that is already gone is `Conflict`, never `Ok`: a backend may
    /// report the delete of a missing key as `NotFound` rather than as a
    /// revision conflict, and a caller must not take that as a win. Only the
    /// plain [`Store::delete`] treats a missing key as nothing to do.
    pub async fn delete_at(&self, key: &str, revision: u64) -> Result<(), ApiError> {
        if !borant_core::keys::valid_key(key) {
            // The store cannot hold the key, so no caller can claim it.
            return Err(taken(key));
        }
        match self.0.delete(key, Some(revision)).await {
            Ok(()) => Ok(()),
            Err(e) if e.is(PortErrorKind::NotFound) => Err(taken(key)),
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
    use borant_core::ports::{Change, Entry, PortError, PortResult, StateStore};
    use borant_memory::MemoryState;
    use bytes::Bytes;
    use futures::stream::BoxStream;

    /// A backend that reports the conditional delete of a missing key as
    /// `NotFound`, where another reports a stale revision.
    #[derive(Debug)]
    struct Vanishing;

    #[async_trait::async_trait]
    impl StateStore for Vanishing {
        async fn get(&self, _key: &str) -> PortResult<Option<Entry>> {
            Ok(None)
        }

        async fn create(&self, _key: &str, _value: Bytes) -> PortResult<u64> {
            Ok(1)
        }

        async fn update(&self, _key: &str, _value: Bytes, _expected: u64) -> PortResult<u64> {
            Ok(2)
        }

        async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()> {
            match expected {
                None => Ok(()),
                Some(_) => Err(PortError::not_found(format!("no key {key}"))),
            }
        }

        async fn list(&self, _prefix: &str) -> PortResult<Vec<Entry>> {
            Ok(Vec::new())
        }

        async fn watch(&self, _prefix: &str) -> PortResult<BoxStream<'static, Change>> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

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

    /// Rule: a conditional delete that finds no key at all is `Conflict`, not
    /// a win, because a backend may report a missing key where another
    /// reports a stale revision.
    #[tokio::test]
    async fn a_conditional_delete_of_a_missing_key_is_conflict() {
        let conflict = borant_core::proto::ApiErrorCode::Conflict;
        let (store, _state) = store();

        // A reader reads, another writer deletes, the reader's claim loses.
        store
            .put("join.e1", &serde_json::json!({ "a": 1 }), None)
            .await
            .expect("a key");
        let doc = store
            .get::<serde_json::Value>("join.e1")
            .await
            .unwrap()
            .expect("the key");
        store.delete("join.e1").await.expect("the other writer");
        let err = store
            .delete_at("join.e1", doc.revision)
            .await
            .expect_err("a missing key is no win");
        assert_eq!(err.code, conflict, "{err}");

        // A live key: the first claim wins, the second loses.
        store
            .put("join.e2", &serde_json::json!({ "a": 1 }), None)
            .await
            .expect("a key");
        let doc = store
            .get::<serde_json::Value>("join.e2")
            .await
            .unwrap()
            .expect("the key");
        store
            .delete_at("join.e2", doc.revision)
            .await
            .expect("the winner");
        let err = store
            .delete_at("join.e2", doc.revision)
            .await
            .expect_err("the second claim loses");
        assert_eq!(err.code, conflict, "{err}");

        // A key that the store cannot hold is no claim either.
        let err = store
            .delete_at("alert.", 1)
            .await
            .expect_err("a bad key is no claim");
        assert_eq!(err.code, conflict, "{err}");

        // A backend that says NotFound for the conditional delete of a
        // missing key must not hand a caller a win either, and a plain
        // delete of nothing stays Ok.
        let vanishing = Store(Arc::new(Vanishing));
        let err = vanishing.delete_at("join.e1", 7).await.expect_err("no win");
        assert_eq!(err.code, conflict, "{err}");
        vanishing
            .delete("join.e1")
            .await
            .expect("a plain delete of nothing is Ok");
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
