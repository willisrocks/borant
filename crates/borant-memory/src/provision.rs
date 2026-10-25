//! The in-memory provisioner. Tests use it, and the hub executor runs on it
//! with a fake clock.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::ids::PoolName;
use borant_core::ports::provision::{
    HostRecord, HostSpec, ProviderHostId, ProviderState, Provisioner, ProvisionerCapabilities,
    SizeInfo,
};
use borant_core::ports::{PortError, PortResult};
use borant_core::time::{Clock, SystemClock};

use crate::Availability;

/// One call that a test made on the fake, in order. The `HostSpec` prints
/// its boot keys only, so a log holds no boot value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Describe,
    Create(HostSpec),
    Delete(ProviderHostId),
    List(PoolName),
}

#[derive(Debug, Clone)]
pub struct MemoryProvisioner {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    hosts: Mutex<BTreeMap<ProviderHostId, HostRecord>>,
    foreign: Mutex<BTreeSet<ProviderHostId>>,
    calls: Mutex<Vec<Call>>,
    sizes: Vec<SizeInfo>,
    start_delay: Mutex<Duration>,
    create_failure: AtomicBool,
    available: Availability,
}

impl Default for MemoryProvisioner {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryProvisioner {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                hosts: Mutex::new(BTreeMap::new()),
                foreign: Mutex::new(BTreeSet::new()),
                calls: Mutex::new(Vec::new()),
                sizes: vec![
                    SizeInfo {
                        id: "small".to_owned(),
                        vcpus: 2,
                        mem_mb: 4096,
                        price_hour_usd: Some(0.1),
                        stopped_price_hour_usd: None,
                    },
                    SizeInfo {
                        id: "medium".to_owned(),
                        vcpus: 4,
                        mem_mb: 8192,
                        price_hour_usd: Some(0.2),
                        stopped_price_hour_usd: None,
                    },
                ],
                start_delay: Mutex::new(Duration::ZERO),
                create_failure: AtomicBool::new(false),
                available: Availability::default(),
            }),
        }
    }

    /// How long a create takes, so a test can see the hub work while it runs.
    pub fn set_start_delay(&self, delay: Duration) {
        *self.lock(&self.inner.start_delay) = delay;
    }

    /// Makes every create fail, as an unreachable backend does.
    pub fn set_create_failure(&self, on: bool) {
        self.inner.create_failure.store(on, Ordering::SeqCst);
    }

    /// Acts as an unreachable backend.
    pub fn set_available(&self, on: bool) {
        self.inner.available.set(on);
    }

    /// A host of another pool that the backend holds and that this
    /// provisioner did not create. `delete` of it is `Denied`.
    pub fn plant_foreign(&self, id: ProviderHostId) {
        self.lock(&self.inner.foreign).insert(id);
    }

    /// The calls so far, oldest first.
    pub fn calls(&self) -> Vec<Call> {
        self.lock(&self.inner.calls).clone()
    }

    /// The calls so far, and forgets them.
    pub fn take_calls(&self) -> Vec<Call> {
        std::mem::take(&mut *self.lock(&self.inner.calls))
    }

    fn log(&self, call: Call) {
        self.lock(&self.inner.calls).push(call);
    }

    fn lock<'a, T>(&self, m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
        m.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait::async_trait]
impl Provisioner for MemoryProvisioner {
    async fn describe(&self) -> PortResult<ProvisionerCapabilities> {
        self.log(Call::Describe);
        self.inner.available.check()?;
        Ok(ProvisionerCapabilities {
            backend: "memory".to_owned(),
            sizes: self.inner.sizes.clone(),
            regions: vec!["r1".to_owned()],
            typical_start: *self.lock(&self.inner.start_delay),
            typical_resume: None,
            billing_step: Duration::from_secs(3600),
            one_host_per_agent: false,
            stop_modes: Vec::new(),
        })
    }

    async fn create(&self, spec: &HostSpec) -> PortResult<HostRecord> {
        self.log(Call::Create(spec.clone()));
        self.inner.available.check()?;
        if self.inner.create_failure.load(Ordering::SeqCst) {
            return Err(PortError::unavailable(
                "the memory provisioner is set to fail a create",
            ));
        }
        let delay = *self.lock(&self.inner.start_delay);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        // The fake rejects a size it does not offer, as a backend does.
        if !self.inner.sizes.iter().any(|s| s.id == spec.size) {
            return Err(PortError::invalid(format!(
                "the memory provisioner offers no size {:?}",
                spec.size
            )));
        }

        let id = ProviderHostId(format!("{}-{}", spec.pool, spec.name));
        let mut hosts = self.lock(&self.inner.hosts);
        // Idempotent by pool and name: a retried create returns the host
        // that the first one made.
        if let Some(record) = hosts.get(&id) {
            return Ok(record.clone());
        }
        let record = HostRecord {
            id: id.clone(),
            pool: spec.pool.clone(),
            name: spec.name.clone(),
            state: ProviderState::Running,
            created: SystemClock.now(),
            provider_labels: spec.provider_labels.clone(),
        };
        hosts.insert(id, record.clone());
        Ok(record)
    }

    async fn delete(&self, id: &ProviderHostId) -> PortResult<()> {
        self.log(Call::Delete(id.clone()));
        self.inner.available.check()?;
        if self.lock(&self.inner.foreign).contains(id) {
            return Err(PortError::denied(format!(
                "{} was not created by this provisioner",
                id.0
            )));
        }
        // A host that is missing is Ok, so a retried delete succeeds.
        self.lock(&self.inner.hosts).remove(id);
        Ok(())
    }

    async fn list(&self, pool: &PoolName) -> PortResult<Vec<HostRecord>> {
        self.log(Call::List(pool.clone()));
        self.inner.available.check()?;
        let hosts = self.lock(&self.inner.hosts);
        Ok(hosts
            .values()
            .filter(|r| &r.pool == pool)
            .cloned()
            .collect())
    }
}
