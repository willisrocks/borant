//! Test-only helpers of the hub: the `[secrets]` of a join, and a log
//! capture that a leak test reads.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};

use borant_core::ports::secrets::{Bundle, SecretRef, SecretStore};
use borant_core::testing::FakeSecretStore;

use crate::secrets::{HubSecrets, host_boot, host_join};

/// The value of `TAILSCALE_AUTHKEY` in bundle `host-boot`.
pub(crate) const BOOT_VALUE: &str = "boot-value-tailscale";

/// The value of `STORE_TOKEN` in bundle `host-join`.
pub(crate) const STORE_TOKEN_VALUE: &str = "join-value-store-token";

/// The value of `NATS_PASSWORD` in bundle `host-join`.
pub(crate) const NATS_VALUE: &str = "join-value-nats";

fn bundle(name: &str, vars: &[(&str, &str)]) -> Bundle {
    Bundle {
        name: name.parse().expect("a bundle name"),
        store: "op".to_owned(),
        vars: vars
            .iter()
            .map(|(env, r)| ((*env).to_owned(), SecretRef((*r).to_owned())))
            .collect(),
        files: BTreeMap::new(),
        pre: Vec::new(),
        members: BTreeMap::new(),
    }
}

/// The hub secrets of a join: one fake store, and the two bundles that the
/// hub resolves.
pub(crate) fn hub_secrets() -> HubSecrets {
    let store: Arc<dyn SecretStore> = Arc::new(FakeSecretStore::new([
        (
            SecretRef("op://borant/tailscale/key".to_owned()),
            BOOT_VALUE.to_owned(),
        ),
        (
            SecretRef("op://borant/store/token".to_owned()),
            STORE_TOKEN_VALUE.to_owned(),
        ),
        (
            SecretRef("op://borant/nats/pool".to_owned()),
            NATS_VALUE.to_owned(),
        ),
    ]));
    let boot = bundle(
        "host-boot",
        &[("TAILSCALE_AUTHKEY", "op://borant/tailscale/key")],
    );
    let join = bundle(
        "host-join",
        &[
            ("STORE_TOKEN", "op://borant/store/token"),
            ("NATS_PASSWORD", "op://borant/nats/pool"),
        ],
    );
    HubSecrets::new(
        BTreeMap::from([("op".to_owned(), store)]),
        BTreeMap::from([(host_boot(), boot), (host_join(), join)]),
    )
}

/// Keeps the lines that the test subscriber writes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn len(&self) -> usize {
        self.0.lock().expect("the capture is not poisoned").len()
    }
}

impl std::io::Write for Capture {
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

/// The capture of this test binary, with its subscriber. It is global, not
/// per thread: a `set_default` of a thread does not outlive its test when the
/// tests run in parallel, and a leak test needs every line of its own run.
static LINES: LazyLock<Capture> = LazyLock::new(|| {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(capture.clone())
        .finish();
    if tracing::subscriber::set_global_default(subscriber).is_err() {
        // Another subscriber came first, so the buffer stays empty and a
        // leak test fails on it rather than passing with no lines.
    }
    capture
});

/// The length of the capture now. A test takes a mark before its work, so it
/// reads its own lines and not those of a test that runs beside it.
pub(crate) fn log_mark() -> usize {
    LINES.len()
}

/// The lines since `mark`. A line of a parallel test is harmless: no test
/// logs a token or a bundle value.
pub(crate) fn log_since(mark: usize) -> String {
    let bytes = LINES.0.lock().expect("the capture is not poisoned");
    let start = mark.min(bytes.len());
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}
