//! The change-request port: the state of a pull request behind neutral types.
//! The core never names a forge; an adapter, for example an exec plugin,
//! knows the vendor.

use serde::{Deserialize, Serialize};

use crate::ports::PortResult;

/// The state of one change request, as the PR watch compares and reports it.
/// The wire shape is the one of the plugin:
/// `{"state":"open","head":"<sha>","checks":{"pass":{}},"review":"none"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStatus {
    pub state: ChangeState,
    /// The head commit that the checks and the review belong to.
    pub head: String,
    pub checks: CheckState,
    pub review: ReviewState,
}

/// Whether the change is open, merged, or closed without a merge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeState {
    Open,
    Merged,
    Closed,
}

/// The checks of the head commit. `None` means that no check reported for
/// that head, which is not a pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pass {},
    Fail { failed: Vec<String> },
    Pending {},
    None {},
}

/// The review decision on the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    None,
}

/// Reads the state of a change request by its URL.
#[async_trait::async_trait]
pub trait ChangeRequests: Send + Sync + std::fmt::Debug {
    async fn status(&self, url: &str) -> PortResult<ChangeStatus>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plugin (`borant-changes-github`) emits exactly this shape.
    #[test]
    fn the_status_parses_the_plugin_json() {
        let pass: ChangeStatus = serde_json::from_str(
            r#"{"state":"open","head":"abc123","checks":{"pass":{}},"review":"none"}"#,
        )
        .unwrap();
        assert_eq!(
            pass,
            ChangeStatus {
                state: ChangeState::Open,
                head: "abc123".into(),
                checks: CheckState::Pass {},
                review: ReviewState::None,
            }
        );

        let fail: ChangeStatus = serde_json::from_str(
            r#"{"state":"open","head":"abc123","checks":{"fail":{"failed":["ci","lint"]}},"review":"changes_requested"}"#,
        )
        .unwrap();
        assert_eq!(
            fail.checks,
            CheckState::Fail {
                failed: vec!["ci".into(), "lint".into()],
            }
        );
        assert_eq!(fail.review, ReviewState::ChangesRequested);

        for (json, checks) in [
            (
                r#"{"state":"merged","head":"d","checks":{"pending":{}},"review":"approved"}"#,
                CheckState::Pending {},
            ),
            (
                r#"{"state":"closed","head":"e","checks":{"none":{}},"review":"none"}"#,
                CheckState::None {},
            ),
        ] {
            let status: ChangeStatus = serde_json::from_str(json).unwrap();
            assert_eq!(status.checks, checks);
        }

        // The round trip keeps the wire names of the plugin.
        let s = serde_json::to_string(&fail).unwrap();
        assert!(
            s.contains(r#""checks":{"fail":{"failed":["ci","lint"]}}"#),
            "{s}"
        );
        assert!(s.contains(r#""review":"changes_requested""#), "{s}");
    }
}
