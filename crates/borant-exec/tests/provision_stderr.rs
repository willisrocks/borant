//! The provisioner adapter logs the text of its plugin's stderr: the stderr of
//! a provisioner is logs, not values, so the adapter sets `log_stderr` itself
//! and does not trust the flag of the caller.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use borant_core::ports::provision::Provisioner;
use borant_exec::ExecConfig;
use borant_exec::provision::ExecProvisioner;
use serde_json::json;

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

fn plugin() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/plugins/provision-file.sh")
        .canonicalize()
        .expect("the file plugin")
}

/// A plugin that notes on stderr, then answers as the file plugin does.
fn noisy_plugin(dir: &Path) -> PathBuf {
    let path = dir.join("noisy.sh");
    let body = format!(
        "#!/bin/sh\necho \"provisioner note for the log\" >&2\nexec {}\n",
        plugin().display()
    );
    std::fs::write(&path, body).expect("the wrapper");
    let mut perms = std::fs::metadata(&path).expect("the wrapper").permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&path, perms).expect("the wrapper");
    path
}

#[tokio::test]
async fn the_adapter_logs_the_stderr_of_its_plugin() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let exec = ExecConfig {
        command: vec![noisy_plugin(dir.path()).display().to_string()],
        env: BTreeMap::new(),
        credential: None,
        timeout: Duration::from_secs(30),
        // The caller says no; the adapter says yes.
        log_stderr: false,
    };
    let file = dir.path().join("hosts.json");
    let config = BTreeMap::from([("file".to_owned(), json!(file.display().to_string()))]);
    let p = ExecProvisioner::new(exec, config);
    let lines = log_lines(async {
        p.describe().await.expect("the describe of the file plugin");
    })
    .await;
    assert!(lines.contains("provisioner note for the log"), "{lines}");
}
