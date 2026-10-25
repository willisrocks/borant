//! The program that starts each harness.

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
    let (custom, default): (&Option<Vec<String>>, &[&str]) = match harness {
        Harness::ClaudeCode => (&cfg.claude, &["claude", "--dangerously-skip-permissions"]),
        Harness::Pi => (&cfg.pi, &["pi"]),
        Harness::Omp => (&cfg.omp, &["omp"]),
        Harness::Codex => (
            &cfg.codex,
            &["codex", "--dangerously-bypass-approvals-and-sandbox"],
        ),
        Harness::Fake => (&cfg.fake, &["borant-fake-harness"]),
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
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
