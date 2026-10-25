//! Command-line interface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use borant_core::model::{CloseReason, Harness, Project, Target, TaskBinding, UsageWindow};
use borant_core::proto::{MoveSelector, MoveTarget, ToolCall};
use borant_core::{
    AgentId, BundleName, ChannelName, CollectionName, HostName, MemberName, ProjectName, RoleName,
    TaskId, Timestamp,
};
use borant_host::LocalClient;
use borant_host::client::Outcome;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use crate::config;

/// The version that `borant --version` prints. A release build sets
/// `BORANT_RELEASE_VERSION` to the release tag without the `v`. Other builds
/// print the crate version.
pub const VERSION: &str = match option_env!("BORANT_RELEASE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Debug, Parser)]
#[command(
    name = "borant",
    version = VERSION,
    about = "Messaging, tasks, and mobility for coding agents"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the hub daemon, or export, import, or move the hub.
    Hub {
        #[command(subcommand)]
        cmd: Option<HubCmd>,
    },
    /// Run the host daemon.
    Host,
    /// Show the local daemon, the hosts, and the agents. With `--json`, the
    /// live view of the swarm as JSON.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// A live screen of the hosts and agents, refreshed every 2 s.
    Top,
    /// Follow the swarm event stream of the local host daemon.
    Events {
        /// Wait for events published from now on, instead of printing and
        /// ending.
        #[arg(long)]
        follow: bool,
        /// Start at this time: a duration such as 30m or 2d, or an RFC 3339
        /// time such as 2026-10-25T09:00:00Z.
        #[arg(long)]
        since: Option<String>,
        /// Only these kinds, for example lease.expired or lease.*.
        #[arg(long = "kind")]
        kind: Vec<String>,
        /// Only the events about this agent.
        #[arg(long)]
        agent: Option<AgentId>,
        /// Only the events about this channel.
        #[arg(long)]
        channel: Option<ChannelName>,
        /// Only the events about this host.
        #[arg(long)]
        host: Option<HostName>,
        /// Only this severity and worse: info, warn, or error.
        #[arg(long, value_parser = crate::events::severity_arg)]
        severity: Option<borant_core::events::Severity>,
        /// Print one JSON event per line instead of a text line.
        #[arg(long)]
        json: bool,
    },
    /// List all agents.
    Agents {
        #[command(subcommand)]
        cmd: Option<AgentsCmd>,
    },
    /// List all hosts.
    Hosts,
    /// Pin an agent so the hub never replaces it and its tools survive a pause.
    Pin {
        agent: AgentId,
        /// Unpin the agent.
        #[arg(long)]
        off: bool,
    },
    /// List the tasks of a channel.
    Tasks {
        channel: ChannelName,
        /// Only tasks that can start now.
        #[arg(long)]
        ready: bool,
    },
    /// Send a direct message to an agent, or a broadcast to #channel.
    Send {
        /// An agent ID, or #channel.
        to: String,
        /// The message text.
        #[arg(required = true, num_args = 1..)]
        text: Vec<String>,
        /// Wake every member for a broadcast.
        #[arg(long)]
        urgent: bool,
    },
    /// Start an agent in a channel.
    Spawn {
        /// The channel that the agent works in.
        #[arg(long)]
        channel: ChannelName,
        /// A file whose text is the first prompt.
        #[arg(long = "prompt-file")]
        prompt_file: PathBuf,
        /// The harness to run: claude-code, pi, or omp.
        #[arg(long)]
        harness: Option<Harness>,
        /// The model for the harness.
        #[arg(long)]
        model: Option<String>,
        /// A role from hub.toml, for its harness, model, and placement.
        #[arg(long)]
        role: Option<RoleName>,
        /// The host to place the agent on.
        #[arg(long)]
        host: Option<HostName>,
        /// Bind the worker to an existing task, as CHANNEL/ID.
        #[arg(long, conflicts_with = "task_title")]
        task: Option<String>,
        /// Bind the worker to a new task with this title.
        #[arg(long = "task-title", conflicts_with = "task")]
        task_title: Option<String>,
        /// Stop the session when the bound task closes.
        #[arg(long, conflicts_with = "no_stop_on_close")]
        stop_on_close: bool,
        /// Keep the session when the bound task closes.
        #[arg(long = "no-stop-on-close", conflicts_with = "stop_on_close")]
        no_stop_on_close: bool,
        /// Higher goes first in the spawn queue.
        #[arg(long)]
        priority: Option<i32>,
        /// The agent ID for the new agent. Defaults to a hash of --key.
        #[arg(long)]
        name: Option<AgentId>,
        /// An idempotency key: a retry with the same key and arguments
        /// returns the first agent.
        #[arg(long)]
        key: Option<String>,
        /// Print the reply as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Wait for an agent or a task to finish.
    Wait {
        /// The agent to wait for.
        agent: Option<AgentId>,
        /// The task to wait for, as CHANNEL/ID.
        #[arg(long)]
        task: Option<String>,
        /// Give up after this long, for example 30m or 2h. Default 2h.
        #[arg(long)]
        timeout: Option<String>,
    },
    /// Stop an agent and cancel its queued spawn.
    Cancel {
        /// The agent to stop.
        agent: AgentId,
        /// Close the task the agent is bound to, with this reason.
        #[arg(long, value_parser = close_reason_arg)]
        close_task: Option<CloseReason>,
    },
    /// List the spawns that wait for a host.
    Ps {
        /// Only the spawns that are still queued.
        #[arg(long)]
        queued: bool,
        /// Print the queue as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print the session log of an agent.
    Logs {
        agent: AgentId,
        /// Keep printing the events that arrive.
        #[arg(long)]
        follow: bool,
    },
    /// Move agents to another host or harness.
    Move {
        /// The agent to move.
        agent: Option<AgentId>,
        /// Move every agent of this host.
        #[arg(long, conflicts_with_all = ["agent", "channel"])]
        from: Option<HostName>,
        /// Move every member of this channel.
        #[arg(long, conflicts_with = "agent")]
        channel: Option<ChannelName>,
        /// The target host.
        #[arg(long, conflicts_with = "here")]
        host: Option<HostName>,
        /// The target harness.
        #[arg(long)]
        harness: Option<Harness>,
        /// Move to this host.
        #[arg(long)]
        here: bool,
    },
    /// Move every agent away from a host and stop placing agents there.
    Drain { host: HostName },
    /// Add or list projects: git repositories that spawns clone.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// Report the usage of a bundle member.
    Usage {
        #[command(subcommand)]
        cmd: UsageCmd,
    },
    /// List the bundle members, their state, and their latest usage report.
    Members,
    /// Attach this terminal to the session of an agent.
    Attach { agent: AgentId },
    /// Serve the Borant MCP tools on stdio (Claude Code starts this).
    Mcp,
    /// Handle one Claude Code hook event (Claude Code runs this).
    Hook { event: String },
    /// Run a program with the secrets that the host wrote into a directory.
    SecretExec {
        /// The secret directory.
        dir: PathBuf,
        /// The program and its arguments, after `--`.
        #[arg(last = true, required = true, num_args = 1..)]
        command: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum AgentsCmd {
    /// List the agents that a prune would remove, without changing anything.
    Prune {
        /// Show the candidates. Real deletion is not available yet.
        #[arg(long)]
        dry_run: bool,
        /// Agents whose lease ended more than this many hours ago.
        #[arg(long, default_value_t = 24)]
        older_than_hours: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Add a project, or replace the project with the same name.
    Add {
        name: ProjectName,
        /// The git remote URL.
        #[arg(required_unless_present = "from_cwd", conflicts_with = "from_cwd")]
        remote_url: Option<String>,
        /// Use the `origin` remote and the current branch of this directory.
        #[arg(long)]
        from_cwd: bool,
        /// The default branch. Defaults to `main`, or the current branch with --from-cwd.
        #[arg(long)]
        branch: Option<String>,
        /// A shell command that runs once in each new clone.
        #[arg(long)]
        setup: Option<String>,
        /// The focused test command of the project.
        #[arg(long)]
        test: Option<String>,
        /// Where checkpoints go, if not the remote URL.
        #[arg(long)]
        checkpoint_remote: Option<String>,
        /// The collection that holds the project and its channels.
        #[arg(long)]
        collection: Option<CollectionName>,
    },
    /// List all projects.
    List,
}

#[derive(Debug, Subcommand)]
pub enum UsageCmd {
    /// Send the usage windows of one member, from a harness or by hand.
    Report {
        /// The bundle, for example claude_auth.
        #[arg(long)]
        bundle: BundleName,
        /// The member of the bundle, for example a.
        #[arg(long)]
        member: MemberName,
        /// NAME:USED:RESETS_AT, repeatable. USED is a share from 0 to 1 and
        /// RESETS_AT is RFC 3339, for example 5h:0.72:2026-10-25T12:00:00Z.
        #[arg(long = "window", required = true)]
        window: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum HubCmd {
    /// Write all hub data to a file. With `--snapshot` the running hub writes
    /// it without maintenance mode; otherwise the hub enters maintenance mode.
    Export {
        #[arg(long)]
        out: PathBuf,
        /// Ask the running hub for a snapshot, without maintenance mode.
        #[arg(long)]
        snapshot: bool,
    },
    /// Load an export into an empty broker.
    Import { file: PathBuf },
    /// Export, copy to an SSH host, and import there.
    Move { ssh_host: String },
}

pub(crate) fn client() -> LocalClient {
    LocalClient::new(config::socket())
}

const CALL: Duration = Duration::from_secs(30);

pub(crate) async fn call(c: &LocalClient, call: ToolCall) -> anyhow::Result<Value> {
    match c
        .post::<_, Value>("/v1/call", &json!({ "call": call }), CALL)
        .await
    {
        Outcome::Reply(Ok(v)) => Ok(v),
        Outcome::Reply(Err(e)) => bail!("{}", e.message),
        Outcome::Down(e) => Err(anyhow!(
            "the Borant host daemon is not running ({e}). Start it with `borant host`."
        )),
    }
}

async fn local_host(c: &LocalClient) -> anyhow::Result<HostName> {
    match c.get::<Value>("/v1/health", Duration::from_secs(2)).await {
        Outcome::Reply(Ok(v)) => v["host"]
            .as_str()
            .context("no host name in the health reply")?
            .parse()
            .map_err(Into::into),
        Outcome::Reply(Err(e)) => bail!("{}", e.message),
        Outcome::Down(e) => bail!("the Borant host daemon is not running: {e}"),
    }
}

fn pretty(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

/// Prints the reply of `agents prune`: one line per candidate with the keys
/// that a prune removes, the reason each other agent stays, and the totals.
fn print_prune(v: &Value) {
    print!("{}", render_prune(v));
}

fn render_prune(v: &Value) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let candidates = v["candidates"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let kept = v["kept"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let _ = writeln!(out, "Candidates ({}):", candidates.len());
    for c in candidates {
        let keys = c["keys"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let _ = writeln!(
            out,
            "  {:<24} since={} keys={} bytes={}",
            c["agent"].as_str().unwrap_or("?"),
            c["since"],
            keys.len(),
            c["bytes"].as_u64().unwrap_or(0),
        );
        for k in keys {
            let _ = writeln!(out, "    {}", k.as_str().unwrap_or("?"));
        }
    }
    let _ = writeln!(out, "Kept ({}):", kept.len());
    for k in kept {
        let _ = writeln!(
            out,
            "  {:<24} {}",
            k["agent"].as_str().unwrap_or("?"),
            k["reason"].as_str().unwrap_or("?"),
        );
    }
    let t = &v["totals"];
    let _ = writeln!(
        out,
        "Totals: candidates={} keys={} bytes={}",
        t["candidates"].as_u64().unwrap_or(0),
        t["keys"].as_u64().unwrap_or(0),
        t["bytes"].as_u64().unwrap_or(0),
    );
    out.push_str("Dry run: nothing was deleted.\n");
    out
}

/// Parses `NAME:USED:RESETS_AT`: USED is a share from 0 to 1, and RESETS_AT is
/// an RFC 3339 time. The reset time holds colons, so USED ends at the first
/// colon after the name.
fn parse_window(text: &str) -> anyhow::Result<UsageWindow> {
    let shape = || anyhow!("{text} is not NAME:USED:RESETS_AT");
    let (name, rest) = text.split_once(':').ok_or_else(shape)?;
    let (used, resets_at) = rest.split_once(':').ok_or_else(shape)?;
    if name.is_empty() {
        bail!("{text} has no window name");
    }
    let used: f64 = used
        .parse()
        .with_context(|| format!("{text}: {used} is not a number"))?;
    if !(0.0..=1.0).contains(&used) {
        bail!("{text}: {used} is not a share from 0 to 1");
    }
    Ok(UsageWindow {
        name: name.to_owned(),
        used,
        resets_at: rfc3339(resets_at)?,
    })
}

/// Parses an RFC 3339 date-time into Unix milliseconds.
pub(crate) fn rfc3339(text: &str) -> anyhow::Result<Timestamp> {
    use toml::value::{Datetime, Offset};
    let at: Datetime = text
        .parse()
        .with_context(|| format!("{text} is not an RFC 3339 time"))?;
    let date = at.date.context("the time has no date")?;
    let time = at.time.context("the time has no time of day")?;
    let offset_mins = match at.offset {
        Some(Offset::Z) => 0,
        Some(Offset::Custom { minutes }) => minutes,
        None => bail!("{text} has no UTC offset, for example Z"),
    };
    let second = i64::from(time.second.context("the time has no seconds")?);
    let millis = i64::from(time.nanosecond.unwrap_or(0) / 1_000_000);
    let secs = days_from_civil(
        i64::from(date.year),
        u32::from(date.month),
        u32::from(date.day),
    ) * 86_400
        + i64::from(time.hour) * 3_600
        + i64::from(time.minute) * 60
        + second
        - i64::from(offset_mins) * 60;
    Ok(Timestamp(secs * 1_000 + millis))
}

/// The days from 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The wait that `borant wait` uses when `--timeout` is absent.
const WAIT_DEFAULT: Duration = Duration::from_secs(2 * 60 * 60);

/// The extra time that the local client gets over the wait itself.
const WAIT_MARGIN: Duration = Duration::from_secs(30);

/// How many session events a `borant logs` page holds.
const LOG_PAGE: usize = 100;

/// How often `borant logs --follow` asks for new events.
const LOG_POLL: Duration = Duration::from_secs(2);

/// Parses `--task CHANNEL/ID`.
fn task_ref(text: &str) -> anyhow::Result<(ChannelName, TaskId)> {
    let shape = || anyhow!("{text} is not CHANNEL/ID");
    let (channel, id) = text.split_once('/').ok_or_else(shape)?;
    let channel = channel
        .trim()
        .parse::<ChannelName>()
        .map_err(|e| anyhow!("{e}"))?;
    let id = id.trim().parse::<TaskId>().map_err(|e| anyhow!("{e}"))?;
    Ok((channel, id))
}

/// The `--close-task` value: one of the four close reasons.
fn close_reason_arg(text: &str) -> Result<CloseReason, String> {
    match text {
        "done" => Ok(CloseReason::Done),
        "wontfix" => Ok(CloseReason::Wontfix),
        "duplicate" => Ok(CloseReason::Duplicate),
        "superseded" => Ok(CloseReason::Superseded),
        other => Err(format!(
            "{other} is not done, wontfix, duplicate, or superseded"
        )),
    }
}

/// A span such as `30m` or `2h`, for `--timeout`.
fn parse_span(text: &str) -> anyhow::Result<Duration> {
    crate::events::parse_duration(text)
        .ok_or_else(|| anyhow!("{text} is not a duration such as 30m or 2h"))
}

/// A span, for a message: seconds under a minute, then whole minutes or hours.
fn span_desc(span: Duration) -> String {
    let secs = span.as_secs();
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h", s / 3600),
    }
}

/// The exit code that the reply of `/v1/wait/*` asks for: 0 done, 1 a task
/// that closed with a reason other than `done`, 2 the timeout. An outcome
/// this build does not know counts as a failure, never as success.
fn wait_code(reply: &Value) -> i32 {
    match reply["outcome"].as_str() {
        Some("done") => 0,
        Some("timeout") => 2,
        _ => 1,
    }
}

/// The line that `borant wait` prints when the wait ended before its
/// timeout.
fn wait_line(reply: &Value) -> String {
    match reply.get("task").filter(|task| task.is_object()) {
        Some(task) => {
            let id = task["id"].as_str().unwrap_or("?");
            match task["close_reason"].as_str() {
                Some(reason) => format!("task {id} closed: {reason}"),
                None => format!("task {id} {}", task["status"].as_str().unwrap_or("?")),
            }
        }
        None => {
            let agent = reply["agent"].as_str().unwrap_or("?");
            match reply["why"].as_str() {
                Some(why) => format!("agent {agent} done: {why}"),
                None => format!("agent {agent} done"),
            }
        }
    }
}

/// Waits for an agent or a task and returns the exit code of the command.
async fn wait(
    c: &LocalClient,
    agent: Option<AgentId>,
    task: Option<String>,
    timeout: Option<String>,
) -> anyhow::Result<i32> {
    let span = match timeout {
        Some(text) => parse_span(&text)?,
        None => WAIT_DEFAULT,
    };
    let ms = span.as_millis();
    let path = match (agent, task) {
        (Some(agent), None) => format!("/v1/wait/agent?agent={agent}&timeout_ms={ms}"),
        (None, Some(task)) => {
            let (channel, id) = task_ref(&task)?;
            format!("/v1/wait/task?channel={channel}&id={id}&timeout_ms={ms}")
        }
        _ => bail!("name one agent, or use --task CHANNEL/ID"),
    };
    let reply = match c.get::<Value>(&path, span + WAIT_MARGIN).await {
        Outcome::Reply(Ok(v)) => v,
        Outcome::Reply(Err(e)) => bail!("{}", e.message),
        Outcome::Down(e) => {
            bail!("the Borant host daemon is not running ({e}). Start it with `borant host`.")
        }
    };
    let code = wait_code(&reply);
    if code == 2 {
        eprintln!("borant: timed out after {}", span_desc(span));
    } else {
        println!("{}", wait_line(&reply));
    }
    Ok(code)
}

/// The task binding of a spawn: an existing task by CHANNEL/ID, or a new task
/// with a title, in the channel of the spawn.
fn task_binding(
    task: Option<String>,
    task_title: Option<String>,
    channel: &ChannelName,
) -> anyhow::Result<Option<TaskBinding>> {
    match (task, task_title) {
        (Some(text), None) => {
            let (channel, id) = task_ref(&text)?;
            Ok(Some(TaskBinding::Existing { channel, id }))
        }
        (None, Some(title)) => Ok(Some(TaskBinding::New {
            channel: channel.clone(),
            title,
        })),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => bail!("use --task or --task-title, not both"),
    }
}

/// `--stop-on-close` and `--no-stop-on-close`, as the call takes them: unset
/// leaves the default of a bound worker to the hub.
fn stop_on_close_flag(on: bool, off: bool) -> Option<bool> {
    match (on, off) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

/// The arguments of `borant spawn`, after the prompt file is read.
struct SpawnArgs {
    channel: ChannelName,
    prompt: String,
    harness: Option<Harness>,
    model: Option<String>,
    role: Option<RoleName>,
    host: Option<HostName>,
    task: Option<TaskBinding>,
    stop_on_close: Option<bool>,
    priority: Option<i32>,
    name: Option<AgentId>,
    key: Option<String>,
}

/// The call that a spawn makes. Split from the command so that a test can
/// check the mapping of the flags.
fn spawn_call(args: SpawnArgs) -> ToolCall {
    ToolCall::Spawn {
        channel: args.channel,
        prompt: args.prompt,
        harness: args.harness,
        host: args.host,
        name: args.name,
        model: args.model,
        role: args.role,
        key: args.key,
        task: args.task,
        stop_on_close: args.stop_on_close,
        priority: args.priority,
    }
}

/// Sends one spawn and prints the agent, and whether it is placed or waits.
async fn spawn(c: &LocalClient, request: ToolCall, json: bool) -> anyhow::Result<()> {
    let v = call(c, request).await?;
    if json {
        pretty(&v);
    } else {
        println!("{}", spawn_line(&v));
    }
    Ok(())
}

/// The text line of a spawn reply.
///
/// The hub nests the answer, so through the host the reply is
/// `{"agent": {"agent": "w1", "placed": true}}`; a direct hub caller sees the
/// same shape, and the bare `{"agent": "w1"}` of an older reply still reads.
fn spawn_line(v: &Value) -> String {
    let inner = if v["agent"].is_object() {
        &v["agent"]
    } else {
        v
    };
    let agent = inner["agent"]
        .as_str()
        .or(inner["id"].as_str())
        .unwrap_or("?");
    match (
        inner["host"].as_str(),
        inner["placed"].as_bool(),
        inner["queued"].as_bool(),
    ) {
        (Some(host), _, _) => format!("agent {agent} placed on {host}"),
        (None, Some(true), _) => format!("agent {agent} placed"),
        (None, Some(false), _) | (None, _, Some(true)) => format!("agent {agent} queued"),
        _ => format!("agent {agent}"),
    }
}

/// One row per spawn of the queue, or the reply as JSON when it is not a
/// list. A reply of the current hub is an error, not a list.
fn render_queue(v: &Value, queued_only: bool) -> String {
    use std::fmt::Write as _;

    let list = v
        .as_array()
        .cloned()
        .or_else(|| v["spawns"].as_array().cloned())
        .or_else(|| v["queue"].as_array().cloned());
    let Some(list) = list else {
        return format!("{}\n", serde_json::to_string_pretty(v).unwrap_or_default());
    };
    let mut out = String::from(
        "AGENT                    STATE      PRIORITY  QUEUED     CHANNEL   LAST REJECT\n",
    );
    for rec in list
        .iter()
        .filter(|rec| !queued_only || rec["state"] == json!("queued"))
    {
        let agent = rec["agent"]
            .as_str()
            .or(rec["id"].as_str())
            .unwrap_or("?")
            .to_owned();
        let state = rec["state"].as_str().unwrap_or("?");
        let priority = rec["priority"].as_i64().unwrap_or(0);
        let queued = rec["queued_at"]
            .as_i64()
            .map(|at| crate::events::utc_time(Timestamp(at)))
            .unwrap_or_else(|| "-".to_owned());
        let channel = rec["spec"]["channel"].as_str().unwrap_or("-");
        let reject = rec["last_reject"].as_str().unwrap_or("-");
        let _ = writeln!(
            out,
            "{agent:<24} {state:<10} {priority:<9} {queued:<10} {channel:<9} {reject}"
        );
    }
    out
}

/// Prints the newest page of the session log of an agent, and then the events
/// that arrive when `follow` is set.
async fn logs(c: &LocalClient, agent: &AgentId, follow: bool) -> anyhow::Result<()> {
    let mut last: Option<String> = None;
    loop {
        let page = call(
            c,
            ToolCall::AgentHistory {
                agent: agent.clone(),
                page: 0,
                page_size: LOG_PAGE,
            },
        )
        .await?;
        print_new_events(&page, &mut last);
        if !follow {
            return Ok(());
        }
        tokio::time::sleep(LOG_POLL).await;
    }
}

/// Prints the events of a page that follow the last one printed.
fn print_new_events(page: &Value, last: &mut Option<String>) {
    let events = page["events"].as_array().cloned().unwrap_or_default();
    let start = match last.as_deref() {
        Some(last) => events
            .iter()
            .position(|e| e["id"] == json!(last))
            .map(|at| at + 1)
            .unwrap_or(0),
        None => 0,
    };
    for e in &events[start..] {
        println!("{}", render_event(e));
        *last = e["id"].as_str().map(str::to_owned);
    }
}

/// One line of the session log.
fn render_event(e: &Value) -> String {
    let at = Timestamp(e["at"].as_i64().unwrap_or(0));
    let kind = e["kind"].as_str().unwrap_or("?");
    let detail = event_detail(kind, e);
    format!("{} {kind:<18} {detail}", crate::events::utc_time(at))
        .trim_end()
        .to_owned()
}

/// The part of a session event that a person reads.
fn event_detail(kind: &str, e: &Value) -> String {
    let text = |key: &str| e[key].as_str().unwrap_or("").to_owned();
    match kind {
        "user_prompt" | "assistant_message" => clip(&text("text"), 100),
        "swarm_message" => format!("from {}", e["envelope"]["from"].as_str().unwrap_or("?")),
        "delivered" => text("message"),
        "tool_call" => format!("{} {}", text("name"), clip(&text("input"), 60)),
        "tool_result" => {
            let result = if e["is_error"] == json!(true) {
                "failed"
            } else {
                "ok"
            };
            format!("{} {result}", text("name"))
        }
        "checkpoint" => text("git_ref"),
        "session_start" => text("cwd"),
        "session_end" => text("reason"),
        "move" => format!(
            "{} -> {}",
            e["from_host"].as_str().unwrap_or("-"),
            e["to_host"].as_str().unwrap_or("?")
        ),
        _ => String::new(),
    }
}

/// The first line of `text`, cut to `max` characters.
fn clip(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    if line.chars().count() <= max {
        line.to_owned()
    } else {
        let mut out: String = line.chars().take(max).collect();
        out.push('…');
        out
    }
}

pub async fn run_user_command(cmd: Command) -> anyhow::Result<i32> {
    let c = client();
    // `borant wait` reports its result in the exit code, so a script can join
    // work with `&&`.
    let cmd = match cmd {
        Command::Wait {
            agent,
            task,
            timeout,
        } => return wait(&c, agent, task, timeout).await,
        cmd => cmd,
    };
    match cmd {
        Command::Status { json } => {
            if json {
                // The live view of the swarm, for a dashboard or a script.
                pretty(&call(&c, ToolCall::View).await?);
                return Ok(0);
            }
            let host = local_host(&c).await?;
            println!("Local host: {host}");
            let hosts = call(&c, ToolCall::ListHosts).await?;
            println!("\nHosts:");
            for h in hosts.as_array().into_iter().flatten() {
                println!(
                    "  {:<20} online={:<5} launcher={:<8} draining={} harnesses={}",
                    h["host"]["name"].as_str().unwrap_or("?"),
                    h["online"],
                    h["host"]["launcher"].as_str().unwrap_or("?"),
                    h["host"]["draining"],
                    h["host"]["harnesses"]
                );
            }
            let agents = call(&c, ToolCall::ListAgents).await?;
            println!("\nAgents:");
            for a in agents.as_array().into_iter().flatten() {
                println!(
                    "  {:<24} lease={:<9} host={:<16} harness={:<12} channels={}",
                    a["agent"].as_str().unwrap_or("?"),
                    a["lease"]["state"].as_str().unwrap_or("none"),
                    a["host"].as_str().unwrap_or("-"),
                    a["harness"].as_str().unwrap_or("?"),
                    a["channels"]
                );
            }
        }
        Command::Events {
            follow,
            since,
            kind,
            agent,
            channel,
            host,
            severity,
            json,
        } => {
            let since = match since {
                Some(text) => Some(crate::events::parse_since(&text, crate::events::now())?),
                None => None,
            };
            let args = crate::events::Args {
                follow,
                since,
                kinds: kind,
                agent,
                channel,
                host,
                min_severity: severity,
                json,
            };
            crate::events::run(&config::socket(), &args).await?;
        }
        Command::Agents { cmd: None } => pretty(&call(&c, ToolCall::ListAgents).await?),
        Command::Agents {
            cmd:
                Some(AgentsCmd::Prune {
                    dry_run,
                    older_than_hours,
                }),
        } => {
            if !dry_run {
                bail!("only --dry-run is available");
            }
            let v = call(
                &c,
                ToolCall::PruneAgents {
                    older_than_secs: older_than_hours.saturating_mul(60 * 60),
                },
            )
            .await?;
            print_prune(&v);
        }
        Command::Hosts => pretty(&call(&c, ToolCall::ListHosts).await?),
        Command::Pin { agent, off } => pretty(&call(&c, ToolCall::Pin { agent, on: !off }).await?),
        Command::Tasks { channel, ready } => pretty(
            &call(
                &c,
                ToolCall::TaskList {
                    channel,
                    ready_only: ready,
                },
            )
            .await?,
        ),
        Command::Send { to, text, urgent } => {
            let to: Target = to.parse()?;
            pretty(
                &call(
                    &c,
                    ToolCall::Send {
                        to,
                        text: text.join(" "),
                        urgent,
                    },
                )
                .await?,
            );
        }
        Command::Move {
            agent,
            from,
            channel,
            host,
            harness,
            here,
        } => {
            let selector = match (agent, from, channel) {
                (Some(a), None, None) => MoveSelector::Agent(a),
                (None, Some(h), None) => MoveSelector::FromHost(h),
                (None, None, Some(ch)) => MoveSelector::Channel(ch),
                _ => bail!("name one agent, or use --from HOST or --channel NAME"),
            };
            let host = if here {
                Some(local_host(&c).await?)
            } else {
                host
            };
            pretty(
                &call(
                    &c,
                    ToolCall::Move {
                        selector,
                        to: MoveTarget { host, harness },
                    },
                )
                .await?,
            );
        }
        Command::Drain { host } => pretty(&call(&c, ToolCall::Drain { host }).await?),
        Command::Project {
            cmd:
                ProjectCmd::Add {
                    name,
                    remote_url,
                    from_cwd,
                    branch,
                    setup,
                    test,
                    checkpoint_remote,
                    collection,
                },
        } => {
            let (remote_url, default_branch) = if from_cwd {
                let cwd = std::env::current_dir()?;
                let facts = borant_host::checkpoint::detect(&cwd, None)
                    .await
                    .context("this directory is not in a git repository with an origin remote")?;
                (facts.info.remote_url, branch.unwrap_or(facts.info.branch))
            } else {
                (
                    remote_url.context("give a remote URL or --from-cwd")?,
                    branch.unwrap_or_else(|| "main".to_owned()),
                )
            };
            let project = Project {
                name,
                remote_url,
                default_branch,
                checkpoint_remote,
                setup,
                test,
                collection,
            };
            pretty(&call(&c, ToolCall::ProjectAdd { project }).await?);
        }
        Command::Project {
            cmd: ProjectCmd::List,
        } => pretty(&call(&c, ToolCall::ProjectList).await?),
        Command::Usage {
            cmd:
                UsageCmd::Report {
                    bundle,
                    member,
                    window,
                },
        } => {
            let windows = window
                .iter()
                .map(|w| parse_window(w))
                .collect::<anyhow::Result<Vec<_>>>()?;
            pretty(
                &call(
                    &c,
                    ToolCall::UsageReport {
                        bundle,
                        member,
                        windows,
                    },
                )
                .await?,
            );
        }
        Command::Members => pretty(&call(&c, ToolCall::ListMembers).await?),
        Command::Spawn {
            channel,
            prompt_file,
            harness,
            model,
            role,
            host,
            task,
            task_title,
            stop_on_close,
            no_stop_on_close,
            priority,
            name,
            key,
            json,
        } => {
            let prompt = std::fs::read_to_string(&prompt_file)
                .with_context(|| format!("cannot read {}", prompt_file.display()))?;
            let task = task_binding(task, task_title, &channel)?;
            let stop_on_close = stop_on_close_flag(stop_on_close, no_stop_on_close);
            let args = SpawnArgs {
                channel,
                prompt,
                harness,
                model,
                role,
                host,
                task,
                stop_on_close,
                priority,
                name,
                key,
            };
            spawn(&c, spawn_call(args), json).await?;
        }
        Command::Cancel { agent, close_task } => {
            pretty(&call(&c, ToolCall::Cancel { agent, close_task }).await?)
        }
        Command::Ps { queued, json } => {
            let v = call(&c, ToolCall::SpawnQueue).await?;
            if json {
                pretty(&v);
            } else {
                print!("{}", render_queue(&v, queued));
            }
        }
        Command::Logs { agent, follow } => logs(&c, &agent, follow).await?,
        Command::Attach { agent } => attach(&c, &agent).await?,
        Command::Top => crate::top::run().await?,
        Command::Hub { .. } | Command::Host | Command::Mcp | Command::Hook { .. } => {
            unreachable!("daemon roles run in main")
        }
        Command::Wait { .. } => unreachable!("a wait returns from the arm above"),
        Command::SecretExec { .. } => {
            unreachable!("the secret-exec shim runs in main before tracing")
        }
    }
    Ok(0)
}

/// The attach command for a launcher kind.
pub fn attach_command(launcher: &str, agent: &AgentId) -> Option<Vec<String>> {
    let name = format!("borant-{agent}");
    match launcher {
        "tmux" => Some(vec![
            "tmux".into(),
            "attach".into(),
            "-t".into(),
            format!("={name}"),
        ]),
        "zellij" => Some(vec!["zellij".into(), "attach".into(), name]),
        _ => None,
    }
}

async fn attach(c: &LocalClient, agent: &AgentId) -> anyhow::Result<()> {
    let agents = call(c, ToolCall::ListAgents).await?;
    let a = agents
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["agent"] == json!(agent))
        .with_context(|| format!("no agent {agent}"))?;
    let host = a["lease"]["host"]
        .as_str()
        .or(a["host"].as_str())
        .with_context(|| format!("agent {agent} runs nowhere now"))?
        .to_owned();
    let hosts = call(c, ToolCall::ListHosts).await?;
    let info = hosts
        .as_array()
        .into_iter()
        .flatten()
        .find(|h| h["host"]["name"] == json!(host))
        .with_context(|| format!("no host {host}"))?;
    let launcher = info["host"]["launcher"].as_str().unwrap_or_default();
    let cmd = attach_command(launcher, agent).with_context(|| {
        format!("host {host} uses the {launcher} launcher, which has no terminal attach")
    })?;
    let local = local_host(c).await?;
    let mut argv = cmd;
    if local.as_str() != host {
        let ssh = info["host"]["ssh"]
            .as_str()
            .with_context(|| format!("host {host} has no ssh destination in its host.toml"))?;
        let mut remote = vec!["ssh".to_owned(), "-t".to_owned(), ssh.to_owned()];
        remote.extend(argv);
        argv = remote;
    }
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()?;
    if !status.success() {
        bail!("{} exited with {status}", argv.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// `status --json` asks the hub for the live view; plain `status` keeps
    /// today's text.
    #[test]
    fn status_json_flag_parses() {
        let cli = Cli::try_parse_from(["borant", "status", "--json"]).unwrap();
        assert!(matches!(cli.command, Command::Status { json: true }));
        let cli = Cli::try_parse_from(["borant", "status"]).unwrap();
        assert!(matches!(cli.command, Command::Status { json: false }));
        // `top` is a command of its own.
        let cli = Cli::try_parse_from(["borant", "top"]).unwrap();
        assert!(matches!(cli.command, Command::Top));
    }

    #[test]
    fn parses_all_commands() {
        Cli::command().debug_assert();
        for line in [
            "borant hub",
            "borant hub export --out /tmp/x",
            "borant hub export --snapshot --out /tmp/x",
            "borant hub import /tmp/x",
            "borant hub move vps-1",
            "borant host",
            "borant status",
            "borant events",
            "borant events --follow",
            "borant events --since 1h --kind lease.* --kind task.status --json",
            "borant events --since 2026-10-25T09:00:00Z --agent w1 --channel api --host borant-1",
            "borant events --severity warn",
            "borant agents",
            "borant agents prune --dry-run",
            "borant agents prune --dry-run --older-than-hours 48",
            "borant hosts",
            "borant pin w1",
            "borant pin w1 --off",
            "borant tasks api --ready",
            "borant send #api ship it --urgent",
            "borant send w1 hello",
            "borant move w1 --host vps-1",
            "borant move w1 --harness pi",
            "borant move w1 --here",
            "borant move --from laptop-a --here",
            "borant move --channel api --here",
            "borant drain vps-2",
            "borant attach w1",
            "borant spawn --channel api --prompt-file p.md",
            "borant spawn --channel api --prompt-file p.md --harness omp --model m --role worker --host h1 --task api/01M5S3PATRKMGFD9C0HB1CS4ZK --stop-on-close --priority 5 --name w1 --key k1 --json",
            "borant spawn --channel api --prompt-file p.md --task-title Fix%20the%20parser --no-stop-on-close",
            "borant wait w1 --timeout 30m",
            "borant wait --task api/01M5S3PATRKMGFD9C0HB1CS4ZK",
            "borant cancel w1",
            "borant cancel w1 --close-task wontfix",
            "borant ps",
            "borant ps --queued --json",
            "borant logs w1",
            "borant logs w1 --follow",
            "borant project add api git@github.com:me/api.git --branch dev --setup just",
            "borant project add api --from-cwd",
            "borant project list",
            "borant mcp",
            "borant hook stop-wait",
        ] {
            let args = line.split(' ');
            Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        assert!(Cli::try_parse_from(["borant", "move", "w1", "--from", "h"]).is_err());
        assert!(Cli::try_parse_from(["borant", "events", "--severity", "loud"]).is_err());
        assert!(
            Cli::try_parse_from([
                "borant",
                "spawn",
                "--channel",
                "api",
                "--prompt-file",
                "p.md",
                "--task",
                "api/01M5S3PATRKMGFD9C0HB1CS4ZK",
                "--task-title",
                "t",
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["borant", "cancel", "w1", "--close-task", "maybe"]).is_err());
        assert!(
            Cli::try_parse_from([
                "borant",
                "spawn",
                "--channel",
                "api",
                "--prompt-file",
                "p.md",
                "--stop-on-close",
                "--no-stop-on-close"
            ])
            .is_err()
        );
        assert!(Cli::try_parse_from(["borant", "tasks", "Bad.Name"]).is_err());
        assert!(Cli::try_parse_from(["borant", "project", "add", "api"]).is_err());
        assert!(
            Cli::try_parse_from(["borant", "project", "add", "api", "u", "--from-cwd"]).is_err()
        );
    }

    #[test]
    fn project_add_names_a_collection() {
        let cli = Cli::try_parse_from([
            "borant",
            "project",
            "add",
            "api",
            "git@github.com:me/api.git",
            "--collection",
            "work",
        ])
        .unwrap();
        let Command::Project {
            cmd: ProjectCmd::Add { collection, .. },
        } = cli.command
        else {
            panic!("not a project add");
        };
        assert_eq!(collection, Some("work".parse().unwrap()));
    }

    #[test]
    fn attach_commands() {
        let a: AgentId = "w1".parse().unwrap();
        assert_eq!(
            attach_command("tmux", &a).unwrap(),
            ["tmux", "attach", "-t", "=borant-w1"]
        );
        assert_eq!(attach_command("orca", &a), None);
    }

    /// The ULID that the spawn and wait tests use.
    const TASK: &str = "01M5S3PATRKMGFD9C0HB1CS4ZK";

    fn cli_command(line: &str) -> Command {
        Cli::try_parse_from(line.split(' ')).unwrap().command
    }

    #[test]
    fn cli_spawn_flags_build_the_call() {
        let cmd = cli_command(&format!(
            "borant spawn --channel api --prompt-file p.md --harness omp --model deepseek \
             --role worker --host h1 --task api/{TASK} --stop-on-close --priority 5 \
             --name w1 --key k1"
        ));
        let Command::Spawn {
            channel,
            prompt_file,
            harness,
            model,
            role,
            host,
            task,
            task_title,
            stop_on_close,
            no_stop_on_close,
            priority,
            name,
            key,
            json,
        } = cmd
        else {
            panic!("not a spawn");
        };
        assert_eq!(prompt_file, PathBuf::from("p.md"));
        assert!(!json);
        let args = SpawnArgs {
            channel: channel.clone(),
            prompt: "go".to_owned(),
            harness,
            model,
            role,
            host,
            task: task_binding(task, task_title, &channel).unwrap(),
            stop_on_close: stop_on_close_flag(stop_on_close, no_stop_on_close),
            priority,
            name,
            key,
        };
        let v = serde_json::to_value(spawn_call(args)).unwrap();
        assert_eq!(v["tool"], "spawn");
        assert_eq!(v["channel"], "api");
        assert_eq!(v["prompt"], "go");
        assert_eq!(v["harness"], "omp");
        assert_eq!(v["model"], "deepseek");
        assert_eq!(v["role"], "worker");
        assert_eq!(v["host"], "h1");
        assert_eq!(v["task"], json!({"channel": "api", "id": TASK}));
        assert_eq!(v["stop_on_close"], true);
        assert_eq!(v["priority"], 5);
        assert_eq!(v["name"], "w1");
        assert_eq!(v["key"], "k1");
    }

    #[test]
    fn cli_task_binding_takes_a_title_or_an_id() {
        let channel: ChannelName = "api".parse().unwrap();
        assert_eq!(
            task_binding(Some(format!("api/{TASK}")), None, &channel).unwrap(),
            Some(TaskBinding::Existing {
                channel: channel.clone(),
                id: TASK.parse().unwrap(),
            })
        );
        assert_eq!(
            task_binding(None, Some("Fix the parser".into()), &channel).unwrap(),
            Some(TaskBinding::New {
                channel: channel.clone(),
                title: "Fix the parser".into(),
            })
        );
        assert_eq!(task_binding(None, None, &channel).unwrap(), None);
        assert!(task_binding(Some(format!("api/{TASK}")), Some("t".into()), &channel).is_err());
        // An unbound spawn keeps the hub default; the two flags are exclusive.
        assert_eq!(stop_on_close_flag(false, false), None);
        assert_eq!(stop_on_close_flag(true, false), Some(true));
        assert_eq!(stop_on_close_flag(false, true), Some(false));
    }

    #[test]
    fn cli_task_ref_parses_channel_and_id() {
        let (channel, id) = task_ref(&format!("api/{TASK}")).unwrap();
        assert_eq!(channel.as_str(), "api");
        assert_eq!(id.to_string(), TASK);
        for bad in [
            "api",
            "",
            "Bad.Name/01M5S3PATRKMGFD9C0HB1CS4ZK",
            "api/not-a-ulid",
        ] {
            assert!(task_ref(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn cli_wait_parses_its_target_and_timeout() {
        let Command::Wait {
            agent,
            task,
            timeout,
        } = cli_command("borant wait w1 --timeout 30m")
        else {
            panic!("not a wait");
        };
        assert_eq!(agent, Some("w1".parse().unwrap()));
        assert_eq!(task, None);
        assert_eq!(
            parse_span(timeout.as_deref().unwrap()).unwrap().as_secs(),
            1800
        );

        let Command::Wait { agent, task, .. } =
            cli_command(&format!("borant wait --task api/{TASK}"))
        else {
            panic!("not a wait");
        };
        assert_eq!(agent, None);
        assert_eq!(task, Some(format!("api/{TASK}")));
        assert_eq!(WAIT_DEFAULT.as_secs(), 7200);
        assert!(parse_span("soon").is_err());
        assert_eq!(span_desc(Duration::from_secs(45)), "45s");
        assert_eq!(span_desc(Duration::from_secs(600)), "10m");
        assert_eq!(span_desc(Duration::from_secs(7200)), "2h");
    }

    #[test]
    fn cli_wait_maps_outcomes_to_exit_codes() {
        assert_eq!(wait_code(&json!({"outcome": "done"})), 0);
        assert_eq!(
            wait_code(&json!({"outcome": "failed", "task": {"close_reason": "wontfix"}})),
            1
        );
        assert_eq!(wait_code(&json!({"outcome": "timeout"})), 2);
        // An outcome this build does not know is a failure, never success.
        assert_eq!(wait_code(&json!({"outcome": "moved"})), 1);
        assert_eq!(wait_code(&json!({})), 1);

        let done = json!({"outcome": "done", "task": {"id": TASK, "status": "closed", "close_reason": "done"}});
        assert_eq!(wait_line(&done), format!("task {TASK} closed: done"));
        let failed = json!({"outcome": "failed", "task": {"id": TASK, "status": "closed", "close_reason": "wontfix"}});
        assert_eq!(wait_line(&failed), format!("task {TASK} closed: wontfix"));
        let open = json!({"outcome": "timeout", "task": {"id": TASK, "status": "in_progress"}});
        assert_eq!(wait_line(&open), format!("task {TASK} in_progress"));
        let agent = json!({"outcome": "done", "agent": "w1", "why": "released"});
        assert_eq!(wait_line(&agent), "agent w1 done: released");
    }

    #[test]
    fn cli_cancel_and_ps_and_logs_parse() {
        let Command::Cancel { agent, close_task } =
            cli_command("borant cancel w1 --close-task wontfix")
        else {
            panic!("not a cancel");
        };
        assert_eq!(agent, "w1".parse().unwrap());
        assert_eq!(close_task, Some(CloseReason::Wontfix));
        assert_eq!(close_reason_arg("done"), Ok(CloseReason::Done));
        assert_eq!(close_reason_arg("superseded"), Ok(CloseReason::Superseded));
        assert!(close_reason_arg("closed").is_err());

        let Command::Ps { queued, json } = cli_command("borant ps --queued --json") else {
            panic!("not a ps");
        };
        assert!(queued && json);

        let Command::Logs { agent, follow } = cli_command("borant logs w1 --follow") else {
            panic!("not a logs");
        };
        assert_eq!(agent, "w1".parse().unwrap());
        assert!(follow);
    }

    #[test]
    fn cli_spawn_line_reads_the_nested_reply() {
        assert_eq!(
            spawn_line(&json!({"agent": {"agent": "w1", "placed": true}})),
            "agent w1 placed"
        );
        assert_eq!(
            spawn_line(&json!({"agent": {"agent": "w1", "placed": false}})),
            "agent w1 queued"
        );
        assert_eq!(
            spawn_line(&json!({"agent": {"agent": "w1", "placed": true, "host": "h1"}})),
            "agent w1 placed on h1"
        );
        // An older reply that names the agent at the top still reads.
        assert_eq!(spawn_line(&json!({"agent": "w1"})), "agent w1");
    }

    #[test]
    fn cli_render_queue_lists_the_waiting_spawns() {
        let v = json!([
            {"agent": "w1", "state": "queued", "priority": 5, "queued_at": 1_792_929_600_000i64,
             "spec": {"channel": "api"}, "last_reject": "no host fits"},
            {"agent": "w2", "state": "running", "priority": 0, "queued_at": 1_792_929_600_000i64,
             "spec": {"channel": "api"}, "last_reject": null},
        ]);
        let all = render_queue(&v, false);
        assert!(all.starts_with("AGENT"), "{all}");
        assert!(all.contains("w1") && all.contains("w2"), "{all}");
        assert!(
            all.contains("12:00:00") && all.contains("no host fits"),
            "{all}"
        );
        let queued = render_queue(&v, true);
        assert!(queued.contains("w1"), "{queued}");
        assert!(!queued.contains("w2"), "{queued}");
        // A reply of the current hub is an error; print it rather than pretend
        // that the queue is empty.
        let error = json!({"error": {"code": "invalid", "message": "not built"}});
        assert!(render_queue(&error, false).contains("not built"));
    }

    #[test]
    fn cli_logs_render_a_session_event() {
        let event = json!({"id": "01M5S3PATRKMGFD9C0HB1CS4ZK", "at": 1_792_929_600_000i64,
                           "kind": "tool_call", "name": "bash", "input": "just test\nmore"});
        assert_eq!(
            render_event(&event),
            "12:00:00 tool_call          bash just test"
        );
        let failed = json!({"id": "x", "at": 1_792_929_600_000i64, "kind": "tool_result",
                            "name": "bash", "output": "", "is_error": true});
        assert_eq!(
            render_event(&failed),
            "12:00:00 tool_result        bash failed"
        );
        let ended = json!({"at": 1_792_929_600_000i64, "kind": "session_end", "reason": "closed"});
        assert_eq!(render_event(&ended), "12:00:00 session_end        closed");

        // A follow prints the new events only.
        let page = json!({"events": [
            {"id": "a", "at": 1_792_929_600_000i64, "kind": "turn_start"},
            {"id": "b", "at": 1_792_929_600_000i64, "kind": "session_end", "reason": "closed"},
        ]});
        let mut last = None;
        print_new_events(&page, &mut last);
        assert_eq!(last.as_deref(), Some("b"));
        let next = json!({"events": [
            {"id": "a", "at": 1, "kind": "turn_start"},
            {"id": "b", "at": 2, "kind": "session_end", "reason": "closed"},
            {"id": "c", "at": 3, "kind": "turn_start"},
        ]});
        print_new_events(&next, &mut last);
        assert_eq!(last.as_deref(), Some("c"));
    }

    /// A stub host on a fresh Unix socket that answers one request per reply.
    async fn serve_host(
        socket: PathBuf,
        replies: Vec<(u16, String)>,
    ) -> tokio::task::JoinHandle<Vec<String>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let mut heads = Vec::new();
            for (status, body) in replies {
                let (mut conn, _) = listener.accept().await.unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while conn.read(&mut byte).await.unwrap() == 1 {
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let reply = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                conn.write_all(reply.as_bytes()).await.unwrap();
                conn.flush().await.unwrap();
                conn.shutdown().await.unwrap();
                heads.push(String::from_utf8(head).unwrap());
            }
            heads
        })
    }

    #[tokio::test]
    async fn cli_wait_returns_the_exit_code_of_the_route() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host.sock");
        let closed = |reason: &str| {
            json!({"ok": {
                "outcome": if reason == "done" { "done" } else { "failed" },
                "task": {"id": TASK, "status": "closed", "close_reason": reason},
            }})
            .to_string()
        };
        let replies = vec![
            (200, closed("done")),
            (200, closed("wontfix")),
            (
                200,
                json!({"ok": {"outcome": "timeout", "waited_ms": 10}}).to_string(),
            ),
            (
                200,
                json!({"ok": {"outcome": "done", "agent": "w1", "why": "released"}}).to_string(),
            ),
        ];
        let served = serve_host(socket.clone(), replies).await;
        let c = LocalClient::new(socket);
        let target = Some(format!("api/{TASK}"));

        let code = wait(&c, None, target.clone(), Some("10s".into()))
            .await
            .unwrap();
        assert_eq!(code, 0);
        let code = wait(&c, None, target.clone(), None).await.unwrap();
        assert_eq!(code, 1);
        let code = wait(&c, None, target, Some("1m".into())).await.unwrap();
        assert_eq!(code, 2);
        let code = wait(&c, Some("w1".parse().unwrap()), None, None)
            .await
            .unwrap();
        assert_eq!(code, 0);

        let heads = served.await.unwrap();
        assert!(
            heads[0].starts_with(&format!(
                "GET /v1/wait/task?channel=api&id={TASK}&timeout_ms=10000 HTTP/1.1"
            )),
            "{}",
            heads[0]
        );
        // The default wait is the same two hours that the route clamps to.
        assert!(heads[1].contains("timeout_ms=7200000"), "{}", heads[1]);
        assert!(
            heads[3].starts_with("GET /v1/wait/agent?agent=w1&timeout_ms=7200000 HTTP/1.1"),
            "{}",
            heads[3]
        );

        // A wait names one target.
        let err = wait(&c, None, None, None).await.unwrap_err();
        assert_eq!(err.to_string(), "name one agent, or use --task CHANNEL/ID");
        let err = wait(
            &c,
            Some("w1".parse().unwrap()),
            Some(format!("api/{TASK}")),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "name one agent, or use --task CHANNEL/ID");
    }

    #[tokio::test]
    async fn prune_needs_dry_run() {
        let err = run_user_command(Command::Agents {
            cmd: Some(AgentsCmd::Prune {
                dry_run: false,
                older_than_hours: 24,
            }),
        })
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "only --dry-run is available");
    }

    #[test]
    fn prints_prune_candidates_and_kept() {
        let v = json!({
            "candidates": [{
                "agent": "old",
                "since": 1_000,
                "keys": ["agent.old", "lease.agent.old"],
                "bytes": 42,
            }],
            "kept": [{ "agent": "busy", "reason": "session" }],
            "totals": { "candidates": 1, "keys": 2, "bytes": 42 },
        });
        let out = render_prune(&v);
        assert_eq!(
            out,
            "Candidates (1):\n  old                      since=1000 keys=2 bytes=42\n    agent.old\n    lease.agent.old\nKept (1):\n  busy                     session\nTotals: candidates=1 keys=2 bytes=42\nDry run: nothing was deleted.\n"
        );
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn usage_window_parses_a_name_a_share_and_a_reset() {
        let w = parse_window("5h:0.72:2026-10-25T12:00:00Z").unwrap();
        assert_eq!(w.name, "5h");
        assert_eq!(w.used, 0.72);
        // 2026-10-25T12:00:00Z in milliseconds.
        assert_eq!(w.resets_at, Timestamp(1_792_929_600_000i64));
        // The reset time holds colons; the share ends at the first one.
        let w = parse_window("weekly:1:2026-10-25T12:00:00.250+02:00").unwrap();
        assert_eq!(w.used, 1.0);
        assert_eq!(w.resets_at, Timestamp(1_792_922_400_250));
    }

    #[test]
    fn usage_window_rejects_a_bad_share_or_time() {
        assert!(parse_window("5h:1.5:2026-10-25T12:00:00Z").is_err());
        assert!(parse_window("5h:many:2026-10-25T12:00:00Z").is_err());
        assert!(parse_window("5h:0.5:yesterday").is_err());
        assert!(parse_window("5h:0.5:2026-10-25T12:00:00").is_err());
        assert!(parse_window("0.5").is_err());
    }
}
