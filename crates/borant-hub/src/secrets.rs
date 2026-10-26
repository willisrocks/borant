//! The hub's `[secrets]`: the stores that it opens at start, and the two
//! bundles that it resolves.
//!
//! The hub hands a value to a host in two places only: the boot values of a
//! create (bundle [`HOST_BOOT`], for example the reusable tailnet key) and the
//! reply of `POST /v1/join` (bundle [`HOST_JOIN`], the store token and the
//! NATS credential). A value is never stored in the KV, never logged, and
//! never part of an error message.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use borant_core::ports::secrets::{Bundle, BundleName, SecretRef, SecretStore};
use borant_core::ports::{PortError, PortResult};
use borant_secrets::{StoreConfig, open_store};

/// The bundle of the values that ride in the boot values of a new host.
pub const HOST_BOOT: &str = "host-boot";

/// The bundle of the values that a host gets once, at its join.
pub const HOST_JOIN: &str = "host-join";

/// The bundle name [`HOST_BOOT`].
pub fn host_boot() -> BundleName {
    HOST_BOOT.parse().expect("a constant bundle name")
}

/// The bundle name [`HOST_JOIN`].
pub fn host_join() -> BundleName {
    HOST_JOIN.parse().expect("a constant bundle name")
}

/// The `[secrets]` of `hub.toml` before the stores open: the same stores and
/// bundles that `host.toml` takes.
#[derive(Debug, Clone, Default)]
pub struct HubSecretConfig {
    /// `[secrets.stores.<name>]`.
    pub stores: BTreeMap<String, StoreConfig>,
    /// `[secrets.bundles.<name>]`, with the table key as the bundle name.
    pub bundles: BTreeMap<BundleName, Bundle>,
    /// The folder of the credential file of an exec plugin, when the service
    /// manager gives one.
    pub credentials_dir: Option<PathBuf>,
}

impl HubSecretConfig {
    /// Opens every store at hub start. A store that fails is logged and left
    /// out, which leaves the bundles that name it unresolved; the hub still
    /// runs and creates pool hosts without a join.
    pub async fn open(&self) -> HubSecrets {
        let mut stores = BTreeMap::new();
        for (name, config) in &self.stores {
            match open_store(config, self.credentials_dir.as_deref()).await {
                Ok(store) => {
                    stores.insert(name.clone(), store);
                }
                Err(e) => {
                    tracing::error!(store = %name, "the hub secret store did not open: {e}");
                }
            }
        }
        let secrets = HubSecrets {
            stores,
            bundles: self.bundles.clone(),
        };
        tracing::info!(
            stores = secrets.stores.len(),
            bundles = secrets.bundles.len(),
            join = secrets.joins(),
            "the hub secret stores opened"
        );
        secrets
    }
}

/// The opened stores of the hub and the bundles that name them.
#[derive(Debug, Clone, Default)]
pub struct HubSecrets {
    /// The stores that opened. A store that failed is not here.
    pub stores: BTreeMap<String, Arc<dyn SecretStore>>,
    pub bundles: BTreeMap<BundleName, Bundle>,
}

impl HubSecrets {
    pub fn new(
        stores: BTreeMap<String, Arc<dyn SecretStore>>,
        bundles: BTreeMap<BundleName, Bundle>,
    ) -> Self {
        Self { stores, bundles }
    }

    /// The bundle of that name, when its store opened.
    fn ready(&self, name: &BundleName) -> Option<&Bundle> {
        let bundle = self.bundles.get(name)?;
        self.stores.get(&bundle.store)?;
        Some(bundle)
    }

    /// True when a create may join: both bundles exist and the stores they
    /// name opened.
    pub fn joins(&self) -> bool {
        self.ready(&host_boot()).is_some() && self.ready(&host_join()).is_some()
    }

    /// The env vars of one bundle: every var resolved through the bundle's
    /// store, under the name that the bundle gives it. Files, `pre`, and
    /// members belong to a harness launch, not to the hub.
    ///
    /// The returned map holds values: a caller hands it to the provisioner or
    /// to the join reply, and never logs it or stores it.
    pub async fn vars(&self, name: &BundleName) -> PortResult<BTreeMap<String, String>> {
        let Some(bundle) = self.ready(name) else {
            return Err(PortError::not_found(format!(
                "the hub has no ready bundle {name:?}"
            )));
        };
        if bundle.vars.is_empty() {
            return Ok(BTreeMap::new());
        }
        let refs: Vec<SecretRef> = bundle
            .vars
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let resolved = self.stores[&bundle.store].resolve(&refs).await?;
        let mut out = BTreeMap::new();
        for (env, r) in &bundle.vars {
            let Some(value) = resolved.values.get(r) else {
                return Err(PortError::other(format!(
                    "the store of bundle {name:?} returned no value for {r:?}"
                )));
            };
            out.insert(env.clone(), value.expose().to_owned());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::testing::FakeSecretStore;

    fn bundle(name: &str, store: &str, vars: &[(&str, &str)]) -> Bundle {
        Bundle {
            name: name.parse().unwrap(),
            store: store.to_owned(),
            vars: vars
                .iter()
                .map(|(env, r)| ((*env).to_owned(), SecretRef((*r).to_owned())))
                .collect(),
            files: Default::default(),
            pre: Vec::new(),
            members: Default::default(),
        }
    }

    fn secrets(store: bool, bundles: Vec<Bundle>) -> HubSecrets {
        let mut stores: BTreeMap<String, Arc<dyn SecretStore>> = BTreeMap::new();
        if store {
            stores.insert(
                "op".to_owned(),
                Arc::new(FakeSecretStore::new([
                    (SecretRef("op://boot/key".into()), "boot-value".to_owned()),
                    (SecretRef("op://join/token".into()), "join-value".to_owned()),
                ])),
            );
        }
        HubSecrets::new(
            stores,
            bundles.into_iter().map(|b| (b.name.clone(), b)).collect(),
        )
    }

    #[tokio::test]
    async fn resolves_the_vars_of_a_bundle() {
        let two = secrets(
            true,
            vec![bundle("host-boot", "op", &[("TAILSCALE_AUTHKEY", "op://boot/key")])],
        );
        let vars = two.vars(&host_boot()).await.unwrap();
        assert_eq!(vars["TAILSCALE_AUTHKEY"], "boot-value");
        // The map holds a value, and its Debug is only for a test.
        assert!(!format!("{vars:?}").is_empty());
    }

    #[tokio::test]
    async fn a_store_that_did_not_open_leaves_join_off() {
        let none = secrets(
            false,
            vec![
                bundle("host-boot", "op", &[("TAILSCALE_AUTHKEY", "op://boot/key")]),
                bundle("host-join", "op", &[("NATS_PASSWORD", "op://join/token")]),
            ],
        );
        assert!(!none.joins(), "a missing store leaves join off");
        let err = none.vars(&host_join()).await.expect_err("no store");
        assert_eq!(err.kind, borant_core::ports::PortErrorKind::NotFound);
    }

    #[tokio::test]
    async fn one_missing_bundle_leaves_join_off() {
        let one = secrets(
            true,
            vec![bundle("host-boot", "op", &[("TAILSCALE_AUTHKEY", "op://boot/key")])],
        );
        assert!(!one.joins(), "host-join is missing");
    }

    #[tokio::test]
    async fn a_missing_ref_fails_the_bundle_and_names_no_value() {
        let two = secrets(
            true,
            vec![bundle(
                "host-join",
                "op",
                &[
                    ("NATS_PASSWORD", "op://join/token"),
                    ("STORE_TOKEN", "op://join/absent"),
                ],
            )],
        );
        let err = two.vars(&host_join()).await.expect_err("one missing ref");
        assert!(
            !err.message.contains("join-value"),
            "the error holds no value: {err}"
        );
    }
}
