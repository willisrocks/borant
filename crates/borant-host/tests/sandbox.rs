//! The sandbox of a host: `[sandbox]` decides whether it is offered, the
//! start check drops it when the program fails, and a launch of a role with a
//! sandbox wraps the harness program.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::SystemClock;
use borant_core::model::{Harness, SandboxKind};
use borant_core::ports::secrets::{Bundle, BundleName, SecretRef};
use borant_core::ports::{LaunchHandle, LaunchSpec, Launcher, PortResult};
use borant_core::proto::LaunchOrder;
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig, HostSecrets, SandboxConfig};
use borant_memory::{MemoryBus, MemorySession};
use borant_secrets::EnvStore;

/// The launches that the host started, in order.
#[derive(Debug, Default)]
struct Recording {
    specs: Mutex<Vec<LaunchSpec>>,
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
    _dir: tempfile::TempDir,
}

async fn fixture(sandbox: Option<SandboxConfig>) -> Rig {
    fixture_with(sandbox, None).await
}

async fn fixture_with(sandbox: Option<SandboxConfig>, secrets: Option<HostSecrets>) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().join(".borant"));
    cfg.harnesses = vec![Harness::Fake];
    cfg.hub_timeout = Duration::from_millis(500);
    cfg.sandbox = sandbox;
    cfg.secrets = secrets;
    cfg.secrets_root = Some(dir.path().join("run/agents"));
    tokio::fs::create_dir_all(cfg.home.join("work"))
        .await
        .unwrap();
    // A socket that exists is bound, so the plan names it.
    tokio::fs::write(cfg.socket(), b"").await.unwrap();
    let rec = Arc::new(Recording::default());
    let host = Host::new(
        cfg,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(RecordingLauncher(rec.clone())),
        None,
        None,
        Outbox::in_memory().unwrap(),
    );
    Rig {
        host,
        rec,
        _dir: dir,
    }
}

fn order(sandbox: Option<SandboxKind>) -> LaunchOrder {
    LaunchOrder {
        agent: "w1".parse().unwrap(),
        harness: Harness::Fake,
        sandbox,
        ..Default::default()
    }
}

fn sandbox_config() -> SandboxConfig {
    SandboxConfig {
        program: "bwrap".to_owned(),
        ro: Vec::new(),
        rw: Vec::new(),
    }
}

/// A role with a sandbox on a host that offers one: the harness runs behind
/// the sandbox program, with the clone, the socket, and the cwd bound.
#[tokio::test]
async fn a_launch_with_a_sandbox_wraps_the_program() {
    let rig = fixture(Some(sandbox_config())).await;
    rig.host
        .launch(order(Some(SandboxKind::Namespace)))
        .await
        .unwrap();
    let spec = rig.rec.specs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(spec.program[0], "bwrap");
    assert!(
        !spec.program.iter().any(|a| a == "--unshare-pid"),
        "{:?}",
        spec.program
    );
    let home = rig.host.config().home.parent().unwrap().to_path_buf();
    let work = home.join(".borant/work/w1");
    let socket = home.join(".borant/host.sock");
    let expected = [&work, &socket, &spec.cwd];
    for path in expected {
        assert!(
            spec.program
                .windows(2)
                .any(|w| w[0] == "--bind" && w[1] == path.to_string_lossy()),
            "{} is not bound: {:?}",
            path.display(),
            spec.program
        );
    }
    assert!(spec.program.contains(&"--chdir".to_owned()));
    // The harness is the last program, behind the separator.
    let sep = spec.program.iter().position(|a| a == "--").unwrap();
    assert_eq!(&spec.program[sep + 1..], ["borant-fake-harness"]);
}

/// The same launch on a host without `[sandbox]`, and a role without one on a
/// host with it: the program is the bare harness.
#[tokio::test]
async fn no_sandbox_leaves_the_program_alone() {
    let rig = fixture(Some(sandbox_config())).await;
    rig.host.launch(order(None)).await.unwrap();
    assert_eq!(
        rig.rec.specs.lock().unwrap().last().unwrap().program,
        ["borant-fake-harness"]
    );

    let plain = fixture(None).await;
    plain
        .host
        .launch(order(Some(SandboxKind::Namespace)))
        .await
        .unwrap();
    assert_eq!(
        plain.rec.specs.lock().unwrap().last().unwrap().program,
        ["borant-fake-harness"]
    );
}

/// The start check: a program that fails, or is not there, leaves the host
/// with no sandbox; a program that passes keeps it.
#[tokio::test]
async fn a_failed_start_check_offers_no_sandbox() {
    for program in ["false", "/nonexistent/bwrap"] {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
        cfg.sandbox = Some(SandboxConfig {
            program: program.to_owned(),
            ..Default::default()
        });
        assert_eq!(cfg.info().sandboxes, [SandboxKind::Namespace]);
        borant_host::sandbox::probe(&mut cfg).await;
        assert!(cfg.sandbox.is_none(), "{program} passed the check");
        assert!(cfg.info().sandboxes.is_empty(), "{program} is offered");
    }

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.sandbox = Some(SandboxConfig {
        program: "true".to_owned(),
        ..Default::default()
    });
    borant_host::sandbox::probe(&mut cfg).await;
    assert!(cfg.sandbox.is_some(), "a good program was dropped");
    assert_eq!(cfg.info().sandboxes, [SandboxKind::Namespace]);
}

/// The harness directories and the extra binds of `host.toml` reach the plan,
/// and a path that does not exist is left out: a bind of a missing path would
/// fail the whole launch.
#[tokio::test]
async fn the_plan_binds_what_exists() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), home.join(".borant"));
    cfg.harnesses = vec![Harness::ClaudeCode];
    cfg.sandbox = Some(SandboxConfig {
        program: "bwrap".to_owned(),
        ro: vec![PathBuf::from("~/.config/gh"), home.join(".config/missing")],
        rw: vec![home.join("cache")],
    });
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::create_dir_all(home.join(".local")).unwrap();
    std::fs::create_dir_all(home.join(".config/gh")).unwrap();
    std::fs::create_dir_all(home.join("cache")).unwrap();

    let o = LaunchOrder {
        harness: Harness::ClaudeCode,
        ..order(Some(SandboxKind::Namespace))
    };
    let plan = cfg
        .sandbox_plan(&o, &home.join(".borant/work/w1/repo"), None)
        .expect("a plan");
    assert_eq!(plan.program, "bwrap");
    assert_eq!(
        plan.harness_dirs,
        [home.join(".claude")],
        "only the existing harness paths"
    );
    assert_eq!(plan.ro, [home.join(".local"), home.join(".config/gh")]);
    assert_eq!(plan.rw, [home.join("cache")]);
    assert_eq!(plan.secrets_dir, None);
    assert_eq!(plan.work_dir, home.join(".borant/work/w1"));
    assert!(plan.sockets.is_empty(), "no socket file on this host");
}

/// One variable in one bundle for the fake harness, so the launch resolves
/// values, writes a secret directory, and inserts the shim.
fn secrets() -> HostSecrets {
    let name: BundleName = "github".parse().unwrap();
    let bundle = Bundle {
        name: name.clone(),
        store: "env".into(),
        vars: BTreeMap::from([("GITHUB_TOKEN".to_owned(), SecretRef("gh".to_owned()))]),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::new(),
    };
    let store = EnvStore::from_vars([("gh".to_owned(), "gh-value-123456".to_owned())]);
    HostSecrets {
        stores: BTreeMap::from([("env".to_owned(), Arc::new(store) as _)]),
        bundles: BTreeMap::from([(name.clone(), bundle)]),
        harness: BTreeMap::from([("fake".to_owned(), vec![name])]),
    }
}

/// The sandbox goes outside the secret shim: the shim, and the harness after
/// it, run inside the namespace, and the secrets directory is bound
/// read-write because the shim deletes its file.
#[tokio::test]
async fn the_shim_runs_inside_the_sandbox() {
    let rig = fixture_with(Some(sandbox_config()), Some(secrets())).await;
    rig.host
        .launch(order(Some(SandboxKind::Namespace)))
        .await
        .unwrap();
    let spec = rig.rec.specs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(spec.program[0], "bwrap");
    let sep = spec.program.iter().position(|a| a == "--").unwrap();
    let inner = &spec.program[sep + 1..];
    assert_eq!(inner[1], "secret-exec", "{inner:?}");
    let dir = PathBuf::from(&inner[2]);
    assert_eq!(inner[3], "--");
    assert_eq!(&inner[4..], ["borant-fake-harness"]);
    assert!(
        spec.program
            .windows(2)
            .any(|w| w[0] == "--bind" && w[1] == dir.to_string_lossy()),
        "the secrets directory is not bound: {:?}",
        spec.program
    );
}
