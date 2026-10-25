//! The `borant secret-exec` shim. The host writes the secrets of a session
//! into a directory and starts the harness as `borant secret-exec <dir> -- …`,
//! so the values reach one program and nothing else.
//!
//! The workspace forbids unsafe code, so the variables are not written into
//! the process environment. Each command and the program itself get them with
//! `Command::envs`, which the exec'd program cannot tell apart.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

/// The value file, relative to the secret directory. One `KEY=VALUE` per line.
pub const ENV_FILE: &str = "env";
/// The optional pre-command file, relative to the secret directory. A JSON
/// list of argv lists. `{dir}` in an argument stands for the directory.
pub const PRE_FILE: &str = "pre.json";

/// Takes the secrets of `dir`, runs its pre commands, and replaces this
/// process with `command`. Returns only when the program cannot run.
pub fn run(dir: &Path, command: &[String]) -> anyhow::Result<()> {
    let (program, args) = command.split_first().context("no program to run")?;
    let vars = take_env(dir)?;
    for argv in pre_commands(dir)? {
        let pre = &argv[0];
        let status = Command::new(pre)
            .args(argv[1..].iter().map(|a| with_dir(a, dir)))
            .envs(&vars)
            .status()
            .with_context(|| format!("cannot run the pre command {pre}"))?;
        if !status.success() {
            bail!("the pre command {pre} failed with {status}");
        }
    }
    let err = Command::new(program).args(args).envs(&vars).exec();
    Err(anyhow::anyhow!("cannot run {program}: {err}"))
}

/// Reads `<dir>/env`, deletes it, and returns its variables.
fn take_env(dir: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let path = dir.join(ENV_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            bail!("no {ENV_FILE} file in {}", dir.display())
        }
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    // The file holds the only copy: delete it before anything can fail with
    // the values still on disk.
    fs::remove_file(&path).with_context(|| format!("cannot delete {}", path.display()))?;
    let mut vars = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            bail!("line {} of {ENV_FILE} is not KEY=VALUE", n + 1);
        };
        if name.is_empty() {
            bail!("line {} of {ENV_FILE} has an empty name", n + 1);
        }
        vars.insert(name.to_owned(), value.to_owned());
    }
    Ok(vars)
}

/// Reads `<dir>/pre.json`. A missing file means no pre commands.
fn pre_commands(dir: &Path) -> anyhow::Result<Vec<Vec<String>>> {
    let path = dir.join(PRE_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    let commands: Vec<Vec<String>> =
        serde_json::from_str(&text).with_context(|| format!("bad {}", path.display()))?;
    if commands
        .iter()
        .any(|argv| argv.first().is_none_or(String::is_empty))
    {
        bail!("{} holds a pre command without a program", path.display());
    }
    Ok(commands)
}

fn with_dir(arg: &str, dir: &Path) -> String {
    arg.replace("{dir}", &dir.display().to_string())
}
