//! Launcher adapters for the [`Launcher`] port: a detached process, tmux,
//! zellij, Orca, and herdr. [`harness::command`] builds the program for each
//! harness, [`sandbox::wrap`] puts the bubblewrap sandbox in front of it, and
//! [`trust`] marks the launch directory as trusted for Claude Code.

pub mod harness;
pub mod herdr;
mod orca;
mod process;
pub mod sandbox;
mod tmux;
pub mod trust;
mod zellij;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use borant_core::AgentId;
use borant_core::ports::{LaunchHandle, Launcher, PortError, PortResult};

pub use herdr::{HerdrConfig, HerdrLauncher};
pub use orca::OrcaLauncher;
pub use process::ProcessLauncher;
pub use sandbox::{SandboxPlan, wrap};
pub use tmux::{DEFAULT_TMUX_SOCKET, TmuxConfig, TmuxLauncher};
pub use zellij::ZellijLauncher;

/// The variables of the host daemon that a session must never see: the NATS
/// credential of the host, and the systemd credential folder of its unit. A
/// launcher removes them from the program's environment, whatever the daemon,
/// the terminal server, or an old tmux server passes down.
pub const HOST_ONLY_ENV: &[&str] = &[
    "BORANT_NATS_USER",
    "BORANT_NATS_PASSWORD",
    "CREDENTIALS_DIRECTORY",
];

/// The `env` arguments that remove `HOST_ONLY_ENV`: `-u NAME` for each.
pub(crate) fn unset_host_only() -> Vec<String> {
    HOST_ONLY_ENV
        .iter()
        .flat_map(|name| ["-u".to_owned(), (*name).to_owned()])
        .collect()
}

/// Returns the launcher for a host configuration `launcher` value:
/// `process`, `headless` (the same as `process`), `tmux`, `zellij`, `orca`,
/// or `herdr`. The process launcher writes logs to `log_dir`, and the zellij
/// launcher writes its layout files there.
pub fn launcher_for(kind: &str, log_dir: PathBuf) -> Result<Box<dyn Launcher>, String> {
    launcher_for_with(
        kind,
        log_dir,
        &HerdrConfig::default(),
        &TmuxConfig::default(),
    )
}

/// [`launcher_for`], with the `[herdr]` and `[tmux]` sections of `host.toml`
/// for the herdr and tmux launchers.
pub fn launcher_for_with(
    kind: &str,
    log_dir: PathBuf,
    herdr: &HerdrConfig,
    tmux: &TmuxConfig,
) -> Result<Box<dyn Launcher>, String> {
    match kind {
        "process" | "headless" => Ok(Box::new(ProcessLauncher::new(log_dir))),
        "tmux" => Ok(Box::new(TmuxLauncher::new(tmux.clone()))),
        "zellij" => Ok(Box::new(ZellijLauncher::new(log_dir))),
        "orca" => Ok(Box::new(OrcaLauncher)),
        "herdr" => Ok(Box::new(HerdrLauncher::new(herdr.clone()))),
        other => Err(unknown_launcher(other)),
    }
}

/// The launcher that a host uses when its `host.toml` names none.
pub const DEFAULT_LAUNCHER: &str = "tmux";

/// The program that `kind` needs on `PATH`, or `None` when the launcher is
/// this binary. The herdr program is the one of `[herdr]`. `borant doctor`
/// checks it without starting it.
pub fn launcher_program(kind: &str, herdr: &HerdrConfig) -> Result<Option<String>, String> {
    match kind {
        "process" | "headless" => Ok(None),
        "tmux" => Ok(Some("tmux".to_owned())),
        "zellij" => Ok(Some("zellij".to_owned())),
        "orca" => Ok(Some("orca".to_owned())),
        "herdr" => Ok(Some(herdr.program.clone())),
        other => Err(unknown_launcher(other)),
    }
}

fn unknown_launcher(kind: &str) -> String {
    format!("unknown launcher {kind:?}: use process, headless, tmux, zellij, orca, or herdr")
}

/// The terminal multiplexer session name for an agent.
fn session_name(agent: &AgentId) -> String {
    format!("borant-{agent}")
}

fn check_kind(handle: &LaunchHandle, kind: &str) -> PortResult<()> {
    if handle.kind == kind {
        Ok(())
    } else {
        Err(PortError::other(format!(
            "the {kind} launcher cannot use a {} handle",
            handle.kind
        )))
    }
}

fn path_str(path: &Path) -> PortResult<&str> {
    path.to_str()
        .ok_or_else(|| PortError::other(format!("path is not UTF-8: {}", path.display())))
}

/// Runs a command to completion with no stdin and captures its output.
async fn run(program: &str, args: &[String]) -> PortResult<Output> {
    tokio::process::Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                PortError::unavailable(format!("{program} is not on PATH"))
            } else {
                PortError::other(format!("cannot run {program}: {e}"))
            }
        })
}

/// Runs a command and fails with its stderr if it exits with an error.
async fn run_ok(program: &str, args: &[String]) -> PortResult<Output> {
    let out = run(program, args).await?;
    if out.status.success() {
        Ok(out)
    } else {
        Err(PortError::other(format!(
            "{program} {} failed ({}): {}",
            args.first().map(String::as_str).unwrap_or_default(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_for_known_kinds() {
        for (kind, want) in [
            ("process", "process"),
            ("headless", "process"),
            ("tmux", "tmux"),
            ("zellij", "zellij"),
            ("orca", "orca"),
            ("herdr", "herdr"),
        ] {
            let launcher = launcher_for(kind, PathBuf::from("/tmp")).unwrap();
            assert_eq!(launcher.kind(), want);
        }
        assert!(launcher_for("screen", PathBuf::from("/tmp")).is_err());
        let herdr = HerdrConfig {
            session: "p5".into(),
            program: "herdr-dev".into(),
            ssh: None,
        };
        let launcher = launcher_for_with(
            "herdr",
            PathBuf::from("/tmp"),
            &herdr,
            &TmuxConfig::default(),
        )
        .unwrap();
        assert_eq!(launcher.kind(), "herdr");
        let tmux = launcher_for_with(
            "tmux",
            PathBuf::from("/tmp"),
            &HerdrConfig::default(),
            &TmuxConfig {
                socket: "check".into(),
            },
        )
        .unwrap();
        assert_eq!(tmux.kind(), "tmux");
    }
}
