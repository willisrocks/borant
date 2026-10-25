//! The sandbox of this host: `[sandbox]` of `host.toml`, its start check, and
//! the plan that a launch binds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use borant_core::model::{Harness, SandboxKind};
use borant_core::proto::LaunchOrder;
use borant_launch::SandboxPlan;

use crate::HostConfig;

/// `[sandbox]` of `host.toml`, once the start check passed. The section is
/// absent on a host that offers no sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    /// The program on PATH, normally `bwrap`.
    pub program: String,
    /// Extra read-only binds, such as `~/.config/gh`.
    pub ro: Vec<PathBuf>,
    /// Extra writable binds.
    pub rw: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            program: "bwrap".to_owned(),
            ro: Vec::new(),
            rw: Vec::new(),
        }
    }
}

/// How long the start check may take before the host gives up on it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `<program> --ro-bind / / true` and drops the sandbox when it fails,
/// so a host that cannot sandbox still launches its agents: only the
/// isolation is missing, and the `warn` says so.
pub async fn probe(cfg: &mut HostConfig) {
    let Some(sandbox) = cfg.sandbox.clone() else {
        return;
    };
    let result = tokio::time::timeout(
        PROBE_TIMEOUT,
        tokio::process::Command::new(&sandbox.program)
            .args(["--ro-bind", "/", "/", "true"])
            .stdin(std::process::Stdio::null())
            .output(),
    )
    .await;
    let failure = match result {
        Err(_) => Some(format!("no answer in {} s", PROBE_TIMEOUT.as_secs())),
        Ok(Err(e)) => Some(e.to_string()),
        Ok(Ok(out)) if !out.status.success() => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Some(format!(
                "{}: {}",
                out.status,
                stderr.lines().next().unwrap_or_default()
            ))
        }
        Ok(Ok(_)) => None,
    };
    match failure {
        None => tracing::info!(
            host = %cfg.name,
            program = %sandbox.program,
            "the sandbox is available"
        ),
        Some(why) => {
            tracing::warn!(
                host = %cfg.name,
                program = %sandbox.program,
                "the sandbox start check failed, so this host offers none: {why}"
            );
            cfg.sandbox = None;
        }
    }
}

impl HostConfig {
    /// The plan that sandboxes a launch of `order`, or `None` when the order
    /// asks for no sandbox or this host offers none. `secrets_dir` is the
    /// directory of the shim; the harness may delete its file.
    pub fn sandbox_plan(
        &self,
        order: &LaunchOrder,
        cwd: &Path,
        secrets_dir: Option<&Path>,
    ) -> Option<SandboxPlan> {
        let sandbox = self.sandbox.as_ref()?;
        if order.sandbox != Some(SandboxKind::Namespace) {
            return None;
        }
        let home = self
            .home
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.home.clone());
        let (rw, ro) = harness_dirs(order.harness);
        let under_home = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
            paths.into_iter().map(|p| home.join(p)).collect()
        };
        Some(SandboxPlan {
            program: sandbox.program.clone(),
            system: system_paths(),
            home: home.clone(),
            work_dir: self.home.join("work").join(order.agent.as_str()),
            harness_dirs: existing(under_home(rw)),
            sockets: existing(vec![self.socket()]),
            secrets_dir: secrets_dir.map(Path::to_path_buf),
            ro: existing(
                under_home(ro)
                    .into_iter()
                    .chain(expand(&sandbox.ro, &home))
                    .collect(),
            ),
            rw: expand(&sandbox.rw, &home),
            cwd: cwd.to_path_buf(),
        })
    }
}

/// The directories of each harness under `$HOME`. The read-write ones hold
/// sessions and configuration; the read-only ones hold binaries, which run
/// from `~/.local`, and the model catalog of the Pi extension.
fn harness_dirs(harness: Harness) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let rw: &[&str] = match harness {
        Harness::ClaudeCode => &[".claude", ".claude.json"],
        Harness::Pi => &[".pi"],
        Harness::Omp => &[".omp"],
        Harness::Codex => &[".codex"],
        Harness::Fake => &[],
    };
    let ro: &[&str] = match harness {
        // The Pi model catalog holds no credential, and omp's Lunaroute
        // provider needs it.
        Harness::Omp => &[".local", ".pi/agent/models-store.json"],
        Harness::Fake => &[],
        _ => &[".local"],
    };
    (
        rw.iter().map(PathBuf::from).collect(),
        ro.iter().map(PathBuf::from).collect(),
    )
}

/// The system paths that a harness needs inside the sandbox. A bind of a
/// missing source fails the whole launch, so only the ones that exist on this
/// host are passed: a NixOS host has the store and the nix-ld interpreter, a
/// Debian host has `/lib64` and its libraries from `[sandbox] ro`.
const SYSTEM_PATHS: [&str; 5] = [
    "/nix/store",
    "/run/current-system",
    "/lib64",
    "/etc/resolv.conf",
    "/etc/ssl",
];

fn system_paths() -> Vec<PathBuf> {
    existing(SYSTEM_PATHS.iter().map(PathBuf::from).collect())
}

/// The paths that exist. A bind of a missing path fails the whole launch, and
/// a harness directory is optional (`~/.claude.json` appears on first use).
fn existing(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths
        .into_iter()
        .filter(|p| p.symlink_metadata().is_ok())
        .collect()
}

/// Expands a leading `~` of each path to `home`.
fn expand(paths: &[PathBuf], home: &Path) -> Vec<PathBuf> {
    paths
        .iter()
        .map(|p| match p.strip_prefix("~") {
            Ok(rest) => home.join(rest),
            Err(_) => p.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_dirs_per_harness() {
        let (rw, ro) = harness_dirs(Harness::ClaudeCode);
        assert_eq!(rw, [".claude", ".claude.json"].map(PathBuf::from));
        assert_eq!(ro, [".local"].map(PathBuf::from));

        let (rw, ro) = harness_dirs(Harness::Omp);
        assert_eq!(rw, [".omp"].map(PathBuf::from));
        assert_eq!(
            ro,
            [".local", ".pi/agent/models-store.json"].map(PathBuf::from)
        );

        let (rw, ro) = harness_dirs(Harness::Fake);
        assert!(rw.is_empty() && ro.is_empty());
    }

    #[test]
    fn the_system_binds_are_the_paths_that_exist() {
        let paths = system_paths();
        assert!(
            paths.len() <= SYSTEM_PATHS.len(),
            "only the listed paths can be bound"
        );
        for path in paths {
            assert!(path.exists(), "{} does not exist here", path.display());
        }
    }

    #[test]
    fn a_tilde_expands_to_home() {
        let home = Path::new("/home/chris");
        assert_eq!(
            expand(&[PathBuf::from("~/.config/gh")], home),
            [PathBuf::from("/home/chris/.config/gh")]
        );
        assert_eq!(
            expand(&[PathBuf::from("/var/cache")], home),
            [PathBuf::from("/var/cache")]
        );
    }
}
