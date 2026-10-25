mod common;

use borant_core::model::{Harness, SessionEvent, SessionEventBody, Target};
use borant_core::ports::{MessageBus, SessionStore, StateStore};
use borant_core::proto::ToolCall;
use borant_core::{EventId, Timestamp, subjects, testing};
use borant_memory::{MemoryBus, MemorySession, MemoryState};
use common::Fx;

#[tokio::test]
async fn export_import_round_trip() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let owner = fx.register("h1", Some("boss"), None).await;
    fx.call(
        Some(&owner),
        ToolCall::ChannelCreate {
            name: "api".parse().unwrap(),
            schema: None,
            project: None,
            branch: None,
        },
    )
    .await
    .unwrap();
    fx.sessions
        .append(
            &owner.agent,
            &SessionEvent {
                id: EventId::new(),
                agent: owner.agent.clone(),
                session: Some(owner.session),
                harness: Harness::Fake,
                host: "h1".parse().unwrap(),
                turn: 1,
                at: Timestamp(0),
                body: SessionEventBody::UserPrompt {
                    text: "plan the work".into(),
                },
            },
        )
        .await
        .unwrap();
    let pending = testing::envelope(Target::Agent(owner.agent.clone()), "while you were away");
    fx.bus
        .publish(&subjects::agent(&owner.agent), &pending)
        .await
        .unwrap();

    let mut buf = Vec::new();
    borant_hub::export::export(&fx.hub, &mut buf).await.unwrap();

    let state = MemoryState::new();
    let bus = MemoryBus::new();
    let sessions = MemorySession::new();
    let report = borant_hub::export::import(&state, &bus, &sessions, &mut buf.as_slice())
        .await
        .unwrap();
    assert_eq!(report.sessions, 1);
    assert_eq!(report.pending, 1);
    assert_eq!(
        state.list("").await.unwrap().len(),
        fx.state.list("").await.unwrap().len()
    );
    let got = bus
        .pull(&owner.agent, 10, std::time::Duration::from_millis(100))
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].envelope.id, pending.id);
}
