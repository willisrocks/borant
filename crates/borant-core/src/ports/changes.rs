//! The change-request port: the state of a pull request behind neutral types.
//! The core never names a forge; an adapter, for example an exec plugin,
//! knows the vendor.

use serde::{Deserialize, Serialize};

use crate::ports::PortResult;

/// The state of one change request, as the PR watch compares and reports it.
/// The wire shape is the one of the plugin:
/// `{"state":"open","head":"<sha>","checks":{"pass":{}},"review":"none"}`.
/// An adapter that counts the checks adds `checks_passed` and
/// `checks_total`, and one that knows of a requested review adds
/// `review_requested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeStatus {
    pub state: ChangeState,
    /// The head commit that the checks and the review belong to.
    pub head: String,
    pub checks: CheckState,
    pub review: ReviewState,
    /// How many checks of the head passed, when the adapter counts them. An
    /// adapter without counts leaves both empty, and a reader shows the
    /// state alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_passed: Option<u32>,
    /// How many checks reported on the head, when the adapter counts them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_total: Option<u32>,
    /// A review was requested and none arrived yet. An adapter without the
    /// fact leaves it false.
    #[serde(default)]
    pub review_requested: bool,
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
                checks_passed: None,
                checks_total: None,
                review_requested: false,
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
        // An adapter that leaves the counts out writes nothing for them.
        assert!(!s.contains("checks_passed"), "{s}");
        assert!(!s.contains("checks_total"), "{s}");

        // An adapter that counts the checks and sees a requested review
        // reports both, and they parse back.
        let counted: ChangeStatus = serde_json::from_str(
            r#"{"state":"open","head":"f","checks":{"pending":{}},"review":"none","checks_passed":2,"checks_total":3,"review_requested":true}"#,
        )
        .unwrap();
        assert_eq!((counted.checks_passed, counted.checks_total), (Some(2), Some(3)));
        assert!(counted.review_requested);
        let s = serde_json::to_string(&counted).unwrap();
        assert!(s.contains(r#""checks_passed":2"#), "{s}");
        assert!(s.contains(r#""checks_total":3"#), "{s}");
        assert!(s.contains(r#""review_requested":true"#), "{s}");
    }
}
