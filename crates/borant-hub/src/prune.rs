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
    SpawnState, Target, Task, TaskStatus,
};
use borant_core::ports::Entry;
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::session_log;
use borant_core::{AgentId, Timestamp, keys};
use serde::Serialize;
use serde_json::Value;

use crate::Hub;
use crate::export::SnapshotMark;

/// The state keys of the agent sessions. `keys` has no prefix for them.
const SESSION_PREFIX: &str = "session.";

/// The keys `session.started.{agent}` share that prefix but hold the start time
/// of a session, an integer, not a `Session`. The scan skips them.
const SESSION_STARTED_PREFIX: &str = "session.started.";

/// The most deletions in a batch, before the prune yields.
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
    /// What the apply reports for this agent's inbox and log. Not in the
    /// report: the kinds already count them.
    #[serde(skip)]
    pub messages: usize,
    #[serde(skip)]
    pub events: usize,
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
    /// The candidates that came back between the plan and the delete, so the
    /// apply left them alone.
    pub skipped: usize,
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
    /// The keys of the `leases` and `spawns` kinds: no agent owns them, so an
    /// apply deletes them without a race to lose.
    orphans: Vec<String>,
    inboxes: Vec<InboxItem>,
    /// The logs of the pruned agents.
    logs: Vec<LogItem>,
    /// The logs of agents that stay, past the retention.
    idle_logs: Vec<LogItem>,
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
    let empty = plan.keys.is_empty()
        && plan.inboxes.is_empty()
        && plan.logs.is_empty()
        && plan.idle_logs.is_empty();

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
            skipped = a.skipped,
            batches = a.batches,
            "prune applied"
        ),
        None => tracing::info!(
            mode = report.mode,
            older_than_secs = report.older_than_secs,
            retention_secs = report.retention_secs,
            token = %report.token,
            backup_fresh = report.backup.fresh,
            backup_age_secs = report.backup.age_secs,
            // The report is the reply; the log holds what a reader of a log
            // needs without the key list of every candidate.
            kinds = %serde_json::to_string(&report.kinds).unwrap_or_default(),
            totals = %serde_json::to_string(&report.totals).unwrap_or_default(),
            candidates = report.candidates.len(),
            kept = report.kept.len(),
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
/// between batches.
///
/// The plan is a few seconds old on a hub of 740 agents, and an agent can
/// wake, take a lease, or pause in that time. So each candidate is checked
/// again, and its agent key goes first with the revision that this check
/// read: an agent that changed wins the race, and the apply leaves it alone.
/// The agent key first also means a crash cannot orphan the inbox and the log
/// of an agent that the record no longer names.
async fn execute(hub: &Hub, plan: &Plan) -> Result<Applied, ApiError> {
    let mut applied = Applied::default();
    let mut batch = Batcher::default();
    for c in &plan.candidates {
        let agent_key = keys::agent(&c.agent);
        let Some(agent) = hub.store.get::<Agent>(&agent_key).await? else {
            // The record is already gone: its keys are leftovers.
            for key in &c.keys {
                hub.store.delete(key).await?;
                batch.key(&mut applied).await;
            }
            continue;
        };
        if !prunable_now(hub, &agent.value).await? {
            applied.skipped += 1;
            tracing::debug!(agent = %c.agent, "prune skipped an agent that came back");
            continue;
        }
        match hub.store.delete_at(&agent_key, agent.revision).await {
            Ok(()) => {}
            Err(e) if e.code == ApiErrorCode::Conflict => {
                applied.skipped += 1;
                tracing::debug!(agent = %c.agent, "prune lost the race for an agent");
                continue;
            }
            Err(e) => return Err(e),
        }
        batch.key(&mut applied).await;
        hub.bus.delete_inbox(&c.agent).await?;
        applied.inboxes += 1;
        applied.messages += c.messages;
        batch.done(&mut applied).await;
        if c.events > 0 {
            hub.sessions.delete(&c.agent).await?;
            applied.logs += 1;
            applied.events += c.events;
            batch.done(&mut applied).await;
        }
        for key in c.keys.iter().filter(|k| **k != agent_key) {
            hub.store.delete(key).await?;
            batch.key(&mut applied).await;
        }
    }
    for key in &plan.orphans {
        hub.store.delete(key).await?;
        batch.key(&mut applied).await;
    }
    // The logs of agents that stay: a wake in the window between the plan and
    // this pass keeps its log, because the agent has a session again or a
    // message waits for it.
    for item in &plan.idle_logs {
        let Some(agent) = hub.store.get::<Agent>(&keys::agent(&item.agent)).await? else {
            continue;
        };
        if agent.value.session.is_some() || !hub.bus.unacked(&item.agent).await?.is_empty() {
            applied.skipped += 1;
            tracing::debug!(agent = %item.agent, "prune kept a log that came back");
            continue;
        }
        hub.sessions.delete(&item.agent).await?;
        applied.logs += 1;
        applied.events += item.events;
        batch.done(&mut applied).await;
    }
    batch.finish(&mut applied);
    Ok(applied)
}

/// Counts deletions and yields once one batch of [`BATCH`] is done, so a big
/// prune never runs a long stretch without giving the reactor a turn.
#[derive(Debug, Default)]
struct Batcher {
    in_batch: usize,
}

impl Batcher {
    /// One key that the apply deleted.
    async fn key(&mut self, applied: &mut Applied) {
        applied.keys += 1;
        self.done(applied).await;
    }

    /// One deletion of any kind: a key, an inbox, or a log.
    async fn done(&mut self, applied: &mut Applied) {
        self.in_batch += 1;
        if self.in_batch == BATCH {
            applied.batches += 1;
            self.in_batch = 0;
            tokio::task::yield_now().await;
        }
    }

    fn finish(&self, applied: &mut Applied) {
        if self.in_batch > 0 {
            applied.batches += 1;
        }
    }
}

/// True when the agent that the plan saw is still one that a prune may
/// delete: no session, not paused, and its lease still ended.
async fn prunable_now(hub: &Hub, agent: &Agent) -> Result<bool, ApiError> {
    if agent.session.is_some() || agent.paused.is_some() {
        return Ok(false);
    }
    let Some(lease) = hub
        .store
        .get::<Lease>(&keys::lease_agent(&agent.id))
        .await?
    else {
        return Ok(true);
    };
    Ok(matches!(
        &lease.value.state,
        LeaseState::Expired { .. } | LeaseState::Released { parked: false, .. }
    ))
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
        orphans: Vec::new(),
        inboxes: Vec::new(),
        logs: Vec::new(),
        idle_logs: Vec::new(),
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
        // An ended agent stays a member of its channels, so every broadcast
        // lands in its inbox. Only a direct or urgent message is a reason to
        // keep it; a broadcast alone is not.
        let fresh_direct = unacked
            .iter()
            .any(|e| fresh(e) && (e.urgent || matches!(e.to, Target::Agent(_))));
        let fresh_undelivered = undelivered.iter().any(fresh);
        let reason = kept_reason(
            &scan,
            &pending,
            id,
            agent,
            now,
            older_than,
            fresh_direct,
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
                messages: unacked.len(),
                events: events.len(),
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
            plan.idle_logs.push(log_item(id, &events));
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
        plan.orphans.push(key.clone());
        plan.keys.push((key, scan.size(&keys::lease_agent(id))));
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
        plan.orphans.push(key.clone());
        plan.keys.push((key, scan.size(&keys::spawn(id))));
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
    for item in plan.logs.iter().chain(plan.idle_logs.iter()) {
        log_events += item.events;
        log_bytes += item.bytes;
        logs.see(item.oldest);
        logs.see(item.newest);
    }
    plan.kinds.push(KindReport {
        kind: "session_events",
        items: plan.logs.len() + plan.idle_logs.len(),
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
    plan.token = plan_token(
        older_than,
        retention,
        &plan.keys,
        &plan.logs,
        &plan.idle_logs,
        &plan.inboxes,
    );
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
    idle_logs: &[LogItem],
    inboxes: &[InboxItem],
) -> String {
    let mut all: Vec<String> = keys.iter().map(|(k, _)| k.clone()).collect();
    all.extend(logs.iter().map(|i| format!("log.{}", i.agent)));
    all.extend(idle_logs.iter().map(|i| format!("idle.{}", i.agent)));
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
    // True when an unacked direct or urgent message is inside the retention.
    // A channel broadcast alone does not count: it reaches every member.
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
                && !e.key.starts_with(SESSION_STARTED_PREFIX)
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

#[cfg(test)]
mod tests {
    //! The apply checks each candidate again before it deletes it, because
    //! the plan is a few seconds old on a hub of 740 agents.

    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    use borant_core::model::{
        Agent, Harness, Lease, LeaseState, PauseReason, Sender, SessionEvent, SessionEventBody,
    };
    use borant_core::model::{Envelope, EnvelopeKind, Target};
    use borant_core::ports::StateStore;
    use borant_core::{
        AgentId, ChannelName, EventId, FencingToken, ManualClock, MessageId, SessionId, Timestamp,
        keys, keys::subjects,
    };
    use borant_memory::{MemoryBus, MemorySession, MemoryState};

    use crate::{Hub, HubConfig};

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    /// Collects what `tracing` logs on this thread.
    #[derive(Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    #[test]
    fn a_session_started_key_is_skipped_without_a_warning() {
        let logs = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .finish();
        let entry = |key: &str, value: &str| borant_core::ports::Entry {
            key: key.to_owned(),
            value: value.as_bytes().to_vec().into(),
            revision: 1,
        };
        let scan = tracing::subscriber::with_default(subscriber, || {
            super::Scan::read(&[
                entry("session.started.worker", "1700000000000"),
                entry("session.started.other", "42"),
            ])
        });
        assert!(scan.session_keys.is_empty());
        let out = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(out.is_empty(), "unexpected log output: {out}");
    }

    fn hub() -> (Arc<Hub>, MemoryState) {
        let state = MemoryState::new();
        let hub = Arc::new(Hub::new(
            Arc::new(state.clone()),
            Arc::new(MemoryBus::new()),
            Arc::new(MemorySession::new()),
            Arc::new(ManualClock::new(Timestamp(60 * DAY.as_millis() as i64))),
            HubConfig::default(),
        ));
        (hub, state)
    }

    /// An ended agent (lease expired long ago) with a fresh inbox message:
    /// `to`/`urgent` shape the message, `paused`/`parked` the agent.
    async fn plan_with_message(
        to: Option<Target>,
        urgent: bool,
        paused: bool,
        parked: bool,
    ) -> Option<&'static str> {
        let (hub, state) = hub();
        let id: AgentId = "w".parse().unwrap();
        let chan: ChannelName = "api".parse().unwrap();
        let agent = Agent {
            id: id.clone(),
            session: None,
            paused: paused.then(|| PauseReason {
                by: Sender::Human,
                why: "cap".into(),
                at: Timestamp(1),
            }),
            ..Default::default()
        };
        state
            .create(
                &keys::agent(&id),
                serde_json::to_vec(&agent).unwrap().into(),
            )
            .await
            .unwrap();
        let since = Timestamp(1);
        let lease_state = if parked {
            LeaseState::Released {
                since,
                parked: true,
            }
        } else {
            LeaseState::Expired {
                since,
                last_session: None,
            }
        };
        state
            .create(
                &keys::lease_agent(&id),
                serde_json::to_vec(&Lease {
                    agent: id.clone(),
                    token: FencingToken(1),
                    state: lease_state,
                })
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        hub.bus
            .ensure_inbox(&id, &subjects::inbox(&id, std::slice::from_ref(&chan)))
            .await
            .unwrap();
        if let Some(to) = to {
            let subject = match &to {
                Target::Agent(a) => subjects::agent(a),
                Target::Channel(c) => subjects::broadcast(c),
            };
            let kind = match &to {
                Target::Agent(_) => EnvelopeKind::Direct,
                Target::Channel(_) => EnvelopeKind::Broadcast,
            };
            hub.bus
                .publish(
                    &subject,
                    &Envelope {
                        id: MessageId::new(),
                        from: Sender::Human,
                        to,
                        kind,
                        text: "hi".into(),
                        urgent,
                        task: None,
                        event: None,
                        // Inside the retention of 30 days.
                        sent: Timestamp(59 * DAY.as_millis() as i64),
                    },
                )
                .await
                .unwrap();
        }
        let plan = super::plan(&hub, &HashSet::new(), DAY, DAY * 30)
            .await
            .unwrap();
        plan.kept.first().map(|k| k.reason)
    }

    #[tokio::test]
    async fn an_ended_agent_with_only_fresh_broadcasts_is_a_candidate() {
        let chan: ChannelName = "api".parse().unwrap();
        let r = plan_with_message(Some(Target::Channel(chan)), false, false, false).await;
        assert_eq!(r, None);
    }

    #[tokio::test]
    async fn an_ended_agent_with_a_fresh_direct_message_is_kept() {
        let id: AgentId = "w".parse().unwrap();
        let r = plan_with_message(Some(Target::Agent(id)), false, false, false).await;
        assert_eq!(r, Some("unacked"));
    }

    #[tokio::test]
    async fn an_ended_agent_with_a_fresh_urgent_broadcast_is_kept() {
        let chan: ChannelName = "api".parse().unwrap();
        let r = plan_with_message(Some(Target::Channel(chan)), true, false, false).await;
        assert_eq!(r, Some("unacked"));
    }

    #[tokio::test]
    async fn parked_and_paused_agents_stay_whatever_the_inbox_holds() {
        let chan: ChannelName = "api".parse().unwrap();
        let b = || Some(Target::Channel(chan.clone()));
        assert_eq!(
            plan_with_message(b(), false, false, true).await,
            Some("parked")
        );
        assert_eq!(
            plan_with_message(b(), false, true, false).await,
            Some("paused")
        );
    }

    #[tokio::test]
    async fn an_agent_that_wakes_after_the_plan_wins_the_race() {
        let (hub, state) = hub();
        let id: AgentId = "w".parse().unwrap();
        let agent = Agent {
            id: id.clone(),
            session: None,
            ..Default::default()
        };
        let agent_rev = state
            .create(
                &keys::agent(&id),
                serde_json::to_vec(&agent).unwrap().into(),
            )
            .await
            .unwrap();
        state
            .create(
                &keys::lease_agent(&id),
                serde_json::to_vec(&Lease {
                    agent: id.clone(),
                    token: FencingToken(1),
                    state: LeaseState::Expired {
                        since: Timestamp(1),
                        last_session: None,
                    },
                })
                .unwrap()
                .into(),
            )
            .await
            .unwrap();

        let plan = super::plan(&hub, &HashSet::new(), DAY, DAY * 30)
            .await
            .unwrap();
        assert_eq!(plan.candidates.len(), 1);

        // The agent wakes before the apply reaches it: its session registers.
        let mut awake = agent;
        awake.session = Some(SessionId::new());
        state
            .update(
                &keys::agent(&id),
                serde_json::to_vec(&awake).unwrap().into(),
                agent_rev,
            )
            .await
            .unwrap();

        let applied = super::execute(&hub, &plan).await.unwrap();
        assert_eq!(applied.skipped, 1);
        assert_eq!(applied.keys, 0);
        assert!(state.get(&keys::agent(&id)).await.unwrap().is_some());
        assert!(state.get(&keys::lease_agent(&id)).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_log_that_the_agent_woke_for_is_skipped() {
        let (hub, state) = hub();
        let id: AgentId = "idle".parse().unwrap();
        let agent = Agent {
            id: id.clone(),
            session: None,
            paused: Some(PauseReason {
                by: Sender::Human,
                why: "cap".into(),
                at: Timestamp(1),
            }),
            ..Default::default()
        };
        let agent_rev = state
            .create(
                &keys::agent(&id),
                serde_json::to_vec(&agent).unwrap().into(),
            )
            .await
            .unwrap();
        state
            .create(
                &keys::lease_agent(&id),
                serde_json::to_vec(&Lease {
                    agent: id.clone(),
                    token: FencingToken(1),
                    state: LeaseState::Expired {
                        since: Timestamp(1),
                        last_session: None,
                    },
                })
                .unwrap()
                .into(),
            )
            .await
            .unwrap();
        hub.sessions
            .append(
                &id,
                &SessionEvent {
                    id: EventId::new(),
                    agent: id.clone(),
                    session: None,
                    harness: Harness::Fake,
                    host: "h1".parse().unwrap(),
                    turn: 0,
                    at: Timestamp(1),
                    body: SessionEventBody::TurnStart,
                },
            )
            .await
            .unwrap();

        let plan = super::plan(&hub, &HashSet::new(), DAY, DAY * 30)
            .await
            .unwrap();
        assert_eq!(plan.idle_logs.len(), 1, "the log is past the retention");

        // The agent wakes before the apply: it has a session again.
        let mut awake = agent;
        awake.session = Some(SessionId::new());
        state
            .update(
                &keys::agent(&id),
                serde_json::to_vec(&awake).unwrap().into(),
                agent_rev,
            )
            .await
            .unwrap();

        let applied = super::execute(&hub, &plan).await.unwrap();
        assert_eq!(applied.skipped, 1);
        assert_eq!(applied.logs, 0);
        assert_eq!(hub.sessions.read(&id).await.unwrap().len(), 1);
    }
}
