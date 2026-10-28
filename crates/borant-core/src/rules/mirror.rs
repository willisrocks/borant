//! The mirror rules: which items of a channel go out to an issue tracker,
//! what the mirror writes for each, and the hash that says whether it writes
//! again. Pure: no I/O, and no tracker.

use std::collections::HashSet;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::ids::{ChannelName, TaskId};
use crate::model::{Channel, ChannelKind, CloseReason, Task, TaskKind, TaskStatus};
use crate::ports::issues::{IssueState, IssueWrite};
use crate::rules::saga;
use crate::time::Timestamp;

/// The property that a task gets when its change request opens. A task with a
/// change request is mirrored.
pub const PR_URL: &str = "pr_url";

/// One mirrored item of a channel.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ItemId {
    Channel(ChannelName),
    Task(ChannelName, TaskId),
}

impl ItemId {
    /// The Borant id of the item, as the footer and the create key use it:
    /// `<channel>` for a channel, `<channel>/<task>` for a task.
    pub fn borant_id(&self) -> String {
        match self {
            Self::Channel(channel) => channel.to_string(),
            Self::Task(channel, task) => format!("{channel}/{task}"),
        }
    }
}

/// One item that the mirror writes, before it reads the KV record.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: ItemId,
    /// The nearest mirrored ancestor.
    pub parent: Option<ItemId>,
    pub depth: u32,
    pub title: String,
    pub body: String,
    pub state: IssueState,
    /// The own or the inherited team.
    pub target: Option<String>,
    /// The `issue` of the item today.
    pub link: Option<String>,
    pub closed: bool,
    /// The `updated` of a task, the `created` of a channel.
    pub order: Timestamp,
    /// The task that gets `needs_human` when the item has no team: the task
    /// itself, the gate of a saga, or the root of an epic channel.
    pub ask: TaskId,
}

/// The scope question of a saga, the root of an epic channel, or nothing.
pub fn anchor(channel: &Channel) -> Option<TaskId> {
    match channel.kind {
        ChannelKind::Saga => channel.gate,
        ChannelKind::Epic => channel.root,
        ChannelKind::Project => None,
    }
}

/// Every mirrored item of one channel, sorted by depth, then by order, so
/// parents come before children and the oldest first. A channel without a
/// project gives nothing.
pub fn items(channel: &Channel, tasks: &[Task]) -> Vec<Item> {
    if channel.project.is_none() {
        return Vec::new();
    }
    let mut out: Vec<Item> = container_item(channel, tasks).into_iter().collect();
    for task in tasks {
        if !mirrored(channel, task) {
            continue;
        }
        out.push(task_item(channel, tasks, task));
    }
    out.sort_by_key(|item| (item.depth, item.order));
    out
}

/// Whether the mirror writes an issue for this task.
pub fn mirrored(channel: &Channel, task: &Task) -> bool {
    match channel.kind {
        ChannelKind::Saga if channel.gate == Some(task.id) => false,
        ChannelKind::Epic if channel.root == Some(task.id) => false,
        ChannelKind::Saga if task.kind == TaskKind::Epic => true,
        _ => change_request(task).is_some() || task.issue.is_some(),
    }
}

/// The `pr_url` property of a task, when it is a non-empty string.
pub fn change_request(task: &Task) -> Option<&str> {
    task.properties
        .get(PR_URL)?
        .as_str()
        .filter(|url| !url.is_empty())
}

/// The state of a plain task.
pub fn task_state(task: &Task) -> IssueState {
    match task.status {
        TaskStatus::Open => IssueState::Todo,
        TaskStatus::InProgress | TaskStatus::NeedsHuman | TaskStatus::Stuck => IssueState::Started,
        TaskStatus::Closed => match task.close_reason {
            Some(CloseReason::Done) => IssueState::Done,
            _ => IssueState::Canceled,
        },
    }
}

/// The state of a container: a saga, an epic task, or a standalone epic
/// channel. The first rule that matches wins. `epic` `None` means the channel
/// itself.
pub fn container_state(channel: &Channel, tasks: &[Task], epic: Option<&Task>) -> IssueState {
    if let Some(epic) = epic {
        // Rule 1: a closed epic task takes its own close reason.
        if epic.status == TaskStatus::Closed {
            return closed_state(epic.close_reason);
        }
        // Rule 3: work has begun when a child is not open and not canceled.
        if saga::descendants(tasks, epic.id)
            .iter()
            .any(|task| counts_as_started(task))
        {
            return IssueState::Started;
        }
        return IssueState::Todo;
    }
    let gate = || {
        channel
            .gate
            .and_then(|id| tasks.iter().find(|task| task.id == id))
    };
    let root = || {
        channel
            .root
            .and_then(|id| tasks.iter().find(|task| task.id == id))
    };
    match channel.kind {
        ChannelKind::Saga => {
            // Rule 1: a saga rejected at its gate is canceled, any other
            // closed saga is done.
            if channel.closed.is_some() {
                let rejected = gate().is_some_and(|task| task.status == TaskStatus::Closed)
                    && gate().and_then(|task| task.close_reason) != Some(CloseReason::Done);
                return if rejected {
                    IssueState::Canceled
                } else {
                    IssueState::Done
                };
            }
            // Rule 2: an unapproved saga has not started.
            if !saga::approved(channel, tasks) {
                return IssueState::Backlog;
            }
            // Rule 3: every task except the gate.
            if tasks
                .iter()
                .any(|task| Some(task.id) != channel.gate && counts_as_started(task))
            {
                return IssueState::Started;
            }
            IssueState::Todo
        }
        ChannelKind::Epic => {
            // Rule 1: a root closed as done, or an open root in a closed
            // channel, is done; anything else is canceled.
            let root = root();
            if channel.closed.is_some()
                || root.is_some_and(|task| task.status == TaskStatus::Closed)
            {
                if let Some(root) = root
                    && root.status == TaskStatus::Closed
                {
                    return closed_state(root.close_reason);
                }
                return IssueState::Done;
            }
            // Rule 3: every task except the root.
            if tasks
                .iter()
                .any(|task| Some(task.id) != channel.root && counts_as_started(task))
            {
                return IssueState::Started;
            }
            IssueState::Todo
        }
        ChannelKind::Project => IssueState::Todo,
    }
}

/// The team of an item: its own, the nearest ancestor's, then the anchor's.
pub fn target_of(channel: &Channel, tasks: &[Task], id: &ItemId) -> Option<String> {
    let anchor_task = anchor(channel).and_then(|a| by_id(tasks, a));
    let own = match id {
        ItemId::Channel(_) => target_task(anchor_task),
        ItemId::Task(_, task) => target_task(by_id(tasks, *task)),
    };
    if own.is_some() {
        return own;
    }
    // The nearest task up the `parent` chain with a team.
    if let ItemId::Task(_, task) = id {
        let mut seen = HashSet::new();
        let mut cursor = by_id(tasks, *task).and_then(|task| task.parent);
        while let Some(next) = cursor {
            if !seen.insert(next) {
                break;
            }
            let Some(task) = by_id(tasks, next) else {
                break;
            };
            if let Some(target) = target_task(Some(task)) {
                return Some(target);
            }
            cursor = task.parent;
        }
    }
    // The anchor of a saga or an epic channel.
    target_task(anchor_task)
}

/// How a create gets its team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Team {
    /// The item's own or inherited team.
    Target(String),
    /// The parent issue's team.
    Parent,
    /// No team: the item goes `needs_human`.
    Missing,
}

/// The team of a create. A parent issue without a target takes its own team,
/// which is what the adapter does for a child.
pub fn team(target: Option<&str>, parent_issue: Option<&str>) -> Team {
    match target.filter(|target| !target.is_empty()) {
        Some(target) => Team::Target(target.to_owned()),
        None if parent_issue.is_some() => Team::Parent,
        None => Team::Missing,
    }
}

/// The body of an issue: the text, a blank line, and the footer. The footer
/// adds the change request when the item has one.
pub fn body(text: &str, borant_id: &str, pr_url: Option<&str>) -> String {
    let mut out = String::new();
    if !text.is_empty() {
        out.push_str(text);
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "Mirrored from Borant (`{borant_id}`). Edits here do not flow back."
    ));
    if let Some(url) = pr_url.filter(|url| !url.is_empty()) {
        out.push('\n');
        out.push_str("PR: ");
        out.push_str(url);
    }
    out
}

/// The hash that the record keeps. It covers only what the mirror writes: the
/// title, the body, the parent, and the state for an issue it owns, the state
/// alone for an adopted issue. The key and the team are never in it, so a
/// team set later does not rewrite an issue.
pub fn record_hash(w: &IssueWrite, full: bool) -> String {
    #[derive(Serialize)]
    struct Full<'a> {
        title: &'a str,
        body: &'a str,
        parent: &'a Option<String>,
        state: IssueState,
    }
    #[derive(Serialize)]
    struct StateOnly {
        state: IssueState,
    }
    let bytes = if full {
        serde_json::to_vec(&Full {
            title: &w.title,
            body: &w.body,
            parent: &w.parent,
            state: w.state,
        })
    } else {
        serde_json::to_vec(&StateOnly { state: w.state })
    };
    let bytes = bytes.expect("a record of strings is JSON");
    let digest = Sha256::digest(&bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The v4-shaped UUID of a create, derived from the Borant id, so a create
/// that is sent twice makes one issue and a restarted hub sends the same key.
pub fn issue_key(borant_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"borant:");
    hasher.update(borant_id.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The question of an item with no team. It names the command that answers
/// it, and no tracker.
pub fn no_target_message(channel: &ChannelName, task: &TaskId) -> String {
    format!(
        "Issue team needed: set issue_target on this task (for example \
         `borant task update {channel}/{task} --issue-target ENG`)."
    )
}

fn closed_state(reason: Option<CloseReason>) -> IssueState {
    if reason == Some(CloseReason::Done) {
        IssueState::Done
    } else {
        IssueState::Canceled
    }
}

/// A child that is not open and not closed without a `done` reason counts as
/// work that has begun.
fn counts_as_started(task: &Task) -> bool {
    match task.status {
        TaskStatus::Open => false,
        TaskStatus::Closed => task.close_reason == Some(CloseReason::Done),
        _ => true,
    }
}

fn target_task(task: Option<&Task>) -> Option<String> {
    task.and_then(|task| task.issue_target.clone())
        .filter(|target| !target.is_empty())
}

fn by_id(tasks: &[Task], id: TaskId) -> Option<&Task> {
    tasks.iter().find(|task| task.id == id)
}

/// The channel item of a saga or an epic channel. A saga or an epic channel
/// with no anchor task has no item to ask, so it gives nothing.
fn container_item(channel: &Channel, tasks: &[Task]) -> Option<Item> {
    let ask = anchor(channel)?;
    if !matches!(channel.kind, ChannelKind::Saga | ChannelKind::Epic) {
        return None;
    }
    let anchor_task = by_id(tasks, ask);
    let id = ItemId::Channel(channel.name.clone());
    // The channel's own issue, else its anchor's: the hub writes the root of
    // an epic channel with the channel's issue, and a saga's link can only be
    // set after its create through its gate task.
    let link = channel
        .issue
        .clone()
        .or_else(|| anchor_task.and_then(|task| task.issue.clone()));
    let closed = match channel.kind {
        ChannelKind::Epic => {
            channel.closed.is_some()
                || anchor_task.is_some_and(|task| task.status == TaskStatus::Closed)
        }
        _ => channel.closed.is_some(),
    };
    let text = anchor_task
        .map(|task| task.body.clone())
        .unwrap_or_default();
    Some(Item {
        title: channel
            .title
            .clone()
            .unwrap_or_else(|| channel.name.to_string()),
        body: body(&text, &id.borant_id(), anchor_task.and_then(change_request)),
        state: container_state(channel, tasks, None),
        target: target_of(channel, tasks, &id),
        link,
        order: channel.created,
        closed,
        ask,
        id,
        parent: None,
        depth: 0,
    })
}

fn task_item(channel: &Channel, tasks: &[Task], task: &Task) -> Item {
    let id = ItemId::Task(channel.name.clone(), task.id);
    // An epic of a saga is a container; every other task maps by its status.
    let state = if task.kind == TaskKind::Epic {
        container_state(channel, tasks, Some(task))
    } else {
        task_state(task)
    };
    Item {
        parent: parent_of(channel, tasks, task),
        depth: depth_of(channel, tasks, task),
        title: task.title.clone(),
        body: body(&task.body, &id.borant_id(), change_request(task)),
        state,
        target: target_of(channel, tasks, &id),
        link: task.issue.clone(),
        closed: task.status == TaskStatus::Closed,
        order: task.updated,
        ask: task.id,
        id,
    }
}

/// The nearest mirrored ancestor, skipping the tasks that are not mirrored.
fn mirrored_parent<'a>(channel: &Channel, tasks: &'a [Task], task: &Task) -> Option<&'a Task> {
    let mut seen = HashSet::new();
    let mut cursor = task.parent;
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return None;
        }
        let next = by_id(tasks, id)?;
        if mirrored(channel, next) {
            return Some(next);
        }
        cursor = next.parent;
    }
    None
}

fn parent_of(channel: &Channel, tasks: &[Task], task: &Task) -> Option<ItemId> {
    if let Some(parent) = mirrored_parent(channel, tasks, task) {
        return Some(ItemId::Task(channel.name.clone(), parent.id));
    }
    // A saga's top and an epic channel's top hang under the channel item.
    if matches!(channel.kind, ChannelKind::Saga | ChannelKind::Epic) && anchor(channel).is_some() {
        return Some(ItemId::Channel(channel.name.clone()));
    }
    None
}

fn depth_of(channel: &Channel, tasks: &[Task], task: &Task) -> u32 {
    let mut depth = 0;
    let mut seen = HashSet::new();
    let mut cursor = Some(task.id);
    while let Some(id) = cursor {
        if !seen.insert(id) {
            return depth;
        }
        let Some(current) = by_id(tasks, id) else {
            return depth;
        };
        match parent_of(channel, tasks, current) {
            Some(ItemId::Task(_, parent)) => {
                depth += 1;
                cursor = Some(parent);
            }
            Some(ItemId::Channel(_)) => return depth + 1,
            None => return depth,
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn channel(name: &str, kind: ChannelKind) -> Channel {
        Channel {
            name: name.parse().unwrap(),
            owner: None,
            schema: serde_json::Value::Null,
            created: Timestamp(10),
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

    fn task(title: &str, status: TaskStatus, parent: Option<TaskId>) -> Task {
        Task {
            id: TaskId::new(),
            title: title.into(),
            body: format!("The body of {title}."),
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
            kind: TaskKind::Task,
            issue: None,
            issue_target: None,
            properties: Default::default(),
            created: Timestamp(20),
            updated: Timestamp(20),
            create_key: None,
        }
    }

    fn epic(parent: Option<TaskId>) -> Task {
        Task {
            kind: TaskKind::Epic,
            ..task("an epic", TaskStatus::Open, parent)
        }
    }

    fn child(parent: TaskId) -> Task {
        task("a task", TaskStatus::Open, Some(parent))
    }

    fn with_pr(mut task: Task) -> Task {
        task.properties
            .insert("pr_url".into(), json!("https://x/1"));
        task
    }

    fn with_issue(mut task: Task) -> Task {
        task.issue = Some("ENG-9".into());
        task
    }

    fn with_target(mut task: Task, target: &str) -> Task {
        task.issue_target = Some(target.into());
        task
    }

    fn closed_as(mut task: Task, reason: CloseReason) -> Task {
        task.status = TaskStatus::Closed;
        task.close_reason = Some(reason);
        task
    }

    fn as_status(mut task: Task, status: TaskStatus) -> Task {
        task.status = status;
        task
    }

    fn write() -> IssueWrite {
        IssueWrite {
            key: issue_key("web/one"),
            target: Some("ENG".into()),
            parent: None,
            title: "Mirror the sagas".into(),
            body: "The body.".into(),
            state: IssueState::Todo,
        }
    }

    #[test]
    fn mirror_task_state_maps_each_status() {
        let task = task("t", TaskStatus::Open, None);
        assert_eq!(task_state(&task), IssueState::Todo);
        for status in [
            TaskStatus::InProgress,
            TaskStatus::NeedsHuman,
            TaskStatus::Stuck,
        ] {
            assert_eq!(
                task_state(&as_status(task.clone(), status)),
                IssueState::Started,
                "{status:?}"
            );
        }
        assert_eq!(
            task_state(&closed_as(task.clone(), CloseReason::Done)),
            IssueState::Done
        );
        for reason in [
            CloseReason::Wontfix,
            CloseReason::Duplicate,
            CloseReason::Superseded,
        ] {
            assert_eq!(
                task_state(&closed_as(task.clone(), reason)),
                IssueState::Canceled,
                "{reason:?}"
            );
        }
    }

    /// Rule 2: the container state takes the first rule that matches.
    #[test]
    fn mirror_container_state_rules() {
        let mut saga = channel("one", ChannelKind::Saga);
        let gate = task("gate", TaskStatus::Open, None);
        let one = epic(None);
        let work = child(one.id);
        saga.gate = Some(gate.id);
        let tasks = [gate.clone(), one.clone(), work.clone()];

        // Rule 2: an unapproved saga is backlog.
        assert_eq!(container_state(&saga, &tasks, None), IssueState::Backlog);
        // Rule 4: an approved saga with no started child is todo.
        let done = closed_as(gate.clone(), CloseReason::Done);
        let approved = [done.clone(), one.clone(), work.clone()];
        assert_eq!(container_state(&saga, &approved, None), IssueState::Todo);
        // Rule 3: a started child starts the saga and its epic.
        let started = as_status(work.clone(), TaskStatus::InProgress);
        let begun = [done.clone(), one.clone(), started.clone()];
        assert_eq!(container_state(&saga, &begun, None), IssueState::Started);
        assert_eq!(
            container_state(&saga, &begun, Some(&one)),
            IssueState::Started,
            "an epic follows its children"
        );
        // A child closed without `done` does not start a container.
        let dropped = closed_as(work.clone(), CloseReason::Wontfix);
        let dropped = [done.clone(), one.clone(), dropped];
        assert_eq!(container_state(&saga, &dropped, None), IssueState::Todo);
        // Rule 1: a closed saga is done, and a rejected gate cancels it.
        let mut closed = saga.clone();
        closed.closed = Some(Timestamp(30));
        assert_eq!(container_state(&closed, &approved, None), IssueState::Done);
        let rejected = [
            closed_as(gate.clone(), CloseReason::Wontfix),
            one.clone(),
            work.clone(),
        ];
        assert_eq!(
            container_state(&closed, &rejected, None),
            IssueState::Canceled
        );

        // An epic task takes its own close reason.
        assert_eq!(
            container_state(
                &saga,
                &approved,
                Some(&closed_as(one.clone(), CloseReason::Done))
            ),
            IssueState::Done
        );
        assert_eq!(
            container_state(
                &saga,
                &approved,
                Some(&closed_as(one.clone(), CloseReason::Superseded))
            ),
            IssueState::Canceled
        );

        // An epic channel: a closed root, or a closed channel, is done, and a
        // root closed as anything but done cancels it.
        let mut stand_alone = channel("two", ChannelKind::Epic);
        let root = epic(None);
        let under = child(root.id);
        stand_alone.root = Some(root.id);
        let tasks = [root.clone(), under.clone()];
        assert_eq!(
            container_state(&stand_alone, &tasks, None),
            IssueState::Todo
        );
        assert_eq!(
            container_state(
                &stand_alone,
                &[closed_as(root.clone(), CloseReason::Done), under.clone()],
                None
            ),
            IssueState::Done
        );
        assert_eq!(
            container_state(
                &stand_alone,
                &[closed_as(root.clone(), CloseReason::Wontfix), under.clone()],
                None
            ),
            IssueState::Canceled,
            "a wontfix root cancels the channel"
        );
        let mut closed = stand_alone.clone();
        closed.closed = Some(Timestamp(30));
        assert_eq!(container_state(&closed, &tasks, None), IssueState::Done);
    }

    /// A saga made without an issue takes the link of its gate task, which a
    /// person can set after the create; its own issue still wins.
    #[test]
    fn mirror_saga_link_falls_back_to_its_gate() {
        let mut saga = channel("work-saga", ChannelKind::Saga);
        let mut gate = task("gate", TaskStatus::Closed, None);
        gate.close_reason = Some(CloseReason::Done);
        gate.issue = Some("ENG-7".into());
        saga.gate = Some(gate.id);
        let got = items(&saga, std::slice::from_ref(&gate));
        assert_eq!(got[0].link, Some("ENG-7".into()));

        saga.issue = Some("ENG-1".into());
        let got = items(&saga, &[gate]);
        assert_eq!(got[0].link, Some("ENG-1".into()), "the channel's own wins");
    }

    /// Every item of a saga: the channel, the epics, the tasks with a change
    /// request or a link. The gate, and a task with neither, stay out.
    #[test]
    fn mirror_items_of_a_saga() {
        let mut saga = channel("work-saga", ChannelKind::Saga);
        saga.title = Some("Mirror the sagas".into());
        saga.issue = Some("ENG-1".into());
        let gate = task("gate", TaskStatus::InProgress, None);
        saga.gate = Some(gate.id);
        let one = epic(None);
        let two = epic(None);
        let work = with_pr(child(one.id));
        let linked = with_issue(child(two.id));
        let bare = child(one.id);
        let tasks = vec![
            gate.clone(),
            one.clone(),
            two.clone(),
            work.clone(),
            linked.clone(),
            bare.clone(),
        ];

        let got = items(&saga, &tasks);
        let ids: Vec<_> = got.iter().map(|i| i.id.clone()).collect();
        let channel_id = ItemId::Channel(saga.name.clone());
        assert_eq!(
            ids,
            vec![
                channel_id.clone(),
                ItemId::Task(saga.name.clone(), one.id),
                ItemId::Task(saga.name.clone(), two.id),
                ItemId::Task(saga.name.clone(), work.id),
                ItemId::Task(saga.name.clone(), linked.id),
            ],
            "the gate and the bare task stay out"
        );

        let top = &got[0];
        assert_eq!(top.depth, 0);
        assert_eq!(top.parent, None);
        assert_eq!(top.title, "Mirror the sagas");
        assert_eq!(top.body, body(&gate.body, "work-saga", None));
        assert_eq!(top.link, Some("ENG-1".into()));
        assert_eq!(top.ask, gate.id);
        assert!(!top.closed);

        let one_item = &got[1];
        assert_eq!(one_item.depth, 1);
        assert_eq!(one_item.parent, Some(channel_id.clone()));
        assert_eq!(
            one_item.state,
            IssueState::Todo,
            "a child with a PR but open is not work begun"
        );
        assert_eq!(one_item.ask, one.id);

        let work_item = &got[3];
        assert_eq!(work_item.depth, 2);
        assert_eq!(
            work_item.parent,
            Some(ItemId::Task(saga.name.clone(), one.id))
        );
        assert_eq!(work_item.state, IssueState::Todo);
        assert_eq!(work_item.link, None);
        assert!(
            work_item.body.contains("PR: https://x/1"),
            "{}",
            work_item.body
        );

        let linked_item = &got[4];
        assert_eq!(linked_item.link, Some("ENG-9".into()));
        assert_eq!(linked_item.state, IssueState::Todo, "a bare child of two");
    }

    /// An epic channel: the channel stands for its root, and a child's parent
    /// is the channel item.
    #[test]
    fn mirror_items_of_an_epic_channel() {
        let mut stand_alone = channel("work-epic", ChannelKind::Epic);
        let root = epic(None);
        stand_alone.root = Some(root.id);
        let work = with_pr(child(root.id));
        let tasks = vec![root.clone(), work.clone()];

        let got = items(&stand_alone, &tasks);
        assert_eq!(got.len(), 2, "the root stays out");
        let channel_item = &got[0];
        assert_eq!(channel_item.id, ItemId::Channel(stand_alone.name.clone()));
        assert_eq!(channel_item.body, body(&root.body, "work-epic", None));
        assert_eq!(channel_item.ask, root.id);
        assert_eq!(got[1].parent, Some(channel_item.id.clone()));
        assert_eq!(got[1].depth, 1);

        // The root's issue is the fallback link of the channel.
        let mut linked_root = root.clone();
        linked_root.issue = Some("ENG-4".into());
        let got = items(&stand_alone, &[linked_root, work]);
        assert_eq!(got[0].link, Some("ENG-4".into()));
    }

    /// A project channel: a task with a change request has no parent, a task
    /// under it gets it as the parent, and a channel without a project gives
    /// nothing.
    #[test]
    fn mirror_items_of_a_project_channel() {
        let plain = channel("work-web", ChannelKind::Project);
        let top = with_pr(task("top", TaskStatus::Open, None));
        let under = with_pr(child(top.id));
        let bare = child(top.id);
        let tasks = vec![top.clone(), under.clone(), bare];

        let got = items(&plain, &tasks);
        assert_eq!(got.len(), 2, "a task without a PR stays out");
        assert_eq!(got[0].parent, None);
        assert_eq!(got[0].depth, 0);
        assert_eq!(
            got[1].parent,
            Some(ItemId::Task(plain.name.clone(), top.id))
        );
        assert_eq!(got[1].depth, 1);

        let mut no_project = plain.clone();
        no_project.project = None;
        assert!(items(&no_project, &tasks).is_empty());
    }

    /// The team: the item's own, then the nearest ancestor's, then the
    /// anchor's, then none.
    #[test]
    fn mirror_target_resolution_order() {
        let mut saga = channel("work-saga", ChannelKind::Saga);
        let gate = with_target(task("gate", TaskStatus::Open, None), "GATE");
        saga.gate = Some(gate.id);
        let one = with_target(epic(None), "EPIC");
        let work = child(one.id);
        let own = with_target(child(one.id), "OWN");
        let tasks = vec![gate.clone(), one.clone(), work.clone(), own.clone()];

        // Own first.
        assert_eq!(
            target_of(&saga, &tasks, &ItemId::Task(saga.name.clone(), own.id)),
            Some("OWN".into())
        );
        // Then the ancestor.
        assert_eq!(
            target_of(&saga, &tasks, &ItemId::Task(saga.name.clone(), work.id)),
            Some("EPIC".into())
        );
        // Then the anchor.
        let two = child(one.id);
        let mut tasks_without_epic_target = tasks.clone();
        tasks_without_epic_target[1].issue_target = None;
        assert_eq!(
            target_of(
                &saga,
                &tasks_without_epic_target,
                &ItemId::Task(saga.name.clone(), two.id)
            ),
            Some("GATE".into())
        );
        // Then none.
        let plain = channel("work-web", ChannelKind::Project);
        let loose = task("loose", TaskStatus::Open, None);
        assert_eq!(
            target_of(
                &plain,
                std::slice::from_ref(&loose),
                &ItemId::Task(plain.name.clone(), loose.id)
            ),
            None
        );
        // A channel item reads its anchor's team.
        assert_eq!(
            target_of(&saga, &tasks, &ItemId::Channel(saga.name.clone())),
            Some("GATE".into())
        );
    }

    #[test]
    fn mirror_team_falls_back_to_the_parent() {
        assert_eq!(team(Some("ENG"), None), Team::Target("ENG".into()));
        assert_eq!(team(Some("ENG"), Some("ENG-1")), Team::Target("ENG".into()));
        assert_eq!(team(None, Some("ENG-1")), Team::Parent);
        assert_eq!(team(Some(""), Some("ENG-1")), Team::Parent);
        assert_eq!(team(None, None), Team::Missing);
        assert_eq!(team(Some(""), None), Team::Missing);
    }

    /// The hash covers what the mirror writes: four fields for an owned
    /// issue, the state alone for an adopted one.
    #[test]
    fn mirror_record_hash() {
        let w = write();
        let full = record_hash(&w, true);
        assert_eq!(full, record_hash(&w, true), "stable");
        assert_eq!(full.len(), 64);
        assert!(
            full.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );

        for changed in [
            IssueWrite {
                title: "Another title".into(),
                ..w.clone()
            },
            IssueWrite {
                body: "Another body.".into(),
                ..w.clone()
            },
            IssueWrite {
                parent: Some("ENG-1".into()),
                ..w.clone()
            },
            IssueWrite {
                state: IssueState::Done,
                ..w.clone()
            },
        ] {
            assert_ne!(
                record_hash(&changed, true),
                full,
                "each of the four fields counts"
            );
        }

        // The key and the team never count.
        assert_eq!(
            record_hash(
                &IssueWrite {
                    key: "other".into(),
                    target: Some("OPS".into()),
                    ..w.clone()
                },
                true
            ),
            full
        );

        // An adopted issue hashes the state alone.
        let state_only = record_hash(&w, false);
        assert_ne!(state_only, full);
        assert_eq!(
            state_only,
            record_hash(
                &IssueWrite {
                    title: "Another title".into(),
                    body: "Another body.".into(),
                    parent: Some("ENG-9".into()),
                    ..w.clone()
                },
                false
            ),
            "the title costs no call for an adopted issue"
        );
        assert_ne!(
            record_hash(
                &IssueWrite {
                    state: IssueState::Started,
                    ..w.clone()
                },
                false
            ),
            state_only,
            "a status change counts"
        );
    }

    #[test]
    fn mirror_issue_key_is_v4_shaped() {
        let pattern = regex::Regex::new(
            r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
        )
        .unwrap();
        let key = issue_key("work-saga");
        assert!(pattern.is_match(&key), "{key}");
        assert_eq!(key, issue_key("work-saga"), "stable for one id");
        assert_ne!(key, issue_key("work-web"), "different for two ids");
    }

    #[test]
    fn mirror_body_has_the_footer() {
        let footer = "Mirrored from Borant (`work-web/x`). Edits here do not flow back.";
        assert_eq!(
            body("The body.", "work-web/x", None),
            format!("The body.\n\n{footer}")
        );
        assert_eq!(
            body("The body.", "work-web/x", Some("https://x/1")),
            format!("The body.\n\n{footer}\nPR: https://x/1")
        );
        assert_eq!(body("", "work-web/x", None), footer);
        assert_eq!(
            body("", "work-web/x", Some("https://x/1")),
            format!("{footer}\nPR: https://x/1")
        );
    }

    #[test]
    fn mirror_no_target_message_is_neutral() {
        let channel: ChannelName = "work-web".parse().unwrap();
        let task = TaskId::new();
        let message = no_target_message(&channel, &task);
        // The whole text: a vendor name here would fail the repo's scan.
        assert_eq!(
            message,
            format!(
                "Issue team needed: set issue_target on this task (for example \
                 `borant task update work-web/{task} --issue-target ENG`)."
            )
        );
    }
}
