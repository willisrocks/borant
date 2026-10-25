//! The read-only agent prune: which stale agents a prune would remove, and
//! why every other agent stays.
//!
//! The rules are in the phase 0 safety spec. A prune deletes nothing, but the
//! list must still be exact: it names every key that a later prune would
//! remove, and it keeps an agent whose work is not provably done.
//!
//! Inboxes, session logs, and the KV state are read outside the `mem` mutex,
//! because every heartbeat waits for that mutex. A dry run needs no
//! consistent read, so the lock only copies the pending moves.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use borant_core::model::{
    Agent, Channel, Lease, LeaseState, Member, Session, SpawnRecord, SpawnState, Task, TaskStatus,
};
use borant_core::ports::Entry;
use borant_core::proto::ApiError;
use borant_core::rules::session_log;
use borant_core::{AgentId, Timestamp, keys};
use serde_json::{Value, json};

use crate::Hub;

/// The state keys of the agent sessions. `keys` has no prefix for them.
const SESSION_PREFIX: &str = "session.";

/// The keys that a prune of one agent removes, with their sizes in bytes.
struct Removed {
    keys: Vec<String>,
    bytes: usize,
}

/// Lists the candidates for a prune of agents whose lease ended more than
/// `older_than` ago, and the reason that every other agent stays.
pub(crate) async fn prune_agents(hub: &Hub, older_than: Duration) -> Result<Value, ApiError> {
    let now = hub.now();
    // A dry run needs no consistent read, so the lock copies the pending
    // moves only. `NatsState::list` does one get per key, and every
    // heartbeat waits for `mem`.
    let mut pending: HashSet<AgentId> = {
        let mem = hub.mem.lock().await;
        mem.pending_moves.keys().cloned().collect()
    };
    // A queued spawn is pending too; the scan below finds them in the same
    // list.
    let mut entries = Vec::new();
    for prefix in [
        keys::AGENT_PREFIX,
        keys::LEASE_AGENT_PREFIX,
        keys::CHANNEL_PREFIX,
        SESSION_PREFIX,
        keys::SPAWN_PREFIX,
    ] {
        entries.extend(hub.store.0.list(prefix).await?);
    }

    let scan = Scan::read(&entries);
    pending.extend(scan.queued.iter().cloned());
    let mut candidates = Vec::new();
    let mut kept = Vec::new();
    let mut total_keys = 0;
    let mut total_bytes = 0;
    for agent in &scan.agents {
        let id = &agent.id;
        let Some(reason) = kept_reason(hub, &scan, &pending, id, agent, now, older_than).await?
        else {
            let removed = scan.removed(id);
            total_keys += removed.keys.len();
            total_bytes += removed.bytes;
            candidates.push(json!({
                "agent": id,
                "since": scan.lease(id).and_then(lease_since),
                "keys": removed.keys,
                "bytes": removed.bytes,
            }));
            continue;
        };
        kept.push(json!({ "agent": id, "reason": reason }));
    }

    Ok(json!({
        "candidates": candidates,
        "kept": kept,
        "totals": {
            "candidates": candidates.len(),
            "keys": total_keys,
            "bytes": total_bytes,
        },
    }))
}

/// The first rule of the spec that keeps the agent, or `None` for a
/// candidate.
async fn kept_reason(
    hub: &Hub,
    scan: &Scan,
    pending: &HashSet<AgentId>,
    id: &AgentId,
    agent: &Agent,
    now: Timestamp,
    older_than: Duration,
) -> Result<Option<&'static str>, ApiError> {
    if agent.session.is_some() {
        return Ok(Some("session"));
    }
    match scan.lease(id).and_then(lease_since) {
        Some(since) if now.since(*since) >= older_than => {}
        _ => return Ok(Some("lease")),
    }
    if pending.contains(id) {
        return Ok(Some("pending_move"));
    }
    if scan.claimed.contains(id) {
        return Ok(Some("claims"));
    }
    if scan.owners.contains(id) {
        return Ok(Some("owner"));
    }
    if !hub.bus.unacked(id).await?.is_empty() {
        return Ok(Some("unacked"));
    }
    let events = hub
        .sessions
        .read(id)
        .await?
        .into_iter()
        .map(|(_, e)| e)
        .collect::<Vec<_>>();
    if !session_log::undelivered(&events).is_empty() {
        return Ok(Some("undelivered"));
    }
    Ok(None)
}

fn lease_since(l: &Lease) -> Option<&Timestamp> {
    match &l.state {
        LeaseState::Expired { since, .. } | LeaseState::Released { since, .. } => Some(since),
        LeaseState::Held { .. } | LeaseState::Reserved { .. } => None,
    }
}

/// The state that the rules need, read from one list of the store.
#[derive(Debug, Default)]
struct Scan {
    agents: Vec<Agent>,
    leases: HashMap<AgentId, Lease>,
    owners: HashSet<AgentId>,
    claimed: HashSet<AgentId>,
    member_keys: HashMap<AgentId, Vec<String>>,
    session_keys: HashMap<AgentId, Vec<String>>,
    /// The agents whose spawn record is still `Queued`.
    queued: HashSet<AgentId>,
    sizes: HashMap<String, usize>,
}

impl Scan {
    fn read(entries: &[Entry]) -> Self {
        let mut scan = Self::default();
        for e in entries {
            scan.sizes.insert(e.key.clone(), e.value.len());
            if e.key.starts_with(keys::AGENT_PREFIX)
                && let Some(a) = decode::<Agent>(e)
            {
                scan.agents.push(a);
            } else if e.key.starts_with(keys::LEASE_AGENT_PREFIX)
                && let Some(l) = decode::<Lease>(e)
            {
                scan.leases.insert(l.agent.clone(), l);
            } else if e.key.starts_with(keys::CHANNEL_PREFIX) {
                scan.read_channel(e);
            } else if e.key.starts_with(SESSION_PREFIX)
                && let Some(s) = decode::<Session>(e)
            {
                scan.session_keys
                    .entry(s.agent)
                    .or_default()
                    .push(e.key.clone());
            } else if e.key.starts_with(keys::SPAWN_PREFIX)
                && let Some(r) = decode::<SpawnRecord>(e)
                && r.state == SpawnState::Queued
            {
                scan.queued.insert(r.agent);
            }
        }
        scan
    }

    /// A `chan.` key is a channel (`chan.{name}`), a member, or a task.
    fn read_channel(&mut self, e: &Entry) {
        if e.key.contains(".member.") {
            if let Some(m) = decode::<Member>(e) {
                self.member_keys
                    .entry(m.agent)
                    .or_default()
                    .push(e.key.clone());
            }
        } else if e.key.contains(".task.") {
            if let Some(t) = decode::<Task>(e)
                && let Some(c) = t.claimed_by
                && t.status != TaskStatus::Closed
            {
                self.claimed.insert(c);
            }
        } else if e.key.matches('.').count() == 1
            && let Some(c) = decode::<Channel>(e)
        {
            self.owners.insert(c.owner);
        }
    }

    fn lease(&self, id: &AgentId) -> Option<&Lease> {
        self.leases.get(id)
    }

    /// The keys that a prune of `id` removes: the agent, its lease, its
    /// memberships, its session records, and its spawn record.
    fn removed(&self, id: &AgentId) -> Removed {
        let mut keys = vec![keys::agent(id), keys::lease_agent(id)];
        // The record of a spawn, when the agent has one.
        if self.sizes.contains_key(&keys::spawn(id)) {
            keys.push(keys::spawn(id));
        }
        keys.extend(self.member_keys.get(id).into_iter().flatten().cloned());
        keys.extend(self.session_keys.get(id).into_iter().flatten().cloned());
        keys.sort();
        keys.dedup();
        let bytes = keys
            .iter()
            .map(|k| self.sizes.get(k).copied().unwrap_or(0))
            .sum();
        Removed { keys, bytes }
    }
}

fn decode<T: serde::de::DeserializeOwned>(e: &Entry) -> Option<T> {
    match serde_json::from_slice(&e.value) {
        Ok(v) => Some(v),
        Err(err) => {
            tracing::warn!(key = %e.key, "skip bad document: {err}");
            None
        }
    }
}
