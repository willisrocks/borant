//! `[secrets]` of `host.toml`: the stores open at host start, the harnesses of
//! a failed store drop out, and `HostInfo` announces the bundles with several
//! members.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use borant_core::model::Harness;
use borant_core::ports::secrets::{Bundle, BundleName, MemberName, SecretRef};
use borant_host::{HostConfig, SecretConfig};
use borant_secrets::StoreConfig;

fn bundle_name(name: &str) -> BundleName {
    name.parse().unwrap()
}

fn member(name: &str) -> MemberName {
    name.parse().unwrap()
}

/// A bundle with one env var for each member name.
fn bundle(name: &str, store: &str, members: &[&str]) -> Bundle {
    Bundle {
        name: bundle_name(name),
        store: store.to_owned(),
        vars: BTreeMap::new(),
        files: BTreeMap::new(),
        pre: vec![vec!["sh".into(), "-c".into(), "echo pre".into()]],
        members: members
            .iter()
            .map(|m| {
                (
                    member(m),
                    BTreeMap::from([(
                        "CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
                        SecretRef(format!("token-{m}")),
                    )]),
                )
            })
            .collect(),
    }
}

fn harness_map(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<BundleName>> {
    pairs
        .iter()
        .map(|(harness, bundles)| {
            (
                (*harness).to_owned(),
                bundles.iter().map(|b| bundle_name(b)).collect(),
            )
        })
        .collect()
}

#[tokio::test]
async fn secrets_config_opens_the_stores_and_announces_the_members() {
    let auth = bundle_name("claude_auth");
    let cfg = SecretConfig {
        stores: BTreeMap::from([("env".to_owned(), StoreConfig::Env {})]),
        bundles: BTreeMap::from([
            (auth.clone(), bundle("claude_auth", "env", &["a", "b"])),
            (bundle_name("github"), bundle("github", "env", &[])),
        ]),
        harness: harness_map(&[("*", &["github"]), ("claude-code", &["claude_auth"])]),
        credentials_dir: None,
    };
    let harnesses = [Harness::ClaudeCode, Harness::Omp];
    let (secrets, kept) = cfg.open(&harnesses).await;
    assert_eq!(kept, harnesses, "every store opened");
    assert_eq!(secrets.stores.len(), 1);

    // "*" comes first, then the harness's own bundles.
    assert_eq!(
        secrets.bundles_for(Harness::ClaudeCode),
        vec![bundle_name("github"), auth.clone()]
    );
    assert_eq!(
        secrets.bundles_for(Harness::Omp),
        vec![bundle_name("github")]
    );

    // The host announces the bundle to the hub.
    let mut host = HostConfig::new("h1".parse().unwrap(), PathBuf::from("/tmp/borant-test"));
    host.harnesses = kept.clone();
    host.secrets = Some(secrets);
    let info = host.info();
    let announced = info.members.get(&auth).expect("the bundle is announced");
    assert_eq!(announced.members, vec![member("a"), member("b")]);
    assert_eq!(announced.harnesses, vec![Harness::ClaudeCode]);
    // A bundle with no members is not announced.
    assert!(!info.members.contains_key(&bundle_name("github")));
}

#[tokio::test]
async fn secrets_config_drops_the_harnesses_of_a_failed_store() {
    let auth = bundle_name("claude_auth");
    let cfg = SecretConfig {
        stores: BTreeMap::from([
            ("env".to_owned(), StoreConfig::Env {}),
            (
                "op".to_owned(),
                StoreConfig::Exec {
                    command: vec!["/nonexistent/borant-secrets-plugin".into()],
                    env: BTreeMap::new(),
                    credential: None,
                    timeout_secs: 1,
                },
            ),
        ]),
        bundles: BTreeMap::from([
            (auth.clone(), bundle("claude_auth", "op", &["a", "b"])),
            (bundle_name("github"), bundle("github", "env", &[])),
        ]),
        harness: harness_map(&[("*", &["github"]), ("claude-code", &["claude_auth"])]),
        credentials_dir: None,
    };
    let harnesses = [Harness::ClaudeCode, Harness::Omp];
    let (secrets, kept) = cfg.open(&harnesses).await;
    assert_eq!(
        kept,
        vec![Harness::Omp],
        "claude-code uses a bundle of the store that failed"
    );
    assert!(!secrets.stores.contains_key("op"));
    assert!(secrets.bundles.contains_key(&auth), "the config remains");
    // Nothing is announced for a harness that is gone.
    assert!(secrets.members(&kept).is_empty());
}

#[test]
fn secrets_config_absent_leaves_host_info_as_today() {
    let host = HostConfig::new("laptop".parse().unwrap(), PathBuf::from("/tmp/borant-test"));
    assert!(host.secrets.is_none());
    let info = host.info();
    assert!(info.members.is_empty());
    assert_eq!(info.max_agents, 4);
    assert_eq!(info.launcher, "tmux");
    assert_eq!(host.lease, Duration::from_secs(30));
}
