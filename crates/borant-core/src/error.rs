use crate::ids::{AgentId, ChannelName, HostName, TaskId};

/// Errors from domain rules. Each variant maps to one API error code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error(
        "invalid {kind} '{value}': use 1-63 characters from a-z, 0-9, '-' and '_', starting with a letter or digit"
    )]
    InvalidName { kind: &'static str, value: String },
    #[error("unknown task {0}")]
    UnknownTask(TaskId),
    #[error("a task cannot block itself")]
    SelfEdge,
    #[error("the edge makes a cycle: {}", fmt_path(.path))]
    Cycle { path: Vec<TaskId> },
    #[error("the task has open children: {}", fmt_path(.children))]
    OpenChildren { children: Vec<TaskId> },
    #[error("the task is closed")]
    TaskClosed,
    #[error("invalid task properties: {}", .errors.join("; "))]
    InvalidProperties { errors: Vec<String> },
    #[error("invalid task schema: {0}")]
    InvalidSchema(String),
    #[error("the schema already has the key '{key}', and a change can only add keys")]
    SchemaChange { key: String },
    #[error("the fencing token or session is stale")]
    StaleToken,
    #[error("the lease of the agent is held by a session on host {host}")]
    LeaseHeld { host: HostName },
    #[error("agent {0} has claimed the task")]
    Claimed(AgentId),
    #[error("a spawn asks for continues but binds no task: name the task of the spawn")]
    ContinuesWithoutTask,
    #[error("continues.max is at least 1: a worker with no continue has no policy")]
    ContinueLimit,
    #[error("saga #{0} is not approved: close its gate task as done first")]
    SagaNotApproved(ChannelName),
    #[error("channel #{0} is closed")]
    ChannelClosed(ChannelName),
    #[error("task {0} is an epic: an epic holds tasks and is never work")]
    EpicNotWork(TaskId),
    #[error("invalid channel structure: {0}")]
    Structure(String),
    #[error("task {task} is blocked by {}", fmt_path(.by))]
    Blocked { task: TaskId, by: Vec<TaskId> },
}

fn fmt_path(ids: &[TaskId]) -> String {
    ids.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ")
}
