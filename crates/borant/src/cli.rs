//! Command-line interface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use borant_core::model::{Harness, Project, Target, UsageWindow};
use borant_core::proto::{MoveSelector, MoveTarget, ToolCall};
use borant_core::{
    AgentId, BundleName, ChannelName, CollectionName, HostName, MemberName, ProjectName, Timestamp,
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

pub async fn run_user_command(cmd: Command) -> anyhow::Result<()> {
    let c = client();
    match cmd {
        Command::Status { json } => {
            if json {
                // The live view of the swarm, for a dashboard or a script.
                pretty(&call(&c, ToolCall::View).await?);
                return Ok(());
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
        Command::Attach { agent } => attach(&c, &agent).await?,
        Command::Top => crate::top::run().await?,
        Command::Hub { .. } | Command::Host | Command::Mcp | Command::Hook { .. } => {
            unreachable!("daemon roles run in main")
        }
        Command::SecretExec { .. } => {
            unreachable!("the secret-exec shim runs in main before tracing")
        }
    }
    Ok(())
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
        assert_eq!(w.resets_at, Timestamp(1_792_929_600_000));
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
