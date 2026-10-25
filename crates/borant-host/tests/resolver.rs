//! Resolver mode: a launch resolves the bundles of its harness, writes one
//! directory of values, and starts the harness through the shim. The values
//! never reach the launch arguments, `LaunchSpec.env`, NATS, or the session
//! log, and the directory goes when the session ends.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::model::{Harness, MoveReason, SessionEvent, SessionEventBody};
use borant_core::ports::secrets::{Bundle, BundleName, MemberName, SecretRef};
use borant_core::ports::{
    LaunchHandle, LaunchSpec, Launcher, PortError, PortErrorKind, PortResult, SessionStore,
};
use borant_core::proto::{LaunchOrder, LocalEvent, LocalRegister, LocalSession, LocalState};
use borant_core::{AgentId, ManualClock, SessionId, SystemClock, Timestamp};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig, HostSecrets};
use borant_hub::{Hub, HubConfig};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use borant_secrets::EnvStore;
use common::{eventually, serve_hub};
use tokio_util::sync::CancellationToken;

/// The values of the store of these tests. Each is at least eight characters,
/// so the exact-value redactor matches it.
const GITHUB: &str = "gh-value-123456";
const GH_FILE: &str = "gh-file-value-1234567";
const CLAUDE_A: &str = "claude-token-a-123456";
const CLAUDE_B: &str = "claude-token-b-123456";
const EXTRA: &str = "extra-value-123456";

const VALUES: [&str; 5] = [GITHUB, GH_FILE, CLAUDE_A, CLAUDE_B, EXTRA];

fn bundle_name(name: &str) -> BundleName {
    name.parse().unwrap()
}

fn member(name: &str) -> MemberName {
    name.parse().unwrap()
}

fn agent(name: &str) -> AgentId {
    name.parse().unwrap()
}

fn refs(pairs: &[(&str, &str)]) -> BTreeMap<String, SecretRef> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), SecretRef((*v).to_owned())))
        .collect()
}

/// `github` has one variable and one file; `claude_auth` is a member bundle
/// whose member vars win over its own `CLAUDE_CODE_OAUTH_TOKEN`.
fn secrets() -> HostSecrets {
    let github = Bundle {
        name: bundle_name("github"),
        store: "env".into(),
        vars: refs(&[("GITHUB_TOKEN", "gh-token")]),
        files: refs(&[("gh.pem", "gh-file")]),
        pre: vec![vec!["sh".into(), "-c".into(), "echo {dir}".into()]],
        members: BTreeMap::new(),
    };
    let claude_auth = Bundle {
        name: bundle_name("claude_auth"),
        store: "env".into(),
        vars: refs(&[("CLAUDE_CODE_OAUTH_TOKEN", "bundle-token")]),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::from([
            (member("a"), refs(&[("CLAUDE_CODE_OAUTH_TOKEN", "token-a")])),
            (member("b"), refs(&[("CLAUDE_CODE_OAUTH_TOKEN", "token-b")])),
        ]),
    };
    // The store holds no value for `bundle-token`, so a launch that resolves an
    // overridden variable fails instead of going quiet.
    let store = EnvStore::from_vars([
        ("gh-token".to_owned(), GITHUB.to_owned()),
        ("gh-file".to_owned(), GH_FILE.to_owned()),
        ("token-a".to_owned(), CLAUDE_A.to_owned()),
        ("token-b".to_owned(), CLAUDE_B.to_owned()),
        ("extra-token".to_owned(), EXTRA.to_owned()),
    ]);
    // A bundle of a collection or a role, which overrides the harness default.
    let extra = Bundle {
        name: bundle_name("extra"),
        store: "env".into(),
        vars: refs(&[("GITHUB_TOKEN", "extra-token")]),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::new(),
    };
    // A bundle whose `files` key is not a file name: the directory write
    // fails after the env file, and the launch must leave nothing on disk.
    let broken = Bundle {
        name: bundle_name("broken"),
        store: "env".into(),
        vars: refs(&[("BROKEN_TOKEN", "extra-token")]),
        files: refs(&[("../escape", "gh-file")]),
        pre: Vec::new(),
        members: BTreeMap::new(),
    };
    HostSecrets {
        stores: BTreeMap::from([("env".to_owned(), Arc::new(store) as _)]),
        bundles: BTreeMap::from([
            (bundle_name("github"), github),
            (bundle_name("claude_auth"), claude_auth),
            (bundle_name("extra"), extra),
            (bundle_name("broken"), broken),
        ]),
        harness: BTreeMap::from([
            ("*".to_owned(), vec![bundle_name("github")]),
            ("fake".to_owned(), vec![bundle_name("claude_auth")]),
        ]),
    }
}

/// The launches that the host started, in order.
#[derive(Debug, Default)]
struct Recording {
    specs: Mutex<Vec<LaunchSpec>>,
}

impl Recording {
    fn last(&self) -> LaunchSpec {
        self.specs
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a recorded launch")
    }
}

#[derive(Debug)]
struct RecordingLauncher(Arc<Recording>);

#[async_trait::async_trait]
impl Launcher for RecordingLauncher {
    fn kind(&self) -> &'static str {
        "recording"
    }

    async fn launch(&self, spec: &LaunchSpec) -> PortResult<LaunchHandle> {
        self.0.specs.lock().unwrap().push(spec.clone());
        Ok(LaunchHandle {
            kind: "recording".into(),
            id: spec.agent.to_string(),
        })
    }

    async fn stop(&self, _handle: &LaunchHandle) -> PortResult<()> {
        Ok(())
    }

    async fn is_running(&self, _handle: &LaunchHandle) -> PortResult<bool> {
        Ok(true)
    }

    fn attach_command(&self, _handle: &LaunchHandle) -> Option<Vec<String>> {
        None
    }
}

struct Rig {
    host: Host,
    rec: Arc<Recording>,
    sessions: MemorySession,
    dir: tempfile::TempDir,
    root: PathBuf,
    _hub: CancellationToken,
}

async fn rig(secrets: Option<HostSecrets>) -> Rig {
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let hub = Arc::new(Hub::new(
        Arc::new(MemoryState::new()),
        Arc::new(bus.clone()),
        Arc::new(sessions.clone()),
        Arc::new(ManualClock::new(Timestamp(1_000_000))),
        HubConfig::default(),
    ));
    let hub_stop = serve_hub(&bus, hub).await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("run/agents");
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    // A launch now checks that the program of its harness exists, and the
    // testkit binary is not installed on the box that runs the tests.
    cfg.commands.fake = Some(vec!["true".to_owned()]);
    cfg.hub_timeout = Duration::from_millis(500);
    cfg.secrets_root = Some(root.clone());
    cfg.secrets = secrets;
    let rec = Arc::new(Recording::default());
    let host = Host::new(
        cfg,
        Arc::new(bus),
        Arc::new(sessions.clone()),
        Arc::new(SystemClock),
        Box::new(RecordingLauncher(rec.clone())),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );
    Rig {
        host,
        rec,
        sessions,
        dir,
        root,
        _hub: hub_stop,
    }
}

fn order(name: &str, members: BTreeMap<BundleName, MemberName>) -> LaunchOrder {
    LaunchOrder {
        agent: agent(name),
        harness: Harness::Fake,
        repo: None,
        wip: None,
        channels: Vec::new(),
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        from_harness: None,
        from_host: None,
        model: None,
        reserve_secs: None,
        project: None,
        members,
        bundles: Vec::new(),
        git_author: None,
        sandbox: None,
    }
}

fn member_order() -> BTreeMap<BundleName, MemberName> {
    BTreeMap::from([(bundle_name("claude_auth"), member("a"))])
}

async fn register(rig: &Rig, name: &str) -> LocalSession {
    let cwd = rig.dir.path().join(format!("work/{name}"));
    std::fs::create_dir_all(&cwd).unwrap();
    let s = rig
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            harness_session_id: None,
            pid: None,
            cwd: cwd.to_string_lossy().into_owned(),
            agent: Some(agent(name)),
            channel: None,
            role: None,
            collection: None,
        })
        .await
        .unwrap();
    assert_eq!(s.state, LocalState::Attached, "{s:?}");
    s
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn env_of(spec: &LaunchSpec) -> BTreeMap<String, String> {
    spec.env.iter().cloned().collect()
}

/// The names of the `SecretsIssued` event of a log.
fn issued(events: &[(u64, SessionEvent)]) -> (Vec<BundleName>, BTreeMap<BundleName, MemberName>) {
    events
        .iter()
        .find_map(|(_, e)| match &e.body {
            SessionEventBody::SecretsIssued { bundles, members } => {
                Some((bundles.clone(), members.clone()))
            }
            _ => None,
        })
        .expect("a SecretsIssued event")
}

#[tokio::test]
async fn resolver_launch_writes_the_directory_and_hides_the_values() {
    let rig = rig(Some(secrets())).await;
    rig.host.launch(order("w1", member_order())).await.unwrap();
    let spec = rig.rec.last();
    let dir = rig.root.join("w1");

    // The program is the shim in front of the harness.
    let exe = std::env::current_exe().unwrap();
    assert_eq!(spec.program[0], exe.to_string_lossy());
    assert_eq!(spec.program[1], "secret-exec");
    assert_eq!(Path::new(&spec.program[2]), dir);
    assert_eq!(spec.program[3], "--");
    assert_eq!(&spec.program[4..], ["true"]);

    // Only the path of the values reaches the launch: the tmux launcher turns
    // each `env` pair into `-e KEY=VALUE` and appends `program` as is.
    let env = env_of(&spec);
    assert_eq!(env["BORANT_SECRETS_DIR"], dir.to_string_lossy());
    assert_eq!(
        env.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "BORANT_AGENT",
            "BORANT_HOME",
            "BORANT_PROMPT",
            "BORANT_SECRETS_DIR",
            "BORANT_SOCKET"
        ]
    );
    for value in VALUES {
        assert!(!env.values().any(|v| v.contains(value)), "{env:?}");
        assert!(
            !spec.program.join(" ").contains(value),
            "{}",
            spec.program.join(" ")
        );
        assert!(!format!("{spec:?}").contains(value));
    }

    // The directory holds the values, and only the launch can read them.
    assert_eq!(mode(&dir), 0o700, "{dir:?}");
    let env_file = dir.join("env");
    assert_eq!(mode(&env_file), 0o600);
    let text = std::fs::read_to_string(&env_file).unwrap();
    assert!(text.contains(&format!("GITHUB_TOKEN={GITHUB}")), "{text}");
    // The member vars win over the bundle vars, so the overridden ref is
    // never resolved and never written.
    assert!(
        text.contains(&format!("CLAUDE_CODE_OAUTH_TOKEN={CLAUDE_A}")),
        "{text}"
    );
    assert!(!text.contains(CLAUDE_B), "{text}");
    assert!(!text.contains("bundle-token"), "{text}");

    let file = dir.join("gh.pem");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), GH_FILE);

    let pre: Vec<Vec<String>> =
        serde_json::from_slice(&std::fs::read(dir.join("pre.json")).unwrap()).unwrap();
    assert_eq!(
        pre,
        vec![vec![
            "sh".to_string(),
            "-c".to_string(),
            "echo {dir}".to_string()
        ]]
    );
}

#[tokio::test]
async fn resolver_resolves_the_order_bundles_after_the_defaults() {
    let rig = rig(Some(secrets())).await;
    let mut o = order("w1", member_order());
    // `extra` overrides the harness default on `GITHUB_TOKEN`, and a bundle
    // that `host.toml` does not define is left out.
    o.bundles = vec![bundle_name("extra"), bundle_name("undefined_bundle")];
    rig.host.launch(o).await.unwrap();

    let text = std::fs::read_to_string(rig.root.join("w1/env")).unwrap();
    assert!(text.contains(&format!("GITHUB_TOKEN={EXTRA}")), "{text}");
    assert!(!text.contains(GITHUB), "the default won: {text}");

    register(&rig, "w1").await;
    let events = rig.sessions.read(&agent("w1")).await.unwrap();
    let (bundles, _) = issued(&events);
    assert_eq!(
        bundles,
        vec![
            bundle_name("github"),
            bundle_name("claude_auth"),
            bundle_name("extra")
        ]
    );
}

#[tokio::test]
async fn resolver_logs_the_names_only_and_redacts_the_values() {
    let rig = rig(Some(secrets())).await;
    rig.host.launch(order("w1", member_order())).await.unwrap();
    let session = register(&rig, "w1").await;
    let agent = agent("w1");

    let events = rig.sessions.read(&agent).await.unwrap();
    let (bundles, members) = issued(&events);
    assert_eq!(
        bundles,
        vec![bundle_name("github"), bundle_name("claude_auth")]
    );
    assert_eq!(members, member_order());
    for value in VALUES {
        assert!(!format!("{events:?}").contains(value), "{value} in the log");
    }

    // A value that reaches the log from a tool result is redacted.
    rig.host
        .event(
            session.session,
            LocalEvent::UserPrompt {
                text: format!("my token is {GITHUB} and {CLAUDE_A}"),
            },
        )
        .await
        .unwrap();
    let events = rig.sessions.read(&agent).await.unwrap();
    let logged = format!("{events:?}");
    for value in VALUES {
        assert!(!logged.contains(value), "{value} in the log");
    }
    assert!(logged.contains("[redacted]"), "{logged}");

    // The end of the session deletes the directory.
    rig.host.end_session(session.session, "test").await;
    assert!(!rig.root.join("w1").exists(), "the directory survived");
}

#[tokio::test]
async fn resolver_fails_a_member_bundle_without_a_member() {
    let rig = rig(Some(secrets())).await;
    let err = rig
        .host
        .launch(order("w1", BTreeMap::new()))
        .await
        .unwrap_err();
    let port = err.downcast_ref::<PortError>().expect("a port error");
    assert_eq!(port.kind, PortErrorKind::Invalid, "{port}");
    assert!(port.message.contains("no member"), "{port}");
    assert!(port.message.contains("claude_auth"), "{port}");
    assert!(
        !rig.root.join("w1").exists(),
        "a failed launch wrote a directory"
    );
    assert!(rig.rec.specs.lock().unwrap().is_empty(), "the harness ran");
}

#[tokio::test]
async fn resolver_absent_leaves_the_launch_as_today() {
    let rig = rig(None).await;
    rig.host.launch(order("w1", BTreeMap::new())).await.unwrap();
    let spec = rig.rec.last();
    assert_eq!(spec.program, ["true"]);
    assert!(!env_of(&spec).contains_key("BORANT_SECRETS_DIR"));
    assert!(
        !rig.root.exists(),
        "a host without secrets wrote a directory"
    );
}

#[tokio::test]
async fn resolver_removes_the_directories_of_no_live_session() {
    let rig = rig(Some(secrets())).await;
    // A directory whose session ended while the daemon was down.
    std::fs::create_dir_all(rig.root.join("w-gone")).unwrap();
    // A name that is not an agent id is not ours to delete.
    std::fs::create_dir_all(rig.root.join("not an agent")).unwrap();
    rig.host.launch(order("w1", member_order())).await.unwrap();
    let session = register(&rig, "w1").await;
    assert!(rig.root.join("w1").exists());

    rig.host.remove_stale_secret_dirs().await;
    assert!(
        rig.root.join("w1").exists(),
        "a live session keeps its values"
    );
    assert!(!rig.root.join("w-gone").exists(), "a stale directory went");
    assert!(
        rig.root.join("not an agent").exists(),
        "a foreign name went"
    );

    rig.host.end_session(session.session, "test").await;
    assert!(!rig.root.join("w1").exists());
}

/// A session table row as the daemon writes it: this process is alive, so a
/// restore keeps the session.
fn table_row(
    id: SessionId,
    cwd: &Path,
    dir: &Path,
    reg: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "harness": "fake",
        "harness_session_id": null,
        "pid": std::process::id(),
        "cwd": cwd.display().to_string(),
        "agent_hint": "w1",
        "channel_hint": null,
        "shared_dir": false,
        "reg": reg,
        "fenced": null,
        "turn": 0,
        "launch": null,
        "bundles": ["github", "claude_auth"],
        "members": {"claude_auth": "a"},
        "secrets_dir": dir.display().to_string()
    })
}

#[tokio::test]
async fn resolver_restores_the_names_and_the_directory_of_a_session() {
    let rig = rig(Some(secrets())).await;
    let dir = rig.root.join("w1");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = rig.dir.path().join("work/w1");
    std::fs::create_dir_all(&cwd).unwrap();
    let id = SessionId::new();
    let table = serde_json::json!([table_row(id, &cwd, &dir, None)]);
    std::fs::write(
        rig.dir.path().join("sessions.json"),
        serde_json::to_vec(&table).unwrap(),
    )
    .unwrap();

    rig.host.restore().await;
    // The restore keeps the names and the directory in the table.
    let saved = std::fs::read_to_string(rig.dir.path().join("sessions.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let session = &saved[0];
    assert_eq!(
        session["bundles"],
        serde_json::json!(["github", "claude_auth"])
    );
    assert_eq!(session["members"], serde_json::json!({"claude_auth": "a"}));
    assert_eq!(
        session["secrets_dir"],
        serde_json::json!(dir.display().to_string())
    );

    // The restored session still owns its directory.
    rig.host.end_session(id, "test").await;
    assert!(!dir.exists(), "the restored directory survived");
}

#[tokio::test]
async fn resolver_rebuilds_the_redactor_of_a_restored_session() {
    let rig = rig(Some(secrets())).await;
    let dir = rig.root.join("w1");
    std::fs::create_dir_all(&dir).unwrap();
    let cwd = rig.dir.path().join("work/w1");
    std::fs::create_dir_all(&cwd).unwrap();
    let id = SessionId::new();
    let reg = serde_json::json!({
        "agent": "w1",
        "session": id,
        "token": 1,
        "movable": true,
        "channels": []
    });
    let table = serde_json::json!([table_row(id, &cwd, &dir, Some(reg))]);
    std::fs::write(
        rig.dir.path().join("sessions.json"),
        serde_json::to_vec(&table).unwrap(),
    )
    .unwrap();
    rig.host.restore().await;

    // The store still holds the values, so the restored session resolves them
    // again and its log redacts them.
    rig.host
        .event(
            id,
            LocalEvent::UserPrompt {
                text: format!("the token is {GITHUB}"),
            },
        )
        .await
        .unwrap();
    let events = rig.sessions.read(&agent("w1")).await.unwrap();
    let logged = format!("{events:?}");
    assert!(
        !logged.contains(GITHUB),
        "a restored session logged a value: {logged}"
    );
    assert!(logged.contains("[redacted]"), "{logged}");
}

#[tokio::test]
async fn resolver_deletes_the_directory_of_a_failed_write() {
    let rig = rig(Some(secrets())).await;
    let mut o = order("w1", member_order());
    o.bundles = vec![bundle_name("broken")];
    let err = rig.host.launch(o).await.unwrap_err();
    let port = err.downcast_ref::<PortError>().expect("a port error");
    assert_eq!(port.kind, PortErrorKind::Invalid, "{port}");
    assert!(port.message.contains("not a file name"), "{port}");
    assert!(
        !rig.root.join("w1").exists(),
        "a failed write left values on disk"
    );
    assert!(rig.rec.specs.lock().unwrap().is_empty(), "the harness ran");
}

#[tokio::test]
async fn resolver_deletes_the_directory_of_a_launch_that_never_registers() {
    let rig = rig(Some(secrets())).await;
    let mut o = order("w1", member_order());
    o.reserve_secs = Some(1);
    rig.host.launch(o).await.unwrap();
    let dir = rig.root.join("w1");
    assert!(dir.exists(), "the launch wrote no directory");
    eventually(
        || {
            let dir = dir.clone();
            async move { !dir.exists() }
        },
        "the reservation of the launch to pass",
    )
    .await;
}

#[tokio::test]
async fn resolver_keeps_the_values_of_a_newer_launch() {
    let rig = rig(Some(secrets())).await;
    let dir = rig.root.join("w1");
    // The first launch: member `a`, and a reservation that ends right away.
    let mut first = order("w1", member_order());
    first.reserve_secs = Some(1);
    rig.host.launch(first).await.unwrap();
    assert!(
        std::fs::read_to_string(dir.join("env"))
            .unwrap()
            .contains(CLAUDE_A)
    );

    // The hub retries, as it does for a launch that did not register: member
    // `b`, and a reservation that outlives this test.
    let mut second = order(
        "w1",
        BTreeMap::from([(bundle_name("claude_auth"), member("b"))]),
    );
    second.reserve_secs = Some(30);
    rig.host.launch(second).await.unwrap();
    let env = std::fs::read_to_string(dir.join("env")).unwrap();
    assert!(env.contains(CLAUDE_B), "{env}");

    // The timer of the first launch passes, and must leave the second launch
    // alone: its shim may not have read `env` yet.
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    let env = std::fs::read_to_string(dir.join("env"))
        .expect("the timer of the first launch deleted the second directory");
    assert!(env.contains(CLAUDE_B), "{env}");

    // And the redactor of the second launch still holds its values.
    let session = register(&rig, "w1").await;
    rig.host
        .event(
            session.session,
            LocalEvent::UserPrompt {
                text: format!("the token is {CLAUDE_B}"),
            },
        )
        .await
        .unwrap();
    let events = rig.sessions.read(&agent("w1")).await.unwrap();
    let logged = format!("{events:?}");
    assert!(
        !logged.contains(CLAUDE_B),
        "the redactor of the second launch is gone: {logged}"
    );
}

#[tokio::test]
async fn resolver_sweeps_without_a_secrets_section() {
    // A host whose `host.toml` lost `[secrets]` still clears the old
    // directories of its root.
    let rig = rig(None).await;
    std::fs::create_dir_all(rig.root.join("w-gone")).unwrap();
    rig.host.remove_stale_secret_dirs().await;
    assert!(
        !rig.root.join("w-gone").exists(),
        "a host without [secrets] kept an old directory"
    );
}
