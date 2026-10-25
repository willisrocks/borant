//! The secret store port. Only the host daemon calls it. The hub gets bundle
//! and member names, never a value.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::ports::PortResult;

pub use crate::ids::{BundleName, MemberName};

/// A reference that one backend understands, for example
/// `op://Borant/github/token`, a key, or a file name. Never parsed here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretRef(pub String);

/// A secret value. There is no `Serialize` and no `Clone`, so the compiler
/// stops a value in any wire type; `Debug` prints `Secret(..)`; the memory is
/// zeroed on drop.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }

    /// The only reader of the value. Review greps for `expose`.
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

/// The env vars, files, and `pre` commands of one bundle, as `host.toml`
/// defines it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Bundle {
    pub name: BundleName,
    /// A store in `host.toml`.
    pub store: String,
    /// `ENV_NAME -> ref`.
    #[serde(default)]
    pub vars: BTreeMap<String, SecretRef>,
    /// File name -> ref.
    #[serde(default)]
    pub files: BTreeMap<String, SecretRef>,
    /// Commands that run before the harness, each one argv, with the vars set.
    #[serde(default)]
    pub pre: Vec<Vec<String>>,
    /// Env vars of each member. Member vars win over `vars`.
    #[serde(default)]
    pub members: BTreeMap<MemberName, BTreeMap<String, SecretRef>>,
}

/// What a store can do. Only resolver mode exists in phase 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretCapabilities {
    pub resolve: bool,
}

/// The values of one `resolve` call, one for each requested ref.
#[derive(Debug, Default)]
pub struct Resolved {
    pub values: BTreeMap<SecretRef, Secret>,
}

#[async_trait::async_trait]
pub trait SecretStore: Send + Sync + fmt::Debug {
    fn kind(&self) -> &'static str;

    fn capabilities(&self) -> SecretCapabilities;

    /// All refs or an error. One missing ref fails the whole call.
    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved>;
}
