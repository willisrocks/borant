use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};

use crate::{check_kind, path_str, run, run_ok, session_name};

const KIND: &str = "tmux";

/// The tmux server that holds a host's sessions when `[tmux] socket` is
/// unset. It is separate from the default server, so an agent's own `tmux`
/// command never reaches the server that holds its session.
pub const DEFAULT_TMUX_SOCKET: &str = "borant";

/// The `[tmux]` settings of `host.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxConfig {
    /// The name of the tmux server, `borant` when unset.
    pub socket: String,
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            socket: DEFAULT_TMUX_SOCKET.to_owned(),
        }
    }
}

/// Starts the program in a detached tmux session named `borant-{agent}` on the
/// host's own tmux socket. The handle ID is the session name. The session ends
/// when the program exits.
#[derive(Debug, Clone)]
pub struct TmuxLauncher {
    socket: String,
}

impl Default for TmuxLauncher {
    fn default() -> Self {
        Self::new(TmuxConfig::default())
    }
}

impl TmuxLauncher {
    pub fn new(cfg: TmuxConfig) -> Self {
        Self { socket: cfg.socket }
    }

    /// The tmux server that the launcher starts sessions on.
    pub fn socket(&self) -> &str {
        &self.socket
    }
}

/// The `tmux` arguments that create the session for `spec` on `socket`.
pub fn new_session_args(socket: &str, spec: &LaunchSpec) -> PortResult<Vec<String>> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    let mut args = socket_args(Some(socket));
    args.extend([
        "new-session".to_owned(),
        "-d".to_owned(),
        "-s".to_owned(),
        session_name(&spec.agent),
        "-c".to_owned(),
        path_str(&spec.cwd)?.to_owned(),
    ]);
    for (k, v) in &spec.env {
        args.push("-e".to_owned());
        args.push(format!("{k}={v}"));
    }
    args.push("--".to_owned());
    // tmux sets TMUX and TMUX_PANE in the pane itself, whatever `-e` says, so
    // the program goes through `env` to drop them: an agent's own `tmux` then
    // reaches the default server, never the one that holds its session. The
    // same `env` drops the host's own credentials, which the pane inherits
    // from the daemon and from the global environment of the tmux server.
    args.extend(["env", "-u", "TMUX", "-u", "TMUX_PANE"].map(str::to_owned));
    args.extend(crate::unset_host_only());
    args.push("--".to_owned());
    args.extend(spec.program.iter().cloned());
    Ok(args)
}

/// `-L name` selects a tmux server. `None` is the default server of the host,
/// which only the fallback for sessions of an older launcher uses.
fn socket_args(socket: Option<&str>) -> Vec<String> {
    match socket {
        Some(socket) => vec!["-L".to_owned(), socket.to_owned()],
        None => Vec::new(),
    }
}

/// `=name` makes tmux match the session name exactly, not as a prefix. The
/// session-level calls (`has-session`, `kill-session`, `attach`) accept it.
fn target(name: &str) -> Vec<String> {
    vec!["-t".to_owned(), format!("={name}")]
}

/// The `tmux` arguments that read the last `lines` lines of `name`'s screen.
///
/// `capture-pane` needs a pane target, so the session name carries the
/// `:` separator: `=<name>:` is the first window of the exact session. A bare
/// `=<name>` fails on tmux 3.x with "cannot find pane", which left the
/// watchdog with an empty screen.
fn capture_args(socket: Option<&str>, name: &str, lines: usize) -> Vec<String> {
    let mut args = socket_args(socket);
    args.extend([
        "capture-pane".to_owned(),
        "-p".to_owned(),
        "-S".to_owned(),
        format!("-{lines}"),
        "-t".to_owned(),
        format!("={name}:"),
    ]);
    args
}

async fn has_session(socket: Option<&str>, name: &str) -> PortResult<bool> {
    let mut args = socket_args(socket);
    args.push("has-session".to_owned());
    args.extend(target(name));
    Ok(run("tmux", &args).await?.status.success())
}

#[async_trait::async_trait]
impl Launcher for TmuxLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let name = session_name(&spec.agent);
        if self.is_running_on(Some(&self.socket), &name).await?
            || self.is_running_on(None, &name).await?
        {
            return Err(PortError::conflict(format!(
                "tmux session {name} already exists"
            )));
        }
        let args = new_session_args(&self.socket, spec)?;
        run_ok("tmux", &args).await?;
        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id: name,
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        check_kind(handle, KIND)?;
        let on_borant = self.is_running_on(Some(&self.socket), &handle.id).await?;
        let socket = if on_borant {
            Some(self.socket.as_str())
        } else if self.is_running_on(None, &handle.id).await? {
            // A launcher of an older version started it on the default server.
            None
        } else {
            return Ok(());
        };
        let mut args = socket_args(socket);
        args.push("kill-session".to_owned());
        args.extend(target(&handle.id));
        run_ok("tmux", &args).await.map(drop)
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        check_kind(handle, KIND)?;
        Ok(self.is_running_on(Some(&self.socket), &handle.id).await?
            || self.is_running_on(None, &handle.id).await?)
    }

    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>> {
        Some(vec![
            "tmux".to_owned(),
            "-L".to_owned(),
            self.socket.clone(),
            "attach".to_owned(),
            "-t".to_owned(),
            // `=` matches the name exactly, so agent `a` never attaches to `a1`.
            format!("={}", handle.id),
        ])
    }

    /// The last `lines` lines of the session screen. This is the one tmux
    /// call that reads a screen, so it is synchronous: `capture-pane` answers
    /// in milliseconds, and a server that is gone gives `None`, because a
    /// missing screen is no error.
    fn capture(&self, handle: &LaunchHandle, lines: usize) -> Option<String> {
        if handle.kind != KIND {
            return None;
        }
        capture_on(Some(&self.socket), &handle.id, lines)
            // A launcher of an older version started it on the default server.
            .or_else(|| capture_on(None, &handle.id, lines))
    }
}

impl TmuxLauncher {
    /// Whether a session named `name` runs on `socket`; `None` is the default
    /// server.
    async fn is_running_on(&self, socket: Option<&str>, name: &str) -> PortResult<bool> {
        has_session(socket, name).await
    }
}

fn capture_on(socket: Option<&str>, name: &str, lines: usize) -> Option<String> {
    let args = capture_args(socket, name, lines);
    let out = std::process::Command::new("tmux")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use borant_core::model::Harness;

    use super::*;

    fn spec(agent: &str) -> LaunchSpec {
        LaunchSpec {
            agent: agent.parse().unwrap(),
            harness: Harness::Fake,
            cwd: PathBuf::from("/tmp"),
            program: vec!["sleep".into(), "30".into()],
            env: vec![("A".into(), "1".into()), ("B".into(), "x y".into())],
        }
    }

    fn tmux_available() -> bool {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_ok()
        {
            return true;
        }
        eprintln!("skipping: tmux is not on PATH");
        false
    }

    /// A name that no other test or process uses.
    fn unique(tag: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("{tag}-{}-{nanos}", std::process::id())
    }

    #[test]
    fn new_session_argv() {
        assert_eq!(
            new_session_args("borant", &spec("a1")).unwrap(),
            [
                "-L",
                "borant",
                "new-session",
                "-d",
                "-s",
                "borant-a1",
                "-c",
                "/tmp",
                "-e",
                "A=1",
                "-e",
                "B=x y",
                "--",
                "env",
                "-u",
                "TMUX",
                "-u",
                "TMUX_PANE",
                "-u",
                "BORANT_NATS_USER",
                "-u",
                "BORANT_NATS_PASSWORD",
                "-u",
                "CREDENTIALS_DIRECTORY",
                "--",
                "sleep",
                "30"
            ]
        );
    }

    #[test]
    fn capture_argv() {
        assert_eq!(
            capture_args(Some("borant"), "borant-a1", 20),
            [
                "-L",
                "borant",
                "capture-pane",
                "-p",
                "-S",
                "-20",
                "-t",
                "=borant-a1:"
            ]
        );
    }

    /// Only the fallback for a session of an older launcher leaves the
    /// socket out, and it then means the default server.
    #[test]
    fn socket_argv() {
        assert_eq!(socket_args(Some("borant")), ["-L", "borant"]);
        assert!(socket_args(None).is_empty());
    }

    /// Kills the session when the test ends, even if an assert fails.
    struct KillOnDrop {
        socket: String,
        name: String,
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = std::process::Command::new("tmux")
                .args(["-L", &self.socket, "kill-session", "-t"])
                .arg(format!("={}", self.name))
                .output();
        }
    }

    /// A real tmux 3.x must accept the capture target. The session lives on
    /// its own socket so the server that holds the Borant sessions is never
    /// touched.
    #[test]
    fn capture_a_line_from_a_real_session() {
        if !tmux_available() {
            return;
        }
        let socket = unique("test-cap");
        let name = format!("borant-{}", unique("cap"));
        let _guard = KillOnDrop {
            socket: socket.clone(),
            name: name.clone(),
        };
        let new = std::process::Command::new("tmux")
            .args(["-L", &socket, "new-session", "-d", "-s", &name])
            .args(["--", "sh", "-c", "echo known-line; sleep 30"])
            .output()
            .unwrap();
        assert!(new.status.success(), "tmux new-session failed: {new:?}");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let out = std::process::Command::new("tmux")
                .args(capture_args(Some(&socket), &name, 20))
                .output()
                .unwrap();
            assert!(out.status.success(), "tmux capture-pane failed: {out:?}");
            if String::from_utf8_lossy(&out.stdout).contains("known-line") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no known line after 5 s: {out:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test]
    async fn launch_and_stop() {
        if !tmux_available() {
            return;
        }
        let socket = unique("test-launch");
        let agent = unique("t");
        let _guard = KillOnDrop {
            socket: socket.clone(),
            name: format!("borant-{agent}"),
        };
        let launcher = TmuxLauncher::new(TmuxConfig {
            socket: socket.clone(),
        });
        assert_eq!(launcher.socket(), socket);
        let s = spec(&agent);

        let handle = launcher.launch(&s).await.unwrap();
        assert_eq!(handle.id, format!("borant-{agent}"));
        assert!(launcher.is_running(&handle).await.unwrap());
        let again = launcher.launch(&s).await.unwrap_err();
        assert!(again.is(borant_core::ports::PortErrorKind::Conflict));
        assert_eq!(
            launcher.attach_command(&handle).unwrap(),
            [
                "tmux",
                "-L",
                socket.as_str(),
                "attach",
                "-t",
                format!("={}", handle.id).as_str()
            ]
        );

        launcher.stop(&handle).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!launcher.is_running(&handle).await.unwrap());
    }

    /// An agent's own `tmux kill-server` must not end the Borant server: the
    /// launched program has no TMUX, so its tmux reaches the default server.
    /// The test gives that default server a private socket directory, so a
    /// kill there stays inside the test.
    #[tokio::test]
    async fn an_agents_kill_server_does_not_end_borant() {
        if !tmux_available() {
            return;
        }
        let socket = unique("test-kill");
        let launcher = TmuxLauncher::new(TmuxConfig {
            socket: socket.clone(),
        });
        let home = tempfile::tempdir().unwrap();

        let agent_a = unique("ka");
        let agent_b = unique("kb");
        let _guard_a = KillOnDrop {
            socket: socket.clone(),
            name: format!("borant-{agent_a}"),
        };
        let _guard_b = KillOnDrop {
            socket: socket.clone(),
            name: format!("borant-{agent_b}"),
        };

        let mut a = spec(&agent_a);
        a.program = vec![
            "sh".into(),
            "-c".into(),
            // `scratch` proves that a plain tmux command works, and the kill
            // of that default server proves that it is not the Borant server.
            "echo TMUX=[${TMUX-}]; \
             tmux new-session -d -s scratch -- sleep 30 && echo scratch-up; \
             tmux kill-server && echo default-killed; \
             sleep 30"
                .into(),
        ];
        a.env.push((
            "TMUX_TMPDIR".into(),
            home.path().to_str().unwrap().to_owned(),
        ));
        let b = spec(&agent_b);

        let handle_a = launcher.launch(&a).await.unwrap();
        let handle_b = launcher.launch(&b).await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let screen = loop {
            let screen = launcher.capture(&handle_a, 40).unwrap_or_default();
            if screen.contains("default-killed") {
                break screen;
            }
            assert!(
                Instant::now() < deadline,
                "the harness did not run its tmux after 5 s: {screen:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(
            screen.contains("TMUX=[]"),
            "the harness still has TMUX: {screen:?}"
        );
        assert!(
            screen.contains("scratch-up"),
            "plain tmux does not reach a default server: {screen:?}"
        );
        assert!(
            launcher.is_running(&handle_b).await.unwrap(),
            "the other Borant session ended with the default server"
        );

        launcher.stop(&handle_a).await.unwrap();
        launcher.stop(&handle_b).await.unwrap();
    }
}
