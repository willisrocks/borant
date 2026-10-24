//! Configuration from `$BORANT_HOME/hub.toml` and `$BORANT_HOME/host.toml`,
//! with environment overrides. Every key has a default, so both files are
//! optional.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use borant_core::HostName;
use borant_core::model::Harness;
use borant_core::rules::lease::LeaseConfig;
use borant_host::HostConfig;
use borant_launch::harness::HarnessCommands;
use borant_nats::NatsConfig;
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

pub fn host_config(file: &HostFile) -> anyhow::Result<HostConfig> {
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
    fn default_host_name_is_valid() {
        assert!(default_host_name().parse::<HostName>().is_ok());
    }
}
