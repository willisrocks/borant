//! The daemon roles and the hub export, import, and move.

use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use borant_core::SystemClock;
use borant_core::ports::Notifier;
use borant_core::proto::CtlRequest;
use borant_core::subjects;
use borant_host::outbox::Outbox;
use borant_host::{DesktopNotifier, Host};
use borant_hub::{Hub, HubConfig};
use tokio_util::sync::CancellationToken;

use crate::config;

fn shutdown_on_signal() -> CancellationToken {
    let token = CancellationToken::new();
    let t = token.clone();
    tokio::spawn(async move {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { match term.as_mut() { Some(s) => { s.recv().await; } None => std::future::pending().await } } => {}
        }
        tracing::info!("shutting down");
        t.cancel();
    });
    token
}

async fn hub_parts(
    name: &str,
) -> anyhow::Result<(borant_nats::Nats, HubConfig, Option<Box<dyn Notifier>>)> {
    let file = config::hub_file()?;
    // A name in [alerts] that [notifiers] does not define fails the start,
    // not the first notice. The alert pass takes the notifier this builds.
    let notifier = file.notifier()?;
    // A bad cron or time zone in [schedules] fails the start, not the first
    // tick, and the error names the schedule.
    let schedules = file.schedules()?;
    tracing::info!(
        alerts = %file.alerts.summary(),
        notifier_built = notifier.is_some(),
        "the alert rules of this hub"
    );
    let nats = borant_nats::connect(&file.nats.to_config(name)).await?;
    // The PR watch of `[changes]`: None names no adapter, or a program that is
    // not a change-request plugin (a warn, not a failed start).
    let changes = file.changes_watch().await?;
    let mut cfg = HubConfig {
        lease: file.timing.lease_config()?,
        elastic: file.elastic_config()?,
        roles: file.roles,
        collections: file.collections,
        alerts: file.alerts.to_config(),
        changes,
        schedules,
        member_rules: file.secrets.members,
        http: file.http,
        ..HubConfig::default()
    };
    if let Some(i) = file.instance {
        cfg.instance = i;
    }
    Ok((nats, cfg, notifier))
}

fn hub_from(
    nats: &borant_nats::Nats,
    cfg: HubConfig,
    notifier: Option<Box<dyn Notifier>>,
) -> Arc<Hub> {
    let hub = Hub::new(
        Arc::new(nats.state()),
        Arc::new(nats.bus()),
        Arc::new(nats.session()),
        Arc::new(SystemClock),
        cfg,
    )
    // Every hub write publishes its swarm event to the durable log.
    .with_events(Arc::new(nats.events()));
    // Without a [notifiers] entry the alerts stay in KV and publish events;
    // they only skip the notice.
    Arc::new(match notifier {
        Some(n) => hub.with_notifier(Arc::from(n)),
        None => hub,
    })
}

pub async fn run_hub() -> anyhow::Result<()> {
    let (nats, cfg, notifier) = hub_parts("borant-hub").await?;
    let hub = hub_from(&nats, cfg, notifier);
    // Retry until NATS is up, so the hub can start before the broker.
    let shutdown = shutdown_on_signal();
    loop {
        match hub.clone().run(shutdown.clone()).await {
            Ok(()) => return Ok(()),
            Err(e) if shutdown.is_cancelled() => return Err(e),
            Err(e) => {
                tracing::warn!("hub start failed, retrying in 2 s: {e:#}");
                tokio::select! {
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
            }
        }
    }
}

pub async fn run_host() -> anyhow::Result<()> {
    let file = config::host_file()?;
    let mut cfg = config::host_config(&file).await?;
    // The start check of `[sandbox]`: a host that cannot sandbox still runs
    // its agents, but offers no sandbox to the hub.
    borant_host::sandbox::probe(&mut cfg).await;
    // The desktop notifier stays the default on a laptop; [alerts].notifier
    // replaces it, for example with the phone plugin. A bad name fails the
    // start, before the daemon takes a socket.
    let notifier: Box<dyn Notifier> = match file.notifier()? {
        Some(n) => n,
        None => Box::new(DesktopNotifier),
    };
    cfg.socket = Some(config::socket());
    if tokio::net::UnixStream::connect(cfg.socket()).await.is_ok() {
        anyhow::bail!(
            "another host daemon is running on {}; stop it first",
            cfg.socket().display()
        );
    }
    tokio::fs::create_dir_all(&cfg.home).await?;
    let nats =
        borant_nats::connect(&file.nats.to_config(&format!("borant-host-{}", cfg.name))).await?;
    let launcher = borant_launch::launcher_for_with(
        &cfg.launcher,
        cfg.home.join("logs"),
        &file.herdr_config(),
    )
    .map_err(anyhow::Error::msg)?;
    let outbox = Outbox::open(&cfg.home.join("outbox.db"))?;
    let socket = config::socket();
    tracing::info!(host = %cfg.name, launcher = %cfg.launcher, harnesses = ?cfg.harnesses, "host daemon starting");
    tracing::info!(alerts = %file.alerts.summary(), "the alert rules of this host");
    let host = Host::new(
        cfg,
        Arc::new(nats.bus()),
        Arc::new(nats.session()),
        Arc::new(SystemClock),
        launcher,
        Some(notifier),
        Some(Arc::new(nats.events())),
        outbox,
    );
    if socket.as_os_str().len() > 100 {
        anyhow::bail!(
            "the socket path {} is too long for a Unix socket; set BORANT_SOCKET to a shorter path",
            socket.display()
        );
    }
    let shutdown = shutdown_on_signal();
    let api = {
        let host = host.clone();
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            let r = borant_host::api::serve(host, &socket, shutdown.clone()).await;
            if let Err(e) = &r {
                tracing::error!("local API failed: {e:#}");
                shutdown.cancel();
            }
            r
        })
    };
    host.run(shutdown.clone()).await;
    // Long polls of idle waiters can hold the API open for minutes, so the
    // graceful shutdown gets a short limit.
    match tokio::time::timeout(Duration::from_secs(3), api).await {
        Ok(r) => r.context("local API task")??,
        Err(_) => tracing::info!("closing open API connections"),
    }
    Ok(())
}

pub async fn export(out: &Path, snapshot: bool) -> anyhow::Result<()> {
    if snapshot {
        return export_snapshot(out).await;
    }
    let (nats, cfg, notifier) = hub_parts("borant-export").await?;
    let bus = nats.bus();
    let on = serde_json::to_vec(&CtlRequest::Maintenance { on: true })?;
    match borant_core::ports::MessageBus::request(
        &bus,
        subjects::CTL,
        on.into(),
        Duration::from_secs(5),
    )
    .await
    {
        Ok(_) => eprintln!("The running hub is now in maintenance mode."),
        Err(e) => eprintln!("No running hub answered ({e}). Exporting the stores as they are."),
    }
    let hub = hub_from(&nats, cfg, notifier);
    let file = std::fs::File::create(out).with_context(|| format!("create {}", out.display()))?;
    let mut w = BufWriter::new(file);
    borant_hub::export::export(&hub, &mut w).await?;
    drop(w);
    eprintln!(
        "Wrote {}. Stop this hub before you start the new one.",
        out.display()
    );
    Ok(())
}

/// Asks the running hub for a snapshot. The file lands on the hub box, so
/// `out` must be absolute; the command itself works from any host and
/// without maintenance mode.
async fn export_snapshot(out: &Path) -> anyhow::Result<()> {
    if !out.is_absolute() {
        anyhow::bail!("--out must be an absolute path on the hub box");
    }
    let (nats, _, _) = hub_parts("borant-export").await?;
    let bus = nats.bus();
    let payload = serde_json::to_vec(&CtlRequest::Snapshot {
        out: out.to_path_buf(),
    })?;
    let reply = borant_core::ports::MessageBus::request(
        &bus,
        subjects::CTL,
        payload.into(),
        Duration::from_secs(60),
    )
    .await
    .with_context(|| "no running hub answered the snapshot request")?;
    let reply: borant_core::proto::Reply =
        serde_json::from_slice(&reply).context("the hub sent an unreadable snapshot reply")?;
    let report = reply
        .into_result()
        .map_err(|e| anyhow::anyhow!(e.message))?;
    println!(
        "Wrote {}: {} state keys, {} session events, {} pending messages (hub lock {} ms).",
        report["path"].as_str().unwrap_or("?"),
        report["state"].as_u64().unwrap_or(0),
        report["sessions"].as_u64().unwrap_or(0),
        report["pending"].as_u64().unwrap_or(0),
        report["lock_ms"].as_u64().unwrap_or(0),
    );
    Ok(())
}

pub async fn import(input: &Path) -> anyhow::Result<()> {
    let (nats, _, _) = hub_parts("borant-import").await?;
    let file = std::fs::File::open(input).with_context(|| format!("open {}", input.display()))?;
    let mut r = BufReader::new(file);
    let report =
        borant_hub::export::import(&nats.state(), &nats.bus(), &nats.session(), &mut r).await?;
    eprintln!(
        "Imported {} state keys, {} session events, and {} pending messages.",
        report.state, report.sessions, report.pending
    );
    Ok(())
}

/// Exports here, copies the file to `ssh_host`, and imports it there with
/// the `borant` binary of that host.
pub async fn hub_move(ssh_host: &str) -> anyhow::Result<()> {
    let tmp = std::env::temp_dir().join(format!("borant-export-{}.jsonl", std::process::id()));
    export(&tmp, false).await?;
    let remote_file = "/tmp/borant-hub-import.jsonl";
    let status = tokio::process::Command::new("scp")
        .arg(&tmp)
        .arg(format!("{ssh_host}:{remote_file}"))
        .status()
        .await?;
    anyhow::ensure!(status.success(), "scp to {ssh_host} failed");
    let status = tokio::process::Command::new("ssh")
        .args([ssh_host, "borant", "hub", "import", remote_file])
        .status()
        .await?;
    anyhow::ensure!(status.success(), "the import on {ssh_host} failed");
    eprintln!(
        "Next: stop the hub on this machine, start `borant hub` on {ssh_host}, and point the hosts at its NATS URL."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cli_snapshot_out_must_be_absolute() {
        let err = export(Path::new("hub.jsonl"), true).await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "--out must be an absolute path on the hub box"
        );
    }
}
