//! Configuration from `$BORANT_HOME/hub.toml` and `$BORANT_HOME/host.toml`,
//! with environment overrides. Every key has a default, so both files are
//! optional.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use borant_core::config::{AlertsConfig, ChangesConfig, ChannelConfig};
use borant_core::model::Harness;
use borant_core::ports::Notifier;
use borant_core::ports::provision::Provisioner;
use borant_core::ports::secrets::{Bundle, SecretRef};
use borant_core::rules::lease::LeaseConfig;
use borant_core::rules::members::MemberRules;
use borant_core::rules::pool::PoolConfig;
use borant_core::rules::roles::{CollectionConfig, RoleConfig};
use borant_core::{
    BundleName, ChannelName, CollectionName, HostName, MemberName, PoolName, RoleName, Tag,
};
use borant_exec::ExecConfig;
use borant_exec::changes::{ExecChangeRequests, STATUS_TIMEOUT};
use borant_exec::notify::ExecNotifier;
use borant_exec::provision::ExecProvisioner;
use borant_host::{HostConfig, SecretConfig};
use borant_hub::{ChangeWatch, ElasticConfig};
use borant_launch::harness::HarnessCommands;
use borant_nats::NatsConfig;
use borant_secrets::StoreConfig;
use serde::Deserialize;

pub fn home() -> PathBuf {
    std::env::var_os("BORANT_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".borant")
        })
}

pub fn socket() -> PathBuf {
    std::env::var_os("BORANT_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join("host.sock"))
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn env_secs(key: &str) -> anyhow::Result<Option<Duration>> {
    env(key)
        .map(|v| {
            v.parse::<f64>()
                .map(Duration::from_secs_f64)
                .with_context(|| format!("{key} must be a number of seconds"))
        })
        .transpose()
}

/// Splits a comma-separated environment value, for example
/// `BORANT_TAGS=vps,fast`.
fn split_list<T>(value: &str) -> anyhow::Result<Vec<T>>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .split(',')
        .map(|s| s.trim().parse::<T>().map_err(anyhow::Error::from))
        .collect()
}

/// A duration of `hub.toml`: `"10s"`, `"20m"`, `"1h"`, `"1h30m"`, or a plain
/// number of seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span(pub Duration);

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;

        impl serde::de::Visitor<'_> for V {
            type Value = Span;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a duration such as \"20m\", or a number of seconds")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Span, E> {
                parse_span(v).map(Span).map_err(E::custom)
            }

            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs(v)))
            }

            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs(u64::try_from(v).unwrap_or(0))))
            }

            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Span, E> {
                Ok(Span(Duration::from_secs_f64(v.max(0.0))))
            }
        }

        d.deserialize_any(V)
    }
}

/// Parses `"10s"`, `"20m"`, `"1h"`, `"2d"`, `"1h30m"`, or a number of seconds.
fn parse_span(text: &str) -> anyhow::Result<Duration> {
    let mut total = 0.0;
    let mut number = String::new();
    for c in text.chars() {
        if c.is_ascii_digit() || c == '.' {
            number.push(c);
            continue;
        }
        if c == ' ' {
            continue;
        }
        let value: f64 = number
            .parse()
            .with_context(|| format!("{text:?} is not a duration"))?;
        number.clear();
        let unit = match c {
            's' => 1.0,
            'm' => 60.0,
            'h' => 3600.0,
            'd' => 86_400.0,
            _ => anyhow::bail!("{text:?}: the unit {c:?} is not s, m, h, or d"),
        };
        total += value * unit;
    }
    if !number.is_empty() {
        let value: f64 = number
            .parse()
            .with_context(|| format!("{text:?} is not a duration"))?;
        total += value;
    }
    anyhow::ensure!(total > 0.0, "{text:?} is not a duration");
    Ok(Duration::from_secs_f64(total))
}

/// Resolves the pool and the tags of a host. `BORANT_POOL` and `BORANT_TAGS`
/// override `host.toml`, as the other `BORANT_*` values do.
fn pool_and_tags(
    pool: Option<PoolName>,
    tags: Option<Vec<Tag>>,
    env_pool: Option<String>,
    env_tags: Option<String>,
) -> anyhow::Result<(Option<PoolName>, Vec<Tag>)> {
    let pool = match env_pool {
        Some(v) => Some(v.trim().parse::<PoolName>().context("BORANT_POOL")?),
        None => pool,
    };
    let tags = match env_tags {
        Some(v) => split_list(&v).context("BORANT_TAGS")?,
        None => tags.unwrap_or_default(),
    };
    Ok((pool, tags))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NatsFile {
    #[serde(default)]
    pub urls: Vec<String>,
    pub creds_file: Option<PathBuf>,
    pub user: Option<String>,
    pub password: Option<String>,
    #[serde(default)]
    pub tls_required: bool,
}

impl NatsFile {
    pub fn to_config(&self, name: &str) -> NatsConfig {
        let urls = env("BORANT_NATS_URL")
            .map(|v| v.split(',').map(str::trim).map(str::to_owned).collect())
            .unwrap_or_else(|| self.urls.clone());
        NatsConfig {
            urls,
            creds_file: env("BORANT_NATS_CREDS")
                .map(PathBuf::from)
                .or(self.creds_file.clone()),
            user: env("BORANT_NATS_USER").or(self.user.clone()),
            password: env("BORANT_NATS_PASSWORD").or(self.password.clone()),
            tls_required: self.tls_required,
            name: name.to_owned(),
            max_age: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub lease_secs: Option<f64>,
    pub grace_secs: Option<f64>,
    pub heartbeat_secs: Option<f64>,
    pub claim_timeout_secs: Option<f64>,
    pub reserve_secs: Option<f64>,
    /// How long a launch may take to register before the host stops it.
    pub launch_timeout_secs: Option<f64>,
}

impl Timing {
    pub fn lease_config(&self) -> anyhow::Result<LeaseConfig> {
        let mut c = LeaseConfig::default();
        let pick = |file: Option<f64>, key: &str, d: &mut Duration| -> anyhow::Result<()> {
            if let Some(v) = env_secs(key)? {
                *d = v;
            } else if let Some(v) = file {
                *d = Duration::from_secs_f64(v);
            }
            Ok(())
        };
        pick(self.lease_secs, "BORANT_LEASE_SECS", &mut c.lease)?;
        pick(self.grace_secs, "BORANT_GRACE_SECS", &mut c.grace)?;
        pick(
            self.heartbeat_secs,
            "BORANT_HEARTBEAT_SECS",
            &mut c.heartbeat,
        )?;
        pick(
            self.claim_timeout_secs,
            "BORANT_CLAIM_TIMEOUT_SECS",
            &mut c.claim_timeout,
        )?;
        pick(self.reserve_secs, "BORANT_RESERVE_SECS", &mut c.reserve)?;
        Ok(c)
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubFile {
    #[serde(default)]
    pub nats: NatsFile,
    #[serde(default)]
    pub timing: Timing,
    pub instance: Option<String>,
    #[serde(default)]
    pub elastic: ElasticFile,
    /// `[provisioners.<name>]`, the backend of each pool.
    #[serde(default)]
    pub provisioners: BTreeMap<String, ProvisionerFile>,
    /// `[[pools]]`, the shapes of the hosts that the hub creates.
    #[serde(default)]
    pub pools: Vec<PoolFile>,
    /// The roles of the swarm, from `[roles.<name>]`.
    #[serde(default)]
    pub roles: BTreeMap<RoleName, RoleConfig>,
    /// The collections, by name, for example `personal` and `work`.
    #[serde(default)]
    pub collections: BTreeMap<CollectionName, CollectionConfig>,
    /// `[channels.<name>]`: the caps of a channel.
    #[serde(default)]
    #[allow(dead_code)] // The stop and spawn-caps task reads it.
    pub channels: BTreeMap<ChannelName, ChannelConfig>,
    /// `[changes]`: the PR watch. Not wired yet.
    #[serde(default)]
    #[allow(dead_code)] // The PR watch task reads it.
    pub changes: ChangesConfig,
    /// `[alerts]`: the notifier of the hub and its alert rules.
    #[serde(default)]
    pub alerts: AlertsFile,
    /// `[notifiers.<name>]`, the programs that tell a person.
    #[serde(default)]
    pub notifiers: BTreeMap<String, NotifierFile>,
    /// `[secrets]`, the rules of a bundle member at a usage limit.
    #[serde(default)]
    pub secrets: HubSecretsFile,
}

/// `[secrets]` of `hub.toml`. Phase 1 holds the member rules only.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubSecretsFile {
    /// `[secrets.members]`: the reset time to assume when a limit gives none,
    /// and how often one agent may restart on a new member in an hour.
    #[serde(default)]
    pub members: MemberRules,
}

/// `[elastic]` of `hub.toml`. Phase 4 has no `keep_stopped` and no
/// `min_warm`, so `deny_unknown_fields` refuses them.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElasticFile {
    /// How often the rules run, `"10s"` by default.
    pub tick: Option<Span>,
    /// How long a host may go without a heartbeat, `"10m"` by default.
    pub lost_timeout: Option<Span>,
    /// How long a pool waits after a failed create, `"5m"` by default.
    pub create_backoff: Option<Span>,
    /// The cap of the month, all pools together.
    pub monthly_budget_usd: Option<f64>,
}

/// One `[provisioners.<name>]` entry. The table key is the name, so the file
/// does not repeat it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionerFile {
    /// The kind of adapter, `"exec"`.
    pub adapter: String,
    /// The program of an `exec` adapter, argv.
    #[serde(default)]
    pub command: Vec<String>,
    /// The table that every request carries, as strings.
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// The name of the systemd credential that the program reads.
    pub credential: Option<String>,
    /// One timeout for every op, over the default of each.
    pub timeout_secs: Option<u64>,
}

/// One `[[pools]]` entry of `hub.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PoolFile {
    pub name: PoolName,
    /// One `[provisioners.<name>]` entry.
    pub provisioner: String,
    pub size: String,
    pub region: String,
    /// Opaque: only the backend reads it. `None` asks for its default image.
    pub image: Option<String>,
    /// The prefix of the host names of the pool, the pool name and a dash by
    /// default.
    pub name_prefix: Option<String>,
    #[serde(default)]
    pub tags: Vec<Tag>,
    pub harnesses: Vec<Harness>,
    pub agents_per_host: u32,
    pub max_hosts: usize,
    /// How long a ready host with no agent stays, `"20m"` by default.
    #[serde(default)]
    pub idle_timeout: Option<Span>,
    /// How long a create may take, `"10m"` by default.
    #[serde(default)]
    pub boot_timeout: Option<Span>,
    pub monthly_budget_usd: Option<f64>,
    /// The price of one host-hour, when the backend reports no price.
    pub price_hour_usd: Option<f64>,
}

impl HubFile {
    /// The `[elastic]`, `[provisioners]`, and `[[pools]]` sections, with one
    /// adapter for each provisioner. `None` for a hub that names no pool,
    /// which then behaves as before.
    pub fn elastic_config(&self) -> anyhow::Result<Option<ElasticConfig>> {
        if self.pools.is_empty() {
            return Ok(None);
        }
        let tick = self.elastic.tick.unwrap_or(Span(Duration::from_secs(10))).0;
        let lost_timeout = self
            .elastic
            .lost_timeout
            .unwrap_or(Span(Duration::from_secs(10 * 60)))
            .0;
        let create_backoff = self
            .elastic
            .create_backoff
            .unwrap_or(Span(Duration::from_secs(5 * 60)))
            .0;
        let mut pools = Vec::new();
        for file in &self.pools {
            anyhow::ensure!(
                self.provisioners.contains_key(&file.provisioner),
                "pool {} names the unknown provisioner {}",
                file.name,
                file.provisioner
            );
            anyhow::ensure!(
                file.agents_per_host > 0,
                "pool {} has no slot for an agent",
                file.name
            );
            pools.push(PoolConfig {
                name: file.name.clone(),
                provisioner: file.provisioner.clone(),
                size: file.size.clone(),
                region: file.region.clone(),
                image: file.image.clone(),
                name_prefix: file
                    .name_prefix
                    .clone()
                    .unwrap_or_else(|| format!("{}-", file.name)),
                tags: file.tags.clone(),
                harnesses: file.harnesses.clone(),
                agents_per_host: file.agents_per_host,
                max_hosts: file.max_hosts,
                idle_timeout: file
                    .idle_timeout
                    .unwrap_or(Span(Duration::from_secs(20 * 60)))
                    .0,
                boot_timeout: file
                    .boot_timeout
                    .unwrap_or(Span(Duration::from_secs(10 * 60)))
                    .0,
                lost_timeout,
                backoff_until: None,
                monthly_budget_usd: file.monthly_budget_usd,
                price_hour_usd: file.price_hour_usd,
            });
        }
        Ok(Some(ElasticConfig {
            tick,
            lost_timeout,
            create_backoff,
            monthly_budget_usd: self.elastic.monthly_budget_usd,
            pools,
            provisioners: open_provisioners(&self.provisioners)?,
        }))
    }
}

/// Opens the adapter of each provisioner. A pool host can be created only by
/// a backend that this binary has.
fn open_provisioners(
    files: &BTreeMap<String, ProvisionerFile>,
) -> anyhow::Result<BTreeMap<String, Arc<dyn Provisioner>>> {
    let mut out: BTreeMap<String, Arc<dyn Provisioner>> = BTreeMap::new();
    for (name, file) in files {
        let adapter: Arc<dyn Provisioner> = match file.adapter.as_str() {
            "exec" => {
                anyhow::ensure!(
                    !file.command.is_empty(),
                    "provisioner {name} has no command"
                );
                let credential = match &file.credential {
                    None => None,
                    Some(c) => Some((
                        env("CREDENTIALS_DIRECTORY")
                            .map(PathBuf::from)
                            .context("the provisioner names a credential")?,
                        c.clone(),
                    )),
                };
                let exec = ExecConfig {
                    command: file.command.clone(),
                    env: BTreeMap::new(),
                    credential,
                    timeout: Duration::from_secs(60),
                    // A create carries boot values, one-time join data, so a
                    // plugin that echoes them must not put them in the log.
                    log_stderr: false,
                };
                let config = file
                    .config
                    .iter()
                    .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                    .collect();
                Arc::new(
                    ExecProvisioner::new(exec, config)
                        .with_timeout(file.timeout_secs.map(Duration::from_secs)),
                )
            }
            other => anyhow::bail!("provisioner {name}: unknown adapter {other:?}"),
        };
        out.insert(name.clone(), adapter);
    }
    Ok(out)
}

/// Opens the notifier of `[alerts].notifier`, if it names one. The desktop
/// notifier is the default of a host, so `None` leaves that choice to the
/// caller.
pub fn open_notifier(
    files: &BTreeMap<String, NotifierFile>,
    name: Option<&str>,
) -> anyhow::Result<Option<Box<dyn Notifier>>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let file = files.get(name).with_context(|| {
        format!("[alerts] names the notifier {name:?}, which [notifiers] does not define")
    })?;
    anyhow::ensure!(
        file.adapter == "exec",
        "notifier {name}: unknown adapter {:?}, only \"exec\" exists",
        file.adapter
    );
    anyhow::ensure!(!file.command.is_empty(), "notifier {name} has no command");
    let credential = match &file.credential {
        None => None,
        Some(c) => Some((
            env("CREDENTIALS_DIRECTORY")
                .map(PathBuf::from)
                .context("the notifier names a credential")?,
            c.clone(),
        )),
    };
    let exec = ExecConfig {
        command: file.command.clone(),
        env: BTreeMap::new(),
        credential,
        timeout: Duration::from_secs(file.timeout_secs.unwrap_or(30)),
        // A notice is not a secret, but the runner does not know what the
        // plugin prints, so its stderr stays out of the log.
        log_stderr: false,
    };
    Ok(Some(Box::new(ExecNotifier::new(exec))))
}

impl HubFile {
    /// The notifier of `[alerts]`, if this file names one.
    pub fn notifier(&self) -> anyhow::Result<Option<Box<dyn Notifier>>> {
        open_notifier(&self.notifiers, self.alerts.notifier.as_deref())
    }

    /// The PR watch of `[changes]`: the adapter that reads one change request
    /// and how often the hub polls. `None` when the file names no adapter, and
    /// also when the program is not a change-request plugin: the hub then
    /// serves as before, with a warn, instead of failing to start.
    pub async fn changes_watch(&self) -> anyhow::Result<Option<ChangeWatch>> {
        let Some(adapter) = self.changes.adapter.as_deref() else {
            return Ok(None);
        };
        anyhow::ensure!(
            adapter == "exec",
            "changes: unknown adapter {adapter:?}, only \"exec\" exists"
        );
        let program = self
            .changes
            .program
            .clone()
            .context("changes: an exec adapter needs a program")?;
        // The plugin reads its token from a file of the hub unit's credentials
        // directory, as the provisioner plugins do.
        let credential = match &self.changes.credential {
            None => None,
            Some(c) => Some((
                env("CREDENTIALS_DIRECTORY")
                    .map(PathBuf::from)
                    .context("changes names a credential")?,
                c.clone(),
            )),
        };
        let exec = ExecConfig {
            command: vec![program],
            env: BTreeMap::new(),
            credential,
            timeout: STATUS_TIMEOUT,
            // The status of a change request is not a secret, but the runner
            // does not know what the plugin prints, so its stderr stays out of
            // the log.
            log_stderr: false,
        };
        let changes = match ExecChangeRequests::open(exec, BTreeMap::new()).await {
            Ok(changes) => changes,
            Err(e) => {
                tracing::warn!("the PR watch is off, the plugin did not answer: {e}");
                return Ok(None);
            }
        };
        Ok(Some(ChangeWatch {
            changes: Arc::new(changes),
            every: Duration::from_secs(self.changes.every_secs.max(1)),
        }))
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSection {
    pub name: Option<String>,
    pub launcher: Option<String>,
    pub max_agents: Option<u32>,
    pub always_on: Option<bool>,
    /// The memory that the host keeps free for itself, in megabytes. 1024
    /// when unset.
    pub reserve_mem_mb: Option<u32>,
    pub pool: Option<PoolName>,
    pub tags: Option<Vec<Tag>>,
    pub ssh: Option<String>,
    pub harnesses: Option<Vec<Harness>>,
    pub checkpoint_remote: Option<String>,
    pub notify: Option<bool>,
    pub wakes_per_hour: Option<u32>,
    pub keep_turns: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFile {
    #[serde(default)]
    pub nats: NatsFile,
    #[serde(default)]
    pub timing: Timing,
    #[serde(default)]
    pub host: HostSection,
    #[serde(default)]
    pub commands: HarnessCommands,
    pub secrets: Option<SecretsFile>,
    /// `[alerts]`: the notifier and the alert rules of this host.
    #[serde(default)]
    pub alerts: AlertsFile,
    /// `[notifiers.<name>]`, the programs that tell a person.
    #[serde(default)]
    pub notifiers: BTreeMap<String, NotifierFile>,
    /// `[herdr]`: the herdr launcher of this host, when `launcher = "herdr"`.
    #[serde(default)]
    pub herdr: HerdrFile,
    /// `[sandbox]`: the bubblewrap sandbox of this host.
    #[serde(default)]
    pub sandbox: SandboxFile,
}

/// `[sandbox]` of `host.toml`: whether the host offers a namespace sandbox,
/// the program that builds it, and extra binds.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxFile {
    /// Run the start check at host start and offer `SandboxKind::Namespace`.
    pub enabled: Option<bool>,
    /// The program on PATH, `bwrap` when absent.
    pub program: Option<String>,
    /// Extra read-only binds, for example `["~/.config/gh"]`.
    pub ro: Option<Vec<PathBuf>>,
    /// Extra writable binds.
    pub rw: Option<Vec<PathBuf>>,
}

/// `[herdr]` of `host.toml`: one herdr session per host, and the program that
/// runs it.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HerdrFile {
    /// The herdr session of this host. Its server outlives a host restart.
    pub session: Option<String>,
    /// The herdr program, for example a pinned release binary.
    pub program: Option<String>,
}

/// `[alerts]` of `hub.toml` and `host.toml`: which notifier gets a notice,
/// and the rules that fire one.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsFile {
    /// The name of a `[notifiers.<name>]` entry. `None` keeps the default of
    /// the daemon: the desktop notifier on a host, none in the hub.
    pub notifier: Option<String>,
    /// The laptop sets true: it sends `hub.unreachable` when the hub goes.
    pub watch_hub: Option<bool>,
    /// Send a notice when an agent asks for a human.
    pub needs_human: Option<bool>,
    /// Minutes without an event before an agent counts as stalled.
    pub stall_minutes: Option<u64>,
    /// Minutes without a heartbeat before a host counts as offline.
    pub host_offline_minutes: Option<u64>,
    /// Minutes without a heartbeat before the hub counts as down.
    pub hub_down_minutes: Option<u64>,
    /// A host whose disk is this full gets the `host_disk` alert.
    pub disk_warn_pct: Option<u8>,
}

impl AlertsFile {
    /// One line for the start log of a daemon: which rules are on, and with
    /// which limits. The alert pass reads the values themselves.
    pub fn summary(&self) -> String {
        let flag = |v: Option<bool>, default: bool| if v.unwrap_or(default) { "on" } else { "off" };
        format!(
            "notifier={} needs_human={} stall_minutes={} host_offline_minutes={} hub_down_minutes={} watch_hub={}",
            self.notifier.as_deref().unwrap_or("desktop"),
            flag(self.needs_human, true),
            self.stall_minutes.unwrap_or(30),
            self.host_offline_minutes.unwrap_or(5),
            self.hub_down_minutes.unwrap_or(2),
            flag(self.watch_hub, false),
        )
    }

    /// The rules as the hub holds them: this file's values over the defaults
    /// of [`AlertsConfig`]. The hub alert pass reads the concrete numbers.
    pub fn to_config(&self) -> AlertsConfig {
        let d = AlertsConfig::default();
        AlertsConfig {
            notifier: self.notifier.clone(),
            watch_hub: self.watch_hub.unwrap_or(d.watch_hub),
            needs_human: self.needs_human.unwrap_or(d.needs_human),
            stall_minutes: self.stall_minutes.unwrap_or(d.stall_minutes),
            host_offline_minutes: self.host_offline_minutes.unwrap_or(d.host_offline_minutes),
            hub_down_minutes: self.hub_down_minutes.unwrap_or(d.hub_down_minutes),
            disk_warn_pct: self.disk_warn_pct.unwrap_or(d.disk_warn_pct),
        }
    }
}

/// One `[notifiers.<name>]` entry. The table key is the name, so the file does
/// not repeat it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifierFile {
    /// The kind of adapter, `"exec"`.
    pub adapter: String,
    /// The program of an `exec` adapter, argv.
    #[serde(default)]
    pub command: Vec<String>,
    /// The name of the systemd credential that the program reads.
    pub credential: Option<String>,
    /// The limit for one notice, `30` seconds by default.
    pub timeout_secs: Option<u64>,
}

impl HostFile {
    /// The notifier of `[alerts]`, if this file names one.
    pub fn notifier(&self) -> anyhow::Result<Option<Box<dyn Notifier>>> {
        open_notifier(&self.notifiers, self.alerts.notifier.as_deref())
    }

    /// The herdr launcher settings of `[herdr]`, with the SSH destination that
    /// a laptop attaches through.
    pub fn herdr_config(&self) -> borant_launch::HerdrConfig {
        let default = borant_launch::HerdrConfig::default();
        borant_launch::HerdrConfig {
            session: self.herdr.session.clone().unwrap_or(default.session),
            program: self.herdr.program.clone().unwrap_or(default.program),
            ssh: self.host.ssh.clone(),
        }
    }
}

/// `[secrets]` of `host.toml`. Every key is optional, so a host without
/// secrets (a laptop) parses as before.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsFile {
    /// `[secrets.stores.<name>]`, one entry for each backend.
    #[serde(default)]
    pub stores: BTreeMap<String, StoreConfig>,
    /// `[secrets.bundles.<name>]`, where the key is the bundle name.
    #[serde(default)]
    pub bundles: BTreeMap<BundleName, BundleFile>,
    /// `[secrets.harness]`: `"*"` or a harness name, each with the bundles it
    /// gets by default.
    #[serde(default)]
    pub harness: BTreeMap<String, Vec<BundleName>>,
}

/// One `[secrets.bundles.<name>]` entry. The table key is the name, so the file
/// does not repeat it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleFile {
    /// The store that resolves the refs of this bundle.
    pub store: String,
    #[serde(default)]
    pub vars: BTreeMap<String, SecretRef>,
    #[serde(default)]
    pub files: BTreeMap<String, SecretRef>,
    #[serde(default)]
    pub pre: Vec<Vec<String>>,
    #[serde(default)]
    pub members: BTreeMap<MemberName, BTreeMap<String, SecretRef>>,
}

impl BundleFile {
    /// The bundle as the core holds it, with the name from its key.
    fn bundle(self, name: BundleName) -> Bundle {
        Bundle {
            name,
            store: self.store,
            vars: self.vars,
            files: self.files,
            pre: self.pre,
            members: self.members,
        }
    }
}

impl SecretsFile {
    /// The section with each bundle name filled from its key, ready to open.
    pub fn config(&self) -> SecretConfig {
        SecretConfig {
            stores: self.stores.clone(),
            bundles: self
                .bundles
                .iter()
                .map(|(name, file)| (name.clone(), file.clone().bundle(name.clone())))
                .collect(),
            harness: self.harness.clone(),
            credentials_dir: env("CREDENTIALS_DIRECTORY").map(PathBuf::from),
        }
    }
}

fn read<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> anyhow::Result<T> {
    match std::fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parse {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub fn hub_file() -> anyhow::Result<HubFile> {
    read(&home().join("hub.toml"))
}

pub fn host_file() -> anyhow::Result<HostFile> {
    read(&home().join("host.toml"))
}

/// A host name from the machine name: lowercase, with invalid characters
/// replaced by `-`.
pub fn default_host_name() -> String {
    let raw = sysinfo::System::host_name().unwrap_or_else(|| "host".into());
    let short = raw.split('.').next().unwrap_or("host").to_lowercase();
    let mut name: String = short
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    name = name.trim_matches('-').to_owned();
    name.truncate(63);
    if name.is_empty() { "host".into() } else { name }
}

/// Harnesses whose programs are on `PATH`.
pub fn detect_harnesses() -> Vec<Harness> {
    let on_path = |bin: &str| {
        std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
    };
    [
        (Harness::ClaudeCode, "claude"),
        (Harness::Pi, "pi"),
        (Harness::Omp, "omp"),
        (Harness::Fake, "borant-fake-harness"),
    ]
    .into_iter()
    .filter(|(_, b)| on_path(b))
    .map(|(h, _)| h)
    .collect()
}

/// Reads `host.toml` into a [`HostConfig`], and opens the `[secrets]` stores.
/// A store that fails drops the harnesses whose bundles use it.
pub async fn host_config(file: &HostFile) -> anyhow::Result<HostConfig> {
    let h = &file.host;
    let name: HostName = env("BORANT_HOST_NAME")
        .or(h.name.clone())
        .unwrap_or_else(default_host_name)
        .parse()
        .context("host name")?;
    let mut cfg = HostConfig::new(name, home());
    if let Some(v) = env("BORANT_LAUNCHER").or(h.launcher.clone()) {
        cfg.launcher = v;
    }
    if let Some(v) = env("BORANT_MAX_AGENTS")
        .map(|v| v.parse())
        .transpose()?
        .or(h.max_agents)
    {
        cfg.max_agents = v;
    }
    if let Some(v) = env("BORANT_ALWAYS_ON")
        .map(|v| v == "1" || v == "true")
        .or(h.always_on)
    {
        cfg.always_on = v;
    }
    cfg.ssh = env("BORANT_SSH").or(h.ssh.clone());
    (cfg.pool, cfg.tags) = pool_and_tags(
        h.pool.clone(),
        h.tags.clone(),
        env("BORANT_POOL"),
        env("BORANT_TAGS"),
    )?;
    cfg.harnesses = match env("BORANT_HARNESSES") {
        Some(v) => v
            .split(',')
            .map(|s| s.trim().parse::<Harness>().map_err(anyhow::Error::msg))
            .collect::<anyhow::Result<_>>()?,
        None => h.harnesses.clone().unwrap_or_else(detect_harnesses),
    };
    cfg.checkpoint_remote = env("BORANT_CHECKPOINT_REMOTE").or(h.checkpoint_remote.clone());
    cfg.notify = h.notify.unwrap_or(cfg!(target_os = "macos"));
    // A host that names a notifier means to be told.
    if file.alerts.notifier.is_some() {
        cfg.notify = true;
    }
    // The laptop watches the hub; a box with no watch_hub stays quiet.
    cfg.watch_hub = file.alerts.watch_hub.unwrap_or(false);
    cfg.hub_down = Duration::from_secs(file.alerts.hub_down_minutes.unwrap_or(2) * 60);
    if let Some(v) = h.wakes_per_hour {
        cfg.wakes_per_hour = v;
    }
    if let Some(v) = h.keep_turns {
        cfg.keep_turns = v;
    }
    if let Some(v) = h.reserve_mem_mb {
        cfg.reserve_mem_mb = v;
    }
    let timing = file.timing.lease_config()?;
    cfg.lease = timing.lease;
    cfg.heartbeat = timing.heartbeat;
    // The watchdog of a launch: `launch_timeout_secs` in `[timing]`, or
    // `BORANT_LAUNCH_TIMEOUT_SECS`.
    if let Some(v) = env_secs("BORANT_LAUNCH_TIMEOUT_SECS")?
        .or(file.timing.launch_timeout_secs.map(Duration::from_secs_f64))
    {
        cfg.launch_timeout = v;
    }
    cfg.commands = file.commands.clone();
    // `[sandbox]`: the host offers the namespace sandbox only if it says so.
    // The start check in the daemon drops it again when the program fails.
    if file.sandbox.enabled.unwrap_or(false) {
        cfg.sandbox = Some(borant_host::SandboxConfig {
            program: file
                .sandbox
                .program
                .clone()
                .unwrap_or_else(|| "bwrap".to_owned()),
            ro: file.sandbox.ro.clone().unwrap_or_default(),
            rw: file.sandbox.rw.clone().unwrap_or_default(),
        });
    }
    if let Some(section) = &file.secrets {
        let (opened, harnesses) = section.config().open(&cfg.harnesses).await;
        tracing::info!(
            stores = opened.stores.len(),
            bundles = opened.bundles.len(),
            harnesses = ?harnesses,
            "secrets opened"
        );
        cfg.secrets = Some(opened);
        cfg.harnesses = harnesses;
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hub_collections() {
        let f: HubFile = toml::from_str(
            r#"
            [collections.personal]
            bundles = ["github-personal"]
            git_author = { name = "Chris Fenton", email = "you@example.com" }

            [collections.work]
            bundles = ["github-work"]
            "#,
        )
        .unwrap();
        assert_eq!(f.collections.len(), 2);
        let personal = &f.collections[&"personal".parse().unwrap()];
        assert_eq!(personal.bundles, ["github-personal".parse().unwrap()]);
        let author = personal.git_author.as_ref().unwrap();
        assert_eq!(author.name, "Chris Fenton");
        assert_eq!(author.email, "you@example.com");
        let work = &f.collections[&"work".parse().unwrap()];
        assert!(work.git_author.is_none());
    }

    #[test]
    fn parses_host_file() {
        let f: HostFile = toml::from_str(
            r#"
            [nats]
            urls = ["nats://hub:4222"]
            [host]
            name = "vps-1"
            launcher = "zellij"
            harnesses = ["pi", "omp"]
            always_on = true
            [timing]
            lease_secs = 6
            [commands]
            pi = ["pi", "--model", "x"]
            "#,
        )
        .unwrap();
        assert_eq!(f.nats.urls, vec!["nats://hub:4222"]);
        assert_eq!(f.host.harnesses, Some(vec![Harness::Pi, Harness::Omp]));
        assert_eq!(f.timing.lease_secs, Some(6.0));
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<HostFile>("[host]\nnmae = \"x\"").is_err());
    }

    #[test]
    fn parses_host_pool_and_tags() {
        let f: HostFile = toml::from_str(
            r#"
            [host]
            name = "e-1"
            harnesses = ["pi"]
            pool = "workers"
            tags = ["vps", "fast"]
            "#,
        )
        .unwrap();
        assert_eq!(f.host.pool.as_ref().map(PoolName::as_str), Some("workers"));
        assert_eq!(
            f.host.tags,
            Some(vec!["vps".parse().unwrap(), "fast".parse().unwrap()])
        );

        let none: HostFile = toml::from_str("[host]\nname = \"laptop\"\n").unwrap();
        assert_eq!(none.host.pool, None);
        assert_eq!(none.host.tags, None);
    }

    #[test]
    fn splits_tags_from_the_environment() {
        let tags: Vec<Tag> = split_list("vps, fast").unwrap();
        assert_eq!(tags, vec!["vps".parse().unwrap(), "fast".parse().unwrap()]);
        assert_eq!(tags[0].as_str(), "vps");
        // Names are lower case only.
        assert!(split_list::<Tag>("VPS").is_err());
        assert!(split_list::<PoolName>("").is_err());
    }

    #[tokio::test]
    async fn the_sandbox_section_reaches_the_host_config() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            name = "box-1"
            [sandbox]
            enabled = true
            program = "/nix/store/x-bwrap/bin/bwrap"
            ro = ["~/.config/gh"]
            rw = ["/var/cache/borant"]
            "#,
        )
        .unwrap();
        let cfg = host_config(&file).await.unwrap();
        let sandbox = cfg.sandbox.as_ref().expect("a sandbox");
        assert_eq!(sandbox.program, "/nix/store/x-bwrap/bin/bwrap");
        assert_eq!(sandbox.ro, [PathBuf::from("~/.config/gh")]);
        assert_eq!(sandbox.rw, [PathBuf::from("/var/cache/borant")]);
        assert_eq!(
            cfg.info().sandboxes,
            [borant_core::model::SandboxKind::Namespace]
        );

        // `bwrap` is the default program, and no section means no sandbox.
        let file: HostFile =
            toml::from_str("[host]\nname = \"box-2\"\n[sandbox]\nenabled = true\n").unwrap();
        let cfg = host_config(&file).await.unwrap();
        assert_eq!(cfg.sandbox.unwrap().program, "bwrap");

        let file: HostFile =
            toml::from_str("[host]\nname = \"box-3\"\n[sandbox]\nenabled = false\n").unwrap();
        let cfg = host_config(&file).await.unwrap();
        assert!(cfg.sandbox.is_none());
        assert!(cfg.info().sandboxes.is_empty());
    }

    #[tokio::test]
    async fn host_config_reads_pool_and_tags_from_the_file() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            name = "e-1"
            harnesses = ["pi"]
            pool = "workers"
            tags = ["vps"]
            "#,
        )
        .unwrap();
        let cfg = host_config(&file).await.unwrap();
        assert_eq!(cfg.pool.as_ref().map(PoolName::as_str), Some("workers"));
        assert_eq!(cfg.tags.len(), 1);
        assert_eq!(cfg.tags[0].as_str(), "vps");
    }

    #[test]
    fn env_pool_and_tags_override_the_file() {
        let (pool, tags) = pool_and_tags(
            Some("workers".parse().unwrap()),
            Some(vec!["vps".parse().unwrap()]),
            Some("gpu".into()),
            Some("fast, eu".into()),
        )
        .unwrap();
        assert_eq!(pool.map(|p| p.to_string()).as_deref(), Some("gpu"));
        assert_eq!(tags.len(), 2);
        assert_eq!(tags[1].as_str(), "eu");
        // The file fills in what the environment leaves out.
        let (pool, tags) = pool_and_tags(
            Some("workers".parse().unwrap()),
            Some(vec!["vps".parse().unwrap()]),
            None,
            None,
        )
        .unwrap();
        assert_eq!(pool.map(|p| p.to_string()).as_deref(), Some("workers"));
        assert_eq!(tags.len(), 1);
        // A host with neither reports no pool and no tags.
        assert_eq!(
            pool_and_tags(None, None, None, None).unwrap(),
            (None, Vec::new())
        );
        // A bad value fails the whole config.
        assert!(pool_and_tags(None, None, Some("GPU".into()), None).is_err());
        assert!(pool_and_tags(None, None, None, Some("vps,,x".into())).is_err());
    }

    #[test]
    fn default_host_name_is_valid() {
        assert!(default_host_name().parse::<HostName>().is_ok());
    }

    /// The `[alerts]` and `[notifiers.phone]` tables of the phase 2 spec.
    const ALERTS: &str = r#"
        [alerts]
        notifier = "phone"
        watch_hub = false
        needs_human = true
        stall_minutes = 30
        host_offline_minutes = 5
        hub_down_minutes = 2

        [notifiers.phone]
        adapter = "exec"
        command = ["borant-notify-ntfy"]
        credential = "notify-token"
    "#;

    #[test]
    fn parses_the_alerts_and_a_notifier() {
        for file in [
            toml::from_str::<HubFile>(ALERTS).map(|f| (f.alerts, f.notifiers)),
            toml::from_str::<HostFile>(ALERTS).map(|f| (f.alerts, f.notifiers)),
        ] {
            let (alerts, notifiers) = file.unwrap();
            assert_eq!(alerts.notifier.as_deref(), Some("phone"));
            assert_eq!(alerts.watch_hub, Some(false));
            assert_eq!(alerts.stall_minutes, Some(30));
            assert_eq!(alerts.hub_down_minutes, Some(2));
            let phone = notifiers.get("phone").expect("the phone notifier");
            assert_eq!(phone.adapter, "exec");
            assert_eq!(phone.command, vec!["borant-notify-ntfy"]);
            assert_eq!(phone.credential.as_deref(), Some("notify-token"));
        }
    }

    #[test]
    fn a_notifier_needs_a_definition_and_the_exec_adapter() {
        // A plugin without a credential opens.
        let plain = ALERTS.replace("credential = \"notify-token\"", "");
        let file: HubFile = toml::from_str(&plain).unwrap();
        assert!(file.notifier().unwrap().is_some(), "the phone plugin opens");
        // No [alerts].notifier keeps the default of the daemon.
        let none: HubFile = toml::from_str("[alerts]\nneeds_human = true\n").unwrap();
        assert!(none.notifier().unwrap().is_none());
        // A name without an entry is an error, not a silent desktop notice.
        let missing: HubFile = toml::from_str("[alerts]\nnotifier = \"phone\"\n").unwrap();
        assert!(missing.notifier().is_err());
        let other = plain.replace("adapter = \"exec\"", "adapter = \"carrier-pigeon\"");
        let unknown: HubFile = toml::from_str(&other).unwrap();
        assert!(unknown.notifier().is_err());
        // A credential names a file in the directory that systemd provides.
        if env("CREDENTIALS_DIRECTORY").is_none() {
            let with_credential: HubFile = toml::from_str(ALERTS).unwrap();
            assert!(with_credential.notifier().is_err());
        }
    }

    #[test]
    fn the_alerts_of_a_host_parse_and_default_to_none() {
        let host: HostFile = toml::from_str(ALERTS).unwrap();
        assert_eq!(host.alerts.notifier.as_deref(), Some("phone"));
        assert_eq!(host.notifiers.len(), 1);
        assert_eq!(
            host.alerts.summary(),
            "notifier=phone needs_human=on stall_minutes=30 host_offline_minutes=5 hub_down_minutes=2 watch_hub=off"
        );
        let plain: HostFile = toml::from_str("[host]\nname = \"laptop\"\n").unwrap();
        assert_eq!(plain.alerts.notifier, None);
        assert!(plain.notifiers.is_empty());
        assert!(plain.notifier().unwrap().is_none());
        // The defaults of a daemon that says nothing about alerts.
        assert_eq!(
            plain.alerts.summary(),
            "notifier=desktop needs_human=on stall_minutes=30 host_offline_minutes=5 hub_down_minutes=2 watch_hub=off"
        );
    }

    #[tokio::test]
    async fn reserve_mem_mb_reaches_the_host_config() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            harnesses = ["pi"]
            reserve_mem_mb = 2048
            "#,
        )
        .unwrap();
        assert_eq!(host_config(&file).await.unwrap().reserve_mem_mb, 2048);
        // A host that names no reserve keeps the default of 1024 MB.
        let plain: HostFile = toml::from_str("[host]\nharnesses = [\"pi\"]\n").unwrap();
        assert_eq!(host_config(&plain).await.unwrap().reserve_mem_mb, 1024);
    }

    #[tokio::test]
    async fn alerts_watch_hub_reaches_the_host_config() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            name = "laptop"
            harnesses = ["pi"]
            [alerts]
            watch_hub = true
            hub_down_minutes = 5
            "#,
        )
        .unwrap();
        let cfg = host_config(&file).await.unwrap();
        assert!(cfg.watch_hub);
        assert_eq!(cfg.hub_down, Duration::from_secs(300));
        // A host with no [alerts] watches nothing and keeps the default.
        let plain: HostFile = toml::from_str("[host]\nharnesses = [\"pi\"]\n").unwrap();
        let cfg = host_config(&plain).await.unwrap();
        assert!(!cfg.watch_hub);
        assert_eq!(cfg.hub_down, Duration::from_secs(120));
    }
}

#[cfg(test)]
mod secrets_tests {
    use super::*;

    /// The `host.toml` example of the phase 1 spec, without comments.
    const SPEC_HOST: &str = r#"
[secrets.stores.op]
kind = "exec"
command = ["borant-secrets-1password"]
credential = "store-token"
timeout_secs = 10

[secrets.bundles.claude-auth]
store = "op"
[secrets.bundles.claude-auth.members]
a = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-a/oauth-token" }
b = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-b/oauth-token" }
c = { CLAUDE_CODE_OAUTH_TOKEN = "op://Borant/claude-c/oauth-token" }

[secrets.harness]
"*" = ["github"]
claude-code = ["claude-auth"]
omp = ["lunaroute"]
"#;

    #[test]
    fn config_parses_the_spec_host_toml() {
        let file: HostFile = toml::from_str(SPEC_HOST).unwrap();
        let secrets = file.secrets.expect("a [secrets] section");
        assert!(matches!(
            secrets.stores["op"],
            StoreConfig::Exec {
                timeout_secs: 10,
                ..
            }
        ));
        let name: BundleName = "claude-auth".parse().unwrap();
        let auth = &secrets.bundles[&name];
        assert_eq!(auth.store, "op");
        assert_eq!(auth.members.len(), 3);
        let a: MemberName = "a".parse().unwrap();
        assert_eq!(
            auth.members[&a]["CLAUDE_CODE_OAUTH_TOKEN"],
            SecretRef("op://Borant/claude-a/oauth-token".into())
        );
        assert_eq!(
            secrets.harness["*"],
            vec!["github".parse::<BundleName>().unwrap()]
        );
        assert_eq!(secrets.harness["claude-code"], vec![name.clone()]);
        assert_eq!(
            secrets.harness["omp"],
            vec!["lunaroute".parse::<BundleName>().unwrap()]
        );
        // The bundle name comes from the table key.
        let opened = secrets.config();
        assert_eq!(opened.bundles[&name].name, name);
    }

    #[tokio::test]
    async fn config_announces_the_members_of_a_bundle() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            name = "e-2"
            harnesses = ["claude-code", "omp"]

            [secrets.stores.env]
            kind = "env"

            [secrets.bundles.claude-auth]
            store = "env"
            [secrets.bundles.claude-auth.members]
            a = { CLAUDE_CODE_OAUTH_TOKEN = "TOKEN_A" }
            b = { CLAUDE_CODE_OAUTH_TOKEN = "TOKEN_B" }

            [secrets.harness]
            claude-code = ["claude-auth"]
            "#,
        )
        .unwrap();
        let cfg = host_config(&file).await.unwrap();
        let info = cfg.info();
        let name: BundleName = "claude-auth".parse().unwrap();
        let bundle = info.members.get(&name).expect("the bundle is announced");
        assert_eq!(
            bundle.members,
            vec!["a".parse().unwrap(), "b".parse().unwrap()]
        );
        assert_eq!(bundle.harnesses, vec![Harness::ClaudeCode]);
        assert_eq!(
            bundle.members.len(),
            2,
            "the hub picks only for two or more"
        );
    }

    #[tokio::test]
    async fn config_without_secrets_is_todays_host() {
        let file: HostFile = toml::from_str(
            r#"
            [host]
            name = "laptop"
            harnesses = ["pi", "omp"]
            "#,
        )
        .unwrap();
        let cfg = host_config(&file).await.unwrap();
        assert!(cfg.secrets.is_none());
        assert!(cfg.info().members.is_empty());
        assert_eq!(cfg.harnesses, vec![Harness::Pi, Harness::Omp]);
    }

    #[test]
    fn config_rejects_an_unknown_secrets_key() {
        assert!(
            toml::from_str::<HostFile>("[secrets]\nnope = 1\n").is_err(),
            "deny_unknown_fields stays"
        );
        assert!(
            toml::from_str::<HostFile>("[secrets.bundles.x]\nstore = \"s\"\nnope = 1\n").is_err(),
            "deny_unknown_fields stays"
        );
    }

    #[test]
    fn hub_file_parses_roles() {
        let f: HubFile = toml::from_str(
            r#"
            [roles.chief]
            harness = "claude-code"
            model = "opus"
            pinned = true
            one_per = "collection"
            spawns = ["worker"]
            skill = "borant:chief-of-staff"

            [roles.worker]
            harness = "omp"
            tools_deny = ["spawn", "move"]
            sandbox = "namespace"
            spawns = []
            "#,
        )
        .unwrap();
        let chief = f.roles.get(&"chief".parse().unwrap()).unwrap();
        assert_eq!(chief.harness, Some(Harness::ClaudeCode));
        assert_eq!(chief.model.as_deref(), Some("opus"));
        assert!(chief.pinned);
        assert_eq!(
            chief.one_per,
            Some(borant_core::rules::roles::OnePer::Collection)
        );
        assert_eq!(chief.spawns, vec!["worker".parse().unwrap()]);
        assert_eq!(chief.skill.as_deref(), Some("borant:chief-of-staff"));
        let worker = f.roles.get(&"worker".parse().unwrap()).unwrap();
        assert_eq!(worker.tools_deny, vec!["spawn", "move"]);
        assert_eq!(
            worker.sandbox,
            Some(borant_core::model::SandboxKind::Namespace),
            "a role names the sandbox of its launches"
        );
        assert!(worker.spawns.is_empty());
    }

    #[test]
    fn hub_file_parses_caps_channels_and_changes() {
        let f: HubFile = toml::from_str(
            r#"
            [roles.worker]
            max_turns_per_day = 200
            max_session_hours = 8

            [channels.api]
            max_agents = 4

            [changes]
            adapter = "exec"
            program = "borant-changes-github"
            every_secs = 30
            "#,
        )
        .unwrap();
        let worker = f.roles.get(&"worker".parse().unwrap()).unwrap();
        assert_eq!(worker.max_turns_per_day, Some(200));
        assert_eq!(worker.max_session_hours, Some(8));
        let api = f.channels.get(&"api".parse().unwrap()).unwrap();
        assert_eq!(api.max_agents, Some(4));
        assert_eq!(f.changes.adapter.as_deref(), Some("exec"));
        assert_eq!(f.changes.program.as_deref(), Some("borant-changes-github"));
        assert_eq!(f.changes.every_secs, 30);

        // A hub that says nothing about the new tables keeps the defaults.
        let plain: HubFile = toml::from_str("").unwrap();
        assert!(plain.channels.is_empty());
        assert!(plain.roles.is_empty());
        assert_eq!(plain.changes, ChangesConfig::default());
        assert_eq!(plain.changes.every_secs, 60);
    }
}

#[cfg(test)]
mod elastic_tests {
    use super::*;

    /// The phase 4 spec example of `hub.toml`, without comments and with a
    /// neutral plugin name.
    const SPEC_HUB: &str = r#"
[elastic]
tick = "10s"
lost_timeout = "10m"
create_backoff = "5m"
monthly_budget_usd = 80

[provisioners.cloud]
adapter = "exec"
command = ["borant-provision-fake"]
config = { firewall = "locked", ssh_key = "borant" }

[[pools]]
name = "workers"
provisioner = "cloud"
size = "small"
region = "r1"
image = "worker"
name_prefix = "borant-e"
tags = ["vps"]
harnesses = ["claude-code", "omp"]
agents_per_host = 4
max_hosts = 2
idle_timeout = "20m"
boot_timeout = "10m"
monthly_budget_usd = 60
price_hour_usd = 0.1
"#;

    #[test]
    fn config_parses_the_spec_hub_toml() {
        let file: HubFile = toml::from_str(SPEC_HUB).expect("the spec example");
        let elastic = file.elastic_config().unwrap().expect("a pool");
        assert_eq!(elastic.tick, Duration::from_secs(10));
        assert_eq!(elastic.lost_timeout, Duration::from_secs(600));
        assert_eq!(elastic.create_backoff, Duration::from_secs(300));
        assert_eq!(elastic.monthly_budget_usd, Some(80.0));
        assert_eq!(elastic.provisioners.len(), 1);
        assert!(elastic.provisioners.contains_key("cloud"));

        let pool = &elastic.pools[0];
        assert_eq!(pool.name, "workers".parse::<PoolName>().unwrap());
        assert_eq!(pool.provisioner, "cloud");
        assert_eq!(pool.size, "small");
        assert_eq!(pool.region, "r1");
        assert_eq!(pool.image.as_deref(), Some("worker"));
        assert_eq!(pool.name_prefix, "borant-e");
        assert_eq!(pool.tags, vec!["vps".parse::<Tag>().unwrap()]);
        assert_eq!(pool.harnesses, vec![Harness::ClaudeCode, Harness::Omp]);
        assert_eq!(pool.agents_per_host, 4);
        assert_eq!(pool.max_hosts, 2);
        assert_eq!(pool.idle_timeout, Duration::from_secs(1200));
        assert_eq!(pool.boot_timeout, Duration::from_secs(600));
        assert_eq!(pool.monthly_budget_usd, Some(60.0));
        assert_eq!(pool.price_hour_usd, Some(0.1));
        // The rules take the lost timeout and the backoff from the pool.
        assert_eq!(pool.lost_timeout, elastic.lost_timeout);
        assert_eq!(pool.backoff_until, None);
    }

    /// The hub of the elastic e2e scenario. CI does not run that scenario, so
    /// this test is the check that its `hub.toml` parses.
    #[test]
    fn config_parses_the_elastic_e2e_hub_toml() {
        let text = include_str!("../../../tests/e2e/elastic/hub.toml");
        let file: HubFile = toml::from_str(text).expect("tests/e2e/elastic/hub.toml");
        let elastic = file.elastic_config().unwrap().expect("one pool");
        assert_eq!(elastic.tick, Duration::from_secs(2));
        assert_eq!(elastic.lost_timeout, Duration::from_secs(8));
        assert_eq!(elastic.create_backoff, Duration::from_secs(5));
        assert!(elastic.provisioners.contains_key("docker"));
        let docker = &file.provisioners["docker"];
        assert_eq!(docker.command, vec!["/plugins/provision-docker.sh"]);
        // The `config` table is a flat map of strings: the command is words
        // and each env variable is one key.
        assert_eq!(docker.config["command"], "borant host");
        assert_eq!(docker.config["env.BORANT_MAX_AGENTS"], "2");
        let pool = &elastic.pools[0];
        assert_eq!(pool.name, "e2e".parse::<PoolName>().unwrap());
        assert_eq!(pool.harnesses, vec![Harness::Fake]);
        assert_eq!(pool.agents_per_host, 2);
        assert_eq!(pool.max_hosts, 2);
        assert_eq!(pool.idle_timeout, Duration::from_secs(30));
    }

    #[test]
    fn config_reads_the_member_rules() {
        let file: HubFile = toml::from_str(
            "[secrets.members]\nunknown_reset_mins = 60\nmax_restarts_per_hour = 3\n",
        )
        .unwrap();
        assert_eq!(file.secrets.members.unknown_reset_mins, 60);
        assert_eq!(file.secrets.members.max_restarts_per_hour, 3);
        // A missing key takes its default, and a missing section takes both.
        let file: HubFile = toml::from_str("[secrets.members]\nunknown_reset_mins = 30\n").unwrap();
        assert_eq!(file.secrets.members.unknown_reset_mins, 30);
        assert_eq!(file.secrets.members.max_restarts_per_hour, 3);
        let file: HubFile = toml::from_str("").unwrap();
        assert_eq!(file.secrets.members, MemberRules::default());
        assert!(
            toml::from_str::<HubFile>("[secrets.members]\nnope = 1\n").is_err(),
            "deny_unknown_fields stays"
        );
    }

    #[test]
    fn config_without_pools_has_no_elastic() {
        let file: HubFile = toml::from_str("[elastic]\ntick = \"5s\"\n").unwrap();
        assert!(file.elastic_config().unwrap().is_none());
        let file: HubFile = toml::from_str("").unwrap();
        assert!(file.elastic_config().unwrap().is_none());
    }

    #[test]
    fn config_refuses_the_cut_and_unknown_keys() {
        for cut in ["keep_stopped = 1", "min_warm = 1", "nope = 1"] {
            let toml = format!(
                "[[pools]]\nname = \"workers\"\nprovisioner = \"cloud\"\nsize = \"small\"\nregion = \"r1\"\nharnesses = [\"omp\"]\nagents_per_host = 1\nmax_hosts = 1\n{cut}\n"
            );
            assert!(
                toml::from_str::<HubFile>(&toml).is_err(),
                "{cut} is refused"
            );
        }
        assert!(
            toml::from_str::<HubFile>("[elastic]\nnope = 1\n").is_err(),
            "deny_unknown_fields stays"
        );
        assert!(
            toml::from_str::<HubFile>("[provisioners.cloud]\nadapter = \"exec\"\nnope = 1\n")
                .is_err(),
            "deny_unknown_fields stays"
        );
    }

    #[test]
    fn config_refuses_a_pool_with_an_unknown_provisioner() {
        let toml = r#"
[[pools]]
name = "workers"
provisioner = "cloud"
size = "small"
region = "r1"
harnesses = ["omp"]
agents_per_host = 1
max_hosts = 1
"#;
        let file: HubFile = toml::from_str(toml).unwrap();
        let err = file.elastic_config().unwrap_err().to_string();
        assert!(err.contains("unknown provisioner"), "{err}");
    }

    #[test]
    fn spans_parse_human_durations() {
        for (text, secs) in [
            ("10s", 10),
            ("20m", 1200),
            ("1h", 3600),
            ("2d", 172_800),
            ("1h30m", 5400),
            ("45", 45),
            ("1.5h", 5400),
        ] {
            assert_eq!(
                parse_span(text).unwrap(),
                Duration::from_secs(secs),
                "{text}"
            );
        }
        assert!(parse_span("").is_err());
        assert!(parse_span("10x").is_err());
        assert!(parse_span("m").is_err());
    }
}

#[cfg(test)]
mod changes_tests {
    use super::*;

    /// The file plugin of the repository root: change statuses in one file.
    fn plugin() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/plugins/changes-file.sh")
            .canonicalize()
            .expect("the file plugin")
    }

    fn file(toml: &str) -> HubFile {
        toml::from_str(toml).expect("a hub file")
    }

    /// `[changes]` with an exec adapter builds the watch of the hub.
    #[tokio::test]
    async fn changes_the_exec_adapter_builds_the_watch() {
        let f = file(&format!(
            "[changes]\nadapter = \"exec\"\nprogram = \"{}\"\nevery_secs = 30\n",
            plugin().display()
        ));
        let watch = f.changes_watch().await.unwrap().expect("the watch");
        assert_eq!(watch.every, Duration::from_secs(30));
    }

    /// A hub that names no adapter watches nothing, as before.
    #[tokio::test]
    async fn changes_without_an_adapter_watch_nothing() {
        assert!(file("").changes_watch().await.unwrap().is_none());
        assert!(
            file("[changes]\nevery_secs = 5\n")
                .changes_watch()
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A credential names a file in the directory that systemd provides, as
    /// the provisioner and the notifier plugins read theirs: without the
    /// directory the watch fails the start, not the first poll.
    #[tokio::test]
    async fn changes_a_credential_needs_the_directory_of_the_unit() {
        let f = file(&format!(
            "[changes]\nadapter = \"exec\"\nprogram = \"{}\"\ncredential = \"changes-token\"\n",
            plugin().display()
        ));
        assert_eq!(f.changes.credential.as_deref(), Some("changes-token"));
        if env("CREDENTIALS_DIRECTORY").is_none() {
            assert!(
                f.changes_watch().await.is_err(),
                "a credential without its directory is a configuration error"
            );
        }
    }

    /// A program of another kind, or none at all, leaves the watch off (a
    /// program of another kind is a warn) or fails the start (no program).
    #[tokio::test]
    async fn changes_a_program_of_another_kind_leaves_the_watch_off() {
        let notifier = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../crates/borant-exec/tests/fixtures/describe_notifier.sh")
            .canonicalize()
            .expect("the notifier fixture");
        let f = file(&format!(
            "[changes]\nadapter = \"exec\"\nprogram = \"{}\"\n",
            notifier.display()
        ));
        assert!(f.changes_watch().await.unwrap().is_none());
        let f = file("[changes]\nadapter = \"exec\"\n");
        assert!(
            f.changes_watch().await.is_err(),
            "an exec adapter needs a program"
        );
        let f = file("[changes]\nadapter = \"github\"\nprogram = \"x\"\n");
        assert!(f.changes_watch().await.is_err(), "only exec exists");
    }
}
