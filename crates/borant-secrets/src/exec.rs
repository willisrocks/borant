//! The exec store: a ref goes to a secrets plugin of the exec protocol. The
//! plugin is an outside program, so only the runner sees its reply; this
//! store turns the values of one reply into `Secret`s and names only refs in
//! its errors.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use borant_core::ports::secrets::{Resolved, Secret, SecretCapabilities, SecretRef, SecretStore};
use borant_core::ports::{PortError, PortResult};
use borant_exec::{ExecConfig, call};
use serde_json::{Value, json};

/// Asks the plugin for all refs of a call at once, as the protocol defines
/// `resolve`. The plugin starts for each call.
#[derive(Clone)]
pub struct ExecStore {
    cfg: ExecConfig,
}

impl ExecStore {
    /// The caller has already run `describe`, as `open_store` does.
    pub fn new(cfg: ExecConfig) -> Self {
        Self { cfg }
    }
}

/// Prints the argv, the env key names, and the credential file name, never a
/// value.
impl fmt::Debug for ExecStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExecStore")
            .field("command", &self.cfg.command)
            .field("env_keys", &self.cfg.env.keys().collect::<Vec<_>>())
            .field(
                "credential",
                &self.cfg.credential.as_ref().map(|(_, name)| name),
            )
            .field("timeout", &self.cfg.timeout)
            .finish()
    }
}

#[async_trait]
impl SecretStore for ExecStore {
    fn kind(&self) -> &'static str {
        "exec"
    }

    fn capabilities(&self) -> SecretCapabilities {
        SecretCapabilities { resolve: true }
    }

    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved> {
        if refs.is_empty() {
            return Ok(Resolved::default());
        }
        let asked: Vec<&str> = refs.iter().map(|r| r.0.as_str()).collect();
        let reply = call(&self.cfg, "resolve", json!({ "refs": asked })).await?;
        let values = reply
            .get("values")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                PortError::other("the resolve reply of the secrets plugin has no values object")
            })?;
        let mut resolved = BTreeMap::new();
        for r in refs {
            let value = values
                .get(r.0.as_str())
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    PortError::not_found(format!("the secrets plugin has no value for {:?}", r.0))
                })?;
            resolved.insert(r.clone(), Secret::new(value.to_owned()));
        }
        Ok(Resolved { values: resolved })
    }
}
