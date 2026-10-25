//! The env store: a ref is a variable name of the host process.

use std::collections::BTreeMap;
use std::fmt;

use async_trait::async_trait;
use borant_core::ports::secrets::{Resolved, Secret, SecretCapabilities, SecretRef, SecretStore};
use borant_core::ports::{PortError, PortResult};

/// Reads the environment of the host process. The host resolves at launch, so
/// a variable set after the daemon started is visible.
#[derive(Clone)]
pub struct EnvStore {
    vars: Vars,
}

#[derive(Clone)]
enum Vars {
    Process,
    /// A fixed map instead of the process environment.
    Fixed(BTreeMap<String, String>),
}

impl Default for EnvStore {
    fn default() -> Self {
        Self {
            vars: Vars::Process,
        }
    }
}

impl EnvStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A fixed set of variables. Tests use this; a daemon uses `new`.
    pub fn from_vars(vars: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            vars: Vars::Fixed(vars.into_iter().collect()),
        }
    }

    fn lookup(&self, name: &str) -> Option<String> {
        match &self.vars {
            Vars::Process => std::env::var(name).ok(),
            Vars::Fixed(vars) => vars.get(name).cloned(),
        }
    }
}

/// The variable count only: the map holds values.
impl fmt::Debug for EnvStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.vars {
            Vars::Process => f.write_str("EnvStore { process environment }"),
            Vars::Fixed(vars) => write!(f, "EnvStore {{ {} fixed vars }}", vars.len()),
        }
    }
}

#[async_trait]
impl SecretStore for EnvStore {
    fn kind(&self) -> &'static str {
        "env"
    }

    fn capabilities(&self) -> SecretCapabilities {
        SecretCapabilities { resolve: true }
    }

    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved> {
        let mut values = BTreeMap::new();
        for r in refs {
            let value = self
                .lookup(&r.0)
                .ok_or_else(|| PortError::not_found(format!("env var {:?} is not set", r.0)))?;
            values.insert(r.clone(), Secret::new(value));
        }
        Ok(Resolved { values })
    }
}
