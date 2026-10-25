//! The bubblewrap sandbox: a pure argv function in front of a harness program.
//!
//! The sandbox is an allow list of binds, so a path that the host does not
//! name does not exist inside: the credentials, `~/.ssh`, the other agents'
//! clones, and the multiplexer sockets stay outside. The wrapper sits outside
//! the secret shim, so the shim and the harness run in the same namespace.
//!
//! The argv never has `--unshare-pid`: the host tests liveness by the harness
//! PID that the hooks report, and a PID namespace renumbers it.

use std::path::{Path, PathBuf};

/// What one launch binds inside its sandbox. The host builds it; [`wrap`]
/// turns it into the argv, and nothing here does I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPlan {
    /// The sandbox program, for example `bwrap` or a pinned path.
    pub program: String,
    /// The system paths that exist on this host and that a harness needs: the
    /// store and the nix-ld interpreter on NixOS, `/lib64` for a generic ELF,
    /// the resolver, and TLS. A bind of a missing source fails the whole
    /// launch, so the host passes only the ones that exist.
    pub system: Vec<PathBuf>,
    /// The home directory of the host user. It starts empty inside.
    pub home: PathBuf,
    /// The clone of the agent: the only work directory it sees.
    pub work_dir: PathBuf,
    /// The directories of the harness that it reads and writes: sessions,
    /// logs, and its own configuration.
    pub harness_dirs: Vec<PathBuf>,
    /// Unix sockets that the harness reaches, such as the host socket. They
    /// carry MCP, the hooks, and the omp extension.
    pub sockets: Vec<PathBuf>,
    /// The secret directory of the launch. The shim deletes its file, so it
    /// is bound read-write.
    pub secrets_dir: Option<PathBuf>,
    /// Extra read-only binds from `[sandbox] ro` of `host.toml`.
    pub ro: Vec<PathBuf>,
    /// Extra writable binds from `[sandbox] rw` of `host.toml`.
    pub rw: Vec<PathBuf>,
    /// The working directory inside the sandbox, normally the repository.
    pub cwd: PathBuf,
}

/// The program to launch: the sandbox program, its binds, then `program`.
pub fn wrap(plan: &SandboxPlan, program: Vec<String>) -> Vec<String> {
    let mut argv = vec![plan.program.clone()];
    argv.extend(["--die-with-parent", "--unshare-ipc", "--new-session"].map(str::to_owned));
    for path in &plan.system {
        same(&mut argv, "--ro-bind", path);
    }
    argv.extend(["--proc", "/proc", "--dev", "/dev"].map(str::to_owned));
    // An empty /tmp takes the multiplexer socket with it.
    argv.extend(["--tmpfs", "/tmp"].map(str::to_owned));
    // An empty home: every harness path below is a bind of its own.
    one(&mut argv, "--tmpfs", &plan.home);
    for path in &plan.ro {
        same(&mut argv, "--ro-bind", path);
    }
    for path in &plan.harness_dirs {
        same(&mut argv, "--bind", path);
    }
    for path in &plan.sockets {
        same(&mut argv, "--bind", path);
    }
    same(&mut argv, "--bind", &plan.work_dir);
    if let Some(dir) = &plan.secrets_dir {
        same(&mut argv, "--bind", dir);
    }
    for path in &plan.rw {
        same(&mut argv, "--bind", path);
    }
    one(&mut argv, "--chdir", &plan.cwd);
    argv.push("--".to_owned());
    argv.extend(program);
    argv
}

/// Adds `flag <path>`.
fn one(argv: &mut Vec<String>, flag: &str, path: &Path) {
    argv.push(flag.to_owned());
    argv.push(path.to_string_lossy().into_owned());
}

/// Adds `flag <path> <path>`: a bind of a path onto itself.
fn same(argv: &mut Vec<String>, flag: &str, path: &Path) {
    one(argv, flag, path);
    argv.push(path.to_string_lossy().into_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> SandboxPlan {
        let home = PathBuf::from("/home/chris");
        SandboxPlan {
            program: "bwrap".to_owned(),
            system: [
                "/nix/store",
                "/run/current-system",
                "/lib64",
                "/etc/resolv.conf",
                "/etc/ssl",
            ]
            .map(PathBuf::from)
            .to_vec(),
            home: home.clone(),
            work_dir: home.join(".borant/work/w-1"),
            harness_dirs: vec![home.join(".claude"), home.join(".claude.json")],
            sockets: vec![home.join(".borant/host.sock")],
            secrets_dir: Some(PathBuf::from("/run/borant/agents/w-1")),
            ro: vec![home.join(".local")],
            rw: Vec::new(),
            cwd: home.join(".borant/work/w-1/borant"),
        }
    }

    fn claude() -> Vec<String> {
        wrap(
            &plan(),
            ["claude", "--dangerously-skip-permissions", "--", "hi"]
                .map(str::to_owned)
                .to_vec(),
        )
    }

    /// A host without a Nix store binds none of it: a bind of a missing
    /// source fails the whole launch.
    #[test]
    fn a_plan_binds_only_the_system_paths_it_carries() {
        let mut p = plan();
        p.system = vec![PathBuf::from("/lib64")];
        let argv = wrap(&p, vec!["true".to_owned()]);
        assert!(
            !argv.iter().any(|a| a == "/nix/store"),
            "no store in {argv:?}"
        );
        assert!(
            !argv.iter().any(|a| a == "/run/current-system"),
            "no system in {argv:?}"
        );
        let lib = argv.iter().position(|a| a == "/lib64").expect("the lib");
        assert_eq!(argv[lib - 1], "--ro-bind");
    }

    /// The exact argv of a Claude Code launch: every bind, in order, and the
    /// harness last.
    #[test]
    fn the_argv_of_a_claude_launch() {
        assert_eq!(
            claude(),
            [
                "bwrap",
                "--die-with-parent",
                "--unshare-ipc",
                "--new-session",
                "--ro-bind",
                "/nix/store",
                "/nix/store",
                "--ro-bind",
                "/run/current-system",
                "/run/current-system",
                "--ro-bind",
                "/lib64",
                "/lib64",
                "--ro-bind",
                "/etc/resolv.conf",
                "/etc/resolv.conf",
                "--ro-bind",
                "/etc/ssl",
                "/etc/ssl",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--tmpfs",
                "/home/chris",
                "--ro-bind",
                "/home/chris/.local",
                "/home/chris/.local",
                "--bind",
                "/home/chris/.claude",
                "/home/chris/.claude",
                "--bind",
                "/home/chris/.claude.json",
                "/home/chris/.claude.json",
                "--bind",
                "/home/chris/.borant/host.sock",
                "/home/chris/.borant/host.sock",
                "--bind",
                "/home/chris/.borant/work/w-1",
                "/home/chris/.borant/work/w-1",
                "--bind",
                "/run/borant/agents/w-1",
                "/run/borant/agents/w-1",
                "--chdir",
                "/home/chris/.borant/work/w-1/borant",
                "--",
                "claude",
                "--dangerously-skip-permissions",
                "--",
                "hi",
            ]
            .map(str::to_owned)
        );
    }

    /// An omp launch: its own harness directory and the model catalog, and
    /// the secret shim behind the bind of its directory.
    #[test]
    fn the_argv_of_an_omp_launch() {
        let home = PathBuf::from("/home/chris");
        let mut p = plan();
        p.harness_dirs = vec![home.join(".omp")];
        p.ro = vec![
            home.join(".local"),
            home.join(".pi/agent/models-store.json"),
        ];
        let program = [
            "/usr/bin/borant",
            "secret-exec",
            "/run/borant/agents/w-1",
            "--",
            "omp",
            "-p",
            "hi",
        ]
        .map(str::to_owned);
        let argv = wrap(&p, program.to_vec());
        assert_eq!(&argv[argv.len() - program.len()..], program.as_slice());
        assert_eq!(
            &argv[argv.len() - program.len() - 3..argv.len() - program.len()],
            ["--chdir", "/home/chris/.borant/work/w-1/borant", "--"].map(str::to_owned)
        );
        assert!(
            argv.windows(2)
                .any(|w| w == ["--ro-bind", "/home/chris/.pi/agent/models-store.json"])
        );
        assert!(
            argv.windows(2).any(|w| w == ["--bind", "/home/chris/.omp"]),
            "the omp harness directory: {argv:?}"
        );
        assert!(
            argv.windows(2)
                .any(|w| w == ["--bind", "/run/borant/agents/w-1"]),
            "the secrets directory is writable: {argv:?}"
        );
    }

    /// A PID namespace would renumber the harness, and the host tests
    /// liveness by that PID.
    #[test]
    fn never_a_pid_namespace() {
        let argv = claude();
        assert!(!argv.iter().any(|a| a == "--unshare-pid"), "{argv:?}");
        assert!(!argv.iter().any(|a| a == "--unshare-user-try"), "{argv:?}");
    }

    /// Without a secret directory the shim is not there, and no bind names
    /// one.
    #[test]
    fn no_secret_directory_no_bind() {
        let mut p = plan();
        p.secrets_dir = None;
        let argv = wrap(&p, vec!["claude".to_owned()]);
        assert!(
            !argv.iter().any(|a| a.contains("/run/borant/agents")),
            "{argv:?}"
        );
    }

    /// The extra writable binds come after the clones, so a host can widen
    /// the sandbox without breaking the order of the fixed binds.
    #[test]
    fn extra_binds_of_the_host() {
        let mut p = plan();
        p.ro = vec![PathBuf::from("/home/chris/.config/gh")];
        p.rw = vec![PathBuf::from("/var/cache/borant")];
        let argv = wrap(&p, vec!["claude".to_owned()]);
        let ro = argv
            .iter()
            .position(|a| a == "/home/chris/.config/gh")
            .unwrap();
        let rw = argv.iter().position(|a| a == "/var/cache/borant").unwrap();
        let cwd = argv.iter().position(|a| a == "--chdir").unwrap();
        assert!(ro < rw && rw < cwd);
    }
}
