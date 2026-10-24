//! Export and import of all hub data for a cold hub move (spec section 16).
//! The export goes through the ports, so it works for any adapter.

use std::io::{BufRead, Write};

use borant_core::model::{Agent, Envelope, SessionEvent, Target};
use borant_core::ports::{MessageBus, SessionStore, StateStore};
use borant_core::{AgentId, keys, subjects};
use serde::{Deserialize, Serialize};

use crate::Hub;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum Record {
    State {
        key: String,
        value: serde_json::Value,
    },
    Session {
        agent: AgentId,
        event: SessionEvent,
    },
    Pending {
        agent: AgentId,
        envelope: Envelope,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub state: usize,
    pub sessions: usize,
    pub pending: usize,
}

/// Writes every state key, every session log, and the unacked messages of
/// every inbox as JSONL. The hub lease is not exported.
pub async fn export(hub: &Hub, out: &mut (dyn Write + Send)) -> anyhow::Result<()> {
    let hub_lease = keys::lease_hub();
    for e in hub.store.0.list("").await? {
        if e.key == hub_lease {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(&e.value)?;
        write(out, &Record::State { key: e.key, value })?;
    }
    for agent in hub.sessions.agents().await? {
        for (_, event) in hub.sessions.read(&agent).await? {
            write(
                out,
                &Record::Session {
                    agent: agent.clone(),
                    event,
                },
            )?;
        }
    }
    for a in hub.store.list::<Agent>(keys::AGENT_PREFIX).await? {
        for envelope in hub.bus.unacked(&a.value.id).await? {
            write(
                out,
                &Record::Pending {
                    agent: a.value.id.clone(),
                    envelope,
                },
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

fn write(out: &mut (dyn Write + Send), r: &Record) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *out, r)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Loads an export into empty stores. Inboxes are created again, and each
/// unacked message is published again. Receivers drop duplicates by ID.
pub async fn import(
    state: &dyn StateStore,
    bus: &dyn MessageBus,
    sessions: &dyn SessionStore,
    input: &mut (dyn BufRead + Send),
) -> anyhow::Result<ImportReport> {
    let mut report = ImportReport::default();
    let mut agents = Vec::new();
    let mut pending = Vec::new();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Record>(&line)? {
            Record::State { key, value } => {
                if key.starts_with(keys::AGENT_PREFIX)
                    && let Ok(a) = serde_json::from_value::<Agent>(value.clone())
                {
                    agents.push(a);
                }
                state
                    .create(&key, serde_json::to_vec(&value)?.into())
                    .await?;
                report.state += 1;
            }
            Record::Session { agent, event } => {
                sessions.append(&agent, &event).await?;
                report.sessions += 1;
            }
            Record::Pending { envelope, .. } => pending.push(envelope),
        }
    }
    for a in &agents {
        bus.ensure_inbox(&a.id, &subjects::inbox(&a.id, &a.channels))
            .await?;
    }
    for env in pending {
        let subject = match (&env.to, env.event.is_some()) {
            (Target::Agent(a), _) => subjects::agent(a),
            (Target::Channel(c), true) => subjects::tasks(c),
            (Target::Channel(c), false) => subjects::broadcast(c),
        };
        bus.publish(&subject, &env).await?;
        report.pending += 1;
    }
    Ok(report)
}
