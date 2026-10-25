//! The provisioner port on the exec plugin protocol: one program per
//! `[provisioners]` entry of `hub.toml` (spec "Provisioner ops" in
//! docs/superpowers/specs/2026-10-24-elastic-compute-design.md). Every request
//! carries the `config` table of the provisioner, and this instance runs one
//! op at a time, so a plugin needs no locks.

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::Timestamp;
use borant_core::ids::{HostName, PoolName};
use borant_core::ports::provision::{
    HostRecord, HostSpec, ProviderHostId, ProviderState, Provisioner, ProvisionerCapabilities,
    SizeInfo, StopMode,
};
use borant_core::ports::{PortError, PortResult};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::{ExecConfig, VERSION, call};

/// A provisioner that is an exec plugin program. It never logs a value of the
/// `boot` of a spec, and no part of its errors comes from a reply.
#[derive(Debug)]
pub struct ExecProvisioner {
    exec: ExecConfig,
    /// The `config` table of the provisioner, in every request.
    config: BTreeMap<String, Value>,
    /// The limit of every op, from `timeout_secs`; `None` keeps the default
    /// of each op.
    timeout: Option<Duration>,
    /// One op at a time for this instance.
    lock: tokio::sync::Mutex<()>,
}

impl ExecProvisioner {
    /// `config` is the `config` table of the provisioner in `hub.toml`, the
    /// same table in every request of every op, `describe` included.
    pub fn new(exec: ExecConfig, config: BTreeMap<String, Value>) -> Self {
        Self {
            exec,
            config,
            timeout: None,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The limit of every op, from `timeout_secs` of the provisioner. The
    /// plugin then has one value for all of them, not the default of each.
    pub fn with_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    fn timeout_of(&self, op: &str) -> Duration {
        self.timeout.unwrap_or_else(|| default_timeout(op))
    }

    /// One request of the protocol with the timeout of the op, while this
    /// instance runs nothing else.
    async fn request(&self, op: &str, args: Value) -> PortResult<Value> {
        let _one = self.lock.lock().await;
        let exec = ExecConfig {
            timeout: self.timeout_of(op),
            ..self.exec.clone()
        };
        call(&exec, op, args).await
    }

    /// The `config` of the provisioner, for one request.
    fn config(&self) -> Value {
        json!(&self.config)
    }
}

#[async_trait::async_trait]
impl Provisioner for ExecProvisioner {
    async fn describe(&self) -> PortResult<ProvisionerCapabilities> {
        let args = json!({ "config": self.config() });
        let value = self.request("describe", args).await?;
        let caps: WireCapabilities = reply("describe", value)?;
        if caps.v != VERSION || caps.kind != "provisioner" {
            return Err(PortError::invalid(format!(
                "the program is not a provisioner plugin of protocol version {VERSION}"
            )));
        }
        Ok(ProvisionerCapabilities {
            backend: caps.backend,
            sizes: caps
                .sizes
                .into_iter()
                .map(|size| SizeInfo {
                    id: size.id,
                    vcpus: size.vcpus,
                    mem_mb: size.mem_mb,
                    price_hour_usd: size.price_hour_usd,
                    stopped_price_hour_usd: size.stopped_price_hour_usd,
                })
                .collect(),
            regions: caps.regions,
            typical_start: Duration::from_secs(caps.typical_start_s),
            typical_resume: caps.typical_resume_s.map(Duration::from_secs),
            billing_step: Duration::from_secs(caps.billing_step_s),
            one_host_per_agent: caps.one_host_per_agent,
            stop_modes: caps.stop_modes,
        })
    }

    async fn create(&self, spec: &HostSpec) -> PortResult<HostRecord> {
        let args = json!({ "config": self.config(), "spec": WireSpec::of(spec) });
        let value = self.request("create", args).await?;
        let host: WireHost = reply("create", value)?;
        host.into_record()
    }

    async fn delete(&self, id: &ProviderHostId) -> PortResult<()> {
        let args = json!({ "config": self.config(), "id": id.0 });
        self.request("delete", args).await?;
        Ok(())
    }

    async fn list(&self, pool: &PoolName) -> PortResult<Vec<HostRecord>> {
        let args = json!({ "config": self.config(), "pool": pool });
        let value = self.request("list", args).await?;
        let hosts: Vec<WireHost> = reply("list", value)?;
        hosts.into_iter().map(WireHost::into_record).collect()
    }
}

/// The limit of an op of the protocol. A create waits for a backend, two
/// minutes at most.
fn default_timeout(op: &str) -> Duration {
    match op {
        "describe" => Duration::from_secs(10),
        "list" => Duration::from_secs(30),
        "create" => Duration::from_secs(120),
        _ => Duration::from_secs(60),
    }
}

/// Reads the value of a reply into the type of the op. The message holds
/// nothing of the reply, which can hold values that never go into a log.
fn reply<T: DeserializeOwned>(op: &str, value: Value) -> PortResult<T> {
    serde_json::from_value(value).map_err(|_| {
        PortError::invalid(format!(
            "the {op} reply of the provisioner does not match the protocol"
        ))
    })
}

/// The capabilities of a provisioner, in the names of the protocol.
#[derive(serde::Deserialize)]
struct WireCapabilities {
    v: u64,
    kind: String,
    backend: String,
    regions: Vec<String>,
    sizes: Vec<WireSize>,
    typical_start_s: u64,
    #[serde(default)]
    typical_resume_s: Option<u64>,
    billing_step_s: u64,
    #[serde(default)]
    one_host_per_agent: bool,
    #[serde(default)]
    stop_modes: Vec<StopMode>,
}

/// One size of a backend, in the names of the protocol.
#[derive(serde::Deserialize)]
struct WireSize {
    id: String,
    vcpus: u32,
    mem_mb: u32,
    #[serde(default)]
    price_hour_usd: Option<f64>,
    #[serde(default)]
    stopped_price_hour_usd: Option<f64>,
}

/// A spec of a create, in the names of the protocol: its `labels` are the
/// `provider_labels` of the port. The tags of the port stay home; the hub
/// puts them in the `boot` values of the new host.
#[derive(Serialize)]
struct WireSpec<'a> {
    pool: &'a PoolName,
    name: &'a HostName,
    size: &'a str,
    region: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<&'a str>,
    labels: &'a BTreeMap<String, String>,
    boot: &'a BTreeMap<String, String>,
}

impl<'a> WireSpec<'a> {
    fn of(spec: &'a HostSpec) -> Self {
        Self {
            pool: &spec.pool,
            name: &spec.name,
            size: &spec.size,
            region: &spec.region,
            image: spec.image.as_deref(),
            labels: &spec.provider_labels,
            boot: &spec.boot,
        }
    }
}

/// A host as the protocol reports it: its `labels` are the
/// `provider_labels` of the port, and `created` is an RFC 3339 UTC time.
#[derive(serde::Deserialize)]
struct WireHost {
    id: String,
    pool: PoolName,
    name: HostName,
    state: ProviderState,
    created: String,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

impl WireHost {
    fn into_record(self) -> PortResult<HostRecord> {
        let created = parse_created(&self.created).ok_or_else(|| {
            PortError::invalid("the created time of a host is not an RFC 3339 UTC time")
        })?;
        Ok(HostRecord {
            id: ProviderHostId(self.id),
            pool: self.pool,
            name: self.name,
            state: self.state,
            created,
            provider_labels: self.labels,
        })
    }
}

/// The unix ms of `created`, the RFC 3339 UTC form of the protocol,
/// `2026-10-24T18:00:00Z`, with an optional fraction that the port drops.
fn parse_created(text: &str) -> Option<Timestamp> {
    let bytes = text.as_bytes();
    let number = |from: usize, to: usize| -> Option<i64> {
        let part = bytes.get(from..to)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    let separators_ok = bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':');
    if !separators_ok {
        return None;
    }
    let mut end = 19;
    if bytes.get(end) == Some(&b'.') {
        end += 1;
        let from = end;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        if end == from {
            return None;
        }
    }
    if bytes.get(end) != Some(&b'Z') || bytes.len() != end + 1 {
        return None;
    }
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = ((days * 24 + hour) * 60 + minute) * 60 + second;
    Some(Timestamp(seconds * 1000))
}

/// The days of a month of a year of the proleptic Gregorian calendar.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// The days from 1970-01-01 to a date of the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisioner_ops_have_their_own_timeouts() {
        assert_eq!(default_timeout("describe"), Duration::from_secs(10));
        assert_eq!(default_timeout("list"), Duration::from_secs(30));
        assert_eq!(default_timeout("create"), Duration::from_secs(120));
        assert_eq!(default_timeout("delete"), Duration::from_secs(60));
        assert_eq!(default_timeout("stop"), Duration::from_secs(60));
    }

    #[test]
    fn provisioner_records_read_the_rfc_3339_time_of_the_protocol() {
        assert_eq!(parse_created("1970-01-01T00:00:00Z"), Some(Timestamp(0)));
        assert_eq!(
            parse_created("2026-10-24T18:00:00Z"),
            Some(Timestamp(1_792_864_800_000))
        );
        assert_eq!(
            parse_created("2024-02-29T23:59:59Z"),
            Some(Timestamp(1_709_251_199_000))
        );
        assert_eq!(
            parse_created("2026-10-24T18:00:00.250Z"),
            Some(Timestamp(1_792_864_800_000))
        );
        for bad in [
            "",
            "2026-10-24 18:00:00Z",
            "2026-10-24T18:00:00",
            "2026-10-24T18:00:00+02:00",
            "2026-10-24T18:00:00.Z",
            "2026-13-01T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-10-24T24:00:00Z",
        ] {
            assert_eq!(parse_created(bad), None, "{bad}");
        }
    }
}
