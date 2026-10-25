//! The change-request port on the exec plugin protocol: one program per
//! `[changes]` entry of `hub.toml` (spec phase 6, "PR watch"). The program
//! knows the forge and holds its token; the hub names neither.

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::ports::changes::{ChangeRequests, ChangeStatus};
use borant_core::ports::{PortError, PortResult};
use serde_json::{Value, json};

use crate::{ExecConfig, call, describe};

/// The kind that a change-request plugin reports in `describe`.
pub const KIND: &str = "change_requests";

/// The limit of one `status` op, when the caller sets none: a forge answers in
/// seconds, and the watch runs again on the next tick anyway.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(30);

/// A change-request source that is an exec plugin program.
#[derive(Debug)]
pub struct ExecChangeRequests {
    exec: ExecConfig,
    /// The `config` of the adapter, in every request.
    config: BTreeMap<String, Value>,
}

impl ExecChangeRequests {
    /// The runner of one program, with its `config` in every request.
    pub fn new(exec: ExecConfig, config: BTreeMap<String, Value>) -> Self {
        Self { exec, config }
    }

    /// Runs the `describe` handshake and returns the adapter, so a program
    /// that is not a change-request plugin leaves the watch off instead of
    /// failing every poll.
    pub async fn open(exec: ExecConfig, config: BTreeMap<String, Value>) -> PortResult<Self> {
        describe(&exec, KIND).await?;
        Ok(Self::new(exec, config))
    }
}

#[async_trait::async_trait]
impl ChangeRequests for ExecChangeRequests {
    async fn status(&self, url: &str) -> PortResult<ChangeStatus> {
        let args = json!({ "config": &self.config, "url": url });
        let value = call(&self.exec, "status", args).await?;
        // A reply of the wrong shape is `Other`, not `Invalid`: `invalid`
        // disables the plugin, while a fault of the plugin may pass next time.
        // The message holds nothing of the reply.
        serde_json::from_value(value).map_err(|_| {
            PortError::other("the status reply of the change plugin does not match the protocol")
        })
    }
}
