use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::PortResult;
use crate::ids::AgentId;
use crate::model::Harness;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub agent: AgentId,
    pub harness: Harness,
    pub cwd: PathBuf,
    pub program: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Identifies a launched session, for example a tmux session name or a PID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchHandle {
    pub kind: String,
    pub id: String,
}

/// Starts and stops harness sessions on one host.
#[async_trait::async_trait]
pub trait Launcher: Send + Sync + std::fmt::Debug {
    fn kind(&self) -> &'static str;
    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle>;
    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()>;
    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool>;
    /// The command that attaches a terminal to the session, if any.
    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>>;
    /// The last `lines` lines of the session screen, for a launch that did not
    /// register. A launcher without a screen returns `None`.
    fn capture(&self, _handle: &LaunchHandle, _lines: usize) -> Option<String> {
        None
    }
}
