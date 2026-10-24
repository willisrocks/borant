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
        (
            r"(?i)\b(api[_-]?key|token|secret|password)(\s*[=:]\s*)\S+",
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
