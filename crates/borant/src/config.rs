//! Configuration from `$BORANT_HOME/hub.toml` and `$BORANT_HOME/host.toml`,
//! with environment overrides. Every key has a default, so both files are
//! optional.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use borant_core::model::Harness;
use borant_core::ports::secrets::{Bundle, SecretRef};
use borant_core::rules::lease::LeaseConfig;
use borant_core::{BundleName, HostName, MemberName, PoolName, Tag};
use borant_host::{HostConfig, SecretConfig};
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
