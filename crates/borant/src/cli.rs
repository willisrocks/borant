//! Command-line interface.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use borant_core::model::{Harness, Target};
use borant_core::proto::{MoveSelector, MoveTarget, ToolCall};
use borant_core::{AgentId, ChannelName, HostName};
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
    /// Show the local daemon, the hosts, and the agents.
    Status,
    /// List all agents.
    Agents,
    /// List all hosts.
    Hosts,
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
    /// Attach this terminal to the session of an agent.
    Attach { agent: AgentId },
    /// Serve the Borant MCP tools on stdio (Claude Code starts this).
    Mcp,
    /// Handle one Claude Code hook event (Claude Code runs this).
    Hook { event: String },
}

#[derive(Debug, Subcommand)]
pub enum HubCmd {
    /// Write all hub data to a file and put the running hub in maintenance mode.
    Export {
        #[arg(long)]
        out: PathBuf,
    },
    /// Load an export into an empty broker.
    Import { file: PathBuf },
    /// Export, copy to an SSH host, and import there.
    Move { ssh_host: String },
}

fn client() -> LocalClient {
    LocalClient::new(config::socket())
}

const CALL: Duration = Duration::from_secs(30);

async fn call(c: &LocalClient, call: ToolCall) -> anyhow::Result<Value> {
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

pub async fn run_user_command(cmd: Command) -> anyhow::Result<()> {
    let c = client();
    match cmd {
        Command::Status => {
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
        Command::Agents => pretty(&call(&c, ToolCall::ListAgents).await?),
        Command::Hosts => pretty(&call(&c, ToolCall::ListHosts).await?),
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
        Command::Attach { agent } => attach(&c, &agent).await?,
        Command::Hub { .. } | Command::Host | Command::Mcp | Command::Hook { .. } => {
            unreachable!("daemon roles run in main")
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

    #[test]
    fn parses_all_commands() {
        Cli::command().debug_assert();
        for line in [
            "borant hub",
            "borant hub export --out /tmp/x",
            "borant hub import /tmp/x",
            "borant hub move vps-1",
            "borant host",
            "borant status",
            "borant agents",
            "borant hosts",
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
            "borant mcp",
            "borant hook stop-wait",
        ] {
            let args = line.split(' ');
            Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{line}: {e}"));
        }
        assert!(Cli::try_parse_from(["borant", "move", "w1", "--from", "h"]).is_err());
        assert!(Cli::try_parse_from(["borant", "tasks", "Bad.Name"]).is_err());
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
}
