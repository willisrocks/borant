use bytes::Bytes;
use futures::stream::BoxStream;

use super::PortResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: Bytes,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Put(Entry),
    Delete { key: String },
}

/// Key-value documents with revisions and compare-and-set. Keys use dots.
#[async_trait::async_trait]
pub trait StateStore: Send + Sync + std::fmt::Debug {
    async fn get(&self, key: &str) -> PortResult<Option<Entry>>;
    /// Fails with `Conflict` if the key exists.
    async fn create(&self, key: &str, value: Bytes) -> PortResult<u64>;
    /// Compare-and-set: fails with `Conflict` if the current revision is not
    /// `expected`.
    async fn update(&self, key: &str, value: Bytes, expected: u64) -> PortResult<u64>;
    /// Deletes the key. With `expected`, fails with `Conflict` on another
    /// revision. A missing key is not an error.
    async fn delete(&self, key: &str, expected: Option<u64>) -> PortResult<()>;
    /// All entries whose keys start with `prefix`, in key order.
    async fn list(&self, prefix: &str) -> PortResult<Vec<Entry>>;
    /// Changes to keys that start with `prefix`, from now on.
    async fn watch(&self, prefix: &str) -> PortResult<BoxStream<'static, Change>>;
}
