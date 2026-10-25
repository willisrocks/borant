//! The secrets and member interfaces of the core: the `SecretStore` contract,
//! the `Secret` Debug output, and the JSON of the records that gained fields.

use std::collections::BTreeMap;

use borant_core::model::{Agent, Harness, HostInfo, MoveReason};
use borant_core::ports::secrets::{
    BundleName, MemberName, Resolved, Secret, SecretCapabilities, SecretRef, SecretStore,
};
use borant_core::ports::{PortError, PortResult};
use borant_core::proto::LaunchOrder;
use borant_core::rules::members::MemberRules;
use borant_core::testing::secret_store_contract;
use borant_core::{ChannelName, Timestamp};

/// A store that knows a fixed set of refs, for the contract suite.
#[derive(Debug)]
struct MapStore {
    values: BTreeMap<SecretRef, &'static str>,
}

#[async_trait::async_trait]
impl SecretStore for MapStore {
    fn kind(&self) -> &'static str {
        "memory"
    }

    fn capabilities(&self) -> SecretCapabilities {
        SecretCapabilities { resolve: true }
    }

    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved> {
        let mut values = BTreeMap::new();
        for r in refs {
            let value = self
                .values
                .get(r)
                .ok_or_else(|| PortError::not_found(format!("no secret {r:?}")))?;
            values.insert(r.clone(), Secret::new((*value).to_owned()));
        }
        Ok(Resolved { values })
    }
}

#[tokio::test]
async fn secrets_contract_holds_for_a_test_store() {
    let known = [
        (SecretRef("memory://a".to_owned()), "alpha-value"),
        (SecretRef("memory://b".to_owned()), "beta-value"),
    ];
    let store = MapStore {
        values: known.iter().map(|(r, v)| (r.clone(), *v)).collect(),
    };
    let missing = SecretRef("memory://missing".to_owned());
    secret_store_contract(&store, &known, missing).await;
}

#[test]
fn secrets_secret_debug_prints_no_value() {
    let secret = Secret::new("hunter2-not-in-any-log".to_owned());
    assert_eq!(secret.expose(), "hunter2-not-in-any-log");
    assert_eq!(format!("{secret:?}"), "Secret(..)");
}

#[test]
fn secrets_old_json_parses_without_the_new_fields() {
    let agent = Agent {
        id: "w1".parse().unwrap(),
        preferred_harness: Harness::Fake,
        repo: None,
        movable: false,
        channels: vec!["api".parse::<ChannelName>().unwrap()],
        session: None,
        host: None,
        harness_session_id: None,
        created: Timestamp(1_000),
        model: None,
        model_harness: None,
        project: None,
        members: BTreeMap::new(),
    };
    let old = without(&agent, "members");
    let parsed: Agent = serde_json::from_value(old).expect("old agent JSON parses");
    assert!(parsed.members.is_empty());

    let host = HostInfo {
        name: "h1".parse().unwrap(),
        harnesses: vec![Harness::Fake],
        launcher: "process".into(),
        max_agents: 4,
        always_on: false,
        draining: false,
        ssh: None,
        members: BTreeMap::new(),
        pool: Some("workers".parse().unwrap()),
        tags: vec!["vps".parse().unwrap()],
    };
    let old = without(&host, "members");
    let parsed: HostInfo = serde_json::from_value(old).expect("old host JSON parses");
    assert!(parsed.members.is_empty());
    // A host from before pools and tags reports neither.
    let old = without(&without(&host, "pool"), "tags");
    let parsed: HostInfo = serde_json::from_value(old).expect("old host JSON parses");
    assert_eq!(parsed.pool, None);
    assert!(parsed.tags.is_empty());

    let order = LaunchOrder {
        agent: "w1".parse().unwrap(),
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
        members: BTreeMap::new(),
    };
    let old = without(&order, "members");
    let parsed: LaunchOrder = serde_json::from_value(old).expect("old launch order JSON parses");
    assert!(parsed.members.is_empty());
}

/// The JSON of `value` without one field, as an older hub or host wrote it.
fn without(value: &impl serde::Serialize, field: &str) -> serde_json::Value {
    let mut json = serde_json::to_value(value).expect("serialize");
    let map = json.as_object_mut().expect("an object");
    map.remove(field);
    json
}

#[test]
fn secrets_member_records_and_rules() {
    assert_eq!(
        MemberRules::default(),
        MemberRules {
            unknown_reset_mins: 60,
            max_restarts_per_hour: 3,
        }
    );
    // The names of a bundle and a member are safe to log and to send.
    let bundle: BundleName = "claude_auth".parse().unwrap();
    let member: MemberName = "a".parse().unwrap();
    assert_eq!(bundle.as_str(), "claude_auth");
    assert_eq!(member.as_str(), "a");
    assert!("Not A Name!".parse::<BundleName>().is_err());
    assert_eq!(
        serde_json::to_value(MoveReason::Limit).unwrap(),
        serde_json::json!("limit")
    );
}
