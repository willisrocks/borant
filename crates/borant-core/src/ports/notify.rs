//! Tells the user about events that need a human.

use serde::{Deserialize, Serialize};

use super::PortResult;

/// How much a notice wants attention.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Default,
    High,
    Urgent,
}

/// One notice for a person. An adapter maps it to its own format: the desktop
/// notifier uses the title and the body, a phone notifier may use the
/// priority, the tags, and the link.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub priority: Priority,
    /// Free-form tags, for example a channel and a kind.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Where a person can read more, for example the dashboard.
    #[serde(default)]
    pub link: Option<String>,
}

impl Notice {
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            ..Self::default()
        }
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_link(mut self, link: impl Into<String>) -> Self {
        self.link = Some(link.into());
        self
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.tags = tags.into_iter().collect();
        self
    }
}

#[async_trait::async_trait]
pub trait Notifier: Send + Sync + std::fmt::Debug {
    async fn notify(&self, notice: &Notice) -> PortResult<()>;
}
