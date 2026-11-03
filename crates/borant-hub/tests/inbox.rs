//! The human inbox, and answering the question of a task (phase 6 spec,
//! "Human inbox").

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use borant_core::model::{
    Agent, ChannelKind, Envelope, Harness, NewTask, PauseReason, Sender, Target, TaskPatch,
    TaskStatus,
};
use borant_core::ports::MessageBus;
use borant_core::ports::StateStore;
use borant_core::proto::{ApiErrorCode, Registered, ToolCall};
use borant_core::rules::roles::RoleConfig;
use borant_core::{AgentId, ChannelName, Clock, TaskId, keys};
use borant_hub::HubConfig;
use common::Fx;
use futures::StreamExt;
use serde_json::{Value, json};

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// A host, a channel owner, and the channel.
async fn with_channel(fx: &Fx) -> Registered {
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
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
    owner
}

/// A task with a title, created by `who`.
async fn create(fx: &Fx, who: &Registered, title: &str) -> Value {
    fx.call(
        Some(who),
        ToolCall::TaskCreate {
            channel: api(),
            task: NewTask {
                title: title.into(),
                ..NewTask::default()
            },
        },
    )
    .await
    .unwrap()
}

/// Claims the task, then asks a question with `status` and `msg`.
async fn ask(fx: &Fx, who: &Registered, id: TaskId, status: TaskStatus, msg: &str) {
    fx.call(Some(who), ToolCall::TaskClaim { channel: api(), id })
        .await
        .unwrap();
    fx.call(
        Some(who),
        ToolCall::TaskUpdate {
            channel: api(),
            id,
            patch: TaskPatch {
                status: Some(status),
                attention_msg: Some(msg.into()),
                ..TaskPatch::default()
            },
        },
    )
    .await
    .unwrap();
}

/// Pauses an agent the way the stop task will: the record carries `paused`
/// (there is no `stop` op on this branch yet).
async fn pause(fx: &Fx, agent: &AgentId, why: &str) {
    let key = keys::agent(agent);
    let entry = fx.state.get(&key).await.unwrap().expect("the agent record");
    let mut record: Agent = serde_json::from_slice(&entry.value).unwrap();
    record.paused = Some(PauseReason {
        by: Sender::Human,
        why: why.into(),
        at: fx.clock.now(),
    });
    fx.state
        .update(
            &key,
            serde_json::to_vec(&record).unwrap().into(),
            entry.revision,
        )
        .await
        .unwrap();
}

/// The `updated` of a task, which an answer must name.
async fn updated(fx: &Fx, who: &Registered, id: TaskId) -> i64 {
    let task = fx
        .call(Some(who), ToolCall::TaskGet { channel: api(), id })
        .await
        .unwrap();
    task["updated"].as_i64().unwrap()
}

#[tokio::test]
async fn inbox_lists_needs_human_and_stuck_tasks_and_paused_agents() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let w2 = fx.register("h1", Some("w2"), Some("api")).await;

    let asked = create(&fx, &owner, "decide the storage").await;
    ask(
        &fx,
        &w1,
        asked["id"].as_str().unwrap().parse().unwrap(),
        TaskStatus::NeedsHuman,
        "which database?",
    )
    .await;
    fx.clock.advance(Duration::from_secs(600));

    let stuck = create(&fx, &owner, "the migration").await;
    ask(
        &fx,
        &w1,
        stuck["id"].as_str().unwrap().parse().unwrap(),
        TaskStatus::Stuck,
        "blocked on the schema",
    )
    .await;
    fx.clock.advance(Duration::from_secs(300));

    // A paused agent with no claim is in the inbox; one that still holds a
    // claim is not, because its work is in flight.
    pause(&fx, &w2.agent, "cap: 200 turns today").await;
    pause(&fx, &w1.agent, "cap: 200 turns today").await;

    let inbox = fx.call(None, ToolCall::Inbox).await.unwrap();
    let tasks = inbox["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "{inbox}");
    // Oldest first.
    assert_eq!(tasks[0]["id"], asked["id"], "{inbox}");
    assert_eq!(tasks[0]["channel"], "api");
    assert_eq!(tasks[0]["title"], "decide the storage");
    assert_eq!(tasks[0]["status"], "needs_human");
    assert_eq!(tasks[0]["claimer"], "w1");
    assert_eq!(tasks[0]["attention_msg"], "which database?");
    assert_eq!(tasks[0]["age_ms"], 900_000);
    assert_eq!(tasks[1]["id"], stuck["id"], "{inbox}");
    assert_eq!(tasks[1]["status"], "stuck");
    assert_eq!(tasks[1]["attention_msg"], "blocked on the schema");
    assert_eq!(tasks[1]["age_ms"], 300_000);

    let agents = inbox["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{inbox}");
    assert_eq!(agents[0]["agent"], "w2");
    assert_eq!(agents[0]["why"], "cap: 200 turns today");
    assert_eq!(agents[0]["age_ms"], 0);
    assert_eq!(inbox["at"], 1_000_000 + 900_000);
}

#[tokio::test]
async fn inbox_is_for_a_person_only() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let err = fx.call(Some(&owner), ToolCall::Inbox).await.unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
}

#[tokio::test]
async fn inbox_answer_reaches_the_claimer() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "which database?").await;
    let expect = updated(&fx, &owner, id).await;

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&w1.agent))
        .await
        .unwrap();
    let reply = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "use postgres".into(),
                expect_updated: borant_core::Timestamp(expect),
                key: None,
            },
        )
        .await
        .unwrap();

    // The task is back in progress with no question, and the claimer keeps it.
    assert_eq!(reply["status"], "in_progress", "{reply}");
    assert_eq!(reply["to"], "w1");
    assert_eq!(reply["task"]["status"], "in_progress");
    assert_eq!(reply["task"]["claimed_by"], "w1");
    assert!(reply["task"]["attention_msg"].is_null(), "{reply}");

    // The claimer got one direct message from a person.
    let incoming = messages.next().await.expect("a direct message");
    let envelope: Envelope = incoming;
    assert_eq!(envelope.from, Sender::Human);
    assert_eq!(envelope.to, Target::Agent(w1.agent.clone()));
    assert_eq!(envelope.text, "use postgres");

    // It leaves the inbox.
    let inbox = fx.call(None, ToolCall::Inbox).await.unwrap();
    assert!(inbox["tasks"].as_array().unwrap().is_empty(), "{inbox}");
}

#[tokio::test]
async fn inbox_answer_with_a_stale_updated_is_a_conflict() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "which database?").await;
    let stale = updated(&fx, &owner, id).await;
    // The task changes after the person read it.
    fx.clock.advance(Duration::from_secs(1));
    fx.call(
        Some(&w1),
        ToolCall::TaskUpdate {
            channel: api(),
            id,
            patch: TaskPatch {
                body: Some("more context".into()),
                ..TaskPatch::default()
            },
        },
    )
    .await
    .unwrap();

    let err = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "use postgres".into(),
                expect_updated: borant_core::Timestamp(stale),
                key: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(err.message.contains("changed at"), "{err:?}");
}

#[tokio::test]
async fn inbox_answer_without_a_claimer_goes_to_the_owner() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    // A question with no claimer: the owner asked it.
    fx.call(
        Some(&owner),
        ToolCall::TaskUpdate {
            channel: api(),
            id,
            patch: TaskPatch {
                status: Some(TaskStatus::NeedsHuman),
                attention_msg: Some("which database?".into()),
                ..TaskPatch::default()
            },
        },
    )
    .await
    .unwrap();
    let expect = updated(&fx, &owner, id).await;

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&owner.agent))
        .await
        .unwrap();
    let reply = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "use postgres".into(),
                expect_updated: borant_core::Timestamp(expect),
                key: None,
            },
        )
        .await
        .unwrap();

    // The owner gets the text, and the status stays where it was.
    assert_eq!(reply["status"], "unchanged", "{reply}");
    assert_eq!(reply["to"], "boss");
    assert_eq!(reply["task"]["status"], "needs_human");
    assert_eq!(reply["task"]["attention_msg"], "which database?");
    let incoming = messages.next().await.expect("a direct message");
    let envelope: Envelope = incoming;
    assert_eq!(envelope.from, Sender::Human);
    assert_eq!(envelope.to, Target::Agent(owner.agent.clone()));
    assert_eq!(envelope.text, "use postgres");

    // It stays in the inbox: nobody was unblocked.
    let inbox = fx.call(None, ToolCall::Inbox).await.unwrap();
    assert_eq!(inbox["tasks"].as_array().unwrap().len(), 1, "{inbox}");
}

#[tokio::test]
async fn inbox_answer_retry_with_the_same_key_sends_nothing() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "which database?").await;
    let expect = updated(&fx, &owner, id).await;

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&w1.agent))
        .await
        .unwrap();
    let answer = |expect: i64| ToolCall::Answer {
        channel: api(),
        task: id,
        text: "use postgres".into(),
        expect_updated: borant_core::Timestamp(expect),
        key: Some("draft-1".into()),
    };
    // The first answer changes the task a moment after the person read it.
    fx.advance(1);
    let first = fx.call(None, answer(expect)).await.unwrap();
    let envelope: Envelope = messages.next().await.expect("the first answer");
    assert_eq!(envelope.text, "use postgres");

    // The person's draft times out and the mod retries with the same key and
    // the version of the task that the person saw. The retry returns the
    // first result and sends nothing, although the first answer changed the
    // task.
    let again = fx.call(None, answer(expect)).await.unwrap();
    assert_eq!(again, first);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), messages.next())
            .await
            .is_err(),
        "the retry sent a second message"
    );
    let now = fx
        .call(Some(&owner), ToolCall::TaskGet { channel: api(), id })
        .await
        .unwrap();
    assert_eq!(now["updated"], first["task"]["updated"]);
}

#[tokio::test]
async fn inbox_answer_key_with_another_text_is_a_conflict() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "which database?").await;
    let expect = updated(&fx, &owner, id).await;
    fx.call(
        None,
        ToolCall::Answer {
            channel: api(),
            task: id,
            text: "use postgres".into(),
            expect_updated: borant_core::Timestamp(expect),
            key: Some("draft-1".into()),
        },
    )
    .await
    .unwrap();

    // An edited draft needs a new key: the old one holds another text.
    let err = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "use sqlite".into(),
                expect_updated: borant_core::Timestamp(expect),
                key: Some("draft-1".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(err.message.contains("new key"), "{err:?}");
}

/// The key and its result live for a day, so a key that a person sent again
/// after that is new and its text may differ.
#[tokio::test]
async fn inbox_answer_key_expires_after_a_day() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "which database?").await;
    let expect = updated(&fx, &owner, id).await;
    // The first answer changes the task a moment later.
    fx.advance(1);
    fx.call(
        None,
        ToolCall::Answer {
            channel: api(),
            task: id,
            text: "use postgres".into(),
            expect_updated: borant_core::Timestamp(expect),
            key: Some("draft-1".into()),
        },
    )
    .await
    .unwrap();

    fx.advance(24 * 60 * 60);
    // The answer is new, and the task has changed since the person read it.
    let err = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "use sqlite".into(),
                expect_updated: borant_core::Timestamp(expect),
                key: Some("draft-1".into()),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Conflict, "{err:?}");
    assert!(err.message.contains("changed at"), "{err:?}");
}

#[tokio::test]
async fn inbox_answer_needs_text() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let task = create(&fx, &owner, "decide the storage").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    let expect = updated(&fx, &owner, id).await;
    let err = fx
        .call(
            None,
            ToolCall::Answer {
                channel: api(),
                task: id,
                text: "   ".into(),
                expect_updated: borant_core::Timestamp(expect),
                key: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");
}

/// The hub config of the approval tests: a chief that approves, and a worker
/// that does not.
fn approve_config() -> HubConfig {
    HubConfig {
        roles: BTreeMap::from([
            (
                "chief".parse().unwrap(),
                RoleConfig {
                    approves: true,
                    ..RoleConfig::default()
                },
            ),
            ("worker".parse().unwrap(), RoleConfig::default()),
        ]),
        ..HubConfig::default()
    }
}

/// A channel `api` with an owner, on a hub with the approval roles.
async fn approve_fixture() -> (Fx, Registered) {
    let fx = Fx::with(approve_config());
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
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
    (fx, owner)
}

async fn swarm_events(fx: &Fx) -> Vec<borant_core::events::SwarmEvent> {
    fx.events
        .read(&borant_core::events::EventFilter::default(), false)
        .await
        .expect("read the event log")
        .collect()
        .await
}

/// A chief's approval answers a needs_human task, sends the claimer the
/// person's words under the `approval` sender, and records both events.
#[tokio::test]
async fn approve_by_a_chief_answers_and_attests_the_person_s_word() {
    let (fx, owner) = approve_fixture().await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let chief = fx.register_roled("h1", "chief-1", None, "chief").await;
    let task = create(&fx, &owner, "ship the release").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    ask(&fx, &w1, id, TaskStatus::NeedsHuman, "may I ship?").await;

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&w1.agent))
        .await
        .unwrap();
    let reply = fx
        .call(
            Some(&chief),
            ToolCall::Approve {
                channel: api(),
                task: id,
                quote: "ship it, token=abc123".into(),
            },
        )
        .await
        .unwrap();

    // The question is answered: no attention, and the claimer keeps the task.
    assert_eq!(reply["status"], "in_progress", "{reply}");
    assert_eq!(reply["to"], "w1");
    assert_eq!(reply["task"]["status"], "in_progress");
    assert_eq!(reply["task"]["claimed_by"], "w1");
    assert!(reply["task"]["attention_msg"].is_null(), "{reply}");

    // The claimer gets the person's words from the hub, not from an agent,
    // and the quote is redacted.
    let envelope: Envelope = messages.next().await.expect("a direct message");
    assert_eq!(envelope.from, Sender::Approval(chief.agent.clone()));
    assert_eq!(envelope.to, Target::Agent(w1.agent.clone()));
    assert!(!envelope.text.contains("abc123"), "{}", envelope.text);
    assert!(envelope.text.contains("ship it"), "{}", envelope.text);

    // The log holds the approval with its session and redacted quote, and the
    // answer as the chief's relay.
    let events = swarm_events(&fx).await;
    let approved = events
        .iter()
        .find(|e| {
            matches!(
                &e.body,
                borant_core::events::SwarmEventBody::TaskApproved { .. }
            )
        })
        .expect("a task.approved event");
    assert_eq!(approved.task, Some(id));
    let borant_core::events::SwarmEventBody::TaskApproved { by, session, quote } = &approved.body
    else {
        unreachable!()
    };
    assert_eq!(by, &chief.agent);
    assert_eq!(*session, chief.session);
    assert!(!quote.contains("abc123"), "{quote}");
    let answered = events
        .iter()
        .find_map(|e| match &e.body {
            borant_core::events::SwarmEventBody::TaskAnswered { by, via, .. } => {
                Some((by.clone(), *via))
            }
            _ => None,
        })
        .expect("a task.answered event");
    assert_eq!(answered.0, Sender::Approval(chief.agent.clone()));
    assert_eq!(answered.1, borant_core::events::AnswerVia::Chief);

    // The question leaves the inbox.
    let inbox = fx.call(None, ToolCall::Inbox).await.unwrap();
    assert!(inbox["tasks"].as_array().unwrap().is_empty(), "{inbox}");
}

/// Only a role with `approves = true` approves: a worker's call is denied and
/// changes nothing, and a person's shell cannot approve at all.
#[tokio::test]
async fn approve_by_a_role_without_the_flag_is_denied() {
    let (fx, owner) = approve_fixture().await;
    let worker = fx.register_roled("h1", "w2", None, "worker").await;
    let task = create(&fx, &owner, "ship the release").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();

    let err = fx
        .call(
            Some(&worker),
            ToolCall::Approve {
                channel: api(),
                task: id,
                quote: "ship it".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Denied, "{err:?}");
    assert!(err.message.contains("does not approve"), "{err:?}");

    let err = fx
        .call(
            None,
            ToolCall::Approve {
                channel: api(),
                task: id,
                quote: "ship it".into(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, ApiErrorCode::Invalid, "{err:?}");

    // The refusals recorded nothing.
    assert!(!swarm_events(&fx).await.iter().any(|e| matches!(
        &e.body,
        borant_core::events::SwarmEventBody::TaskApproved { .. }
    )));
}

/// An approval of a task that does not wait for a person records the approval
/// and tells the claimer, without touching the task's status.
#[tokio::test]
async fn approve_of_a_working_task_leaves_its_status() {
    let (fx, owner) = approve_fixture().await;
    let w1 = fx.register("h1", Some("w1"), Some("api")).await;
    let chief = fx.register_roled("h1", "chief-1", None, "chief").await;
    let task = create(&fx, &owner, "ship the release").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    fx.call(Some(&w1), ToolCall::TaskClaim { channel: api(), id })
        .await
        .unwrap();

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&w1.agent))
        .await
        .unwrap();
    let reply = fx
        .call(
            Some(&chief),
            ToolCall::Approve {
                channel: api(),
                task: id,
                quote: "ship it".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reply["status"], "unchanged", "{reply}");
    assert_eq!(reply["task"]["status"], "in_progress", "{reply}");
    let envelope: Envelope = messages.next().await.expect("a direct message");
    assert_eq!(envelope.from, Sender::Approval(chief.agent.clone()));
}

/// An approval of a question that nobody holds, such as a saga gate, reaches
/// the channel owner and clears the message, and the task keeps its status.
#[tokio::test]
async fn approve_of_an_unclaimed_question_reaches_the_owner() {
    let (fx, owner) = approve_fixture().await;
    let chief = fx.register_roled("h1", "chief-1", None, "chief").await;
    let task = create(&fx, &owner, "approve the scope").await;
    let id: TaskId = task["id"].as_str().unwrap().parse().unwrap();
    fx.call(
        Some(&owner),
        ToolCall::TaskUpdate {
            channel: api(),
            id,
            patch: TaskPatch {
                status: Some(TaskStatus::NeedsHuman),
                attention_msg: Some("scope?".into()),
                ..TaskPatch::default()
            },
        },
    )
    .await
    .unwrap();

    let mut messages = fx
        .bus
        .subscribe(&borant_core::subjects::agent(&owner.agent))
        .await
        .unwrap();
    let reply = fx
        .call(
            Some(&chief),
            ToolCall::Approve {
                channel: api(),
                task: id,
                quote: "approved".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(reply["status"], "unchanged", "{reply}");
    assert_eq!(reply["to"], "boss");
    assert_eq!(reply["task"]["status"], "needs_human", "{reply}");
    assert!(reply["task"]["attention_msg"].is_null(), "{reply}");
    let envelope: Envelope = messages.next().await.expect("a direct message");
    assert_eq!(envelope.from, Sender::Approval(chief.agent.clone()));
    assert_eq!(envelope.to, Target::Agent(owner.agent.clone()));
}

/// The inbox exposes no value that a test cannot name: the reply is JSON with
/// the fields of the spec, not a raw record.
#[tokio::test]
async fn inbox_reply_is_json_with_the_spec_fields() {
    let fx = Fx::new();
    let owner = with_channel(&fx).await;
    let task = create(&fx, &owner, "decide").await;
    ask(
        &fx,
        &owner,
        task["id"].as_str().unwrap().parse().unwrap(),
        TaskStatus::NeedsHuman,
        "?",
    )
    .await;
    let inbox = fx.call(None, ToolCall::Inbox).await.unwrap();
    let mut keys: Vec<&str> = inbox
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["agents", "at", "tasks"]);
    let mut task_keys: Vec<&str> = inbox["tasks"][0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    task_keys.sort_unstable();
    assert_eq!(
        task_keys,
        [
            "age_ms",
            "attention_msg",
            "channel",
            "claimer",
            "id",
            "status",
            "title"
        ]
    );
    assert_eq!(inbox["tasks"][0]["title"], json!("decide"));
}
