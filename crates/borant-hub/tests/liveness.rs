//! The liveness sweep (liveness spec, "Hub: the sweep"): a session that died
//! without an end is ended from the hub, the spawn stops showing running, a
//! bound worker is relaunched from its checkpoint, the owner hears about each
//! event, silence and a stuck queue are reported, and a host that failed a
//! launch stops blocking after the grace.

mod common;

use std::time::Duration;

use borant_core::model::{
    ChannelKind, Envelope, Harness, Lease, LeaseState, MoveReason, Session, SessionEvent,
    SessionEventBody, SpawnRecord, SpawnState, Task, TaskStatus,
};
use borant_core::ports::{MessageBus, SessionStore, StateStore};
use borant_core::proto::{
    CtlRequest, HostCommand, MoveSelector, MoveTarget, PauseSelector, Registered, ToolCall,
};
use borant_core::{AgentId, ChannelName, Clock, EventId, HostName, keys, subjects};
use common::{Fx, name, next_cmd};
use futures::StreamExt;
use futures::stream::BoxStream;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

fn h1() -> HostName {
    name("h1")
}

fn spawn_call(prompt: &str, agent: &str, host: Option<&str>) -> ToolCall {
    ToolCall::Spawn {
        channel: api(),
        prompt: prompt.to_owned(),
        harness: None,
        host: host.map(name),
        name: Some(agent.parse().unwrap()),
        model: None,
        role: None,
        key: None,
        task: None,
        stop_on_close: None,
        continues: None,
        priority: None,
        bundles: Vec::new(),
    }
}

async fn channel(fx: &Fx, who: &Registered) {
    fx.call(
        Some(who),
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
}

/// A channel owner `boss` on h1, and the worker `w` that it spawned there,
/// with a running spawn record and a held lease.
async fn setup(fx: &Fx) -> (Registered, Registered) {
    fx.host("h1", &[Harness::Fake]).await;
    let boss = fx.register("h1", Some("boss"), None).await;
    channel(fx, &boss).await;
    fx.call(Some(&boss), spawn_call("do the work", "w", Some("h1")))
        .await
        .unwrap();
    let w = fx.register("h1", Some("w"), Some("api")).await;
    (boss, w)
}

/// A task of `api` that `w` claims.
async fn claim(fx: &Fx, boss: &Registered, w: &Registered) -> Task {
    let task: Task = serde_json::from_value(
        fx.call(
            Some(boss),
            ToolCall::TaskCreate {
                channel: api(),
                task: borant_core::model::NewTask {
                    title: "the work".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value(
        fx.call(
            Some(w),
            ToolCall::TaskClaim {
                channel: api(),
                id: task.id,
            },
        )
        .await
        .unwrap(),
    )
    .unwrap()
}

async fn checkpoint(fx: &Fx, w: &Registered) {
    fx.sessions
        .append(
            &w.agent,
            &SessionEvent {
                id: EventId::new(),
                agent: w.agent.clone(),
                session: Some(w.session),
                harness: Harness::Fake,
                host: h1(),
                turn: 1,
                at: fx.clock.now(),
                body: SessionEventBody::Checkpoint {
                    git_ref: "refs/borant/w/wip".into(),
                    sha: "abc123".into(),
                    branch: Some("feat/liveness".into()),
                },
            },
        )
        .await
        .unwrap();
}

/// Expires the lease, leaves the session out of the host's next heartbeat,
/// and waits for the next sweep.
async fn die(fx: &Fx) {
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    // The heartbeat must be newer than the expiry, so it proves that the
    // session is gone.
    fx.advance(1);
    fx.heartbeat("h1", &[]).await;
    fx.advance(60);
    // The host must still be online for the relaunch that follows the end.
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
}

async fn spawn_record(fx: &Fx, agent: &AgentId) -> Option<SpawnRecord> {
    fx.state
        .get(&keys::spawn(agent))
        .await
        .unwrap()
        .map(|e| serde_json::from_slice(&e.value).unwrap())
}

async fn lease(fx: &Fx, agent: &AgentId) -> Lease {
    let e = fx
        .state
        .get(&keys::lease_agent(agent))
        .await
        .unwrap()
        .expect("a lease");
    serde_json::from_slice(&e.value).unwrap()
}

async fn session(fx: &Fx, w: &Registered) -> Session {
    let e = fx
        .state
        .get(&keys::session(&w.session))
        .await
        .unwrap()
        .expect("a session");
    serde_json::from_slice(&e.value).unwrap()
}

async fn task_of(fx: &Fx, task: &Task) -> Task {
    let e = fx
        .state
        .get(&keys::task(&api(), &task.id))
        .await
        .unwrap()
        .expect("a task");
    serde_json::from_slice(&e.value).unwrap()
}

async fn agent_retired(fx: &Fx, agent: &AgentId) -> bool {
    let e = fx
        .state
        .get(&keys::agent(agent))
        .await
        .unwrap()
        .expect("an agent");
    let value: borant_core::model::Agent = serde_json::from_slice(&e.value).unwrap();
    value.retired
}

async fn next_message(stream: &mut BoxStream<'static, Envelope>) -> Option<Envelope> {
    tokio::time::timeout(Duration::from_millis(500), stream.next())
        .await
        .ok()?
}

async fn messages(stream: &mut BoxStream<'static, Envelope>) -> Vec<Envelope> {
    let mut out = Vec::new();
    while let Some(env) = next_message(stream).await {
        out.push(env);
    }
    out
}

async fn no_message(stream: &mut BoxStream<'static, Envelope>, what: &str) {
    if let Some(env) = next_message(stream).await {
        panic!("{what}: {}", env.text);
    }
}

/// The next launch of `agent` on the stream. Another agent's launch, for
/// example an owner that a direct message woke, is skipped.
async fn next_launch(
    stream: &mut BoxStream<'static, HostCommand>,
    agent: &AgentId,
) -> Option<borant_core::proto::LaunchOrder> {
    loop {
        match next_cmd(stream).await {
            Some(HostCommand::Launch(order)) if &order.agent == agent => return Some(order),
            Some(HostCommand::Launch(_)) => continue,
            Some(_) => continue,
            None => return None,
        }
    }
}

async fn no_launch(stream: &mut BoxStream<'static, HostCommand>, agent: &AgentId, what: &str) {
    if let Some(order) = next_launch(stream, agent).await {
        panic!("{what}: {order:?}");
    }
}

#[tokio::test]
async fn sweep_ends_a_dead_session_and_relaunches_its_claim() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    let task = claim(&fx, &boss, &w).await;
    checkpoint(&fx, &w).await;
    let mut launches = fx.commands("h1").await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    die(&fx).await;

    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(
        record.state,
        SpawnState::Ended,
        "ps stops showing it running"
    );
    assert_eq!(record.recoveries, 1);
    assert!(session(&fx, &w).await.ended.is_some(), "the session ended");
    match lease(&fx, &w.agent).await.state {
        LeaseState::Reserved { host, reason, .. } => {
            assert_eq!(host, h1());
            assert_eq!(reason, MoveReason::Recover);
        }
        other => panic!("the worker is relaunched, got {other:?}"),
    }
    let order = next_launch(&mut launches, &w.agent)
        .await
        .expect("a recovery launch");
    assert_eq!(order.reason, MoveReason::Recover);
    assert_eq!(order.wip.as_deref(), Some("abc123"));
    assert_eq!(
        task_of(&fx, &task).await.status,
        TaskStatus::InProgress,
        "the claim stays with the recovered worker"
    );
    let queue = fx.call(None, ToolCall::SpawnQueue).await.unwrap();
    let row = queue
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == w.agent.as_str())
        .expect("the spawn in the queue");
    assert_eq!(row["state"], "ended");

    // A death and a relaunch: one message each, with the task and the
    // checkpoint that the host recorded.
    let sent = messages(&mut to_boss).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].text.contains("died"), "{}", sent[0].text);
    assert!(
        sent[1].text.contains("relaunched"),
        "the relaunch follows the death: {}",
        sent[1].text
    );
    let all: String = sent
        .iter()
        .map(|e| e.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(all.contains("died"), "{all}");
    assert!(all.contains("relaunched"), "{all}");
    assert!(all.contains(&task.id.to_string()), "{all}");
    assert!(all.contains("refs/borant/w/wip"), "{all}");
    assert!(all.contains("abc123"), "{all}");
    assert!(all.contains("feat/liveness"), "{all}");
}

#[tokio::test]
async fn a_late_end_after_expiry_ends_the_spawn_and_relaunches() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    let task = claim(&fx, &boss, &w).await;
    let mut launches = fx.commands("h1").await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    // The hub was down when the host saw the process end, so the end reaches
    // it after the lease expired and the token was raised.
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    // The host is back, so the relaunch has a place to go.
    fx.heartbeat("h1", &[]).await;
    let end = CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "the harness process ended while the host was down".into(),
    };
    fx.req(end.clone()).await.unwrap();

    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(record.state, SpawnState::Ended);
    assert!(session(&fx, &w).await.ended.is_some());
    assert!(
        matches!(
            lease(&fx, &w.agent).await.state,
            LeaseState::Reserved { .. }
        ),
        "the late end relaunches the claim"
    );
    assert!(
        next_launch(&mut launches, &w.agent).await.is_some(),
        "a late end still relaunches"
    );
    assert_eq!(task_of(&fx, &task).await.status, TaskStatus::InProgress);
    let sent = messages(&mut to_boss).await;
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].text.contains("died"), "{}", sent[0].text);
    assert!(sent[1].text.contains("relaunched"), "{}", sent[1].text);

    // A repeat of the same end is a session that already ended.
    fx.req(end).await.unwrap();
    no_launch(&mut launches, &w.agent, "a repeat end launches nothing").await;
    no_message(&mut to_boss, "a repeat end sends no message").await;
}

#[tokio::test]
async fn sweep_gives_up_after_three_recoveries() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    let task = claim(&fx, &boss, &w).await;
    let mut launches = fx.commands("h1").await;
    let mut current = w;

    for round in 1..=3 {
        die(&fx).await;
        let record = spawn_record(&fx, &current.agent).await.expect("the record");
        assert_eq!(record.recoveries, round, "one try per death");
        assert!(
            next_launch(&mut launches, &current.agent).await.is_some(),
            "round {round} relaunches the worker"
        );
        no_launch(
            &mut launches,
            &current.agent,
            &format!("round {round} sent a second launch"),
        )
        .await;
        // The launch registers, and the worker runs again with its claim.
        current = fx.register("h1", Some("w"), Some("api")).await;
        assert_eq!(
            spawn_record(&fx, &current.agent).await.unwrap().state,
            SpawnState::Running
        );
    }

    die(&fx).await;

    let record = spawn_record(&fx, &current.agent).await.expect("the record");
    assert_eq!(record.recoveries, 3, "no fourth try");
    assert_eq!(record.state, SpawnState::Ended);
    assert_eq!(
        task_of(&fx, &task).await.status,
        TaskStatus::NeedsHuman,
        "the claim waits for a person"
    );
    no_launch(
        &mut launches,
        &current.agent,
        "nothing launches the worker again",
    )
    .await;
}

#[tokio::test]
async fn silence_is_reported_once_per_quiet_spell() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    claim(&fx, &boss, &w).await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    // The session never wrote an event: the first turn never started.
    fx.advance(31 * 60);
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    let first = next_message(&mut to_boss).await.expect("a silence message");
    assert!(first.text.contains("silent"), "{}", first.text);
    assert!(first.text.contains("w"), "{}", first.text);

    // The same silence sends nothing again.
    fx.advance(60);
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    no_message(&mut to_boss, "a second message for the same silence").await;

    // A new event starts a new spell.
    checkpoint(&fx, &w).await;
    fx.advance(31 * 60);
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    let second = next_message(&mut to_boss).await.expect("a new silence");
    assert!(second.text.contains("silent"), "{}", second.text);
    assert!(
        second.text.contains("feat/liveness"),
        "the newer checkpoint is named: {}",
        second.text
    );
}

#[tokio::test]
async fn a_silent_host_is_not_swept_before_the_claim_timeout() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    // The host goes quiet: no heartbeat at all, so the sweep has no evidence
    // that the session is gone. A laptop sleep must not end every helper.
    fx.advance(31);
    fx.hub.tick().await.unwrap();
    fx.advance(200);
    fx.hub.tick().await.unwrap();

    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(
        record.state,
        SpawnState::Running,
        "a silent host is not a death"
    );
    assert!(session(&fx, &w).await.ended.is_none());
    no_message(&mut to_boss, "no death message for a silent host").await;

    // The host beats again and leaves the session out: now the sweep ends it.
    fx.heartbeat("h1", &[]).await;
    fx.advance(60);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(record.state, SpawnState::Ended);
    assert!(session(&fx, &w).await.ended.is_some());
    let message = next_message(&mut to_boss).await.expect("a death message");
    assert!(message.text.contains("died"), "{}", message.text);
}

#[tokio::test]
async fn a_reclaimed_session_restores_the_running_spawn() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    claim(&fx, &boss, &w).await;
    // h1 failed a launch, so the recovery has no host and its pending move
    // stays behind.
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "clone failed".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();

    fx.advance(31);
    fx.hub.tick().await.unwrap();
    fx.advance(1);
    fx.heartbeat("h1", &[]).await;
    fx.advance(60);
    fx.heartbeat("h1", &[]).await;
    fx.hub.tick().await.unwrap();
    assert_eq!(
        spawn_record(&fx, &w.agent).await.unwrap().state,
        SpawnState::Ended,
        "the sweep ended the session"
    );

    // The session was only asleep and reclaims its expired lease.
    fx.heartbeat("h1", &[&w]).await;
    assert_eq!(
        spawn_record(&fx, &w.agent).await.unwrap().state,
        SpawnState::Running,
        "a reclaimed session runs again"
    );
    assert!(
        session(&fx, &w).await.ended.is_none(),
        "a reclaimed session is not ended"
    );
    let mut launches = fx.commands("h1").await;
    fx.advance(200);
    fx.heartbeat("h1", &[&w]).await;
    fx.hub.tick().await.unwrap();
    no_launch(&mut launches, &w.agent, "the pending recovery is gone").await;
}

#[tokio::test]
async fn a_recent_failure_sends_the_recovery_to_another_host() {
    let fx = Fx::new();
    let (_boss, w) = setup(&fx).await;
    fx.host("h2", &[Harness::Fake]).await;
    claim(&fx, &_boss, &w).await;
    checkpoint(&fx, &w).await;
    let mut h2 = fx.commands("h2").await;

    // h1 failed a launch of this agent and is still inside the grace.
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "clone failed".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();

    fx.advance(31);
    fx.hub.tick().await.unwrap();
    fx.advance(1);
    fx.heartbeat("h1", &[]).await;
    fx.advance(60);
    fx.heartbeat("h1", &[]).await;
    fx.heartbeat("h2", &[]).await;
    fx.hub.tick().await.unwrap();

    assert!(
        next_launch(&mut h2, &w.agent).await.is_some(),
        "the recovery avoids the recently failed host"
    );
    match lease(&fx, &w.agent).await.state {
        LeaseState::Reserved { host, .. } => assert_eq!(host, name("h2")),
        other => panic!("expected a reservation on h2, got {other:?}"),
    }
}

#[tokio::test]
async fn a_launch_failure_of_a_running_spawn_counts_no_try() {
    let fx = Fx::new();
    let (_boss, w) = setup(&fx).await;
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "the move failed".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();

    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(record.state, SpawnState::Running, "not a failed spawn");
    assert_eq!(record.attempts, 0, "a running spawn counts no launch try");
    assert!(!agent_retired(&fx, &w.agent).await, "the agent stays");
}

#[tokio::test]
async fn resume_clears_failed_hosts_and_a_failed_resume_tells_the_resumer() {
    let fx = Fx::new();
    let (boss, w) = setup(&fx).await;
    let mut launches = fx.commands("h1").await;

    // The session ends, and then a launch of this agent fails on h1, the
    // only host.
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "test".into(),
    })
    .await
    .unwrap();
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "clone failed".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();
    let mut to_boss = fx
        .bus
        .subscribe(&subjects::agent(&boss.agent))
        .await
        .unwrap();

    // The resume clears the failure, so h1 takes the agent again.
    fx.call(
        Some(&boss),
        ToolCall::Resume {
            selector: PauseSelector::Agent(w.agent.clone()),
        },
    )
    .await
    .unwrap();
    let order = next_launch(&mut launches, &w.agent)
        .await
        .expect("the resume launches on the formerly failed host");
    assert_eq!(order.reason, MoveReason::Resume);

    // The resumed launch fails: no try is counted, and the agent that asked
    // to resume hears about it.
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "clone failed again".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();
    let message = next_message(&mut to_boss).await.expect("a resume message");
    assert!(
        message.text.contains("resume of agent w failed"),
        "{}",
        message.text
    );
    let record = spawn_record(&fx, &w.agent).await.expect("the record");
    assert_eq!(record.attempts, 0, "a failed resume counts no spawn try");
    assert!(!agent_retired(&fx, &w.agent).await, "the agent stays");
}

#[tokio::test]
async fn an_explicit_move_clears_failed_hosts() {
    let fx = Fx::new();
    let (_boss, w) = setup(&fx).await;
    let mut launches = fx.commands("h1").await;

    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: "test".into(),
    })
    .await
    .unwrap();
    fx.req(CtlRequest::LaunchFailed {
        agent: w.agent.clone(),
        host: h1(),
        error: "clone failed".into(),
        timeout_secs: None,
    })
    .await
    .unwrap();

    fx.call(
        None,
        ToolCall::Move {
            selector: MoveSelector::Agent(w.agent.clone()),
            to: MoveTarget {
                host: Some(h1()),
                harness: None,
            },
            member: None,
        },
    )
    .await
    .expect("the explicit move clears the failed host");

    let order = next_launch(&mut launches, &w.agent)
        .await
        .expect("the moved launch");
    assert_eq!(order.reason, MoveReason::Planned);
}
