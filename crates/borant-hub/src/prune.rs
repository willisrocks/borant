//! The agent prune: which stale records a prune removes, why every other
//! agent stays, and the dry run that ships by default.
//!
//! The rules are in the phase 0 safety spec. Retention is section 19 of the
//! design: the session logs of an agent that has had no session for 30 days
//! go, while the agent stays. A dry run deletes nothing, but its report must
//! be exact: it names every key that an apply removes, and it keeps an agent
//! whose work is not provably done.
//!
//! Inboxes, session logs, and the KV state are read outside the `mem` mutex,
//! because every heartbeat waits for that mutex. The caller copies the
//! pending moves while it holds the lock and hands them over.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use borant_core::model::{
    Agent, Channel, Envelope, Lease, LeaseState, Member, Session, SessionEvent, SpawnRecord,
    SpawnState, Task, TaskStatus,
};
use borant_core::ports::Entry;
use borant_core::proto::ApiError;
use borant_core::rules::session_log;
use borant_core::{AgentId, Timestamp, keys};
use serde::Serialize;
use serde_json::Value;

use crate::Hub;
use crate::export::SnapshotMark;

/// The state keys of the agent sessions. `keys` has no prefix for them.
const SESSION_PREFIX: &str = "session.";

/// The most deletions of one kind in a batch, before the prune yields.
const BATCH: usize = 200;

/// The age of the last snapshot that an apply accepts. A prune without a
/// backup at least this fresh is refused.
pub const MAX_BACKUP_AGE: Duration = Duration::from_secs(60 * 60);

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Whether a prune only reports, or deletes what its own dry run listed.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Mode<'a> {
    DryRun,
    /// `token` is the one that the dry run printed. An empty plan takes none:
    /// a second apply deletes nothing.
    Apply {
        token: Option<&'a str>,
    },
}

/// What a prune lists, and what an apply removed.
#[derive(Debug, Serialize)]
pub struct PruneReport {
    /// `dry-run` or `applied`.
    pub mode: &'static str,
    pub older_than_secs: u64,
    pub retention_secs: u64,
    /// The token of this plan. An apply echoes it.
    pub token: String,
    /// The age of the last snapshot, which an apply needs.
    pub backup: Backup,
    pub kinds: Vec<KindReport>,
    pub totals: Totals,
    pub candidates: Vec<Candidate>,
    pub kept: Vec<Kept>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub applied: Option<Applied>,
}

/// The last hub snapshot, which an apply needs to be at most an hour old.
#[derive(Debug, Serialize)]
pub struct Backup {
    pub last: Option<Timestamp>,
    pub age_secs: Option<u64>,
    pub fresh: bool,
    pub path: Option<String>,
}

/// The count of one kind of removed record.
#[derive(Debug, Serialize)]
pub struct KindReport {
    pub kind: &'static str,
    /// The records that go: agents, orphan leases, spawn records, or logs.
    pub items: usize,
    /// The store keys that go.
    pub keys: usize,
    /// The inbox messages that go.
    pub messages: usize,
    /// The session events that go.
    pub events: usize,
    pub bytes: usize,
    pub oldest: Option<Timestamp>,
    pub newest: Option<Timestamp>,
}

#[derive(Debug, Default, Serialize)]
pub struct Totals {
    pub keys: usize,
    pub messages: usize,
    pub events: usize,
    pub bytes: usize,
}

/// One agent that an apply removes, with the keys that go.
#[derive(Debug, Serialize)]
pub struct Candidate {
    pub agent: AgentId,
    pub since: Option<Timestamp>,
    pub keys: Vec<String>,
    pub bytes: usize,
}

/// One agent that stays, with the first rule that keeps it.
#[derive(Debug, Serialize)]
pub struct Kept {
    pub agent: AgentId,
    pub reason: &'static str,
}

/// What an apply deleted.
#[derive(Debug, Default, Serialize)]
pub struct Applied {
    pub keys: usize,
    pub inboxes: usize,
    pub messages: usize,
    pub logs: usize,
    pub events: usize,
    /// The batches that the apply ran, each one at most [`BATCH`] deletions.
    pub batches: usize,
}

/// An inbox that goes, with its undelivered messages.
struct InboxItem {
    agent: AgentId,
    messages: usize,
    bytes: usize,
    oldest: Option<Timestamp>,
    newest: Option<Timestamp>,
}

/// A session log that goes.
struct LogItem {
    agent: AgentId,
    events: usize,
    bytes: usize,
    oldest: Option<Timestamp>,
    newest: Option<Timestamp>,
}

/// What a prune would remove, before it removes anything.
struct Plan {
    candidates: Vec<Candidate>,
    kept: Vec<Kept>,
    /// The store keys that go, with the size of each value.
    keys: Vec<(String, usize)>,
    inboxes: Vec<InboxItem>,
    logs: Vec<LogItem>,
    kinds: Vec<KindReport>,
    totals: Totals,
    token: String,
}

/// Runs a prune: a dry run that lists the records, or an apply that deletes
/// them, in batches, after the token and the backup check.
pub(crate) async fn prune(
    hub: &Hub,
    moving: HashSet<AgentId>,
    older_than: Duration,
    retention: Duration,
    mode: Mode<'_>,
) -> Result<Value, ApiError> {
    let now = hub.now();
    let plan = plan(hub, &moving, older_than, retention).await?;
    let backup = backup(hub, now).await?;
    let empty = plan.keys.is_empty() && plan.inboxes.is_empty() && plan.logs.is_empty();

    let applied = match mode {
        Mode::DryRun => None,
        Mode::Apply { token } => {
            // An empty plan deletes nothing, so a retried apply is harmless
            // and needs no token.
            if !empty {
                let Some(token) = token else {
                    return Err(ApiError::invalid(
                        "an apply needs the token of a dry run; run `borant hub prune`",
                    ));
                };
                if token != plan.token {
                    return Err(ApiError::invalid(
                        "the plan changed since the dry run; run `borant hub prune` again for the token",
                    ));
                }
                if !backup.fresh {
                    return Err(ApiError::invalid(format!(
                        "refusing to apply: {}; run `borant hub export --snapshot --out <file>`",
                        backup_why(&backup)
                    )));
                }
            }
            Some(execute(hub, &plan).await?)
        }
    };

    let mode_name = if applied.is_some() {
        "applied"
    } else {
        "dry-run"
    };
    let report = PruneReport {
        mode: mode_name,
        older_than_secs: older_than.as_secs(),
        retention_secs: retention.as_secs(),
        token: plan.token.clone(),
        backup,
        kinds: plan.kinds,
        totals: plan.totals,
        candidates: plan.candidates,
        kept: plan.kept,
        applied,
    };
    match &report.applied {
        Some(a) => tracing::info!(
            mode = report.mode,
            keys = a.keys,
            inboxes = a.inboxes,
            messages = a.messages,
            logs = a.logs,
            events = a.events,
            batches = a.batches,
            "prune applied"
        ),
        None => tracing::info!(
            mode = report.mode,
            report = %serde_json::to_string(&report).unwrap_or_default(),
            "prune dry run"
        ),
    }
    serde_json::to_value(&report).map_err(|e| ApiError::internal(e.to_string()))
}

/// The age of the last snapshot, from the mark that a snapshot writes.
async fn backup(hub: &Hub, now: Timestamp) -> Result<Backup, ApiError> {
    let Some(mark) = hub.store.get::<SnapshotMark>(keys::SNAPSHOT).await? else {
        return Ok(Backup {
            last: None,
            age_secs: None,
            fresh: false,
            path: None,
        });
    };
    let age = now.since(mark.value.at);
    Ok(Backup {
        last: Some(mark.value.at),
        age_secs: Some(age.as_secs()),
        fresh: age <= MAX_BACKUP_AGE,
        path: Some(mark.value.path),
    })
}

fn backup_why(b: &Backup) -> String {
    match (b.last, b.age_secs) {
        (Some(_), Some(age_secs)) => format!(
            "the last hub backup is {age_secs}s old and an apply needs one at most {}s old",
            MAX_BACKUP_AGE.as_secs()
        ),
        _ => "the hub has no snapshot on record, and an apply needs one at most an hour old".into(),
    }
}

/// Deletes what the plan lists, in batches of at most [`BATCH`], yielding
/// between batches. The inboxes and the logs go first: a key that is gone
/// cannot lead a later prune to the inbox and the log that it orphaned.
async fn execute(hub: &Hub, plan: &Plan) -> Result<Applied, ApiError> {
    let mut applied = Applied::default();
    for chunk in plan.inboxes.chunks(BATCH) {
        for item in chunk {
            hub.bus.delete_inbox(&item.agent).await?;
            applied.inboxes += 1;
            applied.messages += item.messages;
        }
        applied.batches += 1;
        tokio::task::yield_now().await;
    }
    for chunk in plan.logs.chunks(BATCH) {
        for item in chunk {
            hub.sessions.delete(&item.agent).await?;
            applied.logs += 1;
            applied.events += item.events;
        }
        applied.batches += 1;
        tokio::task::yield_now().await;
    }
    for chunk in plan.keys.chunks(BATCH) {
        for (key, _) in chunk {
            hub.store.delete(key).await?;
            applied.keys += 1;
        }
        applied.batches += 1;
        tokio::task::yield_now().await;
    }
    Ok(applied)
}

/// Lists the candidates for a prune of agents whose lease ended more than
/// `older_than` ago, the records that no live agent claims, and the logs
/// beyond `retention`.
async fn plan(
    hub: &Hub,
    moving: &HashSet<AgentId>,
    older_than: Duration,
    retention: Duration,
) -> Result<Plan, ApiError> {
    let now = hub.now();
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
    // A queued spawn is pending too; the scan finds them in the same list.
    let mut pending = moving.clone();
    pending.extend(scan.queued.iter().cloned());

    let mut plan = Plan {
        candidates: Vec::new(),
        kept: Vec::new(),
        keys: Vec::new(),
        inboxes: Vec::new(),
        logs: Vec::new(),
        kinds: Vec::new(),
        totals: Totals::default(),
        token: String::new(),
    };
    let mut agents = KindSpan::default();
    let mut agent_keys = 0;
    let mut agent_bytes = 0;

    for agent in &scan.agents {
        let id = &agent.id;
        // One read of the inbox and the log per agent: the rules below need
        // both, and a second read would double the store traffic.
        let unacked = hub.bus.unacked(id).await?;
        let events: Vec<SessionEvent> = hub
            .sessions
            .read(id)
            .await?
            .into_iter()
            .map(|(_, e)| e)
            .collect();
        let undelivered = session_log::undelivered(&events);
        // A message older than the retention is past the age limit of the
        // message streams (design section 19), so it no longer keeps an agent
        // or its log.
        let fresh = |e: &Envelope| now.since(e.sent) < retention;
        let fresh_unacked = unacked.iter().any(fresh);
        let fresh_undelivered = undelivered.iter().any(fresh);
        let reason = kept_reason(
            &scan,
            &pending,
            id,
            agent,
            now,
            older_than,
            fresh_unacked,
            fresh_undelivered,
        );
        let since = scan.lease(id).and_then(lease_since).copied();
        let Some(reason) = reason else {
            let removed = scan.removed(id);
            agent_keys += removed.keys.len();
            agent_bytes += removed.bytes;
            for key in &removed.keys {
                plan.keys.push((key.clone(), scan.size(key)));
            }
            agents.see(since);
            let bytes = envelope_bytes(&unacked);
            plan.inboxes.push(InboxItem {
                agent: id.clone(),
                messages: unacked.len(),
                bytes,
                oldest: sent(&unacked).0,
                newest: sent(&unacked).1,
            });
            if !events.is_empty() {
                plan.logs.push(log_item(id, &events));
            }
            plan.candidates.push(Candidate {
                agent: id.clone(),
                since,
                keys: removed.keys,
                bytes: removed.bytes,
            });
            continue;
        };
        // Retention: an agent that stays keeps no log once its session is
        // older than `retention` and nothing in the log is undelivered
        // (spec section 19).
        if agent.session.is_none()
            && !events.is_empty()
            && !fresh_unacked
            && !fresh_undelivered
            && since.is_some_and(|s| now.since(s) >= retention)
        {
            plan.logs.push(log_item(id, &events));
        }
        plan.kept.push(Kept {
            agent: id.clone(),
            reason,
        });
    }
    plan.kinds.push(KindReport {
        kind: "agents",
        items: plan.candidates.len(),
        keys: agent_keys,
        messages: 0,
        events: 0,
        bytes: agent_bytes,
        oldest: agents.oldest,
        newest: agents.newest,
    });

    // A released lease whose agent record is already gone: the agent half
    // of an interrupted prune.
    let mut leases = KindSpan::default();
    let mut lease_keys = 0;
    let mut lease_bytes = 0;
    for (id, lease) in &scan.leases {
        if scan.agent(id).is_some() {
            continue;
        }
        let Some(since) = lease_since(lease) else {
            // A held lease with no agent record may still have a session, so
            // it waits rather than risk a takeover.
            continue;
        };
        let key = keys::lease_agent(id);
        lease_keys += 1;
        lease_bytes += scan.size(&key);
        leases.see(Some(*since));
        plan.keys.push((key.clone(), scan.size(&key)));
    }
    plan.kinds.push(KindReport {
        kind: "leases",
        items: lease_keys,
        keys: lease_keys,
        messages: 0,
        events: 0,
        bytes: lease_bytes,
        oldest: leases.oldest,
        newest: leases.newest,
    });

    // A spawn record of an agent that has no record and that ended more than
    // `older_than` ago. A queued, launching, or running spawn never goes.
    let mut spawns = KindSpan::default();
    let mut spawn_keys = 0;
    let mut spawn_bytes = 0;
    for (id, record) in &scan.spawns {
        if scan.agent(id).is_some()
            || matches!(
                record.state,
                SpawnState::Queued | SpawnState::Launching | SpawnState::Running
            )
            || now.since(record.queued_at) < older_than
        {
            continue;
        }
        let key = keys::spawn(id);
        spawn_keys += 1;
        spawn_bytes += scan.size(&key);
        spawns.see(Some(record.queued_at));
        plan.keys.push((key.clone(), scan.size(&key)));
    }
    plan.kinds.push(KindReport {
        kind: "spawns",
        items: spawn_keys,
        keys: spawn_keys,
        messages: 0,
        events: 0,
        bytes: spawn_bytes,
        oldest: spawns.oldest,
        newest: spawns.newest,
    });

    // The inboxes and the logs are separate kinds: a key count alone hides
    // the 62,180 undelivered messages that a prune of 740 agents frees.
    let mut inboxes = KindSpan::default();
    let mut messages = 0;
    let mut inbox_bytes = 0;
    for item in &plan.inboxes {
        messages += item.messages;
        inbox_bytes += item.bytes;
        inboxes.see(item.oldest);
        inboxes.see(item.newest);
    }
    plan.kinds.push(KindReport {
        kind: "inboxes",
        items: plan.inboxes.len(),
        keys: 0,
        messages,
        events: 0,
        bytes: inbox_bytes,
        oldest: inboxes.oldest,
        newest: inboxes.newest,
    });

    let mut logs = KindSpan::default();
    let mut log_events = 0;
    let mut log_bytes = 0;
    for item in &plan.logs {
        log_events += item.events;
        log_bytes += item.bytes;
        logs.see(item.oldest);
        logs.see(item.newest);
    }
    plan.kinds.push(KindReport {
        kind: "session_events",
        items: plan.logs.len(),
        keys: 0,
        messages: 0,
        events: log_events,
        bytes: log_bytes,
        oldest: logs.oldest,
        newest: logs.newest,
    });

    for kind in &plan.kinds {
        plan.totals.keys += kind.keys;
        plan.totals.messages += kind.messages;
        plan.totals.events += kind.events;
        plan.totals.bytes += kind.bytes;
    }
    plan.token = plan_token(older_than, retention, &plan.keys, &plan.logs, &plan.inboxes);
    Ok(plan)
}

/// Widens a first and last span with one timestamp. `None` means "no item
/// yet", so it must not compare as the earliest.
fn span(oldest: &mut Option<Timestamp>, newest: &mut Option<Timestamp>, at: Timestamp) {
    *oldest = Some(oldest.map_or(at, |o| o.min(at)));
    *newest = Some(newest.map_or(at, |n| n.max(at)));
}

fn log_item(id: &AgentId, events: &[SessionEvent]) -> LogItem {
    let mut bytes = 0;
    let mut oldest: Option<Timestamp> = None;
    let mut newest: Option<Timestamp> = None;
    for e in events {
        bytes += serde_json::to_vec(e).map(|v| v.len()).unwrap_or(0);
        span(&mut oldest, &mut newest, e.at);
    }
    LogItem {
        agent: id.clone(),
        events: events.len(),
        bytes,
        oldest,
        newest,
    }
}

fn envelope_bytes(envelopes: &[Envelope]) -> usize {
    envelopes
        .iter()
        .map(|e| serde_json::to_vec(e).map(|v| v.len()).unwrap_or(0))
        .sum()
}

fn sent(envelopes: &[Envelope]) -> (Option<Timestamp>, Option<Timestamp>) {
    let mut oldest: Option<Timestamp> = None;
    let mut newest: Option<Timestamp> = None;
    for e in envelopes {
        span(&mut oldest, &mut newest, e.sent);
    }
    (oldest, newest)
}

/// The first and last item of a kind, for the report.
#[derive(Debug, Default)]
struct KindSpan {
    oldest: Option<Timestamp>,
    newest: Option<Timestamp>,
}

impl KindSpan {
    fn see(&mut self, at: Option<Timestamp>) {
        if let Some(at) = at {
            span(&mut self.oldest, &mut self.newest, at);
        }
    }
}

/// A token over the plan, so an apply cannot delete a list that the caller
/// never read. FNV-1a over the sorted records: stable across builds.
fn plan_token(
    older_than: Duration,
    retention: Duration,
    keys: &[(String, usize)],
    logs: &[LogItem],
    inboxes: &[InboxItem],
) -> String {
    let mut all: Vec<String> = keys.iter().map(|(k, _)| k.clone()).collect();
    all.extend(logs.iter().map(|i| format!("log.{}", i.agent)));
    all.extend(inboxes.iter().map(|i| format!("inbox.{}", i.agent)));
    all.sort_unstable();
    let mut hash = FNV_OFFSET;
    let params = [
        older_than.as_secs().to_string(),
        retention.as_secs().to_string(),
    ];
    for part in params.iter().chain(all.iter()) {
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

/// The first rule of the spec that keeps the agent, or `None` for a
/// candidate.
#[allow(clippy::too_many_arguments)]
fn kept_reason(
    scan: &Scan,
    pending: &HashSet<AgentId>,
    id: &AgentId,
    agent: &Agent,
    now: Timestamp,
    older_than: Duration,
    // True when an unacked message is inside the retention.
    fresh_unacked: bool,
    // True when an undelivered log message is inside the retention.
    fresh_undelivered: bool,
) -> Option<&'static str> {
    if agent.session.is_some() {
        return Some("session");
    }
    let lease = scan.lease(id);
    if matches!(
        lease.map(|l| &l.state),
        Some(LeaseState::Released { parked: true, .. })
    ) {
        return Some("parked");
    }
    if agent.paused.is_some() {
        return Some("paused");
    }
    match lease.and_then(lease_since) {
        Some(since) if now.since(*since) >= older_than => {}
        _ => return Some("lease"),
    }
    if let Some(task) = &agent.task
        && scan.open_tasks.contains(&task.id)
    {
        return Some("task");
    }
    if pending.contains(id) {
        return Some("pending_move");
    }
    if scan.claimed.contains(id) {
        return Some("claims");
    }
    if scan.owners.contains(id) {
        return Some("owner");
    }
    if fresh_unacked {
        return Some("unacked");
    }
    if fresh_undelivered {
        return Some("undelivered");
    }
    None
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
    /// Every spawn record, by agent, so an ended one can go on its own.
    spawns: HashMap<AgentId, SpawnRecord>,
    /// The agents whose spawn record is still `Queued`.
    queued: HashSet<AgentId>,
    /// The tasks that are not closed, so a bound agent keeps its record.
    open_tasks: HashSet<borant_core::TaskId>,
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
            {
                if r.state == SpawnState::Queued {
                    scan.queued.insert(r.agent.clone());
                }
                scan.spawns.insert(r.agent.clone(), r);
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
                && t.status != TaskStatus::Closed
            {
                self.open_tasks.insert(t.id);
                if let Some(c) = t.claimed_by {
                    self.claimed.insert(c);
                }
            }
        } else if e.key.matches('.').count() == 1
            && let Some(c) = decode::<Channel>(e)
            && let Some(owner) = c.owner
        {
            self.owners.insert(owner);
        }
    }

    fn agent(&self, id: &AgentId) -> Option<&Agent> {
        self.agents.iter().find(|a| &a.id == id)
    }

    fn lease(&self, id: &AgentId) -> Option<&Lease> {
        self.leases.get(id)
    }

    fn size(&self, key: &str) -> usize {
        self.sizes.get(key).copied().unwrap_or(0)
    }

    /// The keys that a prune of `id` removes: the agent, its lease, its
    /// memberships, its session records, and its spawn record.
    fn removed(&self, id: &AgentId) -> Removed {
        let mut keys = vec![keys::agent(id), keys::lease_agent(id)];
        if self.spawns.contains_key(id) {
            keys.push(keys::spawn(id));
        }
        keys.extend(self.member_keys.get(id).into_iter().flatten().cloned());
        keys.extend(self.session_keys.get(id).into_iter().flatten().cloned());
        keys.sort();
        keys.dedup();
        let bytes = keys.iter().map(|k| self.size(k)).sum();
        Removed { keys, bytes }
    }
}

/// The keys that a prune of one agent removes, with their sizes in bytes.
struct Removed {
    keys: Vec<String>,
    bytes: usize,
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
