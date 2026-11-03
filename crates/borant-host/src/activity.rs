//! The last activity of a session, which its host sends with the lease
//! renewal: the last tool call, the tool that runs now, and the last text.
//!
//! The text that a tool call and an assistant message hold is project
//! content, so the host redacts it before it stores it and sends it. Nothing
//! here leaves the host: the report travels only in `SessionLeaseRef`.

use std::time::Duration;

use borant_core::Timestamp;
use borant_core::model::Activity;

/// How often the lease renewal carries the activity of a session. The
/// heartbeat runs more often than this; the hub keeps the last report.
pub(crate) const REPORT_EVERY: Duration = Duration::from_secs(30);

/// The characters of the main argument of a tool call.
const TOOL_CHARS: usize = 80;
/// The characters of the last assistant text.
const TEXT_CHARS: usize = 120;

/// The report of a tool call that started now. `input` is the redacted tool
/// input, as the harness sent it.
pub(crate) fn tool_started(activity: &mut Activity, name: &str, input: &str, now: Timestamp) {
    let detail = clip(&argument_of(input), TOOL_CHARS);
    activity.last_tool_at = Some(now);
    activity.tool_running_since = Some(now);
    activity.last_tool = Some(if detail.is_empty() {
        name.to_owned()
    } else {
        format!("{name} {detail}")
    });
}

/// The report of the result of a tool call: nothing runs now.
pub(crate) fn tool_finished(activity: &mut Activity) {
    activity.tool_running_since = None;
}

/// The report of the last assistant text of the session, already redacted.
pub(crate) fn text(activity: &mut Activity, text: &str, now: Timestamp) {
    activity.last_text_at = Some(now);
    activity.last_text = Some(clip(text, TEXT_CHARS));
}

/// The main argument of a tool call: the command of Bash, the path of Read
/// or Edit, and otherwise the first argument that the tool input names. The
/// host gets the input as the JSON text of the harness, and a harness that
/// sends plain text gets it whole.
fn argument_of(input: &str) -> String {
    const KEYS: [&str; 8] = [
        "command",
        "file_path",
        "path",
        "notebook_path",
        "pattern",
        "url",
        "query",
        "description",
    ];
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(input)
    else {
        return input.to_owned();
    };
    for key in KEYS {
        if let Some(serde_json::Value::String(s)) = map.get(key) {
            return s.clone();
        }
    }
    // A tool that names its argument differently: the first string value in
    // key order, and no nested object, which is a schema more often than an
    // argument.
    map.values()
        .find_map(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The first `max` characters of `text` as one line. The report is a line on
/// a screen, so a newline becomes a space, and a long text is cut at a
/// character boundary.
fn clip(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max * 4));
    let mut chars = 0;
    let mut last_space = false;
    for c in text.chars() {
        // The space that a newline, a tab, and a run of spaces become.
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' {
            if last_space || out.is_empty() {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        if chars == max {
            break;
        }
        out.push(c);
        chars += 1;
    }
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_detail_of_a_call_is_its_main_argument() {
        assert_eq!(
            argument_of(r#"{"command":"git status --short","description":"show"}"#),
            "git status --short"
        );
        assert_eq!(
            argument_of(r#"{"file_path":"/work/src/lib.rs","limit":10}"#),
            "/work/src/lib.rs"
        );
        // Plain text, as a harness that does not send JSON.
        assert_eq!(argument_of("some text"), "some text");
        assert_eq!(argument_of("{}"), "");
    }

    #[test]
    fn a_long_text_is_one_line_cut_at_a_character_boundary() {
        assert_eq!(clip("a\nb\tc   d", 40), "a b c d");
        assert_eq!(clip("  leading", 40), "leading");
        assert_eq!(clip("abcdef", 3), "abc");
        // A multi-byte character is never cut in half.
        assert_eq!(clip("éééé", 2), "éé");
    }

    #[test]
    fn the_report_redacts_before_it_clips() {
        // The host redacts, then the report clips: a token is never cut into
        // a shape that the pattern misses.
        let redact = |t: &str| t.replace("sk-secret", "[redacted]");
        let mut a = Activity::default();
        tool_started(
            &mut a,
            "Bash",
            &redact(r#"{"command":"echo sk-secret"}"#),
            Timestamp(5),
        );
        assert_eq!(a.last_tool.as_deref(), Some("Bash echo [redacted]"));
        assert_eq!(a.last_tool_at, Some(Timestamp(5)));
        assert_eq!(a.tool_running_since, Some(Timestamp(5)));
        tool_finished(&mut a);
        assert_eq!(a.tool_running_since, None);

        text(&mut a, &redact("the token sk-secret"), Timestamp(7));
        assert_eq!(a.last_text.as_deref(), Some("the token [redacted]"));
        assert_eq!(a.last_text_at, Some(Timestamp(7)));
    }

    #[test]
    fn a_session_with_no_hook_has_no_report() {
        assert!(Activity::default().is_empty());
        let mut a = Activity::default();
        tool_finished(&mut a);
        assert!(a.is_empty());
        tool_started(&mut a, "Read", "{}", Timestamp(1));
        assert!(!a.is_empty());
    }
}
