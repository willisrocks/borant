use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{PortError, PortResult};
use crate::ids::AgentId;
use crate::model::Harness;

/// What a launcher with no window of its own answers a [`Launcher::focus`]:
/// the caller prints the attach command to copy instead.
pub const FOCUS_NOT_SUPPORTED: &str = "not supported";

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
    /// The pids of the processes that the launcher started for the session of
    /// `handle`, such as the pane processes of a tmux session. The host
    /// snapshots their tree before it stops a session, because a tool can
    /// leave the tree of the harness below them. A launcher that cannot name
    /// them answers none.
    async fn session_pids(&self, _handle: &LaunchHandle) -> PortResult<Vec<u32>> {
        Ok(Vec::new())
    }
    /// Ends any session that this launcher holds for `agent`, even one whose
    /// handle the caller lost: a daemon that restarted, a launch order that
    /// never arrived, or a session of an older attempt. It ends that one named
    /// session, never a server or another agent's session, and returns whether
    /// it ended one. A launcher that cannot name the session of an agent ends
    /// nothing and answers `false`.
    async fn release(&self, _agent: &AgentId) -> PortResult<bool> {
        Ok(false)
    }
    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool>;
    /// The command that attaches a terminal to the session, if any.
    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>>;
    /// The last `lines` lines of the session screen, for a launch that did not
    /// register. A launcher without a screen returns `None`.
    fn capture(&self, _handle: &LaunchHandle, _lines: usize) -> Option<String> {
        None
    }
    /// Opens the agent's session in a new window of the person's launcher:
    /// a window of their tmux client, a focused Orca terminal, or a zellij
    /// tab (mod actions spec, "Go to an agent"). A launcher with no window of
    /// its own returns [`FOCUS_NOT_SUPPORTED`], and the caller prints the
    /// attach command to copy.
    async fn focus(&self, _agent: &AgentId) -> PortResult<()> {
        Err(PortError::invalid(FOCUS_NOT_SUPPORTED))
    }
}
