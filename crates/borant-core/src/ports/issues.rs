//! The issue-tracker port: the mirror writes one issue per mirrored item
//! through neutral types. The core never names a tracker or a team; an
//! adapter, for example an exec plugin, knows the vendor.

use serde::{Deserialize, Serialize};

use crate::ports::PortResult;

/// The neutral state of an issue. An adapter maps it to the first workflow
/// state of the matching type of its backend, so a tracker with custom states
/// still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueState {
    Backlog,
    Todo,
    Started,
    Done,
    Canceled,
}

/// What the mirror writes. The wire shape is the one of the plugin:
/// `{"key":"<uuid>","target":null,"parent":null,"title":"..","body":"..",
/// "state":"todo"}`. `target` and `parent` are always present, as `null` when
/// unset, so the plugin reads one key set for every write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueWrite {
    /// A v4-shaped UUID, so a create that is sent twice makes one issue.
    pub key: String,
    /// The team of the issue; `None` takes the team of the parent issue.
    pub target: Option<String>,
    /// The key of the parent issue.
    pub parent: Option<String>,
    pub title: String,
    pub body: String,
    pub state: IssueState,
}

/// The issue that an adapter created or updated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueRef {
    pub key: String,
    pub url: String,
}

/// Creates and updates the issues of the mirror. The hub owns the record and
/// the link; an adapter writes only the issue itself.
#[async_trait::async_trait]
pub trait IssueTracker: Send + Sync + std::fmt::Debug {
    /// Creates the issue of `w`, or returns the issue that an earlier create
    /// with the same `key` made. A write with neither a target nor a parent
    /// is `Invalid`.
    async fn create(&self, w: &IssueWrite) -> PortResult<IssueRef>;

    /// Updates the issue named by a key or a URL. `full` false writes the
    /// state only (an adopted issue).
    async fn update(&self, issue: &str, w: &IssueWrite, full: bool) -> PortResult<IssueRef>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The write is the one of the plugin: every field present, and `null`
    /// for an unset target and parent.
    #[test]
    fn issues_write_has_the_wire_shape() {
        let w = IssueWrite {
            key: "3f2b8c1e-9d4a-4f6b-8c2d-1e5a7b9c0d3f".into(),
            target: None,
            parent: Some("ENG-1".into()),
            title: "Mirror the sagas".into(),
            body: "Body".into(),
            state: IssueState::Todo,
        };
        assert_eq!(
            serde_json::to_string(&w).expect("json"),
            r#"{"key":"3f2b8c1e-9d4a-4f6b-8c2d-1e5a7b9c0d3f","target":null,"parent":"ENG-1","title":"Mirror the sagas","body":"Body","state":"todo"}"#
        );

        for (state, wire) in [
            (IssueState::Backlog, "backlog"),
            (IssueState::Todo, "todo"),
            (IssueState::Started, "started"),
            (IssueState::Done, "done"),
            (IssueState::Canceled, "canceled"),
        ] {
            assert_eq!(serde_json::to_value(state).expect("json"), wire);
        }

        let r: IssueRef = serde_json::from_str(r#"{"key":"ENG-1","url":"u"}"#).expect("a ref");
        assert_eq!(r.key, "ENG-1");
        assert_eq!(r.url, "u");
    }
}
