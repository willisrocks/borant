//! The blocked-work alarm (blocked-work alarm spec): a spawn that waits on a
//! refusal that a wait cannot clear tells the person, the spawner, and the
//! chief, and puts its bound task in `stuck`; a claimed task whose claimer
//! cannot work it goes `stuck` with the claimer and the tasks that it holds;
//! capacity alone tells nobody; and the board lists the blocked spawns.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{
    Agent, ChannelKind, Envelope, Harness, HostInfo, Lease, LeaseState, NewTask, SpawnRecord,
    SpawnState, Task, TaskBinding, TaskRef, TaskStatus,
};
use borant_core::ports::{MessageBus, StateStore};
use borant_core::proto::{CtlRequest, PauseSelector, RegisterSession, Registered, ToolCall};
use borant_core::rules::lease::LeaseConfig;
use borant_core::rules::roles::RoleConfig;
use borant_core::{AgentId, ChannelName, Clock, SessionId, keys, subjects};
use borant_hub::HubConfig;
use bytes::Bytes;
use common::{Fx, name, repo};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde_json::Value;

fn api() -> ChannelName {
    "work-api".parse().unwrap()
}

fn agent(name: &str) -> AgentId {
    name.parse().unwrap()
}

/// The roles of the fixture: a chief that approves and spawns workers, and a
/// worker.
fn roles() -> BTreeMap<borant_core::RoleName, RoleConfig> {
    let chief = RoleConfig {
        harness: Some(Harness::Fake),
        approves: true,
        spawns: vec!["worker".parse().unwrap()],
        // A child gets only the bundles that its parent would carry, so the
        // chief holds the one that no host does.
        bundles: vec!["github-work".parse().unwrap()],
        ..RoleConfig::default()
    };
    let worker = RoleConfig {
        harness: Some(Harness::Fake),
        ..RoleConfig::default()
    };
    BTreeMap::from([
        ("chief".parse().unwrap(), chief),
        ("worker".parse().unwrap(), worker),
    ])
}

fn hub() -> Fx {
    Fx::with(HubConfig {
        roles: roles(),
        // A claim survives an expired lease longer than the stall window, so
        // a test sees the stalled card before the claim timeout releases the
        // claim.
        lease: LeaseConfig {
            claim_timeout: Duration::from_secs(3_600),
            ..LeaseConfig::default()
        },
        ..HubConfig::default()
    })
}

/// Registers a session that reports a role and a collection.
async fn register_role(fx: &Fx, who: &str, role: &str, collection: Option<&str>) -> Registered {
    let v = fx
        .req(CtlRequest::RegisterSession(RegisterSession {
            session: SessionId::new(),
            host: name("h1"),
            harness: Harness::Fake,
            pid: Some(1),
            cwd: "/work".into(),
            repo: Some(repo()),
            agent: Some(agent(who)),
            role: Some(role.parse().unwrap()),
            collection: collection.map(|c| c.parse().unwrap()),
            ..Default::default()
        }))
        .await
        .unwrap();
    serde_json::from_value(v).unwrap()
}

async fn create_channel(fx: &Fx, by: &Registered, name: &str) {
    fx.call(
        Some(by),
        ToolCall::ChannelCreate {
            name: name.parse().unwrap(),
            schema: None,
            project: None,
            branch: None,
            kind: ChannelKind::Project,
            title: None,
            issue: None,
            issue_target: None,
        },
    )
    .await
    .unwrap();
}

async fn create_task(fx: &Fx, by: &Registered, title: &str, blocked_by: Vec<Task>) -> Task {
    serde_json::from_value(
        fx.call(
            Some(by),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: title.into(),
                    blocked_by: blocked_by.iter().map(|t| t.id).collect(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

/// A spawn of `who` by `by`, in `work-api`, with the bundle that no host has.
async fn spawn(
    fx: &Fx,
    by: &Registered,
    who: &str,
    task: Option<TaskRef>,
) -> Result<Value, borant_core::proto::ApiError> {
    fx.call(
        Some(by),
        ToolCall::Spawn {
            channel: api(),
            prompt: "do the work".into(),
            harness: None,
            host: None,
            name: Some(agent(who)),
            model: None,
            role: Some("worker".parse().unwrap()),
            key: None,
            task: task.map(|t| TaskBinding::Existing {
                channel: t.channel,
                id: t.id,
            }),
            stop_on_close: None,
            continues: None,
            priority: None,
            bundles: vec!["github-work".parse().unwrap()],
        },
    )
    .await
}

async fn record(fx: &Fx, who: &str) -> SpawnRecord {
    let e = fx
        .state
        .get(&keys::spawn(&agent(who)))
        .await
        .unwrap()
        .expect("a spawn record");
    serde_json::from_slice(&e.value).unwrap()
}

async fn agent_of(fx: &Fx, who: &str) -> Agent {
    let e = fx
        .state
        .get(&keys::agent(&agent(who)))
        .await
        .unwrap()
        .expect("the agent");
    serde_json::from_slice(&e.value).unwrap()
}

async fn task_of(fx: &Fx, id: borant_core::TaskId) -> Task {
    let e = fx
        .state
        .get(&keys::task(&api(), &id))
        .await
        .unwrap()
        .expect("the task");
    serde_json::from_slice(&e.value).unwrap()
}

/// Replaces a stored record with `value`, at the revision that it has now.
async fn replace<T: serde::Serialize>(fx: &Fx, key: &str, value: &T) {
    let entry = fx.state.get(key).await.unwrap().expect("the stored record");
    let bytes = Bytes::from(serde_json::to_vec(value).unwrap());
    fx.state.update(key, bytes, entry.revision).await.unwrap();
}

fn blocked_notices(fx: &Fx) -> Vec<String> {
    fx.recorder
        .notices()
        .into_iter()
        .filter(|notice| notice.title == "A spawn is blocked")
        .map(|notice| notice.body)
        .collect()
}

async fn messages(stream: &mut BoxStream<'static, Envelope>) -> Vec<Envelope> {
    let mut out = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_millis(300), stream.next())
            .await
            .ok()
            .flatten();
        match next {
            Some(env) => out.push(env),
            None => return out,
        }
    }
}

#[tokio::test]
async fn a_blocked_spawn_tells_the_person_the_spawner_and_the_chief() {
    let fx = hub();
    fx.host("h1", &[Harness::Fake]).await;
    let chief = register_role(&fx, "chief", "chief", Some("work")).await;
    create_channel(&fx, &chief, "work-api").await;
    let task = create_task(&fx, &chief, "the work", Vec::new()).await;
    let bound = TaskRef {
        channel: api(),
        id: task.id,
    };
    spawn(&fx, &chief, "helper", Some(bound)).await.unwrap();
    spawn(&fx, &chief, "helper2", None).await.unwrap();
    let mut to_chief = fx
        .bus
        .subscribe(&subjects::agent(&chief.agent))
        .await
        .unwrap();

    // The refusal is the bundle that no host has, and the spawn waits.
    let queued = record(&fx, "helper").await;
    assert_eq!(queued.state, SpawnState::Queued);
    assert_eq!(
        queued.last_reject,
        vec![(name("h1"), "missing bundle github-work".to_owned())]
    );

    // Inside the first 15 minutes nobody hears about it.
    fx.advance(14 * 60);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    assert!(blocked_notices(&fx).is_empty());
    assert!(messages(&mut to_chief).await.is_empty());

    // The first sighting: one alert for the person, one DM each to the
    // spawner and the chief (here the same agent), and the bound task goes
    // stuck with the alarm text.
    fx.advance(60);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    let notices = blocked_notices(&fx);
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(
        notices[0].starts_with(
            "spawn helper blocked 15m: missing bundle github-work. Unblock: add bundle github-work"
        ),
        "{}",
        notices[0]
    );
    let sent = messages(&mut to_chief).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(
        sent.iter()
            .any(|e| e.text.starts_with("spawn helper blocked")),
        "{sent:?}"
    );
    assert!(
        sent.iter()
            .any(|e| e.text.starts_with("spawn helper2 blocked")),
        "{sent:?}"
    );
    let stuck = task_of(&fx, task.id).await;
    assert_eq!(stuck.status, TaskStatus::Stuck);
    assert_eq!(stuck.attention_msg.as_deref(), Some(notices[0].as_str()));

    // The board lists the unbound spawn only: the bound one shows through
    // its task.
    let board = fx
        .call(None, ToolCall::Board { collection: None })
        .await
        .unwrap();
    let blocked = board["collections"][0]["blocked"]
        .as_array()
        .expect("the blocked list");
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0]["agent"], "helper2");
    assert_eq!(blocked[0]["channel"], "work-api");
    assert_eq!(blocked[0]["reason"], "missing bundle github-work");
    assert_eq!(blocked[0]["spawner"], "chief");

    // No repeat inside the four hours.
    fx.advance(3 * 3_600);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    assert_eq!(blocked_notices(&fx).len(), 2);
    assert!(messages(&mut to_chief).await.is_empty());

    // One repeat per four hours while it stays blocked.
    fx.advance(3_600);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    assert_eq!(blocked_notices(&fx).len(), 4, "a second round of two");
}

#[tokio::test]
async fn a_capacity_wait_tells_nobody() {
    let fx = Fx::with(HubConfig {
        roles: roles(),
        // The chief is in the channel, so the cap of one holds the spawn
        // back at enqueue, and the one-slot host holds it back at placement.
        channels: BTreeMap::from([(
            api(),
            borant_core::config::ChannelConfig {
                max_agents: Some(1),
                ..Default::default()
            },
        )]),
        ..HubConfig::default()
    });
    fx.req(CtlRequest::RegisterHost {
        info: HostInfo {
            name: name("h1"),
            harnesses: vec![Harness::Fake],
            launcher: "process".into(),
            max_agents: 1,
            ..Default::default()
        },
    })
    .await
    .unwrap();
    let chief = register_role(&fx, "chief", "chief", Some("work")).await;
    create_channel(&fx, &chief, "work-api").await;
    spawn(&fx, &chief, "helper", None).await.unwrap();

    let queued = record(&fx, "helper").await;
    assert_eq!(queued.queue_reason.as_deref(), Some("channel full (2/1)"));
    assert!(
        queued.last_reject.is_empty(),
        "a spawn at the channel cap is not placed yet"
    );

    // Every refusal is capacity, so nobody hears about it.
    fx.advance(16 * 60);
    fx.heartbeat("h1", &[&chief]).await;
    fx.hub.tick().await.unwrap();
    assert!(blocked_notices(&fx).is_empty());
    let queued = record(&fx, "helper").await;
    assert_eq!(queued.state, SpawnState::Queued);
    assert_eq!(
        queued.last_reject,
        vec![(name("h1"), "full (1/1)".to_owned())]
    );
}

/// An owner with `work-api` and the worker `w` that it spawned there, with a
/// claimed task and a held lease. With `with_chief`, the collection also has
/// a chief that is not the owner.
async fn working(fx: &Fx, with_chief: bool) -> (Registered, Registered, Task) {
    fx.host("h1", &[Harness::Fake]).await;
    let boss = fx.register("h1", Some("boss"), None).await;
    if with_chief {
        register_role(fx, "chief", "chief", None).await;
    }
    create_channel(fx, &boss, "work-api").await;
    fx.call(
        Some(&boss),
        ToolCall::Spawn {
            channel: api(),
            prompt: "do the work".into(),
            harness: None,
            host: Some(name("h1")),
            name: Some(agent("w")),
            model: None,
            role: Some("worker".parse().unwrap()),
            key: None,
            task: None,
            stop_on_close: None,
            continues: None,
            priority: None,
            bundles: Vec::new(),
        },
    )
    .await
    .unwrap();
    let w = register_role(fx, "w", "worker", None).await;
    let task = create_task(fx, &boss, "the work", Vec::new()).await;
    fx.call(
        Some(&w),
        ToolCall::TaskClaim {
            channel: api(),
            id: task.id,
        },
    )
    .await
    .unwrap();
    (boss, w, task)
}

#[tokio::test]
async fn a_stalled_card_names_what_it_holds_and_tells_the_owner_and_the_chief() {
    let fx = hub();
    let (boss, w, task) = working(&fx, true).await;
    let waiting = create_task(&fx, &boss, "the release", vec![task_of(&fx, task.id).await]).await;
    let chief = agent("chief");
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();
    let mut to_chief = fx.bus.subscribe(&subjects::agent(&chief)).await.unwrap();

    // The session is in a turn, so the stop waits for the turn end, and the
    // grace runs out: the hub fences it.
    fx.heartbeat_idle("h1", &[(&w, None)]).await;
    fx.call(
        Some(&boss),
        ToolCall::Stop {
            selector: PauseSelector::Agent(agent("w")),
            grace_secs: Some(60),
        },
    )
    .await
    .unwrap();
    fx.advance(61);
    fx.hub.tick().await.unwrap();

    // The fenced session is still held on the host, and nothing changed the
    // claim for the stall window.
    fx.advance(31 * 60);
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    let stalled = task_of(&fx, task.id).await;
    assert_eq!(stalled.status, TaskStatus::Stuck);
    let attention = stalled.attention_msg.clone().expect("the reason");
    assert_eq!(
        attention,
        format!(
            "its claimer w is fenced; holds: {}",
            task_of(&fx, waiting.id).await.title
        )
    );
    // The owner of the channel and the chief hear it once, with the same
    // words.
    let to_owner = messages(&mut to_boss).await;
    assert_eq!(to_owner.len(), 1, "{to_owner:?}");
    assert!(
        to_owner[0].text.contains("is stalled and stuck"),
        "{}",
        to_owner[0].text
    );
    assert!(
        to_owner[0].text.contains("its claimer w is fenced"),
        "{}",
        to_owner[0].text
    );
    assert!(
        to_owner[0].text.contains("the release"),
        "{}",
        to_owner[0].text
    );
    let to_the_chief = messages(&mut to_chief).await;
    assert_eq!(to_the_chief.len(), 1, "{to_the_chief:?}");
    assert_eq!(to_the_chief[0].text, to_owner[0].text);

    // A second pass says nothing new.
    fx.advance(60);
    fx.hub.tick().await.unwrap();
    assert!(messages(&mut to_boss).await.is_empty());
    assert!(messages(&mut to_chief).await.is_empty());
    assert_eq!(task_of(&fx, task.id).await.attention_msg, Some(attention));
}

/// A dead claimer is not a stalled card: the hub ends its session through the
/// liveness sweep, and the claim timeout returns its claim to open with one
/// attempt counted, or a pinned agent keeps the claim for its own host.
#[tokio::test]
async fn a_dead_claimer_is_left_to_the_retry_rules() {
    let fx = hub();
    let (boss, w, task) = working(&fx, false).await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    // The hub never learned that the session ended, and the agent is pinned,
    // so no rule returns the claim to open.
    let mut record_w = record(&fx, "w").await;
    record_w.state = SpawnState::Ended;
    replace(&fx, &keys::spawn(&agent("w")), &record_w).await;
    let lease = Lease {
        agent: agent("w"),
        token: w.token,
        state: LeaseState::Expired {
            since: fx.clock.now().minus(Duration::from_secs(600)),
            last_session: Some(w.session),
        },
    };
    replace(&fx, &keys::lease_agent(&agent("w")), &lease).await;
    let mut agent_w = agent_of(&fx, "w").await;
    agent_w.session = None;
    agent_w.pinned = true;
    replace(&fx, &keys::agent(&agent("w")), &agent_w).await;

    fx.advance(31 * 60);
    fx.hub.tick().await.unwrap();
    let still = task_of(&fx, task.id).await;
    assert_eq!(still.status, TaskStatus::InProgress);
    assert_eq!(still.attention_msg, None);
    for env in messages(&mut to_boss).await {
        assert!(!env.text.contains("is stalled and stuck"), "{}", env.text);
    }
}

#[tokio::test]
async fn a_queued_claimer_marks_its_claim_stuck() {
    let fx = hub();
    let (_boss, w, task) = working(&fx, false).await;

    // The spawn went back to the queue, and its refusal is one that a wait
    // cannot clear: the host has no bundle that the worker carries, and the
    // lease of its last session expired.
    let mut record_w = record(&fx, "w").await;
    record_w.state = SpawnState::Queued;
    record_w.queued_at = fx.clock.now().minus(Duration::from_secs(3 * 3_600));
    replace(&fx, &keys::spawn(&agent("w")), &record_w).await;
    let mut agent_w = agent_of(&fx, "w").await;
    agent_w.bundles = vec!["github-work".parse().unwrap()];
    replace(&fx, &keys::agent(&agent("w")), &agent_w).await;
    let lease = Lease {
        agent: agent("w"),
        token: w.token,
        state: LeaseState::Expired {
            since: fx.clock.now(),
            last_session: Some(w.session),
        },
    };
    replace(&fx, &keys::lease_agent(&agent("w")), &lease).await;

    fx.advance(31 * 60);
    // The host is still online, so the refusal that the queue refreshes is
    // the missing bundle.
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    let queued = record(&fx, "w").await;
    assert_eq!(queued.state, SpawnState::Queued);
    assert_eq!(
        queued.last_reject,
        vec![(name("h1"), "missing bundle github-work".to_owned())]
    );
    let attention = task_of(&fx, task.id)
        .await
        .attention_msg
        .clone()
        .expect("the reason");
    assert!(
        attention.contains("its claimer w is queued and blocked (missing bundle github-work)"),
        "{attention}"
    );
    assert_eq!(task_of(&fx, task.id).await.status, TaskStatus::Stuck);
}

#[tokio::test]
async fn a_live_claimer_leaves_its_claim_alone() {
    let fx = hub();
    let (boss, w, task) = working(&fx, false).await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();
    fx.advance(31 * 60);
    // The host keeps the session alive, so the claimer still works the task.
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    assert_eq!(task_of(&fx, task.id).await.status, TaskStatus::InProgress);
    assert_eq!(task_of(&fx, task.id).await.attention_msg, None);
    assert_eq!(record(&fx, "w").await.state, SpawnState::Running);
    // The owner may hear about the silence, never about a stalled card.
    for env in messages(&mut to_boss).await {
        assert!(!env.text.contains("is stalled and stuck"), "{}", env.text);
    }
}
