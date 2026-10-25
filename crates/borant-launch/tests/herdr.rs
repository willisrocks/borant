//! The herdr launcher against a fake `herdr` script that records its argv and
//! answers like the real CLI. No herdr server or session is started.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use borant_core::model::Harness;
use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher};
use borant_launch::{HerdrConfig, HerdrLauncher};

/// The stand-in CLI: it appends each argv to `calls`, and answers the JSON of
/// the real herdr 0.9.5 for the calls that the launcher makes. The first
/// `workspace create` fails with `server_not_running`, so the launcher has to
/// start a server (recorded as `server`, with its environment in
/// `server-env`) before it succeeds.
const FAKE: &str = r#"#!/bin/sh
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
printf '%s\n' "$*" >> "$dir/calls"
if [ "$1" = "--session" ]; then shift 2; fi
cmd="$1"
shift
case "$cmd" in
  server)
    env >> "$dir/server-env"
    : > "$dir/started"
    ;;
  workspace)
    case "$1" in
      create)
        if [ ! -f "$dir/started" ]; then
          echo '{"error":{"code":"server_not_running","message":"no herdr server is running at /tmp/x/herdr.sock; run `herdr session attach p5` to start or attach it"}}'
          exit 1
        fi
        echo '{"id":"cli:workspace:create","result":{"workspace":{"workspace_id":"w1"},"root_pane":{"pane_id":"w1:p1"}}}'
        ;;
      close)
        if [ "$2" = "gone" ]; then
          echo '{"error":{"code":"workspace_not_found","message":"no workspace gone"}}'
          exit 1
        fi
        echo '{"id":"cli:workspace:close","result":{"type":"ok"}}'
        ;;
    esac
    ;;
  pane)
    case "$1" in
      run)
        echo '{"id":"cli:pane:run","result":{"type":"ok"}}'
        ;;
      process-info)
        case "$2" in
          *gone*)
            echo '{"error":{"code":"pane_not_found","message":"no pane"}}'
            exit 1
            ;;
        esac
        echo '{"id":"cli:pane:process-info","result":{"type":"pane_process_info","process_info":{"pane_id":"w1:p1","foreground_processes":[{"pid":10,"command":"claude"}]}}}'
        ;;
      read)
        i=1
        while [ "$i" -le 12 ]; do
          echo "line $i"
          i=$((i + 1))
        done
        ;;
    esac
    ;;
esac
"#;

struct Fx {
    dir: tempfile::TempDir,
    program: String,
    launcher: HerdrLauncher,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("herdr");
        std::fs::write(&program, FAKE).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let program = program.to_string_lossy().into_owned();
        let launcher = HerdrLauncher::new(HerdrConfig {
            session: "p5".into(),
            program: program.clone(),
            ssh: Some("me@vps-1".into()),
        });
        Self {
            dir,
            program,
            launcher,
        }
    }

    /// The argv of each call, in order.
    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn server_env(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("server-env")).unwrap_or_default()
    }
}

fn spec(agent: &str) -> LaunchSpec {
    LaunchSpec {
        agent: agent.parse().unwrap(),
        harness: Harness::Fake,
        cwd: PathBuf::from("/work/a"),
        program: vec!["sleep".into(), "30".into()],
        env: vec![("A".into(), "1".into())],
    }
}

fn handle(id: &str) -> LaunchHandle {
    LaunchHandle {
        kind: "herdr".into(),
        id: id.into(),
    }
}

#[tokio::test]
async fn herdr_launch_starts_the_server_then_a_workspace_and_a_pane() {
    let fx = Fx::new();
    let h = fx.launcher.launch(&spec("w1")).await.unwrap();
    assert_eq!(h.kind, "herdr");
    assert_eq!(h.id, "w1 w1:p1");

    let calls = fx.calls();
    assert!(
        calls.iter().any(|c| c == "--session p5 server"),
        "the server started: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c
            == "--session p5 workspace create --cwd /work/a --label b-w1 --env A=1 --no-focus"),
        "the workspace call: {calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c == "--session p5 pane run w1:p1 sleep 30"),
        "the pane call: {calls:?}"
    );
    // The server runs without the variables of a Borant session, because panes
    // inherit them.
    let env = fx.server_env();
    assert!(
        !env.lines().any(|l| l.starts_with("BORANT_")),
        "server env: {env}"
    );

    assert!(fx.launcher.is_running(&h).await.unwrap());
    assert!(!fx.launcher.is_running(&handle("w1 gone:1")).await.unwrap());
    assert_eq!(
        fx.launcher.capture(&h, 3).as_deref(),
        Some("line 10\nline 11\nline 12")
    );
    assert_eq!(
        fx.launcher.attach_command(&h),
        Some(
            [
                fx.program.as_str(),
                "--remote",
                "me@vps-1",
                "--session",
                "p5"
            ]
            .map(str::to_owned)
            .to_vec()
        )
    );

    fx.launcher.stop(&h).await.unwrap();
    assert!(
        fx.calls()
            .iter()
            .any(|c| c == "--session p5 workspace close w1"),
        "the close call: {:?}",
        fx.calls()
    );
    // A workspace that is already gone is not an error.
    fx.launcher.stop(&handle("gone w1:p1")).await.unwrap();
}

#[tokio::test]
async fn herdr_shortens_the_label_of_a_long_agent() {
    let fx = Fx::new();
    let agent = "a".repeat(40);
    let _ = fx.launcher.launch(&spec(&agent)).await.unwrap();
    let create = fx
        .calls()
        .into_iter()
        .find(|c| c.starts_with("--session p5 workspace create"))
        .expect("a workspace call");
    let label = create
        .split("--label ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .expect("a label");
    assert_eq!(label.len(), 32, "{create}");
    assert!(label.starts_with("b-aaaa"), "{create}");
    assert!(
        label.split('-').next_back().is_some_and(|s| s.len() == 6),
        "{create}"
    );
}
