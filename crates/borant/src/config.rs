//! Configuration from `$BORANT_HOME/hub.toml` and `$BORANT_HOME/host.toml`,
//! with environment overrides. Every key has a default, so both files are
//! optional.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use borant_core::model::Harness;
use borant_core::ports::provision::Provisioner;
use borant_core::ports::secrets::{Bundle, SecretRef};
use borant_core::rules::lease::LeaseConfig;
use borant_core::rules::pool::PoolConfig;
use borant_core::{BundleName, HostName, MemberName, PoolName, Tag};
use borant_exec::provision::ExecProvisioner;
use borant_exec::ExecConfig;
use borant_host::{HostConfig, SecretConfig};
use borant_hub::ElasticConfig;
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

impl Span {
    pub fn secs(self) -> Duration {
        self.0
    }
}

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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostSection {
    pub name: Option<String>,
    pub launcher: Option<String>,
    pub max_agents: Option<u32>,
    pub always_on: Option<bool>,
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
    if let Some(v) = h.wakes_per_hour {
        cfg.wakes_per_hour = v;
    }
    if let Some(v) = h.keep_turns {
        cfg.keep_turns = v;
    }
    let timing = file.timing.lease_config()?;
    cfg.lease = timing.lease;
    cfg.heartbeat = timing.heartbeat;
    cfg.commands = file.commands.clone();
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
