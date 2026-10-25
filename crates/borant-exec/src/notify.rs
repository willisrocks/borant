//! The notifier port on the exec plugin protocol: one program per
//! `[notifiers.<name>]` entry of `hub.toml` or `host.toml` (spec "Ports" in
//! docs/superpowers/specs/2026-10-25-phase-2-see-the-swarm-design.md). The
//! program gets the notice as JSON and answers `{"ok": null}` when the person
//! has it.

use borant_core::ports::{Notice, Notifier};
use borant_core::ports::{PortError, PortResult};
use serde_json::json;

use crate::{ExecConfig, call, describe};

/// A notifier that is an exec plugin program, of protocol kind `notifier`.
#[derive(Debug)]
pub struct ExecNotifier {
    exec: ExecConfig,
}

impl ExecNotifier {
    /// The caller has run `describe`, as `open_notifier` does, or calls
    /// [`ExecNotifier::check`] first.
    pub fn new(exec: ExecConfig) -> Self {
        Self { exec }
    }

    /// The `describe` handshake: the program must be a notifier plugin of this
    /// protocol version.
    pub async fn check(&self) -> PortResult<()> {
        describe(&self.exec, "notifier").await.map(|_| ())
    }
}

#[async_trait::async_trait]
impl Notifier for ExecNotifier {
    async fn notify(&self, notice: &Notice) -> PortResult<()> {
        // The wire shape of the first plugin, `borant-notify-ntfy` in
        // `borant-infra`: the notice under `notice`, the priority lowercase.
        let notice = serde_json::to_value(notice)
            .map_err(|e| PortError::invalid(format!("the notice is not JSON: {e}")))?;
        call(&self.exec, "notify", json!({ "notice": notice })).await?;
        Ok(())
    }
}
