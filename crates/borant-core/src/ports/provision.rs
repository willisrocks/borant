//! The provisioner port. Only the hub calls it. Size, region, and image are
//! opaque strings that only the backend reads, so the core never names a
//! vendor or a shape of one.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::HostName;
use crate::ports::{PortError, PortResult};
use crate::time::Timestamp;

pub use crate::ids::{PoolName, Tag};

/// The backend's own id for a host, for example a server id. Opaque here.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderHostId(pub String);

/// What the hub asks a backend to create. The hub chooses the name, so a
/// retried create returns the same host.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    pub pool: PoolName,
    pub name: HostName,
    /// The size, for example `small`. Only the backend reads it.
    pub size: String,
    /// The region, for example `r1`. Only the backend reads it.
    pub region: String,
    /// `None` asks for the default image of the backend.
    #[serde(default)]
    pub image: Option<String>,
    /// Backend metadata, not placement tags. The adapter adds its own
    /// ownership labels on top.
    #[serde(default, rename = "labels")]
    pub provider_labels: BTreeMap<String, String>,
    /// The tags of the pool. The new host reports them in `HostInfo.tags`.
    #[serde(default)]
    pub tags: Vec<Tag>,
    /// Values the new host reads at boot: its name, its pool, and one-time
    /// join data. Adapters must never log them, and `Debug` prints the keys
    /// only.
    #[serde(default)]
    pub boot: BTreeMap<String, String>,
}

/// The keys of `boot`, for a `Debug` output that holds no value.
struct BootKeys<'a>(&'a BTreeMap<String, String>);

impl fmt::Debug for BootKeys<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl fmt::Debug for HostSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostSpec")
            .field("pool", &self.pool)
            .field("name", &self.name)
            .field("size", &self.size)
            .field("region", &self.region)
            .field("image", &self.image)
            .field("provider_labels", &self.provider_labels)
            .field("tags", &self.tags)
            .field("boot", &BootKeys(&self.boot))
            .finish()
    }
}

/// A host as the backend reports it. The hub keeps its own view at the
/// `elastic.<host>` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRecord {
    pub id: ProviderHostId,
    pub pool: PoolName,
    pub name: HostName,
    pub state: ProviderState,
    /// Unix ms.
    pub created: Timestamp,
    /// Backend metadata, the same as `HostSpec.provider_labels` plus the
    /// ownership labels that the adapter adds.
    pub provider_labels: BTreeMap<String, String>,
}

/// The lifecycle state of a host, as the backend names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderState {
    Creating,
    Running,
    Stopping,
    Stopped,
    Deleting,
    /// The host is unusable. The reason is local detail; the wire form is
    /// the bare `failed`, which deserializes with an empty reason.
    Failed(String),
}

const PROVIDER_STATES: &[&str] = &[
    "creating", "running", "stopping", "stopped", "deleting", "failed",
];

impl ProviderState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Deleting => "deleting",
            Self::Failed(_) => "failed",
        }
    }
}

impl Serialize for ProviderState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let name = String::deserialize(d)?;
        Ok(match name.as_str() {
            "creating" => Self::Creating,
            "running" => Self::Running,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            "deleting" => Self::Deleting,
            "failed" => Self::Failed(String::new()),
            other => return Err(D::Error::unknown_variant(other, PROVIDER_STATES)),
        })
    }
}

/// How a backend parks a host. Phase 4 never stops a host; Fly may later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    Stop,
    Suspend,
}

/// One size that a backend offers. The price is what the backend reports.
#[derive(Debug, Clone, PartialEq)]
pub struct SizeInfo {
    pub id: String,
    pub vcpus: u32,
    pub mem_mb: u32,
    pub price_hour_usd: Option<f64>,
    pub stopped_price_hour_usd: Option<f64>,
}

/// What a backend can do. The hub reads it once, at start.
#[derive(Debug, Clone, PartialEq)]
pub struct ProvisionerCapabilities {
    /// The name of the backend, for logs only: `"fake"`, `"fly"`. The core
    /// never branches on it.
    pub backend: String,
    pub sizes: Vec<SizeInfo>,
    pub regions: Vec<String>,
    /// Create until the first heartbeat.
    pub typical_start: Duration,
    /// Start after a stop, when the backend supports it.
    pub typical_resume: Option<Duration>,
    /// The granularity of the bill, an hour on some backends and a second on
    /// others.
    pub billing_step: Duration,
    /// Whether the natural shape of the backend is one host for one agent.
    pub one_host_per_agent: bool,
    /// Empty means delete only.
    pub stop_modes: Vec<StopMode>,
}

/// The backend behind one `[provisioners]` entry of `hub.toml`. One instance
/// serves several pools.
#[async_trait::async_trait]
pub trait Provisioner: Send + Sync + fmt::Debug {
    /// What the backend offers.
    async fn describe(&self) -> PortResult<ProvisionerCapabilities>;

    /// Returns when the backend accepted the order. Idempotent by pool and
    /// name, so a retried create returns the same host.
    async fn create(&self, spec: &HostSpec) -> PortResult<HostRecord>;

    /// Deletes the host and its disk. A missing host is Ok; a host that this
    /// provisioner did not create is `Denied`.
    async fn delete(&self, id: &ProviderHostId) -> PortResult<()>;

    /// The hosts of this pool that this provisioner created.
    async fn list(&self, pool: &PoolName) -> PortResult<Vec<HostRecord>>;

    /// Optional. The hub calls it only for a mode in `stop_modes`.
    async fn stop(&self, _id: &ProviderHostId, _mode: StopMode) -> PortResult<()> {
        Err(PortError::invalid("stop is not supported"))
    }

    /// Optional. The hub calls it only for a mode in `stop_modes`.
    async fn start(&self, _id: &ProviderHostId) -> PortResult<HostRecord> {
        Err(PortError::invalid("start is not supported"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> HostSpec {
        HostSpec {
            pool: "workers".parse().unwrap(),
            name: "e1".parse().unwrap(),
            size: "small".to_owned(),
            region: "r1".to_owned(),
            image: None,
            provider_labels: BTreeMap::new(),
            tags: vec!["vps".parse().unwrap()],
            boot: BTreeMap::from([("BORANT_JOIN".to_owned(), "one-time-value".to_owned())]),
        }
    }

    #[test]
    fn debug_holds_the_boot_keys_and_no_value() {
        let text = format!("{:?}", spec());
        assert!(!text.contains("one-time-value"), "{text}");
        assert!(text.contains("BORANT_JOIN"), "{text}");
    }

    #[test]
    fn state_round_trips_as_the_bare_name() {
        for state in [
            ProviderState::Creating,
            ProviderState::Running,
            ProviderState::Stopping,
            ProviderState::Stopped,
            ProviderState::Deleting,
            ProviderState::Failed("gone".to_owned()),
        ] {
            let json = serde_json::to_string(&state).unwrap();
            assert_eq!(json, format!("\"{}\"", state.as_str()));
            let back: ProviderState = serde_json::from_str(&json).unwrap();
            if let ProviderState::Failed(_) = state {
                assert_eq!(back, ProviderState::Failed(String::new()));
            } else {
                assert_eq!(back, state);
            }
        }
        assert!(serde_json::from_str::<ProviderState>("\"lost\"").is_err());
    }
}
