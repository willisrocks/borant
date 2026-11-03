//! The person changes feed on a recorded event log: the new-task grouping,
//! one primary change per task, the PR transitions, and the task history. The
//! fixture is a log that the hub wrote, so a rule change that moves a change
//! shows here.

use borant_core::changes::{
    ChangeMark, ChangeVerb, ChangesInput, HISTORY_LIMIT, HistoryKind, build, history,
};
use borant_core::events::SwarmEvent;
use borant_core::model::{Channel, ChannelKind, CloseReason, Sender, Task, TaskStatus};
use borant_core::{ChannelName, ProjectName, TaskId, Timestamp};
use serde_json::json;

/// The recorded log, oldest first.
fn events() -> Vec<SwarmEvent> {
    let text = include_str!("fixtures/changes-log.jsonl");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("a recorded event"))
        .collect()
}

fn chan(name: &str) -> ChannelName {
    name.parse().expect("a channel name")
}

fn project(name: &str) -> ProjectName {
    name.parse().expect("a project name")
}

fn id(text: &str) -> TaskId {
    text.parse().expect("a task id")
}

const EXPORT: &str = "01M6G0KYWD0TN7VTBNT3Q0D12B";
const HEADER: &str = "01M6G0KYWE0TN7VTBNT3Q0D12C";
const REPORT: &str = "01M6G0KYWF0TN7VTBNT3Q0D12D";
const GATE: &str = "01M6G0KYWG0TN7VTBNT3Q0D12E";
const RELEASE: &str = "01M6G0KYWH0TN7VTBNT3Q0D12F";

fn channel(name: &str, kind: ChannelKind, gate: Option<&str>) -> Channel {
    Channel {
        name: chan(name),
        owner: Some("orch".parse().expect("an agent")),
        schema: json!({}),
        created: Timestamp(0),
        closed: None,
        project: Some(project("steve")),
        branch: Some("main".into()),
        collection: None,
        paused: false,
        kind,
        title: None,
        issue: None,
        gate: gate.map(id),
        root: None,
    }
}

fn task(id_text: &str, title: &str, close: Option<CloseReason>, props: Vec<(&str, bool)>) -> Task {
    let mut properties = serde_json::Map::new();
    for (key, on) in props {
        properties.insert(key.to_owned(), json!(on));
    }
    Task {
        id: id(id_text),
        title: title.into(),
        body: String::new(),
        status: if close.is_some() {
            TaskStatus::Closed
        } else {
            TaskStatus::Open
        },
        close_reason: close,
        close_message: None,
        attention_msg: None,
        claimed_by: None,
        deadline: None,
        attempts: 1,
        max_attempts: None,
        blocked_by: Vec::new(),
        parent: None,
        kind: Default::default(),
        issue: None,
        issue_target: None,
        properties: properties.into_iter().collect(),
        created: Timestamp(0),
        updated: Timestamp(0),
        create_key: None,
    }
}

fn records() -> (Vec<Channel>, Vec<(ChannelName, Task)>) {
    let channels = vec![
        channel("steve", ChannelKind::Project, None),
        channel("steve-saga", ChannelKind::Saga, Some(GATE)),
    ];
    let mut export = task(
        EXPORT,
        "Add the export button",
        Some(CloseReason::Done),
        vec![],
    );
    export.properties.insert(
        "pr_url".into(),
        json!("https://github.com/acme/steve/pull/42"),
    );
    let tasks = vec![
        (chan("steve"), export),
        (
            chan("steve"),
            task(HEADER, "Fix the CSV header", None, vec![]),
        ),
        (
            chan("steve"),
            task(REPORT, "Cache the report", None, vec![]),
        ),
        (
            chan("steve-saga"),
            task(
                GATE,
                "Approve the export epic",
                Some(CloseReason::Done),
                vec![],
            ),
        ),
        (
            chan("steve"),
            task(
                RELEASE,
                "1.2",
                Some(CloseReason::Done),
                vec![("release", true)],
            ),
        ),
    ];
    (channels, tasks)
}

fn feed(from_seq: u64) -> borant_core::changes::ChangesView {
    let (channels, tasks) = records();
    let events: Vec<SwarmEvent> = events().into_iter().filter(|e| e.seq > from_seq).collect();
    build(&ChangesInput {
        at: Timestamp(1_793_687_000_000),
        events: &events,
        channels: &channels,
        tasks: &tasks,
    })
}

/// The verbs of the feed, newest first.
fn verbs() -> Vec<ChangeVerb> {
    feed(0).changes.into_iter().map(|c| c.verb).collect()
}

#[test]
fn changes_read_the_recorded_log_newest_first() {
    let view = feed(0);
    let seqs: Vec<u64> = view.changes.iter().map(|c| c.seq).collect();
    let mut sorted = seqs.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(seqs, sorted, "newest first");
    assert_eq!(
        verbs(),
        vec![
            ChangeVerb::OrchestratorBack,
            ChangeVerb::OrchestratorOffline,
            ChangeVerb::Answered,
            ChangeVerb::Released,
            ChangeVerb::GateApproved,
            ChangeVerb::PrMerged,
            ChangeVerb::Done,
            ChangeVerb::NewTasks,
            ChangeVerb::NewTasks,
        ],
        "every change of the log, one per task where it closed"
    );
}

#[test]
fn changes_group_new_tasks_and_never_span_the_mark() {
    let view = feed(0);
    let new: Vec<&borant_core::changes::Change> = view
        .changes
        .iter()
        .filter(|c| c.verb == ChangeVerb::NewTasks)
        .collect();
    // The two creates a minute apart group; the one ten minutes later does
    // not. Newest first, so the single task comes first.
    assert_eq!(new.len(), 2);
    assert_eq!(new[0].task, Some(id(REPORT)));
    assert_eq!(new[0].text, "new task: Cache the report");
    assert_eq!(new[0].seq, 3, "the last event of the group");
    assert_eq!(new[1].task, None, "a group of two names no one task");
    assert_eq!(
        new[1].text,
        "2 new tasks: Add the export button, Fix the CSV header"
    );
    assert_eq!(new[1].by, Some(Sender::Agent("orch".parse().unwrap())));

    // After the mark of the first group, the single create starts its own
    // group: a group never spans the seen mark.
    let after = feed(2);
    let new: Vec<&borant_core::changes::Change> = after
        .changes
        .iter()
        .filter(|c| c.verb == ChangeVerb::NewTasks)
        .collect();
    assert_eq!(new.len(), 1);
    assert_eq!(new[0].text, "new task: Cache the report");
}

#[test]
fn changes_give_one_primary_change_per_task() {
    let view = feed(0);
    let verbs_of = |task: &str| -> Vec<ChangeVerb> {
        view.changes
            .iter()
            .filter(|c| c.task == Some(id(task)))
            .map(|c| c.verb)
            .collect()
    };
    assert_eq!(
        verbs_of(EXPORT),
        vec![ChangeVerb::PrMerged, ChangeVerb::Done],
        "a done task with a merged PR is done and merged, once each"
    );
    assert_eq!(
        verbs_of(GATE),
        vec![ChangeVerb::GateApproved],
        "a closed gate is approved, not done"
    );
    assert_eq!(
        verbs_of(RELEASE),
        vec![ChangeVerb::Released],
        "a release is released, not done"
    );
    let done = view
        .changes
        .iter()
        .find(|c| c.task == Some(id(EXPORT)) && c.verb == ChangeVerb::Done)
        .expect("the done change");
    assert_eq!(
        done.text,
        "done: Add the export button (PR https://github.com/acme/steve/pull/42)"
    );
    assert_eq!(done.project, Some(project("steve")));
}

#[test]
fn changes_read_only_after_the_mark() {
    let view = feed(10);
    let seqs: Vec<u64> = view.changes.iter().map(|c| c.seq).collect();
    assert_eq!(seqs, vec![12, 11], "only the session changes remain");
    // The serialized mark is the one that the mod stores.
    assert_eq!(
        serde_json::to_value(ChangeMark::Seq(12)).unwrap(),
        json!({"seq": 12})
    );
    assert_eq!(
        serde_json::to_value(ChangeMark::At(Timestamp(5))).unwrap(),
        json!({"at": 5})
    );
}

#[test]
fn changes_history_reads_its_events_oldest_first() {
    let entries = history(&events(), &chan("steve"), &id(HEADER));
    let kinds: Vec<HistoryKind> = entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            HistoryKind::Created,
            HistoryKind::Answered,
            HistoryKind::Message
        ],
        "created, answered, and the message that names it"
    );
    assert!(entries[1].text.contains("key q1"));
    assert_eq!(entries[1].by, Some(Sender::Human));
    assert_eq!(
        entries[2].text, "orch to chief: Use RFC 4180 for the header.",
        "the message text, so the person reads what the orchestrator said"
    );
}

#[test]
fn changes_history_claims_and_leaves_out_other_tasks() {
    let entries = history(&events(), &chan("steve"), &id(EXPORT));
    let kinds: Vec<HistoryKind> = entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            HistoryKind::Created,
            HistoryKind::Claimed,
            HistoryKind::Status
        ]
    );
    assert_eq!(entries[1].text, "claimed by w1");
    assert_eq!(
        entries[1].by,
        Some(Sender::Agent("w1".parse().expect("an agent")))
    );
    assert_eq!(entries[2].status, Some(TaskStatus::Closed));
}

#[test]
fn changes_history_keeps_the_last_fifty_entries() {
    let mut log = events();
    let at = Timestamp(1_793_686_100_000);
    for n in 0..60u64 {
        let mut event = log[4].clone();
        event.seq = 100 + n;
        event.at = Timestamp(at.0 + i64::try_from(n).unwrap());
        log.push(event);
    }
    let entries = history(&log, &chan("steve"), &id(EXPORT));
    assert_eq!(entries.len(), HISTORY_LIMIT);
    assert_eq!(
        entries.last().map(|e| e.seq),
        Some(159),
        "the newest events are the ones kept"
    );
}
