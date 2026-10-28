//! The issue-tracker port on the exec plugin protocol: one program per
//! `[mirror]` entry of `hub.toml`. The program knows the tracker and holds its
//! credential; the hub names neither, and nothing of a reply reaches a log
//! line or an error message.

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::ports::{IssueRef, IssueTracker, IssueWrite, PortError, PortResult};
use serde_json::{Value, json};

use crate::{ExecConfig, VERSION, call};

/// The kind that an issue-tracker plugin reports in `describe`.
pub const KIND: &str = "issues";

/// The limit of the `describe` handshake: a plugin reads its credential and
/// answers, or it does not.
pub const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The limit of a `create` or an `update`: the plugin talks to the tracker.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// An issue tracker that is an exec plugin program.
#[derive(Debug)]
pub struct ExecIssues {
    exec: ExecConfig,
    /// The `config` of the adapter, in every request, `describe` included.
    config: BTreeMap<String, Value>,
}

impl ExecIssues {
    /// The runner of one program, with its `config` in every request.
    pub fn new(exec: ExecConfig, config: BTreeMap<String, Value>) -> Self {
        Self { exec, config }
    }

    /// Runs the `describe` handshake and returns the adapter, so a program
    /// that is not an issue-tracker plugin leaves the mirror off instead of
    /// failing every sweep.
    pub async fn open(exec: ExecConfig, config: BTreeMap<String, Value>) -> PortResult<Self> {
        // `describe` carries the config, as a provisioner's does, so the
        // shared `describe` helper, which sends none, does not fit.
        let args = json!({ "config": &config });
        let reply = call(&with_timeout(&exec, DESCRIBE_TIMEOUT), "describe", args).await?;
        let version = reply.get("v").and_then(Value::as_u64);
        let kind = reply.get("kind").and_then(Value::as_str);
        if version != Some(VERSION) || kind != Some(KIND) {
            return Err(PortError::invalid(format!(
                "the program is not an {KIND} plugin of protocol version {VERSION}"
            )));
        }
        Ok(Self::new(exec, config))
    }

    /// One op with its own timeout. A reply of the wrong shape is `Other`: a
    /// fault of the plugin may pass next time, while `invalid` skips the item.
    async fn request(&self, op: &str, args: Value, timeout: Duration) -> PortResult<IssueRef> {
        let reply = call(&with_timeout(&self.exec, timeout), op, args).await?;
        // The message holds nothing of the reply.
        serde_json::from_value(reply).map_err(|_| {
            PortError::other("the reply of the issues plugin does not match the protocol")
        })
    }
}

/// The runner of one op: its timeout, and no stderr text, because the plugin
/// holds a credential and may print it.
fn with_timeout(exec: &ExecConfig, timeout: Duration) -> ExecConfig {
    ExecConfig {
        timeout,
        log_stderr: false,
        ..exec.clone()
    }
}

#[async_trait::async_trait]
impl IssueTracker for ExecIssues {
    async fn create(&self, w: &IssueWrite) -> PortResult<IssueRef> {
        let args = json!({ "config": &self.config, "write": w });
        self.request("create", args, WRITE_TIMEOUT).await
    }

    async fn update(&self, issue: &str, w: &IssueWrite, full: bool) -> PortResult<IssueRef> {
        let args = json!({
            "config": &self.config,
            "issue": issue,
            "write": w,
            "full": full,
        });
        self.request("update", args, WRITE_TIMEOUT).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_default_timeouts() {
        assert_eq!(KIND, "issues");
        assert_eq!(DESCRIBE_TIMEOUT, Duration::from_secs(10));
        assert_eq!(WRITE_TIMEOUT, Duration::from_secs(30));
    }
}
