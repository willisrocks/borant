//! A launched session never inherits the host's own credentials: the NATS
//! user and password of the host daemon, and its systemd credential folder.
//! The process launcher always runs; the tmux launcher runs when `tmux` is on
//! the PATH, on a server of its own.

use std::path::Path;
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::{LaunchSpec, Launcher};
use borant_launch::{HOST_ONLY_ENV, ProcessLauncher, TmuxConfig, TmuxLauncher};

/// The environment that the session sees, written by the session itself.
fn spec(agent: &str, dir: &Path) -> LaunchSpec {
    let out = dir.join(format!("{agent}.env"));
    LaunchSpec {
        agent: agent.parse().expect("an agent id"),
        harness: Harness::ClaudeCode,
        cwd: dir.to_path_buf(),
        program: vec![
            "sh".into(),
            "-c".into(),
            format!("env > {}.tmp && mv {0}.tmp {0}", out.display()),
        ],
        env: vec![("BORANT_AGENT".into(), agent.into())],
    }
}

async fn read_env(dir: &Path, agent: &str) -> String {
    let path = dir.join(format!("{agent}.env"));
    for _ in 0..100 {
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the session never wrote {}", path.display());
}

fn assert_clean(env: &str) {
    assert!(
        env.contains("BORANT_AGENT="),
        "the spec's own variables stay: {env}"
    );
    for name in HOST_ONLY_ENV {
        assert!(
            !env.lines()
                .any(|line| line.starts_with(&format!("{name}="))),
            "the session sees {name}"
        );
    }
    assert!(
        !env.contains("leak-check-value"),
        "a host credential leaked"
    );
}

/// Set in the child run, which holds the host's variables in its environment.
const CHILD: &str = "BORANT_HOST_ENV_TEST_CHILD";

/// The parent: runs this test binary again with the host's variables set, the
/// way systemd starts the host daemon, so no test changes its own environment.
#[test]
fn host_env_never_reaches_a_session() {
    let out = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .args([
            "--exact",
            "host_env_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("BORANT_NATS_USER", "host-test")
        .env("BORANT_NATS_PASSWORD", "leak-check-value")
        .env("CREDENTIALS_DIRECTORY", "/run/credentials/leak-check-value")
        .output()
        .expect("the child run");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "the child ran no test: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// The child: launches with each launcher and reads what the session sees.
#[tokio::test]
async fn host_env_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    assert_eq!(
        std::env::var("BORANT_NATS_PASSWORD").as_deref(),
        Ok("leak-check-value"),
        "the child holds the host's variables"
    );
    let dir = tempfile::tempdir().expect("a temp dir");

    let process = ProcessLauncher::new(dir.path().to_path_buf());
    process
        .launch(&spec("p1", dir.path()))
        .await
        .expect("the process launch");
    assert_clean(&read_env(dir.path(), "p1").await);

    let has_tmux = std::process::Command::new("tmux")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    if has_tmux {
        let socket = format!("borant-env-test-{}", std::process::id());
        let tmux = TmuxLauncher::new(TmuxConfig {
            socket: socket.clone(),
        });
        tmux.launch(&spec("t1", dir.path()))
            .await
            .expect("the tmux launch");
        let env = read_env(dir.path(), "t1").await;
        let _ = std::process::Command::new("tmux")
            .args(["-L", &socket, "kill-server"])
            .output();
        assert_clean(&env);
    }
}
