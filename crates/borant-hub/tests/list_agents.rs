//! The rows of `ToolCall::ListAgents`, which `borant agents` prints: the role,
//! the collection, the pin, the model, and the retirement state of each agent.

mod common;

use borant_core::model::{Agent, Harness};
use borant_core::ports::StateStore;
use borant_core::proto::{CtlRequest, RegisterSession, ToolCall};
use borant_core::{AgentId, SessionId, keys};
use common::{Fx, name};
use serde_json::json;

/// Registers a session that reports a role and a collection, as a person
/// starting a chief does.
async fn register_chief(fx: &Fx) -> AgentId {
    let agent: AgentId = "chief".parse().unwrap();
    fx.req(CtlRequest::RegisterSession(RegisterSession {
        session: SessionId::new(),
        host: name("h1"),
        harness: Harness::Fake,
        pid: Some(1),
        cwd: "/work".into(),
        agent: Some(agent.clone()),
        role: Some("chief".parse().unwrap()),
        collection: Some("personal".parse().unwrap()),
        ..Default::default()
    }))
    .await
    .unwrap();
    agent
}

/// The spawn sets the model of an agent, so the test writes it into the
/// record that the hub keeps.
async fn set_model(fx: &Fx, agent: &AgentId, model: &str) {
    let key = keys::agent(agent);
    let entry = fx.state.get(&key).await.unwrap().expect("the agent record");
    let mut record: Agent = serde_json::from_slice(&entry.value).unwrap();
    record.model = Some(model.to_owned());
    fx.state
        .update(
            &key,
            serde_json::to_vec(&record).unwrap().into(),
            entry.revision,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn list_agents_shows_role_collection_pin_model_and_retired() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let chief = register_chief(&fx).await;
    set_model(&fx, &chief, "opus").await;
    fx.call(
        None,
        ToolCall::Pin {
            agent: chief.clone(),
            on: true,
        },
    )
    .await
    .unwrap();

    let rows = fx.call(None, ToolCall::ListAgents).await.unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent"] == json!(chief))
        .expect("the chief is listed");
    assert_eq!(row["role"], "chief", "{row}");
    assert_eq!(row["collection"], "personal", "{row}");
    assert_eq!(row["pinned"], true, "{row}");
    assert_eq!(row["retired"], false, "{row}");
    assert_eq!(row["model"], "opus", "{row}");
    // The fields of the row that were there before stay.
    assert_eq!(row["harness"], "fake", "{row}");
    assert_eq!(row["host"], "h1", "{row}");
}

/// A field that an agent has not set is null, so a reader can tell "no role"
/// from a role that the row does not carry.
#[tokio::test]
async fn list_agents_leaves_an_unset_field_null() {
    let fx = Fx::new();
    fx.host("h1", &[Harness::Fake]).await;
    let plain = fx.register("h1", Some("plain"), None).await;

    let rows = fx.call(None, ToolCall::ListAgents).await.unwrap();
    let row = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["agent"] == json!(plain.agent))
        .expect("the agent is listed");
    for field in ["role", "collection", "model"] {
        assert!(row[field].is_null(), "{field} is null: {row}");
    }
    for field in ["pinned", "retired"] {
        assert_eq!(row[field], false, "{field}: {row}");
    }
}
