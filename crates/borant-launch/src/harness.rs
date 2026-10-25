//! The program that starts each harness, and where a launch finds it.

use std::path::{Path, PathBuf};

use borant_core::model::Harness;
use serde::Deserialize;

/// Per-harness program overrides from the host configuration. An override
/// replaces the default program and its flags. The prompt is still appended
/// as the last argument, except for the fake harness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HarnessCommands {
    pub claude: Option<Vec<String>>,
    pub pi: Option<Vec<String>>,
    pub omp: Option<Vec<String>>,
    pub codex: Option<Vec<String>>,
    pub fake: Option<Vec<String>>,
}

/// Builds the program for `harness` with `prompt`. The fake harness takes no
/// prompt argument: the caller sets the `BORANT_PROMPT` environment variable.
/// A `model` adds `--model <model>` for Claude Code, Pi, and omp.
///
/// `limit_restart` says that the launch has a member of a bundle with several
/// members. Claude Code then stops at a usage limit instead of waiting for the
/// reset, so the hub can restart the agent on the next member.
pub fn command(
    harness: Harness,
    prompt: &str,
    model: Option<&str>,
    cfg: &HarnessCommands,
    limit_restart: bool,
) -> Vec<String> {
    let custom = custom_argv(harness, cfg);
    let default = default_argv(harness);
    let mut argv = match custom {
        Some(program) => program.clone(),
        None => default.iter().map(|s| (*s).to_owned()).collect(),
    };
    if let Some(m) = model
        && matches!(harness, Harness::ClaudeCode | Harness::Pi | Harness::Omp)
    {
        argv.push("--model".to_owned());
        argv.push(m.to_owned());
    }
    if limit_restart && harness == Harness::ClaudeCode {
        // The session stops at a limit instead of waiting for the reset, so a
        // limit restart can move the agent to another member.
        argv.push("--settings".to_owned());
        argv.push(r#"{"autoContinueAtUsageLimit":false}"#.to_owned());
    }
    if harness != Harness::Fake {
        // `--` ends the options, so a prompt that starts with `-` stays a
        // prompt. Claude Code, Pi, and omp accept it.
        argv.push("--".to_owned());
        argv.push(prompt.to_owned());
    }
    argv
}

/// The `[commands]` override of `harness`, when `host.toml` has one.
fn custom_argv<'a>(harness: Harness, cfg: &'a HarnessCommands) -> &'a Option<Vec<String>> {
    match harness {
        Harness::ClaudeCode => &cfg.claude,
        Harness::Pi => &cfg.pi,
        Harness::Omp => &cfg.omp,
        Harness::Codex => &cfg.codex,
        Harness::Fake => &cfg.fake,
    }
}

/// The program of `harness` and its flags, when `[commands]` has no override.
fn default_argv(harness: Harness) -> &'static [&'static str] {
    match harness {
        Harness::ClaudeCode => &["claude", "--dangerously-skip-permissions"],
        Harness::Pi => &["pi"],
        Harness::Omp => &["omp"],
        Harness::Codex => &["codex", "--dangerously-bypass-approvals-and-sandbox"],
        Harness::Fake => &["borant-fake-harness"],
    }
}

/// The program that a launch of `harness` starts: the first word of
/// [`command`], from the `[commands]` override of the host or from its
/// default. A host offers only the harnesses whose program it finds, and a
/// launch of one whose program is gone fails at once.
pub fn program(harness: Harness, cfg: &HarnessCommands) -> String {
    if let Some(program) = custom_argv(harness, cfg)
        .as_ref()
        .and_then(|argv| argv.first())
    {
        return program.clone();
    }
    default_argv(harness)
        .first()
        .copied()
        .unwrap_or_default()
        .to_owned()
}

/// The directories that a launch searches for the program of a harness: the
/// `PATH` of the daemon, then `~/.local/bin`, where a user install of a
/// harness lands. The sandbox binds `~/.local` read-only for the same reason.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join(".local").join("bin"));
    }
    dirs
}

/// The file of `program` in `dirs`: the path itself when it has a slash, and
/// otherwise the first directory that holds it. `None` when no directory does.
pub fn find_program(dirs: &[PathBuf], program: &str) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains('/') {
        let path = Path::new(program);
        return path.is_file().then(|| path.to_path_buf());
    }
    dirs.iter()
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_program_is_the_first_word_of_the_command() {
        let cfg = HarnessCommands::default();
        assert_eq!(program(Harness::ClaudeCode, &cfg), "claude");
        assert_eq!(program(Harness::Pi, &cfg), "pi");
        assert_eq!(program(Harness::Omp, &cfg), "omp");
        assert_eq!(program(Harness::Fake, &cfg), "borant-fake-harness");

        // An override replaces the program with its first word.
        let cfg = HarnessCommands {
            omp: Some(v(&["/opt/omp", "-v"])),
            fake: Some(Vec::new()),
            ..HarnessCommands::default()
        };
        assert_eq!(program(Harness::Omp, &cfg), "/opt/omp");
        // An empty override names no program, and the default stays.
        assert_eq!(program(Harness::Fake, &cfg), "borant-fake-harness");
    }

    #[test]
    fn find_program_searches_the_dirs_or_takes_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let omp = dir.path().join("omp");
        std::fs::write(&omp, "").unwrap();
        // A directory with the name of a program is not one.
        std::fs::create_dir(dir.path().join("claude")).unwrap();
        let dirs = vec![dir.path().to_path_buf()];

        assert_eq!(find_program(&dirs, "omp"), Some(omp.clone()));
        assert_eq!(find_program(&dirs, "claude"), None);
        assert_eq!(find_program(&dirs, "pi"), None);
        assert_eq!(find_program(&dirs, ""), None);
        // A program with a slash is the path itself, and `dirs` is not used.
        assert_eq!(find_program(&[], omp.to_str().unwrap()), Some(omp));
        assert_eq!(find_program(&dirs, "/nonexistent/omp"), None);
    }

    #[test]
    fn the_search_dirs_hold_the_local_install_dir() {
        // `PATH` comes first, then the install dir of a user harness.
        let dirs = search_dirs();
        let expected = std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/bin"));
        assert_eq!(dirs.last().cloned(), expected);
    }

    #[test]
    fn defaults_and_overrides() {
        let cfg = HarnessCommands::default();
        assert_eq!(
            command(Harness::ClaudeCode, "hi", None, &cfg, false),
            v(&["claude", "--dangerously-skip-permissions", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Pi, "hi", None, &cfg, false),
            v(&["pi", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Omp, "hi", None, &cfg, false),
            v(&["omp", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Codex, "hi", None, &cfg, false),
            v(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--",
                "hi"
            ])
        );
        assert_eq!(
            command(Harness::Fake, "hi", None, &cfg, false),
            v(&["borant-fake-harness"])
        );

        let cfg = HarnessCommands {
            claude: Some(v(&["/opt/claude", "--model", "x"])),
            pi: Some(v(&["pi-dev"])),
            fake: Some(v(&["/bin/fake", "--fast"])),
            ..HarnessCommands::default()
        };
        assert_eq!(
            command(Harness::ClaudeCode, "hi", None, &cfg, false),
            v(&["/opt/claude", "--model", "x", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Pi, "hi", None, &cfg, false),
            v(&["pi-dev", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Omp, "hi", None, &cfg, false),
            v(&["omp", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Fake, "hi", None, &cfg, false),
            v(&["/bin/fake", "--fast"])
        );
    }

    #[test]
    fn model_flag_goes_before_the_prompt() {
        let cfg = HarnessCommands {
            pi: Some(v(&["pi-dev", "-v"])),
            ..HarnessCommands::default()
        };
        let m = Some("opus");
        assert_eq!(
            command(Harness::ClaudeCode, "hi", m, &cfg, false),
            v(&[
                "claude",
                "--dangerously-skip-permissions",
                "--model",
                "opus",
                "--",
                "hi"
            ])
        );
        assert_eq!(
            command(Harness::Pi, "hi", m, &cfg, false),
            v(&["pi-dev", "-v", "--model", "opus", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Omp, "hi", m, &cfg, false),
            v(&["omp", "--model", "opus", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Fake, "hi", m, &cfg, false),
            v(&["borant-fake-harness"])
        );
    }

    #[test]
    fn a_limit_restart_stops_claude_code_at_the_limit() {
        let cfg = HarnessCommands::default();
        assert_eq!(
            command(Harness::ClaudeCode, "hi", None, &cfg, true),
            v(&[
                "claude",
                "--dangerously-skip-permissions",
                "--settings",
                r#"{"autoContinueAtUsageLimit":false}"#,
                "--",
                "hi"
            ])
        );
        // No other harness gets the setting.
        assert_eq!(
            command(Harness::Pi, "hi", None, &cfg, true),
            v(&["pi", "--", "hi"])
        );
        assert_eq!(
            command(Harness::Fake, "hi", None, &cfg, true),
            v(&["borant-fake-harness"])
        );
    }

    #[test]
    fn config_deserializes_with_missing_fields() {
        let cfg: HarnessCommands = serde_json::from_str(r#"{"pi":["pi","-v"]}"#).unwrap();
        assert_eq!(cfg.pi, Some(v(&["pi", "-v"])));
        assert_eq!(cfg.claude, None);
    }
}
