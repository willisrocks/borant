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
pub fn command(harness: Harness, prompt: &str, cfg: &HarnessCommands) -> Vec<String> {
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
            command(Harness::ClaudeCode, "hi", &cfg),
            v(&["claude", "--dangerously-skip-permissions", "--", "hi"])
        );
        assert_eq!(command(Harness::Pi, "hi", &cfg), v(&["pi", "--", "hi"]));
        assert_eq!(command(Harness::Omp, "hi", &cfg), v(&["omp", "--", "hi"]));
        assert_eq!(
            command(Harness::Codex, "hi", &cfg),
            v(&[
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "--",
                "hi"
            ])
        );
        assert_eq!(
            command(Harness::Fake, "hi", &cfg),
            v(&["borant-fake-harness"])
        );

        let cfg = HarnessCommands {
            claude: Some(v(&["/opt/claude", "--model", "x"])),
            pi: Some(v(&["pi-dev"])),
            fake: Some(v(&["/bin/fake", "--fast"])),
            ..HarnessCommands::default()
        };
        assert_eq!(
            command(Harness::ClaudeCode, "hi", &cfg),
            v(&["/opt/claude", "--model", "x", "--", "hi"])
        );
        assert_eq!(command(Harness::Pi, "hi", &cfg), v(&["pi-dev", "--", "hi"]));
        assert_eq!(command(Harness::Omp, "hi", &cfg), v(&["omp", "--", "hi"]));
        assert_eq!(
            command(Harness::Fake, "hi", &cfg),
            v(&["/bin/fake", "--fast"])
        );
    }

    #[test]
    fn config_deserializes_with_missing_fields() {
        let cfg: HarnessCommands = serde_json::from_str(r#"{"pi":["pi","-v"]}"#).unwrap();
        assert_eq!(cfg.pi, Some(v(&["pi", "-v"])));
        assert_eq!(cfg.claude, None);
    }
}
