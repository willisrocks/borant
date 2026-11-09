//! A stop or a move ends the whole tree of a session, not only its harness: a
//! tool that leaves the group of the harness (`setsid`), that is reparented
//! with its cwd in the agent's work dir, or that daemonizes away from both
//! while keeping the agent in its environment, must not keep a host's memory.
//! The sweep never reaches a sibling agent's work dir, or anything outside the
//! work root.

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex, Once};
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use borant_core::model::{Harness, MoveReason};
use borant_core::proto::{HostCommand, LaunchOrder, LocalRegister, LocalSession, LocalState};
use borant_core::{AgentId, SystemClock};
use borant_host::outbox::Outbox;
use borant_host::{Host, HostConfig};
use borant_launch::{TmuxConfig, TmuxLauncher};
use borant_memory::{MemoryBus, MemorySession};

mod common;
use common::Fx;

/// Kills the process when the test ends, even if an assert fails.
struct KillOnDrop(u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

fn system_for(pid: u32) -> sysinfo::System {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    system
}

/// True while the process runs. A zombie is dead: its pid only waits for its
/// parent to reap it, and no signal reaches it.
fn alive(pid: u32) -> bool {
    system_for(pid)
        .process(sysinfo::Pid::from_u32(pid))
        .is_some_and(|p| {
            !matches!(
                p.status(),
                sysinfo::ProcessStatus::Zombie | sysinfo::ProcessStatus::Dead
            )
        })
}

/// The parent of `pid`.
fn parent_of(pid: u32) -> Option<u32> {
    system_for(pid)
        .process(sysinfo::Pid::from_u32(pid))
        .and_then(|p| p.parent())
        .map(|parent| parent.as_u32())
}

/// True when `pid` is a descendant of `root`.
fn below(pid: u32, root: u32) -> bool {
    let mut cur = parent_of(pid);
    for _ in 0..64 {
        match cur {
            Some(a) if a == root => return true,
            Some(a) => cur = parent_of(a),
            None => return false,
        }
    }
    false
}

fn setsid_available() -> bool {
    if Command::new("setsid").arg("--version").output().is_ok() {
        return true;
    }
    eprintln!("skipping the setsid child: setsid is not on PATH");
    false
}

fn tmux_available() -> bool {
    if Command::new("tmux").arg("-V").output().is_ok() {
        return true;
    }
    eprintln!("skipping the tmux test: tmux is not on PATH");
    false
}

/// A name that no other test or process uses.
fn unique(tag: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{tag}-{}-{nanos}", std::process::id())
}

/// A tmux call on the test's own socket, never on a host's server.
fn tmux(socket: &str, args: &[&str]) -> std::process::Output {
    Command::new("tmux")
        .args(["-L", socket])
        .args(args)
        .output()
        .expect("run tmux")
}

/// Kills the test's tmux session when the test ends, on its own socket.
struct KillSessionOnDrop {
    socket: String,
    name: String,
}

impl Drop for KillSessionOnDrop {
    fn drop(&mut self) {
        let _ = tmux(
            &self.socket,
            &["kill-session", "-t", &format!("={}", self.name)],
        );
    }
}

/// A host whose launcher starts agents in tmux on the test's own socket.
fn tmux_host(dir: &Path, socket: &str, script: String) -> Host {
    let mut cfg = HostConfig::new("h1".parse().unwrap(), dir.to_path_buf());
    cfg.harnesses = vec![Harness::Fake];
    cfg.commands.fake = Some(vec!["sh".into(), "-c".into(), script]);
    Host::new(
        cfg,
        Arc::new(MemoryBus::new()),
        Arc::new(MemorySession::new()),
        Arc::new(SystemClock),
        Box::new(TmuxLauncher::new(TmuxConfig {
            socket: socket.to_owned(),
        })),
        None,
        None,
        Outbox::in_memory().unwrap(),
    )
}

/// The lines that the subscriber formats, per thread: the tests of this file
/// run in one process, and each one reads its own.
static LINES: LazyLock<Mutex<HashMap<ThreadId, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lines() -> &'static Mutex<HashMap<ThreadId, Vec<u8>>> {
    &LINES
}

/// A writer that keeps what the subscriber formats, for the thread that
/// formats it.
#[derive(Clone)]
struct Capture;

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        lines()
            .lock()
            .expect("the capture is not poisoned")
            .entry(std::thread::current().id())
            .or_default()
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Installs the capture once for the whole test binary. A scoped subscriber is
/// not enough: a callsite that another test hits first, outside any
/// subscriber, would stay off, and the sweep's lines would never reach the
/// capture.
fn install_capture() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(Capture)
            .finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// Runs `f` with the capture, and returns the lines of this thread.
async fn log_lines<F>(f: F) -> String
where
    F: Future<Output = ()>,
{
    install_capture();
    let me = std::thread::current().id();
    lines()
        .lock()
        .expect("the capture is not poisoned")
        .remove(&me);
    f.await;
    let bytes = lines()
        .lock()
        .expect("the capture is not poisoned")
        .remove(&me)
        .unwrap_or_default();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Waits until the process writes its pid to `path`.
async fn wait_for_pid(path: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if let Ok(pid) = text.trim().parse::<u32>() {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "no pid in {} after 10 s",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until `pid` is no longer a descendant of `root`: the shell that
/// started it is gone, so only the cwd sweep can still name it.
async fn wait_out_of_tree(pid: u32, root: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while below(pid, root) {
        assert!(
            Instant::now() < deadline,
            "the reparented tool (pid {pid}) still hangs below the harness after 10 s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Asserts that the process is gone, within a moment: a SIGKILL lands at once,
/// and a SIGTERM that a tool handles may take a poll.
async fn assert_gone(pid: u32, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "{what} (pid {pid}) survived the sweep"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Asserts that the process still runs: the sweep must not reach it.
async fn assert_alive(pid: u32, what: &str) {
    // A kill that the sweep does send would land at once; a moment of grace
    // catches one that a task sends late.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid), "{what} (pid {pid}) did not survive the sweep");
}

/// A process of its own with its cwd in `dir`, which outlives the test's
/// harness as a tool does. It leads its own process group, as the launcher
/// starts the harness, so the daemon's group guard hides nothing from the
/// sweep. The caller reaps it.
fn sleeper(dir: &Path) -> Child {
    Command::new("sh")
        .arg("-c")
        .arg("exec sleep 300")
        .current_dir(dir)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start a sleeper")
}

/// Kills and reaps a process that a test started, after the assertions.
fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// A fake harness in its own process group, as the process launcher starts
/// one: a grandchild that is reparented out of the harness with its cwd in
/// `cwd`, and, with `setsid`, a child that stays in the tree but leaves the
/// process group of the harness and runs with its cwd in `elsewhere`, so only
/// the tree snapshot can name it. Both write their pid to a file. Returns the
/// harness child, the setsid pid file, and the orphan pid file.
fn forking_harness(
    cwd: &Path,
    elsewhere: &Path,
    dir: &Path,
    setsid: bool,
) -> (Child, PathBuf, PathBuf) {
    let escaped = dir.join("escaped.pid");
    let orphan = dir.join("orphan.pid");
    let mut script = if setsid {
        // The tool leaves both the tree and the process group of the harness
        // before the stop: only the cwd sweep can name it.
        format!(
            "( cd \"{cwd}\"; setsid sh -c 'echo $$ > \"{orphan}\"; exec sleep 300' & ) ;",
            cwd = cwd.display(),
            orphan = orphan.display(),
        )
    } else {
        format!(
            "( cd \"{cwd}\" && sh -c 'echo $$ > \"{orphan}\"; exec sleep 300' & ) ;",
            cwd = cwd.display(),
            orphan = orphan.display(),
        )
    };
    if setsid {
        script.push_str(&format!(
            " ( cd \"{elsewhere}\"; exec setsid sh -c 'echo $$ > \"{escaped}\"; exec sleep 300' ) &",
            elsewhere = elsewhere.display(),
            escaped = escaped.display(),
        ));
    }
    script.push_str(" while :; do sleep 1; done");
    let child = Command::new("sh")
        .arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start the fake harness");
    (child, escaped, orphan)
}

/// A fake harness that starts a tool which leaves both the tree and the
/// process group of the harness, as `pg_ctl` leaves a database: a double fork
/// plus `setsid`, with its cwd in `elsewhere`, outside the agent's work root
/// that the cwd sweep matches. `env` is the environment of the harness, and
/// the tool inherits it, as every process of a launch inherits `BORANT_AGENT`.
/// The tool writes its pid to `pid_file`. When `setsid` is missing, the tool
/// stays in the group of the harness and the test must skip.
fn daemonizing_harness(
    cwd: &Path,
    env: &[(&str, &str)],
    elsewhere: &Path,
    pid_file: &Path,
) -> Child {
    let script = format!(
        "( cd \"{elsewhere}\"; setsid sh -c 'echo $$ > \"{pid}\"; exec sleep 300' & ) ; \
         while :; do sleep 1; done",
        elsewhere = elsewhere.display(),
        pid = pid_file.display(),
    );
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .process_group(0)
        // The test runner can itself run in an agent's session; a test that
        // checks for the absence of the variable must not inherit one.
        .env_remove("BORANT_AGENT");
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.spawn().expect("start the fake harness")
}

/// Registers a fake harness as the session of `agent`, with its cwd in `cwd`.
async fn register(fx: &Fx, agent: &AgentId, cwd: &Path, pid: u32) -> LocalSession {
    let session = fx
        .host
        .register(LocalRegister {
            harness: Harness::Fake,
            cwd: cwd.to_string_lossy().into_owned(),
            pid: Some(pid),
            agent: Some(agent.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(session.state, LocalState::Attached, "{session:?}");
    session
}

/// A plain harness that stays alive in its own process group, as the process
/// launcher starts one.
fn stayer(cwd: &Path) -> Child {
    Command::new("sh")
        .arg("-c")
        .arg("while :; do sleep 1; done")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start the fake harness")
}

/// A stop ends the harness, a child that left its process group with
/// `setsid`, and a grandchild that was reparented with its cwd in the agent's
/// work dir.
#[tokio::test]
async fn a_stop_ends_the_tree_of_the_session() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "tree-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let (mut harness, escaped, orphan) =
        forking_harness(&work, fx.dir.path(), &work, setsid_available());
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let orphan_pid = wait_for_pid(&orphan).await;
    let _orphan_guard = KillOnDrop(orphan_pid);
    wait_out_of_tree(orphan_pid, pid).await;
    assert!(alive(orphan_pid), "the reparented tool did not start");
    let escaped_pid = if escaped.exists() {
        let p = wait_for_pid(&escaped).await;
        assert!(alive(p), "the setsid child (pid {p}) did not start");
        Some(p)
    } else {
        None
    };
    let _escaped_guard = escaped_pid.map(KillOnDrop);

    let session = register(&fx, &agent, &work, pid).await;
    let logs = log_lines(async {
        fx.host
            .handle_command(HostCommand::Stop {
                session: session.session,
            })
            .await;
    })
    .await;

    assert_gone(pid, "the harness").await;
    assert_gone(orphan_pid, "the reparented tool").await;
    if let Some(p) = escaped_pid {
        assert_gone(p, "the setsid child").await;
    }
    // One line for each killed process, with its name, RSS, and why, and one
    // summary. The command line of a tool never reaches the log.
    assert!(
        logs.contains("killed a process of a stopped session"),
        "no line for a killed process: {logs}"
    );
    assert!(
        logs.contains(&format!("pid={orphan_pid}")),
        "no line for the reparented tool: {logs}"
    );
    assert!(
        logs.contains("why=\"cwd\"") || logs.contains("why=cwd"),
        "no cwd line: {logs}"
    );
    if escaped_pid.is_some() {
        assert!(
            logs.contains("why=\"tree\"") || logs.contains("why=tree"),
            "no tree line: {logs}"
        );
    }
    assert!(
        logs.contains("rss_mb="),
        "the lines do not carry the RSS: {logs}"
    );
    assert!(
        !logs.contains("sleep 300"),
        "the log carries the arguments of a tool: {logs}"
    );
    assert!(
        logs.contains("swept the processes of a stopped session"),
        "no summary line: {logs}"
    );
    let status = harness.try_wait().expect("wait for the harness");
    assert!(status.is_some(), "the harness still runs after the stop");
}

/// The cwd sweep of agent `a` never matches a path of agent `a1`: `starts_with`
/// compares whole components.
#[tokio::test]
async fn a_stop_spares_the_processes_of_a_sibling_agent() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "sib".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    // A sibling whose name only differs by a suffix, as `a` and `a1` do.
    let sibling = fx
        .dir
        .path()
        .join("work")
        .join(format!("{}1", agent.as_str()));
    std::fs::create_dir_all(sibling.join("sub")).unwrap();
    let mut exact = sleeper(&sibling);
    let mut nested = sleeper(&sibling.join("sub"));
    let exact_pid = exact.id();
    let nested_pid = nested.id();
    let _exact_guard = KillOnDrop(exact_pid);
    let _nested_guard = KillOnDrop(nested_pid);

    let mut harness = stayer(&work);
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(pid, "the harness").await;
    assert_alive(exact_pid, "a process with its cwd in the sibling dir").await;
    assert_alive(nested_pid, "a process under the sibling dir").await;
    reap(&mut exact);
    reap(&mut nested);
    let _ = harness.try_wait();
}

/// The sweep of an agent's own dir never reaches a process outside the work
/// root, or the work root itself.
#[tokio::test]
async fn a_stop_spares_a_process_outside_the_work_root() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "root-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let outside = fx.dir.path().join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let root = fx.dir.path().join("work");
    let mut elsewhere = sleeper(&outside);
    let mut at_root = sleeper(&root);
    let elsewhere_pid = elsewhere.id();
    let at_root_pid = at_root.id();
    let _guard_a = KillOnDrop(elsewhere_pid);
    let _guard_b = KillOnDrop(at_root_pid);

    let mut harness = stayer(&work);
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(pid, "the harness").await;
    assert_alive(elsewhere_pid, "a process outside the work root").await;
    assert_alive(at_root_pid, "a process in the work root itself").await;
    reap(&mut elsewhere);
    reap(&mut at_root);
    let _ = harness.try_wait();
}

/// A tool that daemonizes, as `pg_ctl` does, leaves both the tree and the work
/// dir: only the agent in its environment still names it, and a stop ends it.
#[tokio::test]
async fn a_stop_ends_a_daemonized_tool_by_its_agent_env() {
    if !setsid_available() {
        return;
    }
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "env-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    // The data dir of the daemonized tool, outside the agent's work root.
    let pgdata = fx.dir.path().join("pgdata");
    std::fs::create_dir_all(&pgdata).unwrap();
    let pid_file = fx.dir.path().join("pg.pid");
    let mut harness = daemonizing_harness(
        &work,
        &[
            ("BORANT_AGENT", agent.as_str()),
            // A value that must never reach the log.
            ("PGPASSWORD", "hunter2-env-secret"),
        ],
        &pgdata,
        &pid_file,
    );
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let tool = wait_for_pid(&pid_file).await;
    let _tool_guard = KillOnDrop(tool);
    wait_out_of_tree(tool, pid).await;
    assert!(alive(tool), "the daemonized tool did not start");

    let session = register(&fx, &agent, &work, pid).await;
    let logs = log_lines(async {
        fx.host
            .handle_command(HostCommand::Stop {
                session: session.session,
            })
            .await;
    })
    .await;

    assert_gone(pid, "the harness").await;
    assert_gone(tool, "the daemonized tool").await;
    assert!(
        logs.contains(&format!("pid={tool}")),
        "no line for the daemonized tool: {logs}"
    );
    assert!(
        logs.contains("why=\"env\"") || logs.contains("why=env"),
        "no env line: {logs}"
    );
    assert!(
        !logs.contains("hunter2-env-secret"),
        "the log carries an environment value: {logs}"
    );
    let _ = harness.try_wait();
}

/// The env match is exact: a stop of agent `a` never ends a daemonized tool of
/// agent `a1`, even though its cwd is outside both work dirs.
#[tokio::test]
async fn a_stop_spares_a_daemon_of_a_sibling_agent_env() {
    if !setsid_available() {
        return;
    }
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "env-sib".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let pgdata = fx.dir.path().join("pgdata");
    std::fs::create_dir_all(&pgdata).unwrap();
    let pid_file = fx.dir.path().join("pg.pid");
    let mut harness = daemonizing_harness(
        &work,
        &[("BORANT_AGENT", &format!("{}1", agent.as_str()))],
        &pgdata,
        &pid_file,
    );
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let tool = wait_for_pid(&pid_file).await;
    let _tool_guard = KillOnDrop(tool);
    wait_out_of_tree(tool, pid).await;
    assert!(alive(tool), "the daemonized tool did not start");

    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(pid, "the harness").await;
    assert_alive(tool, "a daemonized tool of the sibling agent").await;
    let _ = harness.try_wait();
}

/// A process outside the work dir that carries no `BORANT_AGENT` is no
/// session's to end: a stop spares it.
#[tokio::test]
async fn a_stop_spares_a_daemon_without_the_agent_env() {
    if !setsid_available() {
        return;
    }
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "env-none".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let pgdata = fx.dir.path().join("pgdata");
    std::fs::create_dir_all(&pgdata).unwrap();
    let pid_file = fx.dir.path().join("pg.pid");
    let mut harness = daemonizing_harness(&work, &[], &pgdata, &pid_file);
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let tool = wait_for_pid(&pid_file).await;
    let _tool_guard = KillOnDrop(tool);
    wait_out_of_tree(tool, pid).await;
    assert!(alive(tool), "the daemonized tool did not start");

    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(pid, "the harness").await;
    assert_alive(tool, "a process outside the work dir without the agent env").await;
    let _ = harness.try_wait();
}

/// A planned move ends a session on the old host with the same sweep: the
/// tools of the moved agent go, and the harness with them.
#[tokio::test]
async fn a_move_ends_the_tree_of_the_session() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "move-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let (mut harness, escaped, orphan) =
        forking_harness(&work, fx.dir.path(), &work, setsid_available());
    let pid = harness.id();
    let _guard = KillOnDrop(pid);
    let orphan_pid = wait_for_pid(&orphan).await;
    let _orphan_guard = KillOnDrop(orphan_pid);
    wait_out_of_tree(orphan_pid, pid).await;
    let escaped_pid = if escaped.exists() {
        Some(wait_for_pid(&escaped).await)
    } else {
        None
    };
    let _escaped_guard = escaped_pid.map(KillOnDrop);
    let session = register(&fx, &agent, &work, pid).await;

    fx.host
        .handle_command(HostCommand::PrepareMove {
            agent: agent.clone(),
            session: session.session,
        })
        .await;

    assert!(
        fx.host.local_view(session.session).is_err(),
        "the moved session is still known on the old host"
    );
    assert_gone(pid, "the harness of the moved session").await;
    assert_gone(orphan_pid, "the reparented tool of the moved session").await;
    if let Some(p) = escaped_pid {
        assert_gone(p, "the setsid child of the moved session").await;
    }
    let _ = harness.try_wait();
}

/// A fresh launch of an agent ends what a session of the same agent left in
/// its work dir, before the new harness starts, and never a sibling's.
#[tokio::test]
async fn a_launch_ends_the_leftovers_in_the_agent_dir() {
    let fx = Fx::new(true, |cfg| {
        // The harness exits at once: the test looks at the sweep, not at a
        // session that would never register.
        cfg.commands.fake = Some(vec!["true".into()]);
    })
    .await;
    let agent: AgentId = "boot".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(work.join("repo")).unwrap();
    let sibling = fx.dir.path().join("work").join("boot1");
    std::fs::create_dir_all(&sibling).unwrap();
    // A tool of an older session of the agent, and one of another agent.
    let mut leftover = sleeper(&work.join("repo"));
    let mut other = sleeper(&sibling);
    let leftover_pid = leftover.id();
    let other_pid = other.id();
    let _guard_a = KillOnDrop(leftover_pid);
    let _guard_b = KillOnDrop(other_pid);

    let order = LaunchOrder {
        agent: agent.clone(),
        harness: Harness::Fake,
        repo: None,
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        ..Default::default()
    };
    fx.host
        .launch(order)
        .await
        .expect("the launch of the agent");

    assert_gone(leftover_pid, "a leftover of the agent's last session").await;
    assert_alive(other_pid, "a process of the sibling agent").await;
    reap(&mut leftover);
    reap(&mut other);
}

/// A stop of a tmux launch sweeps the pane pids of its handle: a tool that
/// left the group of the pane and runs outside the agent's dir is still found,
/// because the snapshot starts at the pane and not only at a harness pid.
#[tokio::test]
async fn a_stop_ends_the_tree_of_a_tmux_launch() {
    if !tmux_available() || !setsid_available() {
        return;
    }
    let socket = unique("test-tree");
    let agent: AgentId = unique("w").parse().unwrap();
    let name = format!("borant-{agent}");
    let _session_guard = KillSessionOnDrop {
        socket: socket.clone(),
        name,
    };
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let pane_file = dir.path().join("pane.pid");
    let escaped_file = dir.path().join("escaped.pid");
    let script = format!(
        "echo $$ > \"{pane}\"; \
         ( cd \"{outside}\"; exec setsid sh -c 'echo $$ > \"{escaped}\"; exec sleep 300' ) & \
         sleep 300",
        pane = pane_file.display(),
        outside = outside.display(),
        escaped = escaped_file.display(),
    );
    let host = tmux_host(dir.path(), &socket, script);
    host.launch(LaunchOrder {
        agent: agent.clone(),
        harness: Harness::Fake,
        repo: None,
        prompt: "go".into(),
        reason: MoveReason::Spawn,
        ..Default::default()
    })
    .await
    .expect("the launch of the agent");
    let pane_pid = wait_for_pid(&pane_file).await;
    let escaped_pid = wait_for_pid(&escaped_file).await;
    let _pane_guard = KillOnDrop(pane_pid);
    let _escaped_guard = KillOnDrop(escaped_pid);
    assert!(alive(escaped_pid), "the tool did not start");
    // The tool hangs below the pane, in its own group: the pane pid is what
    // names it, not a harness pid and not the agent's cwd.
    assert!(
        below(escaped_pid, pane_pid),
        "the tool (pid {escaped_pid}) is not below the pane (pid {pane_pid})"
    );

    let logs = log_lines(async { host.stop_closed_launch(&agent).await }).await;

    assert_gone(pane_pid, "the tmux pane process").await;
    assert_gone(escaped_pid, "the tool that left the pane group").await;
    assert!(
        logs.contains(&format!("pid={escaped_pid}")),
        "no line for the tool of the pane: {logs}"
    );
    assert!(
        logs.contains("why=\"tree\"") || logs.contains("why=tree"),
        "no tree line for the pane: {logs}"
    );
}

/// A copy of the shell named `name`, so a test can start a process whose
/// executable carries that name, as a multiplexer server's does. A copied
/// coreutils binary is no good: those dispatch on their own name, and one
/// named `tmux` exits at once.
fn copy_sh_as(dir: &Path, name: &str) -> Option<PathBuf> {
    let src = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join("sh"))
        .find(|path| path.is_file())?;
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).ok()?;
    Some(dst)
}

/// A multiplexer server daemonizes without a chdir, so it can keep a cwd in an
/// agent's dir that it does not belong to. A cwd sweep never ends one: a
/// signal would take every session of the host with it.
#[tokio::test]
async fn a_stop_spares_a_multiplexer_server_in_the_agent_dir() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "mux-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let bin = fx.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let Some(server_bin) = copy_sh_as(&bin, "tmux") else {
        eprintln!("skipping: no sh on PATH to name as a server");
        return;
    };
    // A real process whose executable is named `tmux`, with its cwd in the
    // agent's dir, as a server that a daemon started from there would be.
    let mut server = Command::new(&server_bin)
        .args(["-c", "while :; do sleep 1; done"])
        .current_dir(&work)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start the fake server");
    let server_pid = server.id();
    let _server_guard = KillOnDrop(server_pid);

    let mut harness = stayer(&work);
    let pid = harness.id();
    let _harness_guard = KillOnDrop(pid);
    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(pid, "the harness").await;
    assert_alive(server_pid, "a multiplexer server in the agent dir").await;
    reap(&mut server);
    let _ = harness.try_wait();
}

/// A cwd target gets its own signal, never its group: its group can hold a
/// process that works outside the agent's dir and is no session's to end.
#[tokio::test]
async fn a_stop_ends_a_cwd_target_without_its_group() {
    let fx = Fx::new(true, |_| {}).await;
    let agent: AgentId = "grp-a".parse().unwrap();
    let work = fx.dir.path().join("work").join(agent.as_str());
    std::fs::create_dir_all(&work).unwrap();
    let outside = fx.dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let member_file = fx.dir.path().join("member.pid");
    // The leader works in the agent's dir, leads its own group, and its
    // background member works outside the dir.
    let mut leader = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "( cd \"{outside}\"; exec sh -c 'echo $$ > \"{member}\"; exec sleep 300' ) & \
             while :; do sleep 1; done",
            outside = outside.display(),
            member = member_file.display(),
        ))
        .current_dir(&work)
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("start a group leader");
    let leader_pid = leader.id();
    let _leader_guard = KillOnDrop(leader_pid);
    let member_pid = wait_for_pid(&member_file).await;
    let _member_guard = KillOnDrop(member_pid);
    assert!(alive(member_pid), "the member did not start");
    // Same group, so a killpg on the leader would reach it.
    assert_eq!(
        parent_of(member_pid),
        Some(leader_pid),
        "the member is not a child of the leader"
    );

    let mut harness = stayer(&work);
    let pid = harness.id();
    let _harness_guard = KillOnDrop(pid);
    let session = register(&fx, &agent, &work, pid).await;
    fx.host
        .handle_command(HostCommand::Stop {
            session: session.session,
        })
        .await;

    assert_gone(leader_pid, "the cwd target").await;
    assert_alive(member_pid, "a group member outside the agent dir").await;
    reap(&mut leader);
    let _ = harness.try_wait();
}
