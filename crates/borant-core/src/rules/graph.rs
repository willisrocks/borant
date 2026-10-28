//! The stages of the task graph (phase 8 spec, "Interfaces").
//!
//! A stage is the length of the longest `blocked_by` path from a task with no
//! open prerequisite: the tasks at stage 0 can start now, the tasks at stage
//! 1 wait for one step, and so on. The web view draws one column per stage.

use std::collections::{BTreeMap, BTreeSet};

use crate::ids::TaskId;
use crate::model::{Task, TaskStatus};

/// The stage of every task in `tasks`, by task id.
///
/// Only an open prerequisite counts: a [`TaskStatus::Closed`] prerequisite
/// adds no stage, because it does not block. A prerequisite that `tasks`
/// does not hold, and a cycle, never loop: such a task gets the stage of its
/// known prerequisites plus one.
pub fn stages(tasks: &[Task]) -> BTreeMap<TaskId, u32> {
    let status: BTreeMap<TaskId, TaskStatus> = tasks.iter().map(|t| (t.id, t.status)).collect();
    // The edges that count, and the tasks with a prerequisite `tasks` lacks.
    let mut edges: BTreeMap<TaskId, Vec<TaskId>> = BTreeMap::new();
    let mut missing: BTreeSet<TaskId> = BTreeSet::new();
    for t in tasks {
        let mut open = Vec::new();
        for b in &t.blocked_by {
            match status.get(b) {
                Some(TaskStatus::Closed) => {}
                Some(_) => open.push(*b),
                None => {
                    missing.insert(t.id);
                }
            }
        }
        edges.insert(t.id, open);
    }

    // A component comes after every component it points at, so a stage is
    // known when it is needed. The edges inside a component are the cycle,
    // and they add one stage, never a loop.
    let mut stage: BTreeMap<TaskId, u32> = BTreeMap::new();
    for comp in components(&edges) {
        let member: BTreeSet<TaskId> = comp.iter().copied().collect();
        let mut values = Vec::with_capacity(comp.len());
        for t in &comp {
            let mut outside: Option<u32> = None;
            let mut cyclic = false;
            for b in &edges[t] {
                if member.contains(b) {
                    cyclic = true;
                } else {
                    let s = stage[b];
                    outside = Some(outside.map_or(s, |m: u32| m.max(s)));
                }
            }
            values.push(if outside.is_some() || cyclic || missing.contains(t) {
                1 + outside.unwrap_or(0)
            } else {
                0
            });
        }
        for (t, value) in comp.iter().zip(values) {
            stage.insert(*t, value);
        }
    }
    stage
}

/// The strongly connected components of `edges`, a component before the
/// components that point at it. Tarjan's algorithm, iterative so that a long
/// chain cannot overflow the stack.
fn components(edges: &BTreeMap<TaskId, Vec<TaskId>>) -> Vec<Vec<TaskId>> {
    let mut index = 0;
    let mut index_of: BTreeMap<TaskId, usize> = BTreeMap::new();
    let mut lowlink: BTreeMap<TaskId, usize> = BTreeMap::new();
    let mut on_stack: BTreeSet<TaskId> = BTreeSet::new();
    let mut stack: Vec<TaskId> = Vec::new();
    let mut comps = Vec::new();
    for root in edges.keys() {
        if index_of.contains_key(root) {
            continue;
        }
        index_of.insert(*root, index);
        lowlink.insert(*root, index);
        index += 1;
        stack.push(*root);
        on_stack.insert(*root);
        // One entry per open call: the node and the edge to visit next.
        let mut calls: Vec<(TaskId, usize)> = vec![(*root, 0)];
        while let Some(&(node, next)) = calls.last() {
            let Some(&succ) = edges[&node].get(next) else {
                calls.pop();
                let low = lowlink[&node];
                if let Some(&(parent, _)) = calls.last()
                    && low < lowlink[&parent]
                {
                    lowlink.insert(parent, low);
                }
                if low == index_of[&node] {
                    let mut comp = Vec::new();
                    while let Some(w) = stack.pop() {
                        on_stack.remove(&w);
                        comp.push(w);
                        if w == node {
                            break;
                        }
                    }
                    comps.push(comp);
                }
                continue;
            };
            calls.last_mut().expect("the call is still open").1 += 1;
            match index_of.get(&succ) {
                None => {
                    index_of.insert(succ, index);
                    lowlink.insert(succ, index);
                    index += 1;
                    stack.push(succ);
                    on_stack.insert(succ);
                    calls.push((succ, 0));
                }
                Some(&seen) if on_stack.contains(&succ) => {
                    if seen < lowlink[&node] {
                        lowlink.insert(node, seen);
                    }
                }
                Some(_) => {}
            }
        }
    }
    comps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TaskKind;
    use crate::time::Timestamp;

    fn id(n: u128) -> TaskId {
        TaskId(ulid::Ulid::from(n))
    }

    fn task(n: u128, status: TaskStatus, blocked_by: &[u128]) -> Task {
        Task {
            id: id(n),
            title: "t".into(),
            body: String::new(),
            status,
            close_reason: None,
            close_message: None,
            attention_msg: None,
            claimed_by: None,
            deadline: None,
            attempts: 0,
            max_attempts: None,
            blocked_by: blocked_by.iter().map(|b| id(*b)).collect(),
            parent: None,
            kind: TaskKind::Task,
            issue: None,
            issue_target: None,
            properties: Default::default(),
            created: Timestamp(0),
            updated: Timestamp(0),
            create_key: None,
        }
    }

    fn got(tasks: &[Task]) -> Vec<u32> {
        let stages = stages(tasks);
        let mut ids: Vec<TaskId> = stages.keys().copied().collect();
        ids.sort();
        ids.iter().map(|i| stages[i]).collect()
    }

    #[test]
    fn a_chain_counts_every_step() {
        let tasks = [
            task(1, TaskStatus::Open, &[]),
            task(2, TaskStatus::Open, &[1]),
            task(3, TaskStatus::InProgress, &[2]),
        ];
        assert_eq!(got(&tasks), vec![0, 1, 2]);
    }

    #[test]
    fn a_fan_in_takes_the_longest_path() {
        let tasks = [
            task(1, TaskStatus::Open, &[]),
            task(2, TaskStatus::Open, &[1]),
            task(3, TaskStatus::Open, &[1]),
            // The shorter prerequisite is the first one: the longest wins.
            task(4, TaskStatus::Open, &[1, 2]),
            task(5, TaskStatus::Open, &[3, 2]),
        ];
        assert_eq!(got(&tasks), vec![0, 1, 1, 2, 2]);
    }

    #[test]
    fn a_cycle_adds_one_stage_and_never_loops() {
        let tasks = [
            task(1, TaskStatus::Open, &[2]),
            task(2, TaskStatus::Open, &[1]),
            task(3, TaskStatus::Open, &[2]),
        ];
        assert_eq!(got(&tasks), vec![1, 1, 2]);
    }

    #[test]
    fn a_cycle_after_a_root_sits_one_stage_after_it() {
        let tasks = [
            task(1, TaskStatus::Open, &[]),
            task(2, TaskStatus::Open, &[1, 3]),
            task(3, TaskStatus::Open, &[2]),
            task(4, TaskStatus::Open, &[3]),
        ];
        assert_eq!(got(&tasks), vec![0, 1, 1, 2]);
    }

    #[test]
    fn a_missing_prerequisite_adds_one_stage_and_never_loops() {
        let tasks = [
            task(1, TaskStatus::Open, &[]),
            task(2, TaskStatus::Open, &[1, 99]),
            task(3, TaskStatus::Open, &[2]),
            // Nothing else is known, so the stage of no prerequisite plus one.
            task(4, TaskStatus::Open, &[98]),
            // The status of the task changes nothing.
            task(5, TaskStatus::Closed, &[97]),
        ];
        assert_eq!(got(&tasks), vec![0, 1, 2, 1, 1]);
    }

    #[test]
    fn closed_prerequisites_do_not_block() {
        let tasks = [
            task(1, TaskStatus::Closed, &[]),
            task(2, TaskStatus::Closed, &[1]),
            task(3, TaskStatus::Open, &[1, 2]),
            task(4, TaskStatus::Open, &[3]),
            // A closed prerequisite is no prerequisite, so no stage.
            task(5, TaskStatus::Closed, &[1, 2]),
        ];
        assert_eq!(got(&tasks), vec![0, 0, 0, 1, 0]);
    }
}
