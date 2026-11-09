//! The sweep that ends every process of a session. A stop signals the harness
//! and the group that it leads, but a tool can leave that group (`setsid`, or
//! its own process group, as language servers do), outlive the tmux session,
//! and be reparented to PID 1: an orphaned language server then holds
//! gigabytes on a host that no longer runs the agent. The host snapshots the
//! tree of the session before it signals anything, and afterwards also sweeps
//! the processes that left the tree but kept a working directory in the
//! agent's own work dir.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use borant_core::AgentId;
use nix::sys::signal::{Signal, kill, killpg};
use nix::unistd::Pid as NixPid;
use sysinfo::{ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind};

use crate::STOP_GRACE;

/// How often the sweep looks whether the processes that it signalled are gone.
const POLL: Duration = Duration::from_millis(50);

/// Why the sweep signals a process.
#[derive(Clone, Copy)]
enum Why {
    /// A descendant of the harness, or of a pane, at snapshot time.
    Tree,
    /// Out of the tree, with its cwd in the agent's work dir.
    Cwd,
}

impl Why {
    fn as_str(self) -> &'static str {
        match self {
            Why::Tree => "tree",
            Why::Cwd => "cwd",
        }
    }
}

/// What a stopped session left, taken before any signal: the harness and pane
/// pids with every descendant of them, and the agent's work dir. A pid keeps
/// its meaning after its parent dies, so the list still names a process that
/// another parent picked up between the snapshot and the signals. Each entry
/// keeps the start time of that process, so a pid that another process took
/// over in the gap is skipped.
pub(crate) struct Snapshot {
    agent: Option<AgentId>,
    pids: Vec<(u32, u64)>,
    work_dir: Option<PathBuf>,
}

/// Snapshots the processes of a session. `roots` are the harness pid and the
/// pane pids of a launcher; `work_dir` is the agent's own work dir, which the
/// cwd sweep matches after the stop. Take it before the first signal: nothing
/// can find the descendants of a process that is gone.
pub(crate) fn snapshot(
    agent: Option<&AgentId>,
    roots: &[u32],
    work_dir: Option<PathBuf>,
) -> Snapshot {
    let mut system = System::new();
    refresh_all(&mut system);
    let mut pids: Vec<u32> = roots.to_vec();
    pids.extend(descendants(&system, roots));
    pids.sort_unstable();
    pids.dedup();
    let pids: Vec<(u32, u64)> = pids
        .into_iter()
        .map(|pid| {
            let started = system
                .process(sysinfo::Pid::from_u32(pid))
                .map(sysinfo::Process::start_time)
                .unwrap_or_default();
            (pid, started)
        })
        .collect();
    Snapshot {
        agent: agent.cloned(),
        pids,
        work_dir,
    }
}

impl Snapshot {
    /// SIGTERMs what the snapshot holds and what the cwd sweep finds, waits out
    /// the grace that the harness gets, and SIGKILLs the rest. One line for
    /// each signalled process, with its name, its RSS, and why the sweep
    /// picked it, and one summary line. The command line is never read, so no
    /// argument of a tool can reach the log.
    pub(crate) async fn finish(self) {
        let own = Own::capture();
        let mut system = System::new();
        refresh_all(&mut system);
        let mut seen: HashSet<u32> = HashSet::new();
        let mut targets: Vec<Target> = Vec::new();
        for (raw, started) in &self.pids {
            seen.insert(*raw);
            let Some(p) = system.process(sysinfo::Pid::from_u32(*raw)) else {
                continue;
            };
            // The pid exists again: another process took it in the gap, and
            // this sweep is not its owner.
            if p.start_time() != *started || !may_signal(*raw, p, &own) {
                continue;
            }
            targets.push(Target::of(*raw, Why::Tree, p));
        }
        if let Some(dir) = &self.work_dir {
            // The kernel reports the cwd of a process with its symlinks
            // resolved, so the work dir is compared that way too.
            let dir = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            for (pid, p) in system.processes() {
                let raw = pid.as_u32();
                if seen.contains(&raw) || !may_signal(raw, p, &own) {
                    continue;
                }
                let Some(cwd) = p.cwd() else {
                    // A cwd that this user cannot read is not this daemon's
                    // to guess at.
                    continue;
                };
                if !in_dir(cwd, &dir) {
                    continue;
                }
                // A multiplexer server keeps the cwd that it was started
                // with, and it is no session's to end: one signal would take
                // every session of the host with it.
                if is_multiplexer(p) {
                    continue;
                }
                seen.insert(raw);
                targets.push(Target::of(raw, Why::Cwd, p));
            }
        }
        let agent = self
            .agent
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| "-".to_owned());
        if targets.is_empty() {
            tracing::debug!(%agent, "no process of the session was left");
            return;
        }
        for t in &targets {
            signal(
                t.pid,
                own.group,
                Signal::SIGTERM,
                matches!(t.why, Why::Tree),
            );
            tracing::info!(
                %agent,
                pid = t.pid,
                process = %t.name,
                rss_mb = t.rss_bytes / (1024 * 1024),
                why = t.why.as_str(),
                "killed a process of a stopped session"
            );
        }
        // The same grace as the harness: a tool may need a moment to close
        // its files, and a hung one gets SIGKILL.
        let deadline = Instant::now() + STOP_GRACE;
        while any_alive(&mut system, &targets) && Instant::now() < deadline {
            tokio::time::sleep(POLL).await;
        }
        for t in &targets {
            if alive(&mut system, t.pid) {
                tracing::warn!(
                    pid = t.pid,
                    "a process of the session ignored SIGTERM, sending SIGKILL"
                );
                signal(
                    t.pid,
                    own.group,
                    Signal::SIGKILL,
                    matches!(t.why, Why::Tree),
                );
            }
        }
        tracing::info!(
            %agent,
            killed = targets.len(),
            "swept the processes of a stopped session"
        );
    }
}

/// One process that the sweep signals.
struct Target {
    pid: u32,
    why: Why,
    name: String,
    rss_bytes: u64,
}

impl Target {
    fn of(pid: u32, why: Why, p: &sysinfo::Process) -> Self {
        Self {
            pid,
            why,
            name: p.name().to_string_lossy().into_owned(),
            rss_bytes: p.memory(),
        }
    }
}

/// The identity that no signal may reach: the daemon itself, its group, the
/// executable that it runs (the hub runs the same binary), and its uid.
struct Own {
    pid: u32,
    group: Option<NixPid>,
    exe: Option<PathBuf>,
    uid: u32,
}

impl Own {
    fn capture() -> Self {
        Self {
            pid: std::process::id(),
            group: nix::unistd::getpgid(None).ok(),
            exe: std::env::current_exe().ok(),
            uid: nix::unistd::getuid().as_raw(),
        }
    }
}

/// The executable names of the multiplexer servers that hold sessions. A
/// server daemonizes without a chdir, so it keeps the cwd of whoever started
/// it, and that cwd can be an agent's work dir that the server does not
/// belong to.
const MULTIPLEXERS: &[&str] = &["tmux", "zellij", "herdr"];

/// True for a multiplexer server: its executable names the server, whatever
/// its cwd says. The process name is the fallback when the exe is unreadable.
fn is_multiplexer(p: &sysinfo::Process) -> bool {
    let exe = p.exe().and_then(|exe| exe.file_name());
    MULTIPLEXERS
        .iter()
        .any(|server| exe.is_some_and(|file| file == OsStr::new(server)) || p.name() == *server)
}

/// True when this daemon may signal `pid`. A signal never reaches the daemon,
/// its group, the hub (the same executable), or a process of another user.
fn may_signal(raw: u32, p: &sysinfo::Process, own: &Own) -> bool {
    if raw <= 1 || raw == own.pid {
        return false;
    }
    if let Some(group) = own.group
        && nix::unistd::getpgid(Some(NixPid::from_raw(raw as i32))).is_ok_and(|pgid| pgid == group)
    {
        return false;
    }
    if own.exe.as_deref().is_some_and(|exe| p.exe() == Some(exe)) {
        return false;
    }
    // An unreadable owner is not a process to signal either.
    p.user_id().is_some_and(|uid| **uid == own.uid)
}

/// Every pid below `roots`, at any depth and in any group, the roots
/// included. `roots` are the pids that the caller knows; the bound guards a
/// stale table whose parent links form a cycle.
fn descendants(system: &System, roots: &[u32]) -> Vec<u32> {
    let mut out = Vec::new();
    for (pid, p) in system.processes() {
        let raw = pid.as_u32();
        let mut parent = p.parent().map(|p| p.as_u32());
        for _ in 0..64 {
            let Some(a) = parent else { break };
            if roots.contains(&a) {
                out.push(raw);
                break;
            }
            parent = system
                .process(sysinfo::Pid::from_u32(a))
                .and_then(|q| q.parent().map(|p| p.as_u32()));
        }
    }
    out
}

/// True when `path` is `dir` or below it. `starts_with` compares whole path
/// components, so agent `a` never matches a path of agent `a1`.
fn in_dir(path: &Path, dir: &Path) -> bool {
    path == dir || path.starts_with(dir)
}

/// Sends `signal` to a process and, when `take_group` is set and the process
/// leads its group, to the group too: a tool that started its own session
/// takes its children with it, as the harness does. Only a tree target takes
/// its group: ancestry says that the group is the session's, while a cwd
/// target can lead a group whose members work outside the agent's dir. The
/// daemon's own group is never signalled.
fn signal(raw: u32, own_group: Option<NixPid>, sig: Signal, take_group: bool) {
    let pid = NixPid::from_raw(raw as i32);
    let leads = take_group
        && nix::unistd::getpgid(Some(pid)).is_ok_and(|pgid| pgid == pid)
        && Some(pid) != own_group;
    if leads {
        let _ = killpg(pid, sig);
    } else {
        let _ = kill(pid, sig);
    }
}

fn refresh_all(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_user(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .without_tasks(),
    );
}

/// True while any target runs. A zombie is dead: its pid only waits for its
/// parent to reap it, and no signal reaches it.
fn any_alive(system: &mut System, targets: &[Target]) -> bool {
    targets.iter().any(|t| alive(system, t.pid))
}

fn alive(system: &mut System, raw: u32) -> bool {
    let pid = sysinfo::Pid::from_u32(raw);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system
        .process(pid)
        .is_some_and(|p| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
}
