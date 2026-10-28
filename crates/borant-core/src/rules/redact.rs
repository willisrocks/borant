//! Removes known secret patterns from text before it leaves the host.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            "[redacted private key]",
        ),
        (r"\bsk-[A-Za-z0-9_-]{16,}", "[redacted]"),
        (r"\bgh[pousr]_[A-Za-z0-9]{20,}", "[redacted]"),
        (r"\bgithub_pat_[A-Za-z0-9_]{20,}", "[redacted]"),
        (r"\bxox[abpr]-[A-Za-z0-9-]{10,}", "[redacted]"),
        (r"\bAKIA[0-9A-Z]{16}\b", "[redacted]"),
        // A secret-looking name, alone or inside a longer variable name such
        // as BORANT_NATS_PASSWORD or AWS_SECRET_ACCESS_KEY, then `=` or `:`
        // (a JSON key may close its quote first). A longer name only grows
        // with `_`, so prose such as "input tokens: 500" stays.
        (
            r#"(?i)\b([a-z0-9_]*(?:api[_-]?key|access[_-]?key|private[_-]?key|token|secret|passw(?:or)?d|credential)(?:_[a-z0-9_]*)?|[a-z0-9_]*_key)(["']?\s*[=:]\s*)\S+"#,
            "$1$2[redacted]",
        ),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid redaction pattern"), r))
    .collect()
});

pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    for (re, replacement) in PATTERNS.iter() {
        if re.is_match(&out) {
            out = Cow::Owned(re.replace_all(&out, *replacement).into_owned());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_known_tokens() {
        let got =
            redact("token=abc123 sk-ABCDEFGHIJKLMNOPQRSTUV and ghp_abcdefghijklmnopqrstuvwxyz0123");
        assert!(!got.contains("abc123"), "{got}");
        assert!(!got.contains("sk-ABC"), "{got}");
        assert!(!got.contains("ghp_abc"), "{got}");
        assert!(got.contains("token=[redacted]"), "{got}");
    }

    /// Secret names inside longer variable names, as `env` prints them. The
    /// old pattern needed a word boundary before the name, which `_` is not.
    #[test]
    fn removes_values_of_prefixed_secret_names() {
        for (text, name) in [
            ("BORANT_NATS_PASSWORD=0123abcd", "BORANT_NATS_PASSWORD"),
            ("GH_TOKEN=gho_value_here", "GH_TOKEN"),
            ("AWS_SECRET_ACCESS_KEY=AbC/dEf", "AWS_SECRET_ACCESS_KEY"),
            ("LUNAROUTE_API_KEY=lr-value", "LUNAROUTE_API_KEY"),
            (
                "OP_SERVICE_ACCOUNT_TOKEN=ops_value",
                "OP_SERVICE_ACCOUNT_TOKEN",
            ),
            ("HCLOUD_TOKEN_FILE=/run/x", "HCLOUD_TOKEN_FILE"),
            ("STRIPE_KEY: rk_value", "STRIPE_KEY"),
            ("db_passwd=hunter2", "db_passwd"),
            (r#""password": "hunter2""#, "password"),
            ("NPM_CREDENTIAL=abc", "NPM_CREDENTIAL"),
        ] {
            let got = redact(text);
            assert!(got.contains("[redacted]"), "{text} -> {got}");
            assert!(got.contains(name), "the name stays: {got}");
            for secret in [
                "0123abcd",
                "gho_value",
                "AbC/dEf",
                "lr-value",
                "ops_value",
                "rk_value",
                "hunter2",
                "/run/x",
            ] {
                assert!(!got.contains(secret), "{text} -> {got}");
            }
        }
    }

    #[test]
    fn leaves_prose_with_secret_words() {
        for text in ["input tokens: 500", "the key: press enter", "monkey: 3"] {
            assert_eq!(redact(text), text);
        }
    }

    #[test]
    fn removes_private_keys() {
        let got =
            redact("x\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\ny");
        assert_eq!(got, "x\n[redacted private key]\ny");
    }

    #[test]
    fn leaves_plain_text_borrowed() {
        assert!(matches!(redact("hello world"), Cow::Borrowed(_)));
    }
}
