use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use borant_core::AgentId;
use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid;
use tokio::time::Instant;

use crate::check_kind;

const KIND: &str = "process";
/// Time between SIGTERM and SIGKILL in `stop`.
const STOP_GRACE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(100);

/// Starts the program as a detached child in its own process group, with
/// stdout and stderr appended to `{log_dir}/{agent}.log`. The handle ID is
/// the PID. A background task reaps the child, so it needs a Tokio runtime.
#[derive(Debug, Clone)]
pub struct ProcessLauncher {
    log_dir: PathBuf,
}

impl ProcessLauncher {
    pub fn new(log_dir: PathBuf) -> Self {
        Self { log_dir }
    }

    /// The log file for an agent.
    pub fn log_path(&self, agent: &AgentId) -> PathBuf {
        self.log_dir.join(format!("{agent}.log"))
    }
}

fn pid(handle: &LaunchHandle) -> PortResult<Pid> {
    check_kind(handle, KIND)?;
    handle
        .id
        .parse::<i32>()
        .ok()
        .filter(|p| *p > 0)
        .map(Pid::from_raw)
        .ok_or_else(|| PortError::other(format!("bad PID in handle: {:?}", handle.id)))
}

/// True if any process in the group led by `pgid` still exists.
fn group_alive(pgid: Pid) -> PortResult<bool> {
    match killpg(pgid, None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(e) => Err(PortError::other(format!("cannot check group {pgid}: {e}"))),
    }
}

/// Sends `signal` to the group. A group that is gone is not an error.
fn signal_group(pgid: Pid, signal: Signal) -> PortResult<()> {
    match killpg(pgid, signal) {
        Ok(()) | Err(Errno::ESRCH) => Ok(()),
        Err(e) => Err(PortError::other(format!(
            "cannot send {signal} to group {pgid}: {e}"
        ))),
    }
}

#[async_trait::async_trait]
impl Launcher for ProcessLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let (program, args) = spec
            .program
            .split_first()
            .ok_or_else(|| PortError::other("launch spec has an empty program"))?;
        std::fs::create_dir_all(&self.log_dir).map_err(|e| {
            PortError::other(format!("cannot create {}: {e}", self.log_dir.display()))
        })?;
        let path = self.log_path(&spec.agent);
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| PortError::other(format!("cannot open {}: {e}", path.display())))?;
        let stderr = stdout
            .try_clone()
            .map_err(|e| PortError::other(format!("cannot open {}: {e}", path.display())))?;

        let mut child = tokio::process::Command::new(program)
            .args(args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .process_group(0)
            .spawn()
            .map_err(|e| PortError::other(format!("cannot start {program}: {e}")))?;
        let id = child
            .id()
            .ok_or_else(|| PortError::other(format!("{program} exited before it got a PID")))?;

        let agent = spec.agent.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => tracing::debug!(%agent, pid = id, %status, "process exited"),
                Err(e) => tracing::warn!(%agent, pid = id, error = %e, "cannot wait for process"),
            }
        });

        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id: id.to_string(),
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        let pgid = pid(handle)?;
        signal_group(pgid, Signal::SIGTERM)?;
        let deadline = Instant::now() + STOP_GRACE;
        while Instant::now() < deadline {
            if !group_alive(pgid)? {
                return Ok(());
            }
            tokio::time::sleep(POLL).await;
        }
        tracing::warn!(pid = %pgid, "process group did not stop after SIGTERM, sending SIGKILL");
        signal_group(pgid, Signal::SIGKILL)
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        let pid = pid(handle)?;
        match kill(pid, None) {
            Ok(()) | Err(Errno::EPERM) => Ok(true),
            Err(Errno::ESRCH) => Ok(false),
            Err(e) => Err(PortError::other(format!("cannot check PID {pid}: {e}"))),
        }
    }

    fn attach_command(&self, _handle: &LaunchHandle) -> Option<Vec<String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use borant_core::model::Harness;

    use super::*;

    fn spec(agent: &str, cwd: PathBuf, program: &[&str], env: &[(&str, &str)]) -> LaunchSpec {
        LaunchSpec {
            agent: agent.parse().unwrap(),
            harness: Harness::Fake,
            cwd,
            program: program.iter().map(|s| (*s).to_owned()).collect(),
            env: env
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    async fn wait_stopped(launcher: &ProcessLauncher, handle: &LaunchHandle, limit: Duration) {
        let deadline = Instant::now() + limit;
        while launcher.is_running(handle).await.unwrap() {
            assert!(Instant::now() < deadline, "process still running");
            tokio::time::sleep(POLL).await;
        }
    }

    #[tokio::test]
    async fn launch_and_stop() {
        let logs = tempfile::tempdir().unwrap();
        let launcher = ProcessLauncher::new(logs.path().to_path_buf());
        let s = spec("sleeper", std::env::temp_dir(), &["sleep", "30"], &[]);
        let handle = launcher.launch(&s).await.unwrap();
        assert_eq!(handle.kind, "process");
        assert!(launcher.is_running(&handle).await.unwrap());
        assert_eq!(launcher.attach_command(&handle), None);

        launcher.stop(&handle).await.unwrap();
        wait_stopped(&launcher, &handle, Duration::from_secs(6)).await;
        assert!(launcher.log_path(&s.agent).exists());
    }

    #[tokio::test]
    async fn env_and_cwd() {
        let logs = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // `pwd` prints the physical path, and macOS temp dirs are symlinks.
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let launcher = ProcessLauncher::new(logs.path().join("nested"));
        let s = spec(
            "env-check",
            cwd.clone(),
            &["sh", "-c", "echo $FOO; pwd"],
            &[("FOO", "bar")],
        );
        let handle = launcher.launch(&s).await.unwrap();
        wait_stopped(&launcher, &handle, Duration::from_secs(5)).await;

        let log = std::fs::read_to_string(launcher.log_path(&s.agent)).unwrap();
        assert!(log.contains("bar"), "log: {log}");
        assert!(log.contains(cwd.to_str().unwrap()), "log: {log}");
    }

    #[tokio::test]
    async fn bad_handle_is_an_error() {
        let launcher = ProcessLauncher::new(std::env::temp_dir());
        let tmux = LaunchHandle {
            kind: "tmux".into(),
            id: "1".into(),
        };
        assert!(launcher.is_running(&tmux).await.is_err());
        let bad = LaunchHandle {
            kind: "process".into(),
            id: "nope".into(),
        };
        assert!(launcher.stop(&bad).await.is_err());
    }
}
