//! Test-only helpers of the hub: the `[secrets]` of a join, and a log
//! capture that a leak test reads.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

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

/// Keeps the lines that a subscriber writes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

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

/// Runs `f` with a subscriber that keeps every line from `trace` up, and
/// returns them.
pub(crate) async fn log_lines<F>(f: F) -> String
where
    F: Future<Output = ()>,
{
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
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
