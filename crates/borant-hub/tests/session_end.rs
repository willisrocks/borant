//! A session that ends while its agent holds an `in_progress` claim: the hub
//! places the agent again through the takeover path, or, when no host takes
//! it, puts the claim in front of its owner as `stuck`. A retired agent is
//! not placed again, and an agent without a claim keeps today's release.

mod common;

use borant_core::events::{EventFilter, SwarmEvent, SwarmEventBody, kind_of};
use borant_core::model::{
    Agent, Harness, Lease, LeaseState, MoveReason, NewTask, Task, TaskStatus,
};
use borant_core::ports::{EventLog, StateStore};
use borant_core::proto::{CtlRequest, HostCommand, Registered, ToolCall};
use borant_core::{ChannelName, keys};
use common::{Fx, next_cmd};
use futures::StreamExt;
use futures::stream::BoxStream;

fn api() -> ChannelName {
    "api".parse().unwrap()
}

/// An owner on h1 with channel `api`, and a worker `w` on h1.
async fn setup(fx: &Fx) -> (Registered, Registered) {
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: api(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    let w = fx.register("h1", Some("w"), Some("api")).await;
    (owner, w)
}

/// A task in `api` that `w` claims.
async fn claim(fx: &Fx, owner: &Registered, w: &Registered) -> Task {
    let t: Task = serde_json::from_value(
        fx.call(
            Some(owner),
            ToolCall::TaskCreate {
                channel: api(),
                task: NewTask {
                    title: "t".into(),
                    ..Default::default()
                },
            },
        )
        .await
        .unwrap(),
    )
    .unwrap();
    fx.call(
        Some(w),
        ToolCall::TaskClaim {
            channel: api(),
            id: t.id,
        },
    )
    .await
    .unwrap();
    t
}

async fn end(fx: &Fx, w: &Registered, reason: &str) {
    fx.req(CtlRequest::EndSession {
        session: w.session,
        agent: w.agent.clone(),
        token: w.token,
        reason: reason.into(),
    })
    .await
    .unwrap();
}

async fn lease(fx: &Fx, agent: &borant_core::AgentId) -> Lease {
    let e = fx
        .state
        .get(&keys::lease_agent(agent))
        .await
        .unwrap()
        .expect("the agent has a lease");
    serde_json::from_slice(&e.value).unwrap()
}

async fn task_of(fx: &Fx, id: &borant_core::TaskId) -> Task {
    let e = fx
        .state
        .get(&keys::task(&api(), id))
        .await
        .unwrap()
        .expect("the task exists");
    serde_json::from_slice(&e.value).unwrap()
}

async fn events(fx: &Fx) -> Vec<SwarmEvent> {
    let mut stream = fx
        .events
        .read(&EventFilter::default(), false)
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(event) = stream.next().await {
        out.push(event);
    }
    out
}

/// The `session.relaunch` events, which say which of the two outcomes came.
fn relaunches(events: &[SwarmEvent]) -> Vec<&SwarmEvent> {
    events
        .iter()
        .filter(|e| kind_of(e) == "session.relaunch")
        .collect()
}

async fn expect_launch(cmd: Option<HostCommand>, w: &Registered) {
    match cmd {
        Some(HostCommand::Launch(o)) => {
            assert_eq!(o.agent, w.agent);
            assert_eq!(o.reason, MoveReason::Takeover);
            assert!(
                o.prompt.contains("You continue Borant agent w"),
                "the takeover carries its prompt context: {}",
                o.prompt
            );
        }
        other => panic!("expected a relaunch, got {other:?}"),
    }
}

fn no_command(cmd: Option<HostCommand>, why: &str) {
    assert!(cmd.is_none(), "{why}, got {cmd:?}");
}

#[tokio::test]
async fn session_end_relaunches_a_claimed_agent_once() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let task = claim(&fx, &owner, &w).await;
    let mut h1: BoxStream<'static, HostCommand> = fx.commands("h1").await;

    end(&fx, &w, "session shutdown").await;

    expect_launch(next_cmd(&mut h1).await, &w).await;
    // Placed once: a tick makes no second launch of the same agent.
    fx.hub.tick().await.unwrap();
    no_command(next_cmd(&mut h1).await, "the agent is placed once");
    // The claim stays with the agent that continues its task.
    assert_eq!(task_of(&fx, &task.id).await.status, TaskStatus::InProgress);

    let events = events(&fx).await;
    let relaunch = relaunches(&events);
    assert_eq!(
        relaunch.len(),
        1,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    let SwarmEventBody::SessionRelaunch {
        session,
        reason,
        failed,
    } = &relaunch[0].body
    else {
        panic!("not a relaunch: {:?}", relaunch[0].body);
    };
    assert_eq!(*session, w.session);
    assert_eq!(reason, "session shutdown");
    assert_eq!(*failed, None, "one event says the hub placed it again");
    assert_eq!(relaunch[0].agent, Some(w.agent.clone()));
}

#[tokio::test]
async fn session_end_with_no_host_marks_the_claim_stuck() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let task = claim(&fx, &owner, &w).await;
    let mut h1: BoxStream<'static, HostCommand> = fx.commands("h1").await;
    // The only host goes silent, so no host takes the agent.
    fx.advance(200);

    end(&fx, &w, "session shutdown").await;

    no_command(next_cmd(&mut h1).await, "no relaunch without a host");
    let saved = task_of(&fx, &task.id).await;
    assert_eq!(saved.status, TaskStatus::Stuck);
    let note = "the session of w ended (session shutdown); relaunch failed: h1: offline";
    assert_eq!(saved.attention_msg.as_deref(), Some(note));

    let events = events(&fx).await;
    let relaunch = relaunches(&events);
    assert_eq!(
        relaunch.len(),
        1,
        "{:?}",
        events.iter().map(kind_of).collect::<Vec<_>>()
    );
    let SwarmEventBody::SessionRelaunch { reason, failed, .. } = &relaunch[0].body else {
        panic!("not a relaunch: {:?}", relaunch[0].body);
    };
    assert_eq!(reason, "session shutdown");
    assert_eq!(
        failed.as_deref(),
        Some("h1: offline"),
        "one event says why the relaunch failed"
    );
    // The same reason is in front of the owner on the task.
    let stuck = events
        .iter()
        .find(|e| {
            matches!(
                &e.body,
                SwarmEventBody::TaskStatus {
                    to: TaskStatus::Stuck,
                    ..
                }
            )
        })
        .expect("a task.status event for the stuck claim");
    let SwarmEventBody::TaskStatus { attention_msg, .. } = &stuck.body else {
        unreachable!()
    };
    assert_eq!(attention_msg.as_deref(), Some(note));
}

#[tokio::test]
async fn session_end_of_a_retired_agent_is_not_relaunched() {
    let fx = Fx::new();
    let (owner, w) = setup(&fx).await;
    let task = claim(&fx, &owner, &w).await;
    // A retired agent is never placed again: the existing guard.
    let key = keys::agent(&w.agent);
    let doc = fx.state.get(&key).await.unwrap().unwrap();
    let mut a: Agent = serde_json::from_slice(&doc.value).unwrap();
    a.retired = true;
    fx.state
        .update(&key, serde_json::to_vec(&a).unwrap().into(), doc.revision)
        .await
        .unwrap();
    let mut h1: BoxStream<'static, HostCommand> = fx.commands("h1").await;

    end(&fx, &w, "session shutdown").await;

    no_command(next_cmd(&mut h1).await, "a retired agent is not relaunched");
    // Its claim stays, as the retire and pause paths expect.
    assert_eq!(task_of(&fx, &task.id).await.status, TaskStatus::InProgress);
    assert!(relaunches(&events(&fx).await).is_empty());
}

#[tokio::test]
async fn session_end_without_a_claim_releases_as_before() {
    let fx = Fx::new();
    let (_owner, w) = setup(&fx).await;
    let mut h1: BoxStream<'static, HostCommand> = fx.commands("h1").await;

    end(&fx, &w, "session shutdown").await;

    no_command(next_cmd(&mut h1).await, "no relaunch without a claim");
    assert!(matches!(
        lease(&fx, &w.agent).await.state,
        LeaseState::Released { parked: false, .. }
    ));
    assert!(relaunches(&events(&fx).await).is_empty());
}
