//! The `borant` binary: daemons, CLI, MCP server, and Claude Code hooks.

mod cli;
mod config;
mod hook;
mod mcp;
mod roles;

use clap::Parser;

use crate::cli::{Cli, Command, HubCmd};

fn init_tracing(to_stderr: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_env("BORANT_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    if to_stderr {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // Hooks and the MCP server must stay silent unless asked.
    let quiet = matches!(cli.command, Command::Hook { .. } | Command::Mcp);
    init_tracing(!quiet || std::env::var_os("BORANT_LOG").is_some());
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("borant: cannot start the runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let result: anyhow::Result<i32> = rt.block_on(async move {
        match cli.command {
            Command::Hub { cmd: None } => roles::run_hub().await.map(|_| 0),
            Command::Hub {
                cmd: Some(HubCmd::Export { out, snapshot }),
            } => roles::export(&out, snapshot).await.map(|_| 0),
            Command::Hub {
                cmd: Some(HubCmd::Import { file }),
            } => roles::import(&file).await.map(|_| 0),
            Command::Hub {
                cmd: Some(HubCmd::Move { ssh_host }),
            } => roles::hub_move(&ssh_host).await.map(|_| 0),
            Command::Host => roles::run_host().await.map(|_| 0),
            Command::Mcp => mcp::run().await.map(|_| 0),
            Command::Hook { event } => Ok(hook::run(&event).await),
            other => cli::run_user_command(other).await.map(|_| 0),
        }
    });
    match result {
        Ok(code) => std::process::ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("borant: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}
