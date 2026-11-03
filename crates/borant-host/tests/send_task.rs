//! A message sent with `--task` carries the task, and the host writes a
//! `message.sent` event, so the person's task history can show the reply.

use std::sync::Arc;

use borant_core::events::{EventFilter, SwarmEventBody};
use borant_core::model::{Harness, Target, TaskRef};
use borant_core::ports::{EventLog, MessageBus};
use borant_core::proto::ToolCall;
use borant_core::{SystemClock, TaskId, subjects};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_launch::ProcessLauncher;
use borant_memory::{MemoryBus, MemoryEventLog, MemorySession};
use futures::StreamExt;
use serde_json::json;

#[tokio::test]
async fn a_sent_message_names_its_task_and_writes_an_event() {
    let bus = MemoryBus::new();
    let log = Arc::new(MemoryEventLog::new());
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.path().to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    let host = Host::new(
        cfg,
        Arc::new(bus.clone()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(ProcessLauncher::new(dir.path().join("logs"))),
        None,
        Some(log.clone()),
        Outbox::in_memory().unwrap(),
    );
    let boss = "boss".parse().unwrap();
    let mut inbox = bus.subscribe(&subjects::agent(&boss)).await.unwrap();
    let task = TaskRef {
        channel: "api".parse().unwrap(),
        id: TaskId::new(),
    };
    let v = host
        .call(
            None,
            ToolCall::Send {
                to: Target::Agent(boss.clone()),
                text: "your answer is in the task".into(),
                urgent: false,
                task: Some(task.clone()),
            },
        )
        .await
        .unwrap();
    assert_eq!(v["queued"], json!(false));

    let envelope = inbox.next().await.expect("the message is published");

    assert_eq!(
        envelope.task,
        Some(task.clone()),
        "the message names the task"
    );

    let filter = EventFilter {
        kinds: vec!["message.sent".into()],
        ..EventFilter::default()
    };
    let mut events = log.read(&filter, false).await.unwrap();
    let event = events.next().await.expect("the log has the event");
    assert_eq!(event.channel, Some(task.channel));
    assert_eq!(event.task, Some(task.id));
    match event.body {
        SwarmEventBody::MessageSent { to, text, .. } => {
            assert_eq!(to, Target::Agent(boss));
            assert_eq!(text, "your answer is in the task");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
