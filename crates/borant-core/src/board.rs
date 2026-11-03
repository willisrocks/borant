//! The board (spec: sagas and epics, "API, MCP, and CLI"): the projects,
//! sagas, epics, and questions of each collection, as one computed read. The
//! hub reads the records and calls [`build`]; nothing here stores anything.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ChannelName, CollectionName, HostName, ProjectName, TaskId};
use crate::model::{
    AgentStatus, Channel, ChannelKind, CloseReason, LeaseState, Project, SpawnRecord, Task,
    TaskKind, TaskStatus,
};
use crate::ports::{ChangeState, ChangeStatus, CheckState, ReviewState};
use crate::rules::blocked;
use crate::rules::roles::{self, CollectionConfig};
use crate::rules::{saga, tasks as task_rules};
use crate::time::Timestamp;

/// How far back a task that closed as done counts, when the caller names no
/// time: one day.
pub const DEFAULT_DONE_SINCE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// What an open task waits for when its channel has no orchestrator online.
pub const ORCHESTRATOR_OFFLINE: &str = "orchestrator offline";

/// What the tasks of a saga wait for before its gate closes.
pub const YOUR_APPROVAL: &str = "your approval";

/// The whole board: one board per collection that the caller may see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardView {
    /// When the hub built the board.
    pub at: Timestamp,
    pub collections: Vec<CollectionBoard>,
}

/// One collection, or the records that carry no collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionBoard {
    pub collection: Option<CollectionName>,
    pub projects: Vec<ProjectBoard>,
    /// The tasks in `needs_human` or `stuck`, oldest first.
    pub needs_you: Vec<Question>,
    /// Queued spawns that capacity does not explain and that bind no task,
    /// oldest first. A bound spawn already appears through its stuck task, so
    /// it is not listed here twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<BlockedSpawn>,
}

/// One spawn that waits on something a person must fix (blocked-work alarm
/// spec, "The board's need-you").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedSpawn {
    pub agent: AgentId,
    pub channel: ChannelName,
    pub project: Option<ProjectName>,
    /// The refusal, as `borant ps --queued` prints it.
    pub reason: String,
    /// The one action that unblocks the spawn.
    pub action: String,
    /// When the spawn entered the queue.
    pub since: Timestamp,
    /// The agent that spawned it, when an agent did.
    #[serde(default)]
    pub spawner: Option<AgentId>,
}

/// One project, with its sagas, its standalone epics, and its plain tasks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectBoard {
    pub project: ProjectName,
    /// The resolved setting of the project and its collection.
    pub mirror_issues: bool,
    /// The owners of the project's open channels.
    pub orchestrators: Vec<AgentLine>,
    /// Active first, then proposed, then closed.
    pub sagas: Vec<SagaLine>,
    /// The standalone epic channels of the project.
    pub epics: Vec<EpicLine>,
    /// The plain tasks of the project's open project channels.
    pub tasks: Counts,
    /// One line for each task of the project that is not closed, by channel
    /// and then by age. Only with `--tasks`, which is why it is not `tasks`:
    /// that name holds the counts, and old readers keep it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_lines: Vec<TaskLine>,
    /// How many tasks the project closed as done at or after the cutoff of
    /// the read. Only with `--tasks`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_since: Option<u32>,
    /// The last ten tasks of the project that closed as done, newest first.
    /// Only with `--tasks`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_done: Vec<DoneLine>,
}

/// One orchestrator of a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentLine {
    pub agent: AgentId,
    /// The agent holds its lease.
    pub online: bool,
    pub busy: bool,
    pub host: Option<HostName>,
}

/// Where a saga stands at its gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaState {
    /// The gate is open, or it waits for a person.
    Proposed,
    /// The gate closed as `done`.
    Approved,
    /// The channel is closed, or the gate closed with a reason other than
    /// `done`: a rejected saga.
    Closed,
}

/// One saga channel, with its gate and its epics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SagaLine {
    pub channel: ChannelName,
    pub title: Option<String>,
    pub issue: Option<String>,
    pub state: SagaState,
    /// The gate task, so the mod can close it.
    pub gate: Option<TaskId>,
    pub epics: Vec<EpicLine>,
    /// Epics closed as `done`.
    pub done: u32,
    /// Epics not closed as `wontfix`.
    pub total: u32,
}

/// One epic: a task of a saga, or the root of a standalone epic channel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpicLine {
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    pub status: TaskStatus,
    pub issue: Option<String>,
    /// The top branch of the epic's stack.
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub counts: Counts,
}

/// The counts of the work in one scope. An epic and a gate are never counted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub open: u32,
    pub ready: u32,
    pub running: u32,
    pub blocked: u32,
    pub stuck: u32,
    pub needs_human: u32,
    pub done: u32,
}

/// One task of a project that is not closed, as the chief mod draws it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskLine {
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    pub kind: TaskKind,
    /// The status of the record: open, in progress, needs a person, or stuck.
    pub status: TaskStatus,
    /// Derived, for an open plain task: whether it can start now, later, or
    /// not until a person acts. An epic is not work, so its line has none.
    pub start: Option<StartState>,
    /// What it waits on: the titles of its open `blocked_by` tasks, the
    /// reason of the blocked spawn that is bound to it, your approval, or
    /// [`ORCHESTRATOR_OFFLINE`].
    pub waits_on: Option<String>,
    pub claimed_by: Option<AgentId>,
    /// When the task entered its current status: the time of its last
    /// `task.status` event, or its `updated` time when it has no event.
    pub since: Timestamp,
    pub attention_msg: Option<String>,
    pub pr: Option<PrLine>,
    /// The task's `updated` time. An action sends it back as
    /// `expect_updated`, so a task that changed after the draw is a
    /// conflict and not a silent overwrite.
    pub version: Timestamp,
}

/// Whether an open task can start, per the rules of the task graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartState {
    /// Nothing open in `blocked_by`, no blocked spawn bound to it, and its
    /// owner has a session.
    Ready,
    /// A task in `blocked_by` is open: it waits for that one.
    Later,
    /// A spawn bound to it is blocked, its channel waits for a person, or
    /// its owner has no session.
    Blocked,
}

/// The last state of a task's change request, as the PR watch read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrLine {
    pub url: String,
    pub state: ChangeState,
    pub checks: CheckState,
    /// The checks that passed and the checks that reported, when the adapter
    /// counts them. A reader without them shows the state alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_passed: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_total: Option<u32>,
    pub review: ReviewState,
    #[serde(default)]
    pub review_requested: bool,
}

/// One task of the project that closed as done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DoneLine {
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    /// The agent that held the task when it closed, or the owner of its
    /// channel when nobody held it: the rules let only those, and a person,
    /// close a task as done.
    pub closer: Option<AgentId>,
    /// When it closed, which is its `updated` time.
    pub at: Timestamp,
    pub pr_url: Option<String>,
}

/// One task that waits for a person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub project: Option<ProjectName>,
    pub channel: ChannelName,
    pub task: TaskId,
    pub title: String,
    pub status: TaskStatus,
    pub attention_msg: Option<String>,
    /// When the task last changed, which is when it entered `needs_human` or
    /// `stuck`.
    pub since: Timestamp,
    /// The task is a saga's gate.
    pub gate: bool,
    /// The owner of the task.s channel: the agent that answers for it, and the
    /// only one besides a person that may close a plain task as done.
    #[serde(default)]
    pub owner: Option<AgentId>,
}

/// The records that the board reads, and the time that it shows.
#[derive(Debug)]
pub struct BoardInput<'a> {
    pub at: Timestamp,
    pub projects: &'a [Project],
    pub channels: &'a [Channel],
    /// Every task of every channel.
    pub tasks: &'a [(ChannelName, Task)],
    /// Every spawn record, for the spawns that wait in the queue.
    pub spawns: &'a [SpawnRecord],
    /// The live view of the agents.
    pub agents: &'a [AgentStatus],
    /// The `[collections.<name>]` of `hub.toml`.
    pub collections: &'a BTreeMap<CollectionName, CollectionConfig>,
    /// The task lines, the done count, and the last done tasks of each
    /// project. `None` is the board without them, the reply that an older
    /// reader gets.
    pub lines: Option<TaskLines<'a>>,
}

/// What the task lines read beyond the records: the time of each task's last
/// status change, and the PR watch's last read (spec: board task lines,
/// "Cost").
#[derive(Debug)]
pub struct TaskLines<'a> {
    /// A task closed as done at or after this time counts in `done_since`.
    pub done_since: Timestamp,
    /// The time of the last `task.status` event of a task, by task id.
    pub status_since: &'a [(TaskId, Timestamp)],
    /// The state that the PR watch last read, by channel and task.
    pub prs: &'a [(ChannelName, TaskId, ChangeStatus)],
}

/// Builds the board of each wanted collection, in that order. `None` is the
/// board of the records that carry no collection.
pub fn build(input: &BoardInput, wanted: &[Option<CollectionName>]) -> BoardView {
    let mut tasks_of: HashMap<&ChannelName, Vec<Task>> = HashMap::new();
    for (channel, task) in input.tasks {
        tasks_of.entry(channel).or_default().push(task.clone());
    }
    let agent_of: HashMap<&AgentId, &AgentStatus> =
        input.agents.iter().map(|a| (&a.agent, a)).collect();
    BoardView {
        at: input.at,
        collections: wanted
            .iter()
            .map(|collection| collection_board(input, &tasks_of, &agent_of, collection.as_ref()))
            .collect(),
    }
}

/// The counts of `scope`, a part of the tasks of `channel`. The whole channel
/// is `tasks`, because `ready` reads the `blocked_by` graph and the gate.
/// Choice C13: an epic and a gate are never counted, `blocked` is the open
/// tasks that are not ready, and `done` counts the closed tasks.
pub fn counts(channel: &Channel, tasks: &[Task], scope: &[&Task]) -> Counts {
    let ready: HashSet<TaskId> = saga::ready_in(channel, tasks).into_iter().collect();
    let mut out = Counts::default();
    for task in scope {
        // C13: an epic is not work, and a gate is a question, not a task.
        if task.kind == TaskKind::Epic || channel.gate == Some(task.id) {
            continue;
        }
        match task.status {
            TaskStatus::Open => {
                out.open += 1;
                if ready.contains(&task.id) {
                    out.ready += 1;
                } else {
                    out.blocked += 1;
                }
            }
            TaskStatus::InProgress => out.running += 1,
            TaskStatus::NeedsHuman => out.needs_human += 1,
            TaskStatus::Stuck => out.stuck += 1,
            TaskStatus::Closed => out.done += 1,
        }
    }
    out
}

impl Counts {
    /// Adds the counts of another scope, for a project of several channels.
    fn add(&mut self, more: &Counts) {
        self.open += more.open;
        self.ready += more.ready;
        self.running += more.running;
        self.blocked += more.blocked;
        self.stuck += more.stuck;
        self.needs_human += more.needs_human;
        self.done += more.done;
    }
}

/// One board of the records that carry `collection`.
fn collection_board(
    input: &BoardInput,
    tasks_of: &HashMap<&ChannelName, Vec<Task>>,
    agent_of: &HashMap<&AgentId, &AgentStatus>,
    collection: Option<&CollectionName>,
) -> CollectionBoard {
    let channels: Vec<&Channel> = input
        .channels
        .iter()
        .filter(|channel| channel.collection.as_ref() == collection)
        .collect();
    let mut projects: Vec<&Project> = input
        .projects
        .iter()
        .filter(|project| project.collection.as_ref() == collection)
        .collect();
    projects.sort_by(|a, b| a.name.cmp(&b.name));

    let projects: Vec<ProjectBoard> = projects
        .iter()
        .map(|project| project_board(project, &channels, input, tasks_of, agent_of))
        .collect();

    let mut needs_you = Vec::new();
    for channel in &channels {
        for task in channel_tasks(tasks_of, &channel.name) {
            if !matches!(task.status, TaskStatus::NeedsHuman | TaskStatus::Stuck) {
                continue;
            }
            needs_you.push(Question {
                project: channel.project.clone(),
                channel: channel.name.clone(),
                task: task.id,
                title: task.title.clone(),
                status: task.status,
                attention_msg: task.attention_msg.clone(),
                since: task.updated,
                gate: channel.gate == Some(task.id),
                owner: channel.owner.clone(),
            });
        }
    }
    // As `borant inbox` does: the question that has waited longest first.
    needs_you.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.task.cmp(&b.task)));

    CollectionBoard {
        collection: collection.cloned(),
        projects,
        needs_you,
        blocked: blocked_spawns(input, &channels),
    }
}

/// The queued spawns of `channels` that capacity does not explain. A spawn
/// with a bound task appears through its stuck task instead, so it is left
/// out here.
fn blocked_spawns(input: &BoardInput, channels: &[&Channel]) -> Vec<BlockedSpawn> {
    let mut out: Vec<BlockedSpawn> = input
        .spawns
        .iter()
        .filter(|record| record.spec.task.is_none())
        .filter_map(|record| {
            let blocked = blocked::blocked(record, input.at)?;
            let channel = channels
                .iter()
                .find(|channel| channel.name == record.spec.channel)?;
            Some(BlockedSpawn {
                agent: record.agent.clone(),
                channel: channel.name.clone(),
                project: channel.project.clone(),
                reason: blocked.reason.clone(),
                action: blocked.action(&record.agent),
                since: record.queued_at,
                spawner: blocked::spawner(&record.by),
            })
        })
        .collect();
    // The spawn that has waited longest first, as the questions are.
    out.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.agent.cmp(&b.agent)));
    out
}

/// One project board, from its channels of `channels`.
fn project_board(
    project: &Project,
    channels: &[&Channel],
    input: &BoardInput,
    tasks_of: &HashMap<&ChannelName, Vec<Task>>,
    agent_of: &HashMap<&AgentId, &AgentStatus>,
) -> ProjectBoard {
    let mine: Vec<&Channel> = channels
        .iter()
        .copied()
        .filter(|channel| channel.project.as_ref() == Some(&project.name))
        .collect();

    let mut sagas: Vec<(&Channel, SagaLine)> = Vec::new();
    let mut epics: Vec<(&Task, EpicLine)> = Vec::new();
    let mut tasks = Counts::default();
    let mut orchestrators: Vec<AgentId> = Vec::new();
    for channel in &mine {
        let all = channel_tasks(tasks_of, &channel.name);
        match channel.kind {
            ChannelKind::Saga => sagas.push((channel, saga_line(channel, all))),
            ChannelKind::Epic => {
                if let Some((epic, line)) = epic_channel_line(channel, all) {
                    epics.push((epic, line));
                }
            }
            // A closed channel holds only closed history (rule 9), so its
            // tasks say nothing about the work of the board.
            ChannelKind::Project if channel.closed.is_none() => {
                tasks.add(&counts(channel, all, &all.iter().collect::<Vec<_>>()));
            }
            ChannelKind::Project => {}
        }
        if channel.closed.is_none()
            && let Some(owner) = &channel.owner
            && !orchestrators.contains(owner)
        {
            orchestrators.push(owner.clone());
        }
    }
    // Choice C14: approved, then proposed, then closed; then by created.
    sagas.sort_by(|(ca, a), (cb, b)| {
        state_rank(a.state)
            .cmp(&state_rank(b.state))
            .then_with(|| ca.created.cmp(&cb.created))
            .then_with(|| ca.name.cmp(&cb.name))
    });
    sort_epics(&mut epics);
    orchestrators.sort();

    let collection = project
        .collection
        .as_ref()
        .and_then(|name| input.collections.get(name));
    let (task_lines, done_since, recent_done) = match &input.lines {
        Some(lines) => project_lines(&mine, input, lines, tasks_of, agent_of),
        None => (Vec::new(), None, Vec::new()),
    };
    ProjectBoard {
        project: project.name.clone(),
        mirror_issues: roles::mirror_issues(project, collection),
        orchestrators: orchestrators
            .iter()
            .map(|agent| agent_line(agent, agent_of))
            .collect(),
        sagas: sagas.into_iter().map(|(_, line)| line).collect(),
        epics: epics.into_iter().map(|(_, line)| line).collect(),
        tasks,
        task_lines,
        done_since,
        recent_done,
    }
}

/// One saga line, with its epics, from the tasks of its channel.
fn saga_line(channel: &Channel, tasks: &[Task]) -> SagaLine {
    let epics: Vec<&Task> = tasks
        .iter()
        .filter(|task| task.kind == TaskKind::Epic)
        .collect();
    let done = epics
        .iter()
        .filter(|epic| {
            epic.status == TaskStatus::Closed && epic.close_reason == Some(CloseReason::Done)
        })
        .count();
    // C15: a `wontfix` epic leaves the total.
    let total = epics
        .iter()
        .filter(|epic| {
            !(epic.status == TaskStatus::Closed && epic.close_reason == Some(CloseReason::Wontfix))
        })
        .count();
    let mut lines: Vec<(&Task, EpicLine)> = epics
        .iter()
        .map(|epic| (*epic, epic_line(channel, epic, tasks)))
        .collect();
    sort_epics(&mut lines);
    SagaLine {
        channel: channel.name.clone(),
        title: channel.title.clone(),
        issue: channel.issue.clone(),
        state: saga_state(channel, tasks),
        gate: channel.gate,
        epics: lines.into_iter().map(|(_, line)| line).collect(),
        done: u32::try_from(done).unwrap_or(u32::MAX),
        total: u32::try_from(total).unwrap_or(u32::MAX),
    }
}

/// The root epic of a standalone epic channel, and its line.
fn epic_channel_line<'a>(channel: &Channel, tasks: &'a [Task]) -> Option<(&'a Task, EpicLine)> {
    let epic = channel
        .root
        .and_then(|root| tasks.iter().find(|task| task.id == root))
        .or_else(|| tasks.iter().find(|task| task.kind == TaskKind::Epic))?;
    let mut line = epic_line(channel, epic, tasks);
    // The hub writes the root's issue with the channel's link, so the
    // channel's issue wins; the root's is the fallback.
    if let Some(issue) = &channel.issue {
        line.issue = Some(issue.clone());
    }
    Some((epic, line))
}

/// One epic line: its own status, its branch and PR, and the counts of the
/// tasks below it.
fn epic_line(channel: &Channel, epic: &Task, tasks: &[Task]) -> EpicLine {
    let scope = saga::descendants(tasks, epic.id);
    EpicLine {
        channel: channel.name.clone(),
        task: epic.id,
        title: epic.title.clone(),
        status: saga::epic_status(epic, tasks),
        issue: epic.issue.clone(),
        branch: property(epic, "branch"),
        pr_url: property(epic, "pr_url"),
        counts: counts(channel, tasks, &scope),
    }
}

/// Where a saga stands (C14).
fn saga_state(channel: &Channel, tasks: &[Task]) -> SagaState {
    let rejected = channel
        .gate
        .and_then(|gate| tasks.iter().find(|task| task.id == gate))
        .is_some_and(|gate| {
            gate.status == TaskStatus::Closed && gate.close_reason != Some(CloseReason::Done)
        });
    if channel.closed.is_some() || rejected {
        SagaState::Closed
    } else if saga::approved(channel, tasks) {
        SagaState::Approved
    } else {
        SagaState::Proposed
    }
}

/// The order of the saga states on the board: active first.
fn state_rank(state: SagaState) -> u8 {
    match state {
        SagaState::Approved => 0,
        SagaState::Proposed => 1,
        SagaState::Closed => 2,
    }
}

/// C14: the open epics first, then by `created`.
fn sort_epics(lines: &mut [(&Task, EpicLine)]) {
    lines.sort_by(|(ta, a), (tb, b)| {
        let rank = |status: TaskStatus| u8::from(status == TaskStatus::Closed);
        rank(a.status)
            .cmp(&rank(b.status))
            .then_with(|| ta.created.cmp(&tb.created))
            .then_with(|| ta.id.cmp(&tb.id))
    });
}

/// One orchestrator, from the live view.
fn agent_line(agent: &AgentId, agent_of: &HashMap<&AgentId, &AgentStatus>) -> AgentLine {
    match agent_of.get(agent) {
        Some(status) => AgentLine {
            agent: agent.clone(),
            online: matches!(status.lease, Some(LeaseState::Held { .. })),
            busy: status.busy,
            host: status.host.clone(),
        },
        None => AgentLine {
            agent: agent.clone(),
            online: false,
            busy: false,
            host: None,
        },
    }
}

/// The agent holds its lease.
fn online(agent: &AgentId, agent_of: &HashMap<&AgentId, &AgentStatus>) -> bool {
    agent_of
        .get(agent)
        .is_some_and(|status| matches!(status.lease, Some(LeaseState::Held { .. })))
}

/// How many of the last done tasks of a project the board lists.
const RECENT_DONE: usize = 10;

/// The task lines of a project, the count of the tasks that it closed as done
/// at or after the cutoff, and the last ten of them: the `--tasks` part of
/// the board.
fn project_lines(
    channels: &[&Channel],
    input: &BoardInput,
    lines: &TaskLines,
    tasks_of: &HashMap<&ChannelName, Vec<Task>>,
    agent_of: &HashMap<&AgentId, &AgentStatus>,
) -> (Vec<TaskLine>, Option<u32>, Vec<DoneLine>) {
    let since_of: HashMap<TaskId, Timestamp> = lines.status_since.iter().copied().collect();
    let pr_of: HashMap<(&ChannelName, TaskId), &ChangeStatus> = lines
        .prs
        .iter()
        .map(|(channel, task, status)| ((channel, *task), status))
        .collect();
    let mut open: Vec<(&Channel, &Task)> = Vec::new();
    let mut done: u32 = 0;
    let mut recent: Vec<DoneLine> = Vec::new();
    for channel in channels {
        let all = channel_tasks(tasks_of, &channel.name);
        // Rule 9: a closed channel holds only closed history, and the spec
        // leaves the tasks of a closed saga or epic out.
        let listed = channel.closed.is_none() || channel.kind == ChannelKind::Project;
        for task in all {
            if task.status == TaskStatus::Closed {
                // C13: an epic is not work, and a gate is a question, so
                // neither one is a task that the project closed.
                if task.close_reason != Some(CloseReason::Done)
                    || task.kind == TaskKind::Epic
                    || channel.gate == Some(task.id)
                {
                    continue;
                }
                if task.updated >= lines.done_since {
                    done = done.saturating_add(1);
                }
                recent.push(DoneLine {
                    channel: channel.name.clone(),
                    task: task.id,
                    title: task.title.clone(),
                    closer: closer(channel, task),
                    at: task.updated,
                    pr_url: property(task, "pr_url"),
                });
                continue;
            }
            if listed {
                open.push((channel, task));
            }
        }
    }
    // By channel and then by age, so the tasks of one epic stay together.
    open.sort_by(|(ca, a), (cb, b)| {
        ca.name
            .cmp(&cb.name)
            .then_with(|| a.created.cmp(&b.created))
            .then_with(|| a.id.cmp(&b.id))
    });
    let task_lines = open
        .into_iter()
        .map(|(channel, task)| {
            task_line(channel, task, input, tasks_of, agent_of, &since_of, &pr_of)
        })
        .collect();
    // The newest close first, as the project view lists them.
    recent.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.task.cmp(&b.task)));
    recent.truncate(RECENT_DONE);
    (task_lines, Some(done), recent)
}

/// One line of a task of `channel`.
fn task_line(
    channel: &Channel,
    task: &Task,
    input: &BoardInput,
    tasks_of: &HashMap<&ChannelName, Vec<Task>>,
    agent_of: &HashMap<&AgentId, &AgentStatus>,
    since_of: &HashMap<TaskId, Timestamp>,
    pr_of: &HashMap<(&ChannelName, TaskId), &ChangeStatus>,
) -> TaskLine {
    let all = channel_tasks(tasks_of, &channel.name);
    let (start, waits_on) = start_state(channel, all, task, input, agent_of);
    let pr = property(task, "pr_url").and_then(|url| {
        let status = pr_of.get(&(&channel.name, task.id))?;
        Some(PrLine {
            url,
            state: status.state,
            checks: status.checks.clone(),
            checks_passed: status.checks_passed,
            checks_total: status.checks_total,
            review: status.review,
            review_requested: status.review_requested,
        })
    });
    TaskLine {
        channel: channel.name.clone(),
        task: task.id,
        title: task.title.clone(),
        kind: task.kind,
        status: task.status,
        start,
        waits_on,
        claimed_by: task.claimed_by.clone(),
        // A task that the event log does not hold predates the record of
        // status changes, and its own update is the best time it has.
        since: since_of.get(&task.id).copied().unwrap_or(task.updated),
        attention_msg: task.attention_msg.clone(),
        pr,
        version: task.updated,
    }
}

/// Whether `task` can start, and what it waits on, per the rules of the task
/// graph: `tasks` are the tasks of `channel`.
///
/// A blocked spawn wins over a wait in the graph: a person must act either
/// way, and the spawn's reason is the one to act on.
fn start_state(
    channel: &Channel,
    tasks: &[Task],
    task: &Task,
    input: &BoardInput,
    agent_of: &HashMap<&AgentId, &AgentStatus>,
) -> (Option<StartState>, Option<String>) {
    // Only an open plain task starts: an epic is not work, and a task that
    // runs or waits for a person is not open.
    if task.status != TaskStatus::Open || task.kind == TaskKind::Epic {
        return (None, None);
    }
    // A spawn that is bound to the task and blocked waits on a person, even
    // when the graph would let the task start.
    let bound = input
        .spawns
        .iter()
        .find(|record| record.spec.task.as_ref().is_some_and(|b| b.id == task.id));
    if let Some(blocked) = bound.and_then(|record| blocked::blocked(record, input.at)) {
        return (Some(StartState::Blocked), Some(blocked.reason));
    }
    // The gate of a saga is the question that a person answers.
    if channel.gate == Some(task.id) && !saga::approved(channel, tasks) {
        return (Some(StartState::Blocked), Some(YOUR_APPROVAL.to_owned()));
    }
    // An owner without a lease cannot pick the task up.
    if let Some(owner) = &channel.owner
        && !online(owner, agent_of)
    {
        return (
            Some(StartState::Blocked),
            Some(ORCHESTRATOR_OFFLINE.to_owned()),
        );
    }
    if saga::ready_in(channel, tasks).contains(&task.id) {
        return (Some(StartState::Ready), None);
    }
    // The rules found a reason, and no spawn named it: name the tasks that
    // are still open, or the gate.
    let waits: Vec<String> = task_rules::open_blockers(tasks, task.id)
        .into_iter()
        .map(|id| blocker_title(tasks, id))
        .collect();
    if !waits.is_empty() {
        return (Some(StartState::Later), Some(waits.join(", ")));
    }
    let waits_on = (!saga::approved(channel, tasks)).then(|| YOUR_APPROVAL.to_owned());
    (Some(StartState::Later), waits_on)
}

/// The title of a task that holds another one back, or its id when the
/// channel no longer holds it.
fn blocker_title(tasks: &[Task], id: TaskId) -> String {
    tasks
        .iter()
        .find(|task| task.id == id)
        .map(|task| task.title.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Who closed a task as done, as the record names it: the agent that held it,
/// or the owner of the channel when nobody held it.
fn closer(channel: &Channel, task: &Task) -> Option<AgentId> {
    task.claimed_by.clone().or_else(|| channel.owner.clone())
}

/// The tasks of one channel.
fn channel_tasks<'a>(
    tasks_of: &'a HashMap<&ChannelName, Vec<Task>>,
    channel: &ChannelName,
) -> &'a [Task] {
    tasks_of.get(channel).map(Vec::as_slice).unwrap_or(&[])
}

/// A non-empty string property of a task.
fn property(task: &Task, name: &str) -> Option<String> {
    task.properties
        .get(name)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Sender, SpawnSpec, SpawnState, TaskRef};
    use serde_json::{Map, Value, json};

    fn ts(n: i64) -> Timestamp {
        Timestamp(n)
    }

    fn chan(name: &str) -> ChannelName {
        name.parse().unwrap()
    }

    fn proj(name: &str) -> ProjectName {
        name.parse().unwrap()
    }

    fn col(name: &str) -> CollectionName {
        name.parse().unwrap()
    }

    /// A task id that names its place in the test, in ULID order.
    fn id(n: u8) -> TaskId {
        format!("01J{n:023}").parse().unwrap()
    }

    struct Fixture {
        projects: Vec<Project>,
        channels: Vec<Channel>,
        tasks: Vec<(ChannelName, Task)>,
        spawns: Vec<SpawnRecord>,
        agents: Vec<AgentStatus>,
        collections: BTreeMap<CollectionName, CollectionConfig>,
        /// The board's clock, in milliseconds.
        now: i64,
        /// The board carries the task lines of `--tasks`.
        lines: bool,
        /// The cutoff of `done_since`.
        done_since: i64,
        /// The time of each task's last status change.
        status_since: Vec<(TaskId, Timestamp)>,
        /// The last state that the PR watch read.
        prs: Vec<(ChannelName, TaskId, ChangeStatus)>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                projects: Vec::new(),
                channels: Vec::new(),
                tasks: Vec::new(),
                spawns: Vec::new(),
                agents: Vec::new(),
                collections: BTreeMap::new(),
                now: 9_000,
                lines: false,
                done_since: 0,
                status_since: Vec::new(),
                prs: Vec::new(),
            }
        }

        /// A board with the task lines of `--tasks` and this done cutoff.
        fn with_lines(&mut self, done_since: i64) -> &mut Self {
            self.lines = true;
            self.done_since = done_since;
            self
        }

        /// The time of the last status change of `task`.
        fn status_at(&mut self, task: TaskId, at: i64) -> &mut Self {
            self.status_since.push((task, ts(at)));
            self
        }

        /// The state that the PR watch read for `task` of `channel`.
        fn pr(&mut self, channel: &str, task: TaskId, status: ChangeStatus) -> &mut Self {
            self.prs.push((chan(channel), task, status));
            self
        }

        /// An agent that holds its lease on `h1`, or one that has none.
        fn agent(&mut self, name: &str, online: bool) -> &mut Self {
            let now = self.now;
            let host: HostName = "h1".parse().unwrap();
            self.agents.push(AgentStatus {
                agent: name.parse().unwrap(),
                harness: None,
                host: online.then(|| host.clone()),
                lease: online.then(|| LeaseState::Held {
                    session: crate::SessionId::new(),
                    host,
                    expires: ts(now),
                }),
                session: None,
                busy: false,
                claim: None,
                role: None,
                collection: None,
                pinned: false,
                retired: false,
                model: None,
                last_tool_at: None,
                last_tool: None,
                last_text_at: None,
                last_text: None,
                tool_running_since: None,
            });
            self
        }

        /// A queued spawn of `channel` since `since` that `reason` blocks, or
        /// that binds `task`.
        fn spawn(
            &mut self,
            agent: &str,
            channel: &str,
            since: i64,
            reason: &str,
            task: Option<TaskId>,
        ) -> &mut Self {
            let record = SpawnRecord {
                agent: agent.parse().unwrap(),
                spec: SpawnSpec {
                    channel: chan(channel),
                    prompt: String::new(),
                    harness: None,
                    host: None,
                    name: None,
                    model: None,
                    role: None,
                    task: task.map(|id| TaskRef {
                        channel: chan(channel),
                        id,
                    }),
                    stop_on_close: None,
                    continues: None,
                    priority: None,
                    bundles: Vec::new(),
                },
                resolved: roles::Resolved::default(),
                by: Sender::Human,
                queued_at: ts(since),
                priority: 0,
                state: SpawnState::Queued,
                attempts: 0,
                last_reject: vec![("h1".parse().unwrap(), reason.to_owned())],
                queue_reason: None,
                launch_failures: Vec::new(),
                recoveries: 0,
            };
            self.spawns.push(record);
            self
        }

        fn project(&mut self, name: &str) -> &mut Self {
            self.projects.push(Project {
                name: proj(name),
                remote_url: "git://git/demo.git".into(),
                default_branch: "main".into(),
                checkpoint_remote: None,
                setup: None,
                test: None,
                collection: None,
                mirror_issues: None,
            });
            self
        }

        fn channel(&mut self, name: &str, kind: ChannelKind, project: Option<&str>) -> &mut Self {
            self.channels.push(Channel {
                name: chan(name),
                owner: None,
                schema: json!({}),
                created: ts(1_000),
                closed: None,
                project: project.map(proj),
                branch: None,
                collection: None,
                paused: false,
                kind,
                title: None,
                issue: None,
                gate: None,
                root: None,
            });
            self
        }

        fn task(&mut self, channel: &str, task: Task) -> &mut Self {
            self.tasks.push((chan(channel), task));
            self
        }

        fn at(&mut self, channel: &str, f: impl FnOnce(&mut Channel)) -> &mut Self {
            let c = self
                .channels
                .iter_mut()
                .find(|c| c.name == chan(channel))
                .expect("the fixture channel");
            f(c);
            self
        }

        fn build(&self, wanted: &[Option<CollectionName>]) -> BoardView {
            let lines = self.lines.then(|| TaskLines {
                done_since: ts(self.done_since),
                status_since: &self.status_since,
                prs: &self.prs,
            });
            build(
                &BoardInput {
                    at: ts(self.now),
                    projects: &self.projects,
                    channels: &self.channels,
                    tasks: &self.tasks,
                    spawns: &self.spawns,
                    agents: &self.agents,
                    collections: &self.collections,
                    lines,
                },
                wanted,
            )
        }
    }

    fn task(n: u8, title: &str, status: TaskStatus, created: i64) -> Task {
        Task {
            id: id(n),
            title: title.into(),
            body: String::new(),
            status,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            deadline: None,
            attempts: 0,
            max_attempts: None,
            blocked_by: Vec::new(),
            parent: None,
            kind: TaskKind::Task,
            issue: None,
            issue_target: None,
            properties: Default::default(),
            created: ts(created),
            updated: ts(created),
            create_key: None,
        }
    }

    fn epic(n: u8, title: &str, created: i64) -> Task {
        Task {
            kind: TaskKind::Epic,
            ..task(n, title, TaskStatus::Open, created)
        }
    }

    fn closed(n: u8, title: &str, reason: CloseReason) -> Task {
        Task {
            status: TaskStatus::Closed,
            close_reason: Some(reason),
            updated: ts(2_000),
            ..task(n, title, TaskStatus::Closed, 1_500)
        }
    }

    fn needs_human(n: u8, title: &str, msg: &str, updated: i64) -> Task {
        Task {
            status: TaskStatus::NeedsHuman,
            attention_msg: Some(msg.into()),
            updated: ts(updated),
            ..task(n, title, TaskStatus::NeedsHuman, 1_000)
        }
    }

    /// A task closed as done at `at`, which is its `updated` time.
    fn closed_at(n: u8, title: &str, at: i64) -> Task {
        Task {
            updated: ts(at),
            ..closed(n, title, CloseReason::Done)
        }
    }

    /// The properties of a task that names its change request.
    fn props(url: &str) -> Map<String, Value> {
        serde_json::from_value(json!({ "pr_url": url })).unwrap()
    }

    /// The status that the PR watch records.
    fn watched(state: ChangeState) -> ChangeStatus {
        ChangeStatus {
            state,
            head: "abc123".into(),
            checks: CheckState::Pass {},
            review: ReviewState::None,
            checks_passed: Some(4),
            checks_total: Some(4),
            review_requested: false,
        }
    }

    fn child(n: u8, title: &str, parent: TaskId, status: TaskStatus) -> Task {
        Task {
            parent: Some(parent),
            ..task(n, title, status, 1_200)
        }
    }

    #[test]
    fn board_counts_skip_epics_and_the_gate() {
        let mut fx = Fixture::new();
        fx.channel("saga", ChannelKind::Saga, Some("mirror"));
        fx.at("saga", |c| c.gate = Some(id(1)));
        // The gate is closed as done, so the saga is approved.
        fx.task("saga", closed(1, saga::GATE_TITLE, CloseReason::Done));
        fx.task("saga", epic(2, "the provisioner", 1_100));
        fx.task("saga", child(3, "the adapter", id(2), TaskStatus::Open));
        fx.task(
            "saga",
            child(4, "the client", id(2), TaskStatus::InProgress),
        );
        fx.task(
            "saga",
            Task {
                status: TaskStatus::Closed,
                close_reason: Some(CloseReason::Done),
                ..child(5, "the test", id(2), TaskStatus::Closed)
            },
        );

        let ch = fx.channels[0].clone();
        let tasks: Vec<Task> = fx.tasks.iter().map(|(_, t)| t.clone()).collect();
        let all: Vec<&Task> = tasks.iter().collect();
        assert_eq!(
            counts(&ch, &tasks, &all),
            Counts {
                open: 1,
                ready: 1,
                running: 1,
                blocked: 0,
                stuck: 0,
                needs_human: 0,
                done: 1,
            }
        );
    }

    #[test]
    fn board_counts_blocked_is_open_minus_ready() {
        let mut fx = Fixture::new();
        fx.channel("api", ChannelKind::Project, None);
        fx.task("api", task(1, "one", TaskStatus::Open, 1_000));
        fx.task("api", task(2, "two", TaskStatus::Open, 1_000));
        fx.task("api", task(3, "three", TaskStatus::Open, 1_000));
        // "two" waits for "one".
        fx.tasks[1].1.blocked_by = vec![id(1)];
        let ch = fx.channels[0].clone();
        let tasks: Vec<Task> = fx.tasks.iter().map(|(_, t)| t.clone()).collect();
        let all: Vec<&Task> = tasks.iter().collect();
        let c = counts(&ch, &tasks, &all);
        assert_eq!((c.open, c.ready, c.blocked), (3, 2, 1));
    }

    #[test]
    fn board_saga_state_and_order() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("first", ChannelKind::Saga, Some("mirror"));
        fx.at("first", |c| {
            c.created = ts(1_000);
            c.gate = Some(id(1));
            c.title = Some("First".into());
        });
        fx.task("first", closed(1, saga::GATE_TITLE, CloseReason::Done));

        fx.channel("second", ChannelKind::Saga, Some("mirror"));
        fx.at("second", |c| {
            c.created = ts(900);
            c.gate = Some(id(2));
        });
        fx.task("second", needs_human(2, saga::GATE_TITLE, "waiting", 2_000));

        fx.channel("third", ChannelKind::Saga, Some("mirror"));
        fx.at("third", |c| {
            c.created = ts(800);
            c.gate = Some(id(3));
        });
        fx.task("third", closed(3, saga::GATE_TITLE, CloseReason::Wontfix));

        fx.channel("fourth", ChannelKind::Saga, Some("mirror"));
        fx.at("fourth", |c| {
            c.created = ts(700);
            c.closed = Some(ts(3_000));
        });

        let board = fx.build(&[None]);
        let sagas = &board.collections[0].projects[0].sagas;
        let names: Vec<&str> = sagas.iter().map(|s| s.channel.as_str()).collect();
        // Approved first, then proposed, then closed by created.
        assert_eq!(names, ["first", "second", "fourth", "third"]);
        assert_eq!(sagas[0].state, SagaState::Approved);
        assert_eq!(sagas[1].state, SagaState::Proposed);
        assert_eq!(sagas[2].state, SagaState::Closed);
        // The gate closed as wontfix rejects the saga.
        assert_eq!(sagas[3].state, SagaState::Closed);
        assert_eq!(sagas[0].title.as_deref(), Some("First"));
        assert_eq!(sagas[0].gate, Some(id(1)));
    }

    #[test]
    fn board_saga_done_and_total() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("elastic", ChannelKind::Saga, Some("mirror"));
        fx.at("elastic", |c| c.gate = Some(id(1)));
        fx.task("elastic", closed(1, saga::GATE_TITLE, CloseReason::Done));
        fx.task(
            "elastic",
            Task {
                kind: TaskKind::Epic,
                ..closed(2, "done epic", CloseReason::Done)
            },
        );
        fx.task(
            "elastic",
            Task {
                kind: TaskKind::Epic,
                ..closed(3, "the dropped epic", CloseReason::Wontfix)
            },
        );
        fx.task("elastic", epic(4, "the open epic", 1_400));

        let board = fx.build(&[None]);
        let saga = &board.collections[0].projects[0].sagas[0];
        assert_eq!((saga.done, saga.total), (1, 2));
        // Open first, then by created.
        let titles: Vec<&str> = saga.epics.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["the open epic", "done epic", "the dropped epic"]);
    }

    #[test]
    fn board_epic_line_reads_branch_and_pr_url() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("elastic", ChannelKind::Saga, Some("mirror"));
        fx.at("elastic", |c| c.gate = Some(id(1)));
        fx.task("elastic", closed(1, saga::GATE_TITLE, CloseReason::Done));
        fx.task(
            "elastic",
            Task {
                issue: Some("ENG-12".into()),
                properties: serde_json::from_value(json!({
                    "branch": "stack/elastic",
                    "pr_url": "https://github.com/x/y/pull/1",
                }))
                .unwrap(),
                ..epic(2, "the provisioner", 1_100)
            },
        );
        fx.task("elastic", child(3, "the adapter", id(2), TaskStatus::Open));
        fx.task(
            "elastic",
            child(4, "the client", id(2), TaskStatus::InProgress),
        );

        let board = fx.build(&[None]);
        let line = &board.collections[0].projects[0].sagas[0].epics[0];
        assert_eq!(line.channel, chan("elastic"));
        assert_eq!(line.task, id(2));
        assert_eq!(line.title, "the provisioner");
        assert_eq!(line.status, TaskStatus::InProgress);
        assert_eq!(line.issue.as_deref(), Some("ENG-12"));
        assert_eq!(line.branch.as_deref(), Some("stack/elastic"));
        assert_eq!(
            line.pr_url.as_deref(),
            Some("https://github.com/x/y/pull/1")
        );
        assert_eq!(
            line.counts,
            Counts {
                open: 1,
                ready: 1,
                running: 1,
                ..Counts::default()
            }
        );
    }

    #[test]
    fn board_epic_channel_line_reads_the_channel_issue() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("metrics", ChannelKind::Epic, Some("mirror"));
        fx.at("metrics", |c| {
            c.issue = Some("ENG-1".into());
            c.root = Some(id(1));
        });
        fx.task(
            "metrics",
            Task {
                issue: Some("ENG-9".into()),
                ..epic(1, "the mirror", 1_100)
            },
        );

        // The channel's issue wins: the hub writes the root's issue with the
        // channel's link.
        let board = fx.build(&[None]);
        assert_eq!(
            board.collections[0].projects[0].epics[0].issue.as_deref(),
            Some("ENG-1")
        );

        // Without a channel issue the root task's issue stands in.
        fx.at("metrics", |c| c.issue = None);
        let board = fx.build(&[None]);
        assert_eq!(
            board.collections[0].projects[0].epics[0].issue.as_deref(),
            Some("ENG-9")
        );
    }

    #[test]
    fn board_questions_mark_the_gate() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("elastic", ChannelKind::Saga, Some("mirror"));
        fx.at("elastic", |c| c.gate = Some(id(1)));
        fx.task("elastic", needs_human(1, saga::GATE_TITLE, "scope?", 2_000));
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.task(
            "api",
            Task {
                status: TaskStatus::Stuck,
                ..task(2, "the migration", TaskStatus::Stuck, 1_500)
            },
        );

        let board = fx.build(&[None]);
        let questions = &board.collections[0].needs_you;
        assert_eq!(questions.len(), 2);
        // Oldest first.
        assert_eq!(questions[0].channel, chan("api"));
        assert!(!questions[0].gate);
        assert_eq!(questions[0].since, ts(1_500));
        assert_eq!(questions[1].channel, chan("elastic"));
        assert!(questions[1].gate);
        assert_eq!(questions[1].attention_msg.as_deref(), Some("scope?"));
        assert_eq!(questions[1].project.as_ref(), Some(&proj("mirror")));
    }

    #[test]
    fn board_lists_the_blocked_spawns_that_bind_no_task() {
        let mut fx = Fixture::new();
        fx.now = 10_000_000;
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        // The oldest wait first. A spawn that only capacity holds back, and a
        // spawn that binds a task, are not in the list.
        fx.spawn("w1", "api", 2_000_000, "draining", None);
        fx.spawn("w2", "api", 5_000_000, "missing bundle github-work", None);
        fx.spawn("w3", "api", 0, "full (4/4)", None);
        fx.spawn("w4", "api", 100_000, "offline", Some(id(1)));

        let board = fx.build(&[None]);
        let blocked = &board.collections[0].blocked;
        assert_eq!(
            blocked.iter().map(|b| b.agent.as_str()).collect::<Vec<_>>(),
            vec!["w1", "w2"]
        );
        assert_eq!(blocked[1].reason, "missing bundle github-work");
        assert_eq!(
            blocked[1].action,
            "add bundle github-work to a host that the spawn may use, or spawn without github-work"
        );
        assert_eq!(blocked[1].channel, chan("api"));
        assert_eq!(blocked[1].project.as_ref(), Some(&proj("mirror")));
        assert_eq!(blocked[1].since, ts(5_000_000));
        assert_eq!(blocked[1].spawner, None);
    }

    #[test]
    fn board_mirror_issues_is_resolved() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.collections.insert(
            col("personal"),
            CollectionConfig {
                mirror_issues: true,
                ..CollectionConfig::default()
            },
        );
        // The collection of the project decides when the project is silent.
        fx.projects[0].collection = Some(col("personal"));
        let personal = [Some(col("personal"))];
        assert!(fx.build(&personal).collections[0].projects[0].mirror_issues);

        // The project's own setting wins.
        fx.projects[0].mirror_issues = Some(false);
        assert!(!fx.build(&personal).collections[0].projects[0].mirror_issues);

        // No collection, no setting: off.
        fx.projects[0].collection = None;
        fx.projects[0].mirror_issues = None;
        assert!(!fx.build(&[None]).collections[0].projects[0].mirror_issues);
    }

    #[test]
    fn board_task_lines_read_the_start_state() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.task("api", task(1, "the guide", TaskStatus::Open, 1_000));
        fx.task(
            "api",
            Task {
                blocked_by: vec![id(1)],
                ..task(2, "the skills", TaskStatus::Open, 1_100)
            },
        );
        // A blocker that the channel does not hold counts as open.
        fx.task(
            "api",
            Task {
                blocked_by: vec![id(9)],
                ..task(3, "the notes", TaskStatus::Open, 1_200)
            },
        );
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let project = &board.collections[0].projects[0];
        let lines = &project.task_lines;
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].title, "the guide");
        assert_eq!(lines[0].start, Some(StartState::Ready));
        assert_eq!(lines[0].waits_on, None);
        assert_eq!(lines[1].start, Some(StartState::Later));
        assert_eq!(lines[1].waits_on.as_deref(), Some("the guide"));
        assert_eq!(lines[2].start, Some(StartState::Later));
        assert_eq!(
            lines[2].waits_on.as_deref(),
            Some(id(9).to_string().as_str())
        );
        // A project that closed nothing yet.
        assert_eq!(project.done_since, Some(0));
        assert!(project.recent_done.is_empty());
    }

    #[test]
    fn board_task_lines_a_blocked_spawn_wins_over_a_later_task() {
        let mut fx = Fixture::new();
        fx.now = 10_000_000;
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.task("api", task(1, "the guide", TaskStatus::Open, 1_000));
        // "the skills" waits for "the guide" in the graph, and the spawn
        // that is bound to it waits on a bundle: the person acts on the
        // spawn.
        fx.task(
            "api",
            Task {
                blocked_by: vec![id(1)],
                ..task(2, "the skills", TaskStatus::Open, 1_100)
            },
        );
        fx.spawn(
            "w1",
            "api",
            2_000_000,
            "missing bundle github-work",
            Some(id(2)),
        );
        // A task that the graph lets start is blocked by its spawn too.
        fx.task("api", task(3, "the release notes", TaskStatus::Open, 1_200));
        fx.spawn(
            "w2",
            "api",
            2_000_000,
            "missing bundle github-work",
            Some(id(3)),
        );
        // A spawn inside the wait window explains nothing.
        fx.task("api", task(4, "the smoke test", TaskStatus::Open, 1_300));
        fx.spawn(
            "w3",
            "api",
            9_940_000,
            "missing bundle github-work",
            Some(id(4)),
        );
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        assert_eq!(lines[1].start, Some(StartState::Blocked));
        assert_eq!(
            lines[1].waits_on.as_deref(),
            Some("missing bundle github-work")
        );
        assert_eq!(lines[2].start, Some(StartState::Blocked));
        assert_eq!(lines[3].start, Some(StartState::Ready));
    }

    #[test]
    fn board_task_lines_block_when_the_owner_has_no_session() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.at("api", |c| c.owner = Some("orchestrator".parse().unwrap()));
        // An open blocker and an offline owner: the owner must come back.
        fx.task("api", task(1, "the guide", TaskStatus::Open, 1_000));
        fx.task(
            "api",
            Task {
                blocked_by: vec![id(1)],
                ..task(2, "the skills", TaskStatus::Open, 1_100)
            },
        );
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        assert_eq!(lines[0].start, Some(StartState::Blocked));
        assert_eq!(lines[0].waits_on.as_deref(), Some(ORCHESTRATOR_OFFLINE));
        assert_eq!(lines[1].start, Some(StartState::Blocked));

        // A held lease is a session, and then the graph decides.
        fx.agent("orchestrator", true);
        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        assert_eq!(lines[0].start, Some(StartState::Ready));
        assert_eq!(lines[1].start, Some(StartState::Later));

        // A lease that is not held is no session either.
        fx.agents[0].lease = None;
        let board = fx.build(&[None]);
        assert_eq!(
            board.collections[0].projects[0].task_lines[0].start,
            Some(StartState::Blocked)
        );
    }

    #[test]
    fn board_task_lines_wait_on_the_gate_of_a_saga() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("elastic", ChannelKind::Saga, Some("mirror"));
        fx.at("elastic", |c| c.gate = Some(id(1)));
        fx.task(
            "elastic",
            task(1, saga::GATE_TITLE, TaskStatus::Open, 1_000),
        );
        fx.task("elastic", epic(2, "the provisioner", 1_100));
        fx.task("elastic", child(3, "the adapter", id(2), TaskStatus::Open));
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        // The gate is the question that a person answers.
        assert_eq!(lines[0].task, id(1));
        assert_eq!(lines[0].start, Some(StartState::Blocked));
        assert_eq!(lines[0].waits_on.as_deref(), Some(YOUR_APPROVAL));
        // An epic is not work, so its line has no start state.
        assert_eq!(lines[1].task, id(2));
        assert_eq!(lines[1].kind, TaskKind::Epic);
        assert_eq!(lines[1].start, None);
        // The task below the epic waits for the gate.
        assert_eq!(lines[2].task, id(3));
        assert_eq!(lines[2].start, Some(StartState::Later));
        assert_eq!(lines[2].waits_on.as_deref(), Some(YOUR_APPROVAL));

        // The gate closed as done approves the saga, and the task starts.
        fx.tasks.retain(|(_, task)| task.id != id(1));
        fx.task("elastic", closed(1, saga::GATE_TITLE, CloseReason::Done));
        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        assert_eq!(lines[0].task, id(2));
        assert_eq!(lines[1].task, id(3));
        assert_eq!(lines[1].start, Some(StartState::Ready));
    }

    #[test]
    fn board_task_lines_leave_out_a_closed_saga_or_epic() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("old-saga", ChannelKind::Saga, Some("mirror"));
        fx.at("old-saga", |c| c.closed = Some(ts(5_000)));
        fx.task("old-saga", task(1, "the leftover", TaskStatus::Open, 1_000));
        fx.channel("old-epic", ChannelKind::Epic, Some("mirror"));
        fx.at("old-epic", |c| c.closed = Some(ts(5_000)));
        fx.task("old-epic", epic(2, "the old epic", 1_100));
        // A closed project channel holds closed history: no line, and its
        // task is one of the done tasks.
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.at("api", |c| c.closed = Some(ts(5_000)));
        fx.task("api", closed(3, "the last one", CloseReason::Done));
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let project = &board.collections[0].projects[0];
        assert!(project.task_lines.is_empty());
        assert_eq!(project.done_since, Some(1));
        assert_eq!(project.recent_done.len(), 1);
    }

    #[test]
    fn board_task_lines_count_the_done_tasks_since_the_cutoff() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.at("api", |c| c.owner = Some("orchestrator".parse().unwrap()));
        fx.task("api", closed_at(4, "a while ago", 1_000));
        fx.task("api", closed_at(5, "yesterday", 2_000));
        fx.task(
            "api",
            Task {
                claimed_by: Some("w1".parse().unwrap()),
                ..closed_at(6, "just now", 3_000)
            },
        );
        // A wontfix close is not done, and an epic is not work.
        fx.task(
            "api",
            Task {
                close_reason: Some(CloseReason::Wontfix),
                ..closed_at(7, "dropped", 3_500)
            },
        );
        fx.task(
            "api",
            Task {
                kind: TaskKind::Epic,
                ..closed_at(8, "the old epic", 3_600)
            },
        );
        // Two closed at or after the cutoff: "just now" and 3_500 is a
        // wontfix, so one. The newest close comes first.
        let project = &fx.with_lines(2_500).build(&[None]).collections[0].projects[0];
        assert_eq!(project.done_since, Some(1));
        let titles: Vec<&str> = project
            .recent_done
            .iter()
            .map(|line| line.title.as_str())
            .collect();
        assert_eq!(titles, ["just now", "yesterday", "a while ago"]);
        // The claimer when one held the task, the channel's owner otherwise.
        assert_eq!(
            project.recent_done[0].closer.as_ref().map(|a| a.as_str()),
            Some("w1")
        );
        assert_eq!(
            project.recent_done[1].closer.as_ref().map(|a| a.as_str()),
            Some("orchestrator")
        );
        assert_eq!(project.recent_done[1].at, ts(2_000));
    }

    #[test]
    fn board_task_lines_keep_the_last_ten_done_tasks() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        for n in 1..=12 {
            let at = 1_000 * i64::from(n);
            fx.task("api", closed_at(n, &format!("the task {n}"), at));
        }
        let project = &fx.with_lines(0).build(&[None]).collections[0].projects[0];
        assert_eq!(project.done_since, Some(12));
        assert_eq!(project.recent_done.len(), 10);
        assert_eq!(project.recent_done[0].title, "the task 12");
        assert_eq!(project.recent_done[9].title, "the task 3");
    }

    #[test]
    fn board_task_lines_read_since_from_the_status_event() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        // The record says 5_000; its last status change was at 6_000.
        fx.task(
            "api",
            Task {
                updated: ts(5_000),
                ..needs_human(1, "the migration", "a person must pick a host", 5_000)
            },
        );
        // No status event: the record's own time stands in.
        fx.task(
            "api",
            Task {
                claimed_by: Some("w1".parse().unwrap()),
                updated: ts(4_000),
                ..task(2, "the guide", TaskStatus::InProgress, 1_000)
            },
        );
        fx.status_at(id(1), 6_000);
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        assert_eq!(lines[0].since, ts(6_000));
        assert_eq!(lines[0].version, ts(5_000));
        // Only an open task starts.
        assert_eq!(lines[0].start, None);
        assert_eq!(
            lines[0].attention_msg.as_deref(),
            Some("a person must pick a host")
        );
        assert_eq!(lines[1].since, ts(4_000));
        assert_eq!(lines[1].version, ts(4_000));
        assert_eq!(lines[1].claimed_by.as_ref().map(|a| a.as_str()), Some("w1"));
    }

    #[test]
    fn board_task_lines_read_the_pr_watch() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.task(
            "api",
            Task {
                properties: props("https://github.com/x/y/pull/1"),
                ..task(1, "the guide", TaskStatus::Open, 1_000)
            },
        );
        // A pr_url that the watch has not read yet.
        fx.task(
            "api",
            Task {
                properties: props("https://github.com/x/y/pull/2"),
                ..task(2, "the skills", TaskStatus::Open, 1_100)
            },
        );
        // A watch read for a task that names no change request.
        fx.task("api", task(3, "the notes", TaskStatus::Open, 1_200));
        fx.pr("api", id(1), watched(ChangeState::Open));
        fx.pr("api", id(3), watched(ChangeState::Merged));
        fx.with_lines(0);

        let board = fx.build(&[None]);
        let lines = &board.collections[0].projects[0].task_lines;
        let pr = lines[0].pr.as_ref().expect("the line of the PR");
        assert_eq!(pr.url, "https://github.com/x/y/pull/1");
        assert_eq!(pr.state, ChangeState::Open);
        assert_eq!(pr.checks, CheckState::Pass {});
        assert_eq!((pr.checks_passed, pr.checks_total), (Some(4), Some(4)));
        assert_eq!(pr.review, ReviewState::None);
        assert!(!pr.review_requested);
        assert!(lines[1].pr.is_none());
        assert!(lines[2].pr.is_none());
    }

    #[test]
    fn board_without_task_lines_is_todays_reply() {
        let mut fx = Fixture::new();
        fx.project("mirror");
        fx.channel("api", ChannelKind::Project, Some("mirror"));
        fx.task("api", task(1, "the guide", TaskStatus::Open, 1_000));
        fx.task("api", closed(2, "the old one", CloseReason::Done));
        // An older reader reads a project with these keys only.
        let json = serde_json::to_value(fx.build(&[None])).unwrap();
        let mut keys: Vec<&str> = json["collections"][0]["projects"][0]
            .as_object()
            .expect("the project")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "epics",
                "mirror_issues",
                "orchestrators",
                "project",
                "sagas",
                "tasks",
            ]
        );

        // With `--tasks` the project carries the lines, the count, and the
        // last done tasks.
        fx.with_lines(0);
        let json = serde_json::to_value(fx.build(&[None])).unwrap();
        let project = &json["collections"][0]["projects"][0];
        assert_eq!(project["task_lines"][0]["title"], "the guide");
        assert_eq!(project["task_lines"][0]["start"], "ready");
        assert_eq!(project["done_since"], 1);
        assert!(project["recent_done"].is_array());
    }
}
