use std::path::{Path, PathBuf};

use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortError, PortResult};

use crate::{check_kind, path_str, run, run_ok, session_name};

const KIND: &str = "zellij";

/// Starts the program in a background zellij session named `borant-{agent}`.
/// The handle ID is the session name.
///
/// zellij 0.45 has no flag that runs a command in a new detached session, so
/// the launcher writes a one-pane layout to `{layout_dir}/borant-{agent}.kdl`
/// and runs `zellij attach --create-background borant-{agent} options
/// --default-layout {file}`. The pane runs `env K=V ... program...` in the
/// spec's cwd.
///
/// Limit: when the program exits, zellij keeps the session (and the pane
/// with its exit status) alive. `is_running` reports whether the session is
/// live, not whether the program is.
#[derive(Debug, Clone)]
pub struct ZellijLauncher {
    layout_dir: PathBuf,
}

impl ZellijLauncher {
    pub fn new(layout_dir: PathBuf) -> Self {
        Self { layout_dir }
    }
}

/// A KDL string literal.
fn kdl(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The zellij layout that runs `spec.program` with its env in its cwd.
pub fn layout(spec: &LaunchSpec) -> PortResult<String> {
    if spec.program.is_empty() {
        return Err(PortError::other("launch spec has an empty program"));
    }
    let args: Vec<String> = spec
        .env
        .iter()
        .map(|(k, v)| kdl(&format!("{k}={v}")))
        .chain(spec.program.iter().map(|a| kdl(a)))
        .collect();
    Ok(format!(
        "layout {{\n    pane command=\"env\" cwd={} {{\n        args {}\n    }}\n}}\n",
        kdl(path_str(&spec.cwd)?),
        args.join(" ")
    ))
}

/// The `zellij` arguments that create the background session.
pub fn create_args(session: &str, layout_file: &Path) -> PortResult<Vec<String>> {
    Ok(vec![
        "attach".to_owned(),
        "--create-background".to_owned(),
        session.to_owned(),
        "options".to_owned(),
        "--default-layout".to_owned(),
        path_str(layout_file)?.to_owned(),
    ])
}

/// True if a live (not exited) session has this name.
async fn has_session(name: &str) -> PortResult<bool> {
    // The command exits with an error when there are no sessions.
    let out = run("zellij", &["list-sessions".to_owned(), "-n".to_owned()]).await?;
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|line| line.split_whitespace().next() == Some(name) && !line.contains("EXITED")))
}

#[async_trait::async_trait]
impl Launcher for ZellijLauncher {
    fn kind(&self) -> &'static str {
        KIND
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        let name = session_name(&spec.agent);
        let body = layout(spec)?;
        if has_session(&name).await? {
            return Err(PortError::conflict(format!(
                "zellij session {name} already exists"
            )));
        }
        // An exited session with this name would be resurrected instead.
        run("zellij", &["delete-session".to_owned(), name.clone()]).await?;

        std::fs::create_dir_all(&self.layout_dir).map_err(|e| {
            PortError::other(format!("cannot create {}: {e}", self.layout_dir.display()))
        })?;
        let file = self.layout_dir.join(format!("{name}.kdl"));
        std::fs::write(&file, body)
            .map_err(|e| PortError::other(format!("cannot write {}: {e}", file.display())))?;
        run_ok("zellij", &create_args(&name, &file)?).await?;
        Ok(LaunchHandle {
            kind: KIND.to_owned(),
            id: name,
        })
    }

    async fn stop(&self, handle: &LaunchHandle) -> PortResult<()> {
        check_kind(handle, KIND)?;
        if has_session(&handle.id).await? {
            run_ok("zellij", &["kill-session".to_owned(), handle.id.clone()]).await?;
        }
        // Remove the resurrection entry, if zellij saved one.
        run("zellij", &["delete-session".to_owned(), handle.id.clone()]).await?;
        Ok(())
    }

    async fn is_running(&self, handle: &LaunchHandle) -> PortResult<bool> {
        check_kind(handle, KIND)?;
        has_session(&handle.id).await
    }

    fn attach_command(&self, handle: &LaunchHandle) -> Option<Vec<String>> {
        Some(vec![
            "zellij".to_owned(),
            "attach".to_owned(),
            handle.id.clone(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use borant_core::model::Harness;

    use super::*;

    fn spec(agent: &str, cwd: PathBuf, program: &[&str]) -> LaunchSpec {
        LaunchSpec {
            agent: agent.parse().unwrap(),
            harness: Harness::Fake,
            cwd,
            program: program.iter().map(|s| (*s).to_owned()).collect(),
            env: vec![("FOO".into(), "say \"hi\"".into())],
        }
    }

    #[test]
    fn layout_and_argv() {
        let s = spec(
            "z1",
            PathBuf::from("/work/repo"),
            &["sh", "-c", "echo a\\b"],
        );
        assert_eq!(
            layout(&s).unwrap(),
            "layout {\n    pane command=\"env\" cwd=\"/work/repo\" {\n        \
             args \"FOO=say \\\"hi\\\"\" \"sh\" \"-c\" \"echo a\\\\b\"\n    }\n}\n"
        );
        assert_eq!(
            create_args("borant-z1", Path::new("/logs/borant-z1.kdl")).unwrap(),
            [
                "attach",
                "--create-background",
                "borant-z1",
                "options",
                "--default-layout",
                "/logs/borant-z1.kdl"
            ]
        );
        let launcher = ZellijLauncher::new(PathBuf::from("/logs"));
        let handle = LaunchHandle {
            kind: "zellij".into(),
            id: "borant-z1".into(),
        };
        assert_eq!(
            launcher.attach_command(&handle).unwrap(),
            ["zellij", "attach", "borant-z1"]
        );
    }

    /// Kills the session when the test ends, even if an assert fails.
    struct KillOnDrop(String);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            for cmd in ["kill-session", "delete-session"] {
                let _ = std::process::Command::new("zellij")
                    .args([cmd, &self.0])
                    .output();
            }
        }
    }

    /// Starts a real background session. Run with `--ignored` on a machine
    /// with zellij.
    #[tokio::test]
    #[ignore = "starts a real zellij session"]
    async fn launch_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let agent = format!("z{}", std::process::id());
        let _guard = KillOnDrop(format!("borant-{agent}"));
        let launcher = ZellijLauncher::new(cwd.clone());
        let s = spec(
            &agent,
            cwd.clone(),
            &["sh", "-c", "echo \"$FOO\" > out.txt; sleep 30"],
        );
        let handle = launcher.launch(&s).await.unwrap();
        assert!(launcher.is_running(&handle).await.unwrap());
        for _ in 0..50 {
            if cwd.join("out.txt").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let out = std::fs::read_to_string(cwd.join("out.txt")).unwrap();
        assert_eq!(out.trim(), "say \"hi\"");

        launcher.stop(&handle).await.unwrap();
        assert!(!launcher.is_running(&handle).await.unwrap());
    }
}
