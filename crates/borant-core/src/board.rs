//! The board (spec: sagas and epics, "API, MCP, and CLI"): the projects,
//! sagas, epics, and questions of each collection, as one computed read. The
//! hub reads the records and calls [`build`]; nothing here stores anything.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, ChannelName, CollectionName, HostName, ProjectName, TaskId};
use crate::model::{
    AgentStatus, Channel, ChannelKind, CloseReason, LeaseState, Project, Task, TaskKind, TaskStatus,
};
use crate::rules::roles::{self, CollectionConfig};
use crate::rules::saga;
use crate::time::Timestamp;

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
}

/// The records that the board reads, and the time that it shows.
#[derive(Debug)]
pub struct BoardInput<'a> {
    pub at: Timestamp,
    pub projects: &'a [Project],
    pub channels: &'a [Channel],
    /// Every task of every channel.
    pub tasks: &'a [(ChannelName, Task)],
    /// The live view of the agents.
    pub agents: &'a [AgentStatus],
    /// The `[collections.<name>]` of `hub.toml`.
    pub collections: &'a BTreeMap<CollectionName, CollectionConfig>,
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
            });
        }
    }
    // As `borant inbox` does: the question that has waited longest first.
    needs_you.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.task.cmp(&b.task)));

    CollectionBoard {
        collection: collection.cloned(),
        projects,
        needs_you,
    }
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
    Some((epic, epic_line(channel, epic, tasks)))
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
    use serde_json::json;

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
        agents: Vec<AgentStatus>,
        collections: BTreeMap<CollectionName, CollectionConfig>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                projects: Vec::new(),
                channels: Vec::new(),
                tasks: Vec::new(),
                agents: Vec::new(),
                collections: BTreeMap::new(),
            }
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
            build(
                &BoardInput {
                    at: ts(9_000),
                    projects: &self.projects,
                    channels: &self.channels,
                    tasks: &self.tasks,
                    agents: &self.agents,
                    collections: &self.collections,
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
}
