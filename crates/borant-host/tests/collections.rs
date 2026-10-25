//! The host side of collections: the git author of a launch and the bundles of
//! a launch, which are the harness defaults and then those of the collection
//! and the role.

mod common;

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use borant_core::model::{GitAuthor, Harness, MoveReason};
use borant_core::ports::secrets::Bundle;
use borant_core::proto::LaunchOrder;
use borant_host::HostSecrets;
use common::Fx;
use serde_json::json;

fn order(agent: &str) -> LaunchOrder {
    LaunchOrder {
        agent: agent.parse().unwrap(),
        harness: Harness::Fake,
        repo: None,
        wip: None,
        channels: vec![],
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        from_harness: None,
        from_host: None,
        model: None,
        project: None,
        reserve_secs: None,
        members: Default::default(),
        bundles: Vec::new(),
        git_author: None,
    }
}

fn bundle() -> Bundle {
    serde_json::from_value(json!({ "name": "github", "store": "memory" })).expect("a bundle")
}

/// A host with the `claude-auth` default bundle, `github` on the fake harness,
/// and `github-work` defined but not a default.
fn secrets() -> HostSecrets {
    HostSecrets {
        stores: BTreeMap::new(),
        bundles: BTreeMap::from([
            ("github".parse().unwrap(), bundle()),
            ("github-work".parse().unwrap(), bundle()),
            ("claude-auth".parse().unwrap(), bundle()),
        ]),
        harness: BTreeMap::from([
            ("*".to_owned(), vec!["claude-auth".parse().unwrap()]),
            ("fake".to_owned(), vec!["github".parse().unwrap()]),
        ]),
    }
}

#[test]
fn collections_launch_puts_the_defaults_first_and_a_later_bundle_wins() {
    let secrets = secrets();
    let names = |bundles: Vec<borant_core::BundleName>| -> Vec<String> {
        bundles.iter().map(|b| b.to_string()).collect()
    };

    let bundles = secrets.launch_bundles(Harness::Fake, &["github-work".parse().unwrap()]);
    assert_eq!(names(bundles), vec!["claude-auth", "github", "github-work"]);

    // A bundle that the harness had as a default moves to the end, so it wins.
    let bundles = secrets.launch_bundles(Harness::Fake, &["claude-auth".parse().unwrap()]);
    assert_eq!(names(bundles), vec!["github", "claude-auth"]);

    // A bundle that host.toml does not define is left out.
    let bundles = secrets.launch_bundles(Harness::Fake, &["missing".parse().unwrap()]);
    assert_eq!(names(bundles), vec!["claude-auth", "github"]);
}
/// A writer that keeps what the subscriber formats.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .expect("the capture is not poisoned")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `f` with a subscriber that keeps the lines of level `debug` and
/// returns them.
async fn log_lines<F>(f: F) -> String
where
    F: Future<Output = ()>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    f.await;
    drop(guard);
    let bytes = capture
        .0
        .lock()
        .expect("the capture is not poisoned")
        .clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[tokio::test]
async fn collections_launch_sets_the_git_author_and_committer_env() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let dump = dir.path().join("env.txt");
    let at = dump.display().to_string();
    let fx = Fx::new(false, |cfg| {
        cfg.commands.fake = Some(vec!["sh".into(), "-c".into(), format!("env > {at}")]);
    })
    .await;
    let mut order = order("w1");
    order.git_author = Some(GitAuthor {
        name: "Chris Fenton".into(),
        email: "you@work.example".into(),
    });
    fx.host.launch(order).await.expect("the launch");
    common::eventually(
        || async {
            std::fs::read_to_string(&dump).is_ok_and(|s| s.contains("GIT_COMMITTER_EMAIL"))
        },
        "the env dump of the launch",
    )
    .await;
    let env = std::fs::read_to_string(&dump).expect("the env dump");
    for line in [
        "GIT_AUTHOR_NAME=Chris Fenton",
        "GIT_AUTHOR_EMAIL=you@work.example",
        "GIT_COMMITTER_NAME=Chris Fenton",
        "GIT_COMMITTER_EMAIL=you@work.example",
    ] {
        assert!(
            env.lines().any(|l| l.trim_end() == line),
            "no {line} in the env"
        );
    }
}

#[tokio::test]
async fn collections_launch_logs_both_bundle_lists() {
    let fx = Fx::new(false, |cfg| {
        cfg.commands.fake = Some(vec!["true".into()]);
        cfg.secrets = Some(secrets());
    })
    .await;
    let mut order = order("w2");
    order.bundles = vec!["github-work".parse().unwrap()];
    let lines = log_lines(async {
        fx.host.launch(order).await.expect("the launch");
    })
    .await;
    // Both lists, and only names.
    assert!(
        lines.contains("defaults=[\"claude-auth\", \"github\"]"),
        "{lines}"
    );
    assert!(
        lines.contains("bundles=[\"claude-auth\", \"github\", \"github-work\"]"),
        "{lines}"
    );
}
