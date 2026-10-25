use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};

use crate::{check_kind, path_str, run, run_ok, session_name};

const KIND: &str = "tmux";

/// Starts the program in a detached tmux session named `borant-{agent}`. The
/// handle ID is the session name. The session ends when the program exits.
#[derive(Debug, Clone, Copy, Default)]
pub struct TmuxLauncher;

/// The `tmux` arguments that create the session for `spec`.
pub fn new_session_args(spec: &LaunchSpec) -> PortResult<Vec<String>> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    let mut args = vec![
        "new-session".to_owned(),
        "-d".to_owned(),
        "-s".to_owned(),
        session_name(&spec.agent),
        "-c".to_owned(),
        path_str(&spec.cwd)?.to_owned(),
    ];
    for (k, v) in &spec.env {
        args.push("-e".to_owned());
        args.push(format!("{k}={v}"));
    }
    args.push("--".to_owned());
    args.extend(spec.program.iter().cloned());
    Ok(args)
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
fn capture_args(name: &str, lines: usize) -> Vec<String> {
    vec![
        "capture-pane".to_owned(),
        "-p".to_owned(),
        "-S".to_owned(),
        format!("-{lines}"),
        "-t".to_owned(),
        format!("={name}:"),
    ]
}

async fn has_session(name: &str) -> PortResult<bool> {
    let mut args = vec!["has-session".to_owned()];
    args.extend(target(name));
    Ok(run("tmux", &args).await?.status.success())
}

#[async_trait::async_trait]
impl Launcher for TmuxLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let args = new_session_args(spec)?;
        let name = session_name(&spec.agent);
        if has_session(&name).await? {
            return Err(PortError::conflict(format!(
                "tmux session {name} already exists"
            )));
        }
        run_ok("tmux", &args).await?;
        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id: name,
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        check_kind(handle, KIND)?;
        if !has_session(&handle.id).await? {
            return Ok(());
        }
        let mut args = vec!["kill-session".to_owned()];
        args.extend(target(&handle.id));
        run_ok("tmux", &args).await.map(drop)
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        check_kind(handle, KIND)?;
        has_session(&handle.id).await
    }

    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>> {
        Some(vec![
            "tmux".to_owned(),
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
        let args = capture_args(&handle.id, lines);
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
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    #[test]
    fn new_session_argv() {
        assert_eq!(
            new_session_args(&spec("a1")).unwrap(),
            [
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
                "sleep",
                "30"
            ]
        );
    }

    #[test]
    fn capture_argv() {
        assert_eq!(
            capture_args("borant-a1", 20),
            ["capture-pane", "-p", "-S", "-20", "-t", "=borant-a1:"]
        );
    }

    /// Kills the session when the test ends, even if an assert fails. A
    /// session on its own `-L` socket is named by `socket`; `None` is the
    /// default server, which only the launcher's own test uses.
    struct KillOnDrop {
        socket: Option<String>,
        name: String,
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let mut cmd = std::process::Command::new("tmux");
            if let Some(socket) = &self.socket {
                cmd.arg("-L").arg(socket);
            }
            let _ = cmd
                .args(["kill-session", "-t", &format!("={}", self.name)])
                .output();
        }
    }

    /// A real tmux 3.x must accept the capture target. The session lives on
    /// its own socket so the server that holds the Borant sessions is never
    /// touched.
    #[test]
    fn capture_a_line_from_a_real_session() {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("skipping: tmux is not on PATH");
            return;
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let socket = format!("test-cap-{}-{nanos}", std::process::id());
        let name = format!("borant-cap-{nanos}");
        let _guard = KillOnDrop {
            socket: Some(socket.clone()),
            name: name.clone(),
        };
        let new = std::process::Command::new("tmux")
            .arg("-L")
            .arg(&socket)
            .args([
                "new-session",
                "-d",
                "-s",
                &name,
                "--",
                "sh",
                "-c",
                "echo known-line; sleep 30",
            ])
            .output()
            .unwrap();
        assert!(new.status.success(), "tmux new-session failed: {new:?}");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let out = std::process::Command::new("tmux")
                .arg("-L")
                .arg(&socket)
                .args(capture_args(&name, 20))
                .output()
                .unwrap();
            assert!(out.status.success(), "tmux capture-pane failed: {out:?}");
            if String::from_utf8_lossy(&out.stdout).contains("known-line") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no known line after 5 s: {out:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[tokio::test]
    async fn launch_and_stop() {
        if std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("skipping: tmux is not on PATH");
            return;
        }
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let agent = format!("t{}-{nanos}", std::process::id());
        let _guard = KillOnDrop {
            socket: None,
            name: format!("borant-{agent}"),
        };
        let launcher = TmuxLauncher;
        let s = spec(&agent);

        let handle = launcher.launch(&s).await.unwrap();
        assert_eq!(handle.id, format!("borant-{agent}"));
        assert!(launcher.is_running(&handle).await.unwrap());
        let again = launcher.launch(&s).await.unwrap_err();
        assert!(again.is(borant_core::ports::PortErrorKind::Conflict));
        assert_eq!(
            launcher.attach_command(&handle).unwrap(),
            ["tmux", "attach", "-t", format!("={}", handle.id).as_str()]
        );

        launcher.stop(&handle).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!launcher.is_running(&handle).await.unwrap());
    }
}
