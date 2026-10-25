//! Rules of the task graph (spec section 9).

use std::collections::{HashMap, HashSet};

use crate::error::CoreError;
use crate::ids::TaskId;
use crate::model::{CloseReason, Task, TaskStatus};

/// Open tasks whose prerequisites are all closed. An unknown prerequisite
/// counts as not closed.
pub fn ready(tasks: &[Task]) -> Vec<TaskId> {
    let status: HashMap<TaskId, TaskStatus> = tasks.iter().map(|t| (t.id, t.status)).collect();
    tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| {
            t.blocked_by
                .iter()
                .all(|b| status.get(b) == Some(&TaskStatus::Closed))
        })
        .map(|t| t.id)
        .collect()
}

/// Checks that `task` can have the prerequisites `blocked_by` without a
/// cycle. `task` itself does not have to be in `tasks` yet.
pub fn check_edges(tasks: &[Task], task: TaskId, blocked_by: &[TaskId]) -> Result<(), CoreError> {
    let mut edges: HashMap<TaskId, Vec<TaskId>> =
        tasks.iter().map(|t| (t.id, t.blocked_by.clone())).collect();
    for b in blocked_by {
        if *b == task {
            return Err(CoreError::SelfEdge);
        }
        if !edges.contains_key(b) {
            return Err(CoreError::UnknownTask(*b));
        }
    }
    edges.insert(task, blocked_by.to_vec());

    // A cycle exists if `task` is reachable from one of its prerequisites.
    for start in blocked_by {
        let mut path = vec![task, *start];
        let mut seen = HashSet::new();
        if reaches(&edges, *start, task, &mut path, &mut seen) {
            return Err(CoreError::Cycle { path });
        }
    }
    Ok(())
}

fn reaches(
    edges: &HashMap<TaskId, Vec<TaskId>>,
    from: TaskId,
    goal: TaskId,
    path: &mut Vec<TaskId>,
    seen: &mut HashSet<TaskId>,
) -> bool {
    if !seen.insert(from) {
        return false;
    }
    for next in edges.get(&from).map(Vec::as_slice).unwrap_or_default() {
        path.push(*next);
        if *next == goal || reaches(edges, *next, goal, path, seen) {
            return true;
        }
        path.pop();
    }
    false
}

/// A parent cannot close while it has open children.
pub fn check_close(tasks: &[Task], task: &Task) -> Result<(), CoreError> {
    if task.status == TaskStatus::Closed {
        return Err(CoreError::TaskClosed);
    }
    let children: Vec<TaskId> = tasks
        .iter()
        .filter(|t| t.parent == Some(task.id) && t.status != TaskStatus::Closed)
        .map(|t| t.id)
        .collect();
    if children.is_empty() {
        Ok(())
    } else {
        Err(CoreError::OpenChildren { children })
    }
}

/// What a close of a task should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    /// The task is open: close it.
    Close,
    /// It is already closed with this reason: return it as it is.
    Repeat,
    /// It is closed with another reason.
    Conflict(CloseReason),
}

/// A retried close is idempotent: the same reason returns the task, and
/// another reason is a conflict. A closed task without a reason counts as the
/// same close, because a retry must not fail.
pub fn close_repeat(task: &Task, reason: CloseReason) -> CloseOutcome {
    if task.status != TaskStatus::Closed {
        return CloseOutcome::Close;
    }
    match task.close_reason {
        Some(had) if had != reason => CloseOutcome::Conflict(had),
        _ => CloseOutcome::Repeat,
    }
}

#[cfg(test)]
mod close_tests {
    use super::*;
    use crate::model::CloseReason;
    use crate::time::Timestamp;

    fn task(status: TaskStatus, reason: Option<CloseReason>) -> Task {
        Task {
            id: TaskId::new(),
            title: "t".into(),
            body: String::new(),
            status,
            close_reason: reason,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            blocked_by: Vec::new(),
            parent: None,
            properties: Default::default(),
            created: Timestamp(0),
            updated: Timestamp(0),
            create_key: None,
        }
    }

    #[test]
    fn close_repeat_is_idempotent_for_the_same_reason() {
        let open = task(TaskStatus::InProgress, None);
        assert_eq!(close_repeat(&open, CloseReason::Done), CloseOutcome::Close);

        let done = task(TaskStatus::Closed, Some(CloseReason::Done));
        assert_eq!(close_repeat(&done, CloseReason::Done), CloseOutcome::Repeat);
        assert_eq!(
            close_repeat(&done, CloseReason::Wontfix),
            CloseOutcome::Conflict(CloseReason::Done)
        );

        // A close from a version without a reason still repeats.
        let bare = task(TaskStatus::Closed, None);
        assert_eq!(close_repeat(&bare, CloseReason::Done), CloseOutcome::Repeat);
    }
}

/// Any change is allowed, except a change of a closed task. A close goes
/// through `task_close`, not through a status change.
pub fn check_status_change(from: TaskStatus, to: TaskStatus) -> Result<(), CoreError> {
    if from == TaskStatus::Closed || to == TaskStatus::Closed {
        return Err(CoreError::TaskClosed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    fn task(status: TaskStatus, blocked_by: Vec<TaskId>) -> Task {
        Task {
            id: TaskId::new(),
            title: "t".into(),
            body: String::new(),
            status,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            blocked_by,
            parent: None,
            properties: Default::default(),
            created: Timestamp(0),
            updated: Timestamp(0),
            create_key: None,
        }
    }

    #[test]
    fn ready_requires_closed_prereqs() {
        let a = task(TaskStatus::Closed, vec![]);
        let b = task(TaskStatus::InProgress, vec![]);
        let c = task(TaskStatus::Open, vec![a.id]);
        let d = task(TaskStatus::Open, vec![a.id, b.id]);
        let e = task(TaskStatus::Open, vec![TaskId::new()]);
        let f = task(TaskStatus::Open, vec![]);
        let got = ready(&[a, b, c.clone(), d, e, f.clone()]);
        assert_eq!(got, vec![c.id, f.id]);
    }

    #[test]
    fn cycle_is_rejected() {
        let c = task(TaskStatus::Open, vec![]);
        let b = task(TaskStatus::Open, vec![c.id]);
        let a = task(TaskStatus::Open, vec![b.id]);
        let all = [a.clone(), b.clone(), c.clone()];
        let err = check_edges(&all, c.id, &[a.id]).unwrap_err();
        let CoreError::Cycle { path } = err else {
            panic!("expected a cycle, got {err:?}")
        };
        assert_eq!(path, vec![c.id, a.id, b.id, c.id]);
        assert!(check_edges(&all, a.id, &[c.id]).is_ok());
        assert_eq!(check_edges(&all, a.id, &[a.id]), Err(CoreError::SelfEdge));
        let ghost = TaskId::new();
        assert_eq!(
            check_edges(&all, a.id, &[ghost]),
            Err(CoreError::UnknownTask(ghost))
        );
    }

    #[test]
    fn parent_with_open_child_cannot_close() {
        let parent = task(TaskStatus::Open, vec![]);
        let mut child = task(TaskStatus::Open, vec![]);
        child.parent = Some(parent.id);
        let all = [parent.clone(), child.clone()];
        assert_eq!(
            check_close(&all, &parent),
            Err(CoreError::OpenChildren {
                children: vec![child.id]
            })
        );
        child.status = TaskStatus::Closed;
        assert!(check_close(&[parent.clone(), child], &parent).is_ok());
    }

    #[test]
    fn closed_task_cannot_change_status() {
        assert!(check_status_change(TaskStatus::Open, TaskStatus::Stuck).is_ok());
        assert_eq!(
            check_status_change(TaskStatus::Closed, TaskStatus::Open),
            Err(CoreError::TaskClosed)
        );
    }
}
