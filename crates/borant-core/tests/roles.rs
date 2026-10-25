//! The role and collection fields of the records: `#[serde(default)]` keeps old
//! JSON readable, and the new fields round-trip when they are set.

use std::collections::BTreeMap;

use borant_core::model::{
    Agent, Channel, GitAuthor, Harness, HostInfo, MemberBundle, Project, RepoInfo,
};
use borant_core::proto::{LaunchOrder, RegisterSession};
use borant_core::{
    AgentId, BundleName, ChannelName, CollectionName, ProjectName, RoleName, SessionId, Timestamp,
};

/// The JSON of `value` without one field, as an older hub or host wrote it.
fn without(value: &impl serde::Serialize, field: &str) -> serde_json::Value {
    let mut json = serde_json::to_value(value).expect("serialize");
    json.as_object_mut().expect("an object").remove(field);
    json
}

fn agent() -> Agent {
    Agent {
        id: "worker-1".parse().unwrap(),
        spawn_key: None,
        task: None,
        stop_on_close: false,
        retired: false,
        preferred_harness: Harness::Fake,
        channels: vec!["work-api".parse::<ChannelName>().unwrap()],
        created: Timestamp(1_000),
        model: Some("m".into()),
        model_harness: Some(Harness::Fake),
        role: Some("worker".parse().unwrap()),
        collection: Some("work".parse().unwrap()),
        bundles: Vec::new(),
        parent: Some("chief".parse().unwrap()),
        ..Default::default()
    }
}

#[test]
fn roles_old_json_of_every_record_parses() {
    // An agent from before roles and collections.
    let agent = agent();
    let old = without(&agent, "members");
    let old = without(&old, "role");
    let old = without(&old, "collection");
    let old = without(&old, "parent");
    let parsed: Agent = serde_json::from_value(old).expect("old agent JSON parses");
    assert_eq!(parsed.role, None);
    assert_eq!(parsed.collection, None);
    assert_eq!(parsed.parent, None);

    // The new fields round-trip when they are set.
    let back: Agent = serde_json::from_value(serde_json::to_value(&agent).unwrap()).unwrap();
    assert_eq!(back.role, agent.role);
    assert_eq!(back.collection, agent.collection);
    assert_eq!(back.parent, agent.parent);

    // A channel from before collections.
    let channel = Channel {
        name: "work-api".parse().unwrap(),
        owner: "chief".parse().unwrap(),
        schema: serde_json::json!({}),
        created: Timestamp(1),
        closed: None,
        project: None,
        branch: None,
        collection: Some("work".parse().unwrap()),
        paused: false,
    };
    let parsed: Channel = serde_json::from_value(without(&channel, "collection")).unwrap();
    assert_eq!(parsed.collection, None);

    // A project from before collections.
    let project = Project {
        name: "web".parse().unwrap(),
        remote_url: "git://git/demo.git".into(),
        default_branch: "main".into(),
        checkpoint_remote: None,
        setup: None,
        test: None,
        collection: Some("work".parse().unwrap()),
    };
    let parsed: Project = serde_json::from_value(without(&project, "collection")).unwrap();
    assert_eq!(parsed.collection, None);

    // A host from before the bundle list.
    let host = HostInfo {
        name: "h1".parse().unwrap(),
        harnesses: vec![Harness::Fake],
        launcher: "process".into(),
        max_agents: 4,
        members: BTreeMap::from([(
            "claude_auth".parse::<BundleName>().unwrap(),
            MemberBundle {
                members: vec!["a".parse().unwrap()],
                harnesses: vec![Harness::ClaudeCode],
            },
        )]),
        bundles: vec!["github".parse().unwrap()],
        ..Default::default()
    };
    let parsed: HostInfo = serde_json::from_value(without(&host, "bundles")).unwrap();
    assert!(parsed.bundles.is_empty());
    assert_eq!(parsed.members.len(), 1, "members stay");

    // A launch order from before bundles and the git author.
    let order = LaunchOrder {
        agent: "worker-1".parse().unwrap(),
        harness: Harness::Fake,
        repo: Some(RepoInfo {
            remote_url: "git://git/demo.git".into(),
            branch: "main".into(),
            subdir: String::new(),
        }),
        prompt: "go".into(),
        bundles: vec!["github-work".parse().unwrap()],
        git_author: Some(GitAuthor {
            name: "Chris".into(),
            email: "you@work.example".into(),
        }),
        ..Default::default()
    };
    let old = without(&order, "bundles");
    let old = without(&old, "git_author");
    let parsed: LaunchOrder = serde_json::from_value(old).unwrap();
    assert!(parsed.bundles.is_empty());
    assert_eq!(parsed.git_author, None);
    let back: LaunchOrder = serde_json::from_value(serde_json::to_value(&order).unwrap()).unwrap();
    assert_eq!(back.git_author, order.git_author);
    assert_eq!(back.bundles, order.bundles);

    // A session registration from before roles and collections.
    let register = RegisterSession {
        session: SessionId::new(),
        host: "h1".parse().unwrap(),
        harness: Harness::Fake,
        pid: Some(1),
        cwd: "/work".into(),
        agent: Some("worker-1".parse().unwrap()),
        role: Some("worker".parse::<RoleName>().unwrap()),
        collection: Some("work".parse::<CollectionName>().unwrap()),
        ..Default::default()
    };
    let old = without(&register, "role");
    let old = without(&old, "collection");
    let parsed: RegisterSession = serde_json::from_value(old).unwrap();
    assert_eq!(parsed.role, None);
    assert_eq!(parsed.collection, None);
}

#[test]
fn roles_names_validate_like_the_other_names() {
    assert_eq!("worker".parse::<RoleName>().unwrap().as_str(), "worker");
    assert_eq!("work".parse::<CollectionName>().unwrap().as_str(), "work");
    assert!("Not A Role".parse::<RoleName>().is_err());
    assert!(
        "x".repeat(64).parse::<CollectionName>().is_err(),
        "63 characters at most"
    );
    let _: AgentId = "a".parse().unwrap();
    let _: ProjectName = "p".parse().unwrap();
}
