//! Launcher adapters for the [`Launcher`] port: a detached process, tmux,
//! zellij, Orca, and herdr. [`harness::command`] builds the program for each
//! harness.

pub mod harness;
pub mod herdr;
mod orca;
mod process;
mod tmux;
mod zellij;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use borant_core::AgentId;
use borant_core::ports::{LaunchHandle, Launcher, PortError, PortResult};

pub use herdr::{HerdrConfig, HerdrLauncher};
pub use orca::OrcaLauncher;
pub use process::ProcessLauncher;
pub use tmux::TmuxLauncher;
pub use zellij::ZellijLauncher;

/// Returns the launcher for a host configuration `launcher` value:
/// `process`, `headless` (the same as `process`), `tmux`, `zellij`, `orca`,
/// or `herdr`. The process launcher writes logs to `log_dir`, and the zellij
/// launcher writes its layout files there.
pub fn launcher_for(kind: &str, log_dir: PathBuf) -> Result<Box<dyn Launcher>, String> {
    launcher_for_with(kind, log_dir, &HerdrConfig::default())
}

/// [`launcher_for`], with the `[herdr]` section of `host.toml` for the herdr
/// launcher.
pub fn launcher_for_with(
    kind: &str,
    log_dir: PathBuf,
    herdr: &HerdrConfig,
) -> Result<Box<dyn Launcher>, String> {
    match kind {
        "process" | "headless" => Ok(Box::new(ProcessLauncher::new(log_dir))),
        "tmux" => Ok(Box::new(TmuxLauncher)),
        "zellij" => Ok(Box::new(ZellijLauncher::new(log_dir))),
        "orca" => Ok(Box::new(OrcaLauncher)),
        "herdr" => Ok(Box::new(HerdrLauncher::new(herdr.clone()))),
        other => Err(format!(
            "unknown launcher {other:?}: use process, headless, tmux, zellij, orca, or herdr"
        )),
    }
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
        let launcher = launcher_for_with("herdr", PathBuf::from("/tmp"), &herdr).unwrap();
        assert_eq!(launcher.kind(), "herdr");
    }
}
