//! Sagas, epics, and the approval gate (spec: sagas and epics). A saga is a
//! channel with a gate; an epic is a task that holds tasks and is never work.

use std::collections::{HashMap, HashSet};

use crate::error::CoreError;
use crate::ids::{ProjectName, TaskId};
use crate::model::{Channel, ChannelKind, CloseReason, NewTask, Task, TaskKind, TaskStatus};
use crate::rules::tasks::open_blockers;

/// The title of the approval task that the hub writes with a saga.
pub const GATE_TITLE: &str = "Approve the scope and the epics";

/// A saga and an epic channel name a project; a project channel does not.
pub fn check_channel(kind: ChannelKind, project: Option<&ProjectName>) -> Result<(), CoreError> {
    if project.is_some() {
        return Ok(());
    }
    match kind {
        ChannelKind::Project => Ok(()),
        ChannelKind::Saga => Err(CoreError::Structure(
            "a saga channel needs a project".into(),
        )),
        ChannelKind::Epic => Err(CoreError::Structure(
            "an epic channel needs a project".into(),
        )),
    }
}

/// Rule 2: a saga is approved only when its gate is closed as `done`. A gate
/// id with no task fails closed.
pub fn approved(channel: &Channel, tasks: &[Task]) -> bool {
    match channel.gate {
        None => true,
        Some(gate) => tasks.iter().any(|task| {
            task.id == gate
                && task.status == TaskStatus::Closed
                && task.close_reason == Some(CloseReason::Done)
        }),
    }
}

/// Rule 2: `task_list(ready_only)` returns no task before the approval.
pub fn ready_in(channel: &Channel, tasks: &[Task]) -> Vec<TaskId> {
    if approved(channel, tasks) {
        crate::rules::tasks::ready(tasks)
    } else {
        Vec::new()
    }
}

/// Rule 1 and the three kinds: the parent that a create stores, or the reason
/// it is refused.
///
/// - A project channel refuses an epic.
/// - In a saga an epic has no parent, and every other task hangs under an
///   epic. The gate is not an epic.
/// - An epic channel has one root epic; a second epic is refused, and a task
///   without a parent gets the root.
pub fn check_create(
    channel: &Channel,
    tasks: &[Task],
    new: &NewTask,
) -> Result<Option<TaskId>, CoreError> {
    if channel.closed.is_some() {
        return Err(CoreError::ChannelClosed(channel.name.clone()));
    }
    match channel.kind {
        ChannelKind::Project => {
            if new.kind == TaskKind::Epic {
                return Err(CoreError::Structure(
                    "a project channel holds no epic".into(),
                ));
            }
            Ok(new.parent)
        }
        ChannelKind::Saga => {
            if new.kind == TaskKind::Epic {
                return match new.parent {
                    None => Ok(None),
                    Some(_) => Err(CoreError::Structure(
                        "an epic of a saga has no parent".into(),
                    )),
                };
            }
            let Some(parent) = new.parent else {
                return Err(CoreError::Structure(format!(
                    "a task of saga #{} needs a parent under an epic",
                    channel.name
                )));
            };
            if reaches_epic(tasks, parent) {
                Ok(Some(parent))
            } else {
                Err(CoreError::Structure(format!(
                    "the parent of a task of saga #{} must hang under an epic",
                    channel.name
                )))
            }
        }
        ChannelKind::Epic => {
            if new.kind == TaskKind::Epic {
                return Err(CoreError::Structure(format!(
                    "epic channel #{} holds one root epic already",
                    channel.name
                )));
            }
            let parent = match new.parent.or(channel.root) {
                Some(parent) => parent,
                None => {
                    return Err(CoreError::Structure(format!(
                        "epic channel #{} has no root epic",
                        channel.name
                    )));
                }
            };
            if reaches_epic(tasks, parent) {
                Ok(Some(parent))
            } else {
                Err(CoreError::Structure(format!(
                    "the parent of a task of epic channel #{} must hang under its root",
                    channel.name
                )))
            }
        }
    }
}

/// A `task_update` with `parent`: outside a project channel an epic and the
/// gate take no parent, and the new parent's chain must reach an epic.
pub fn check_parent(
    channel: &Channel,
    tasks: &[Task],
    task: &Task,
    parent: TaskId,
) -> Result<(), CoreError> {
    if channel.kind == ChannelKind::Project {
        return Ok(());
    }
    if task.kind == TaskKind::Epic {
        return Err(CoreError::Structure("an epic has no parent".into()));
    }
    if channel.gate == Some(task.id) {
        return Err(CoreError::Structure("the gate has no parent".into()));
    }
    if reaches_epic(tasks, parent) {
        Ok(())
    } else {
        Err(CoreError::Structure(format!(
            "the parent of task {} must hang under an epic",
            task.id
        )))
    }
}

/// Rules 2 and 5: a claim is refused for a closed channel, an epic, an
/// unapproved saga, and, in a saga or an epic channel, a blocked task, in that
/// order.
pub fn check_claim(channel: &Channel, tasks: &[Task], task: &Task) -> Result<(), CoreError> {
    if channel.closed.is_some() {
        return Err(CoreError::ChannelClosed(channel.name.clone()));
    }
    if task.kind == TaskKind::Epic {
        return Err(CoreError::EpicNotWork(task.id));
    }
    if !approved(channel, tasks) {
        return Err(CoreError::SagaNotApproved(channel.name.clone()));
    }
    check_blocks(channel, tasks, task)
}

/// Rules 2 and 5: a spawn waits for the gate, and a bound task is neither an
/// epic nor blocked.
pub fn check_spawn(
    channel: &Channel,
    tasks: &[Task],
    bound: Option<&Task>,
) -> Result<(), CoreError> {
    if channel.closed.is_some() {
        return Err(CoreError::ChannelClosed(channel.name.clone()));
    }
    if !approved(channel, tasks) {
        return Err(CoreError::SagaNotApproved(channel.name.clone()));
    }
    let Some(task) = bound else {
        return Ok(());
    };
    if task.kind == TaskKind::Epic {
        return Err(CoreError::EpicNotWork(task.id));
    }
    check_blocks(channel, tasks, task)
}

/// Choice C1: only a saga or an epic channel checks the blocks on a claim or
/// a bound spawn, and only for an open task, so a project channel keeps
/// today's claims.
fn check_blocks(channel: &Channel, tasks: &[Task], task: &Task) -> Result<(), CoreError> {
    if !matches!(channel.kind, ChannelKind::Saga | ChannelKind::Epic)
        || task.status != TaskStatus::Open
    {
        return Ok(());
    }
    let by = open_blockers(tasks, task.id);
    if by.is_empty() {
        Ok(())
    } else {
        Err(CoreError::Blocked { task: task.id, by })
    }
}

/// The task itself when it is an epic, else the first epic up its chain.
pub fn epic_of(tasks: &[Task], id: TaskId) -> Option<&Task> {
    let by_id = by_id(tasks);
    let mut seen = HashSet::new();
    let mut cursor = Some(id);
    while let Some(current) = cursor {
        if !seen.insert(current) {
            return None;
        }
        let task = by_id.get(&current)?;
        if task.kind == TaskKind::Epic {
            return Some(task);
        }
        cursor = task.parent;
    }
    None
}

/// Every task below `id`, without epics.
pub fn descendants(tasks: &[Task], id: TaskId) -> Vec<&Task> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut queue = vec![id];
    seen.insert(id);
    while let Some(current) = queue.pop() {
        for task in tasks.iter().filter(|t| t.parent == Some(current)) {
            if !seen.insert(task.id) {
                continue;
            }
            queue.push(task.id);
            if task.kind != TaskKind::Epic {
                out.push(task);
            }
        }
    }
    out
}

/// Rule 5 and choice C2: the epic's own status when it is closed, stuck, or
/// needs a human, else `in_progress` when one descendant is not open, else
/// `open`. An epic with no children is `open`.
pub fn epic_status(epic: &Task, tasks: &[Task]) -> TaskStatus {
    if matches!(
        epic.status,
        TaskStatus::Closed | TaskStatus::Stuck | TaskStatus::NeedsHuman
    ) {
        return epic.status;
    }
    if descendants(tasks, epic.id)
        .iter()
        .any(|task| task.status != TaskStatus::Open)
    {
        TaskStatus::InProgress
    } else {
        TaskStatus::Open
    }
}

/// Rule 7: the `branch` property of the nearest epic above `id`, when it is a
/// non-empty string.
pub fn epic_branch(tasks: &[Task], id: TaskId) -> Option<String> {
    epic_of(tasks, id)?
        .properties
        .get("branch")?
        .as_str()
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
}

fn by_id(tasks: &[Task]) -> HashMap<TaskId, &Task> {
    tasks.iter().map(|t| (t.id, t)).collect()
}

/// Whether the chain of parents of `id`, `id` included, reaches an epic.
fn reaches_epic(tasks: &[Task], id: TaskId) -> bool {
    let by_id = by_id(tasks);
    let mut seen = HashSet::new();
    let mut cursor = Some(id);
    while let Some(current) = cursor {
        if !seen.insert(current) {
            return false;
        }
        let Some(task) = by_id.get(&current) else {
            return false;
        };
        if task.kind == TaskKind::Epic {
            return true;
        }
        cursor = task.parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;
    use serde_json::json;

    fn channel(name: &str, kind: ChannelKind) -> Channel {
        Channel {
            name: name.parse().unwrap(),
            owner: None,
            schema: serde_json::Value::Null,
            created: Timestamp(0),
            closed: None,
            project: Some("web".parse().unwrap()),
            branch: Some("main".into()),
            collection: None,
            paused: false,
            kind,
            title: None,
            issue: None,
            gate: None,
            root: None,
        }
    }

    fn task(title: &str, kind: TaskKind, status: TaskStatus, parent: Option<TaskId>) -> Task {
        Task {
            id: TaskId::new(),
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
            parent,
            kind,
            issue: None,
            issue_target: None,
            properties: Default::default(),
            created: Timestamp(0),
            updated: Timestamp(0),
            create_key: None,
        }
    }

    fn epic(parent: Option<TaskId>) -> Task {
        task("an epic", TaskKind::Epic, TaskStatus::Open, parent)
    }

    fn child(parent: TaskId) -> Task {
        task("a task", TaskKind::Task, TaskStatus::Open, Some(parent))
    }

    fn new(kind: TaskKind, parent: Option<TaskId>) -> NewTask {
        NewTask {
            title: "a task".into(),
            kind,
            parent,
            ..NewTask::default()
        }
    }

    fn closed_as(mut task: Task, reason: CloseReason) -> Task {
        task.status = TaskStatus::Closed;
        task.close_reason = Some(reason);
        task
    }

    /// Rule 2: only a gate closed as `done` approves the saga, and a gate id
    /// with no task fails closed.
    #[test]
    fn saga_unapproved_until_the_gate_is_done() {
        let mut ch = channel("one", ChannelKind::Saga);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        ch.gate = Some(gate.id);

        assert!(!approved(&ch, std::slice::from_ref(&gate)), "open");
        let waiting = task(GATE_TITLE, TaskKind::Task, TaskStatus::NeedsHuman, None);
        assert!(
            !approved(&ch, std::slice::from_ref(&waiting)),
            "needs_human"
        );
        assert!(
            !approved(&ch, &[closed_as(gate.clone(), CloseReason::Wontfix)]),
            "wontfix rejects the saga"
        );
        assert!(approved(&ch, &[closed_as(gate.clone(), CloseReason::Done)]));
        assert!(!approved(&ch, &[]), "a gate with no task fails closed");

        // A channel without a gate, and every project channel, is approved.
        let mut plain = channel("two", ChannelKind::Project);
        assert!(approved(&plain, &[]));
        plain.gate = None;
        assert!(approved(&plain, &[]));
    }

    /// Rule 2: `task_list(ready_only)` is empty before the approval.
    #[test]
    fn saga_ready_in_is_empty_before_approval() {
        let mut ch = channel("one", ChannelKind::Saga);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        let one = epic(None);
        let work = child(one.id);
        ch.gate = Some(gate.id);
        let tasks = [gate.clone(), one, work.clone()];

        assert!(ready_in(&ch, &tasks).is_empty());
        let done = closed_as(gate, CloseReason::Done);
        assert_eq!(
            ready_in(&ch, &[done, tasks[1].clone(), work.clone()]),
            vec![work.id],
            "an epic is never ready"
        );
    }

    /// Rule 1 and the three kinds: what a create may store in each channel.
    #[test]
    fn saga_create_rules_by_kind() {
        let project = channel("p", ChannelKind::Project);
        assert_eq!(
            check_create(&project, &[], &new(TaskKind::Epic, None)),
            Err(CoreError::Structure(
                "a project channel holds no epic".into()
            ))
        );
        let parent = TaskId::new();
        assert_eq!(
            check_create(&project, &[], &new(TaskKind::Task, Some(parent))),
            Ok(Some(parent))
        );

        let saga = channel("s", ChannelKind::Saga);
        let one = epic(None);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        let tasks = [one.clone(), gate.clone(), child(one.id)];
        assert_eq!(
            check_create(&saga, &tasks, &new(TaskKind::Epic, None)),
            Ok(None)
        );
        assert!(check_create(&saga, &tasks, &new(TaskKind::Epic, Some(one.id))).is_err());
        assert!(
            check_create(&saga, &tasks, &new(TaskKind::Task, None)).is_err(),
            "a task in a saga needs a parent"
        );
        assert!(
            check_create(&saga, &tasks, &new(TaskKind::Task, Some(gate.id))).is_err(),
            "the gate is not an epic"
        );
        assert_eq!(
            check_create(&saga, &tasks, &new(TaskKind::Task, Some(one.id))),
            Ok(Some(one.id))
        );

        let root = epic(None);
        let mut epic_channel = channel("e", ChannelKind::Epic);
        epic_channel.root = Some(root.id);
        assert!(
            check_create(
                &epic_channel,
                std::slice::from_ref(&root),
                &new(TaskKind::Epic, None)
            )
            .is_err()
        );
        assert_eq!(
            check_create(
                &epic_channel,
                std::slice::from_ref(&root),
                &new(TaskKind::Task, None)
            ),
            Ok(Some(root.id)),
            "a task without a parent gets the root"
        );

        let mut closed = saga.clone();
        closed.closed = Some(Timestamp(9));
        assert_eq!(
            check_create(&closed, &tasks, &new(TaskKind::Task, Some(one.id))),
            Err(CoreError::ChannelClosed("s".parse().unwrap()))
        );
    }

    /// The epic channel has one root; every plain task hangs under it.
    #[test]
    fn saga_epic_channel_parents_to_the_root() {
        let root = epic(None);
        let child = child(root.id);
        let mut ch = channel("e", ChannelKind::Epic);
        ch.root = Some(root.id);
        let tasks = [root.clone(), child.clone()];

        assert_eq!(
            check_create(&ch, &tasks, &new(TaskKind::Task, None)),
            Ok(Some(root.id))
        );
        assert_eq!(
            check_create(&ch, &tasks, &new(TaskKind::Task, Some(child.id))),
            Ok(Some(child.id))
        );
        assert!(
            check_create(&ch, &tasks, &new(TaskKind::Epic, None)).is_err(),
            "the root is the only epic"
        );
    }

    /// A `task_update` with `parent` keeps the chain: in a saga every task
    /// hangs under an epic, and the gate and the epic hang under nothing.
    #[test]
    fn saga_check_parent_keeps_the_chain() {
        let project = channel("p", ChannelKind::Project);
        let loose = task("loose", TaskKind::Task, TaskStatus::Open, None);
        let target = TaskId::new();
        assert_eq!(check_parent(&project, &[], &loose, target), Ok(()));

        let one = epic(None);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        let mut saga = channel("s", ChannelKind::Saga);
        saga.gate = Some(gate.id);
        let work = child(one.id);
        let tasks = [one.clone(), gate.clone(), work.clone()];

        assert!(
            check_parent(&saga, &tasks, &one, work.id).is_err(),
            "an epic has no parent"
        );
        assert!(
            check_parent(&saga, &tasks, &gate, one.id).is_err(),
            "the gate has no parent"
        );
        assert_eq!(check_parent(&saga, &tasks, &work, one.id), Ok(()));
        assert!(
            check_parent(&saga, &tasks, &loose, gate.id).is_err(),
            "the chain must reach an epic"
        );
        assert!(check_parent(&saga, &tasks, &loose, work.id).is_ok());

        let mut epic_channel = channel("e", ChannelKind::Epic);
        epic_channel.root = Some(one.id);
        assert!(check_parent(&epic_channel, &tasks, &loose, one.id).is_ok());
        assert!(check_parent(&epic_channel, &tasks, &loose, gate.id).is_err());
    }

    /// Rule 2 and 5: a claim is refused for a closed channel, an epic, an
    /// unapproved saga, and a blocked task, in that order.
    #[test]
    fn saga_claim_order() {
        let mut ch = channel("s", ChannelKind::Saga);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        let one = epic(None);
        let mut work = child(one.id);
        let blocker = task("blocker", TaskKind::Task, TaskStatus::Open, Some(one.id));
        work.blocked_by = vec![blocker.id];
        ch.gate = Some(gate.id);
        let tasks = [gate.clone(), one.clone(), work.clone(), blocker.clone()];

        let mut closed = ch.clone();
        closed.closed = Some(Timestamp(3));
        assert_eq!(
            check_claim(&closed, &tasks, &one),
            Err(CoreError::ChannelClosed("s".parse().unwrap()))
        );
        assert_eq!(
            check_claim(&ch, &tasks, &one),
            Err(CoreError::EpicNotWork(one.id))
        );
        assert_eq!(
            check_claim(&ch, &tasks, &work),
            Err(CoreError::SagaNotApproved("s".parse().unwrap()))
        );

        ch.gate = Some(closed_as(gate.clone(), CloseReason::Done).id);
        let tasks = [
            closed_as(gate, CloseReason::Done),
            one.clone(),
            work.clone(),
            blocker.clone(),
        ];
        assert_eq!(
            check_claim(&ch, &tasks, &work),
            Err(CoreError::Blocked {
                task: work.id,
                by: vec![blocker.id]
            })
        );
        assert_eq!(check_claim(&ch, &tasks, &blocker), Ok(()));
    }

    /// Choice C1: only a saga or an epic channel checks the blocks on a
    /// claim, so a project channel claims as today.
    #[test]
    fn saga_claim_checks_blocks_only_in_saga_and_epic_channels() {
        let blocker = task("blocker", TaskKind::Task, TaskStatus::Open, None);
        let mut work = task("work", TaskKind::Task, TaskStatus::Open, None);
        work.blocked_by = vec![blocker.id];
        let tasks = [blocker.clone(), work.clone()];

        let project = channel("p", ChannelKind::Project);
        assert_eq!(check_claim(&project, &tasks, &work), Ok(()));

        let saga = channel("s", ChannelKind::Saga);
        assert_eq!(
            check_claim(&saga, &tasks, &work),
            Err(CoreError::Blocked {
                task: work.id,
                by: vec![blocker.id]
            })
        );

        // A task that is already claimed is not checked for blocks.
        let mut taken = work.clone();
        taken.status = TaskStatus::InProgress;
        assert_eq!(check_claim(&saga, &tasks, &taken), Ok(()));
    }

    /// Rule 5: no spawn binds to an epic, and a spawn waits for the gate and
    /// for the blocks of the bound task.
    #[test]
    fn saga_spawn_refuses_an_epic_binding() {
        let mut ch = channel("s", ChannelKind::Saga);
        let gate = task(GATE_TITLE, TaskKind::Task, TaskStatus::Open, None);
        let one = epic(None);
        let blocker = task("blocker", TaskKind::Task, TaskStatus::Open, Some(one.id));
        let mut work = child(one.id);
        work.blocked_by = vec![blocker.id];
        ch.gate = Some(gate.id);
        let tasks = [gate.clone(), one.clone(), work.clone(), blocker.clone()];

        assert_eq!(
            check_spawn(&ch, &tasks, None),
            Err(CoreError::SagaNotApproved("s".parse().unwrap())),
            "an unbound spawn waits for the gate"
        );

        let approved_gate = closed_as(gate.clone(), CloseReason::Done);
        let tasks = [approved_gate, one.clone(), work.clone(), blocker.clone()];
        assert_eq!(
            check_spawn(&ch, &tasks, Some(&one)),
            Err(CoreError::EpicNotWork(one.id))
        );
        assert_eq!(
            check_spawn(&ch, &tasks, Some(&work)),
            Err(CoreError::Blocked {
                task: work.id,
                by: vec![blocker.id]
            })
        );
        assert_eq!(check_spawn(&ch, &tasks, Some(&blocker)), Ok(()));
        assert_eq!(check_spawn(&ch, &tasks, None), Ok(()));

        let mut closed = ch.clone();
        closed.closed = Some(Timestamp(4));
        assert_eq!(
            check_spawn(&closed, &tasks, None),
            Err(CoreError::ChannelClosed("s".parse().unwrap()))
        );
    }

    /// Rule 5 and choice C2: the status of an epic follows every descendant,
    /// not only its direct children.
    #[test]
    fn saga_epic_status_from_descendants() {
        let one = epic(None);
        let work = child(one.id);
        let deep = child(work.id);
        assert_eq!(
            epic_status(&one, std::slice::from_ref(&one)),
            TaskStatus::Open
        );

        let tasks = [one.clone(), work.clone(), deep.clone()];
        assert_eq!(epic_status(&one, &tasks), TaskStatus::Open);

        let mut running = deep.clone();
        running.status = TaskStatus::InProgress;
        let tasks = [one.clone(), work.clone(), running];
        assert_eq!(
            epic_status(&one, &tasks),
            TaskStatus::InProgress,
            "a grandchild that is not open"
        );

        for status in [
            TaskStatus::Closed,
            TaskStatus::Stuck,
            TaskStatus::NeedsHuman,
        ] {
            let mut mine = one.clone();
            mine.status = status;
            assert_eq!(
                epic_status(&mine, &[mine.clone(), deep.clone()]),
                status,
                "the epic's own status wins"
            );
        }
    }

    /// Rule 7: the branch of the nearest epic above the task.
    #[test]
    fn saga_epic_branch_reads_the_nearest_epic() {
        let mut one = epic(None);
        one.properties.insert("branch".into(), json!("epic/one"));
        let work = child(one.id);
        let deep = child(work.id);
        let tasks = [one.clone(), work.clone(), deep.clone()];

        assert_eq!(epic_branch(&tasks, one.id).as_deref(), Some("epic/one"));
        assert_eq!(epic_branch(&tasks, deep.id).as_deref(), Some("epic/one"));

        let mut blank = one.clone();
        blank.properties.insert("branch".into(), json!(""));
        assert_eq!(epic_branch(&[blank, work.clone()], work.id), None);

        let mut plain = task("plain", TaskKind::Task, TaskStatus::Open, None);
        plain.properties.insert("branch".into(), json!("feat/x"));
        assert_eq!(epic_branch(&[plain.clone()], plain.id), None);
        assert_eq!(epic_branch(&tasks, TaskId::new()), None);
    }

    /// A saga and an epic channel name a project; a project channel does not.
    #[test]
    fn saga_check_channel_needs_a_project() {
        assert_eq!(check_channel(ChannelKind::Project, None), Ok(()));
        assert_eq!(
            check_channel(ChannelKind::Saga, None),
            Err(CoreError::Structure(
                "a saga channel needs a project".into()
            ))
        );
        assert_eq!(
            check_channel(ChannelKind::Epic, None),
            Err(CoreError::Structure(
                "an epic channel needs a project".into()
            ))
        );
        let web = "web".parse().unwrap();
        assert_eq!(check_channel(ChannelKind::Saga, Some(&web)), Ok(()));
        assert_eq!(check_channel(ChannelKind::Epic, Some(&web)), Ok(()));
    }
}
