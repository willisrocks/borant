//! Export and import of all hub data for a cold hub move (spec section 16),
//! and the snapshot of a running hub.
//! The export goes through the ports, so it works for any adapter.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use borant_core::model::{Agent, Envelope, SessionEvent};
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

/// What a snapshot wrote, and how long the hub held its request mutex.
#[derive(Debug, Serialize)]
pub struct SnapshotReport {
    pub path: PathBuf,
    pub state: usize,
    pub sessions: usize,
    pub pending: usize,
    pub lock_ms: u64,
}

/// Writes a snapshot of the running hub to `out` without maintenance mode.
///
/// The read order is the one of the spec: the agent list, the inboxes, the KV
/// state under the request mutex, then the session logs. A message that was in
/// an inbox at the snapshot point is then in the inbox copy, the session-log
/// copy, or both, and import drops duplicates by ID.
pub async fn snapshot(hub: &Hub, out: &Path) -> anyhow::Result<SnapshotReport> {
    let tmp = tmp_path(out);
    match write_snapshot(hub, out, &tmp).await {
        Ok(report) => Ok(report),
        Err(e) => {
            // A half file on the hub box looks like a good backup.
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn tmp_path(out: &Path) -> PathBuf {
    let mut tmp = out.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

async fn write_snapshot(hub: &Hub, out: &Path, tmp: &Path) -> anyhow::Result<SnapshotReport> {
    let file = std::fs::File::create(tmp)?;
    let mut w = std::io::BufWriter::new(file);
    let mut report = SnapshotReport {
        path: out.to_path_buf(),
        state: 0,
        sessions: 0,
        pending: 0,
        lock_ms: 0,
    };

    // 1. The agents, without the request mutex.
    let agents = hub.store.list::<Agent>(keys::AGENT_PREFIX).await?;

    // 2. The inboxes, before the session logs.
    for a in &agents {
        for envelope in hub.bus.unacked(&a.value.id).await? {
            write(
                &mut w,
                &Record::Pending {
                    agent: a.value.id.clone(),
                    envelope,
                },
            )?;
            report.pending += 1;
        }
    }

    // 3. All keys while the hub holds its request mutex. Release it after
    // this step, before the session logs and before the file I/O.
    let (entries, lock_ms) = {
        let _mem = hub.mem.lock().await;
        let started = Instant::now();
        let entries = hub.store.0.list("").await?;
        (entries, started.elapsed())
    };
    report.lock_ms = u64::try_from(lock_ms.as_millis()).unwrap_or(u64::MAX);
    let hub_lease = keys::lease_hub();
    let mut kv_agents = Vec::new();
    for e in entries {
        if e.key == hub_lease {
            continue;
        }
        if e.key.starts_with(keys::AGENT_PREFIX)
            && let Ok(a) = serde_json::from_slice::<Agent>(&e.value)
        {
            kv_agents.push(a.id);
        }
        let value: serde_json::Value = serde_json::from_slice(&e.value)?;
        write(&mut w, &Record::State { key: e.key, value })?;
        report.state += 1;
    }

    // 4. The session logs of the agents in the KV state.
    for agent in kv_agents {
        for (_, event) in hub.sessions.read(&agent).await? {
            write(
                &mut w,
                &Record::Session {
                    agent: agent.clone(),
                    event,
                },
            )?;
            report.sessions += 1;
        }
    }

    w.flush()?;
    w.get_ref().sync_all()?;
    drop(w);
    std::fs::rename(tmp, out)?;
    Ok(report)
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
            Record::Pending { agent, envelope } => pending.push((agent, envelope)),
        }
    }
    for a in &agents {
        bus.ensure_inbox(&a.id, &subjects::inbox(&a.id, &a.channels))
            .await?;
    }
    // A pending record goes back to the agent that it was pending for. A
    // broadcast would reach every restored member of the channel again.
    for (agent, env) in pending {
        bus.publish(&subjects::agent(&agent), &env).await?;
        report.pending += 1;
    }
    Ok(report)
}
