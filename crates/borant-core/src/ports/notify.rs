use super::PortResult;

/// Tells the user about events that need a human.
#[async_trait::async_trait]
pub trait Notifier: Send + Sync + std::fmt::Debug {
    async fn notify(&self, title: &str, body: &str) -> PortResult<()>;
}
