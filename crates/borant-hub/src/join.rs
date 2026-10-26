//! The join token of a pool host that the hub creates.
//!
//! A create makes one random token and puts it in the boot values as
//! `BORANT_JOIN`. The hub stores only the token's SHA-256, with the host name
//! and an expiry of the pool's `boot_timeout`, at `join.<host>`. The host
//! trades the token at `POST /v1/join` for the vars of bundle `host-join`,
//! once: the request deletes the key before it resolves them, and the delete
//! is conditional, so of two requests that read the same key exactly one
//! wins.
//!
//! No field of [`JoinToken`] holds a token that works, and neither a log line
//! nor an error names one.

use std::collections::BTreeMap;

use borant_core::proto::ApiErrorCode;
use borant_core::{HostName, Timestamp, keys};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Hub;
use crate::secrets::host_join;

/// The stored half of one join token, at `join.<host>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinToken {
    /// The host that may spend the token. The key repeats it, so a record
    /// read on its own still names its host.
    pub host: HostName,
    /// Lowercase hex of the SHA-256 of the token.
    pub digest: String,
    /// The pool's `boot_timeout` after the create.
    pub expires: Timestamp,
}

/// 256 random bits as 64 lowercase hex characters. `None` when the system has
/// no entropy: the create then goes without a join.
pub fn token() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).ok()?;
    Some(hex(&bytes))
}

/// The SHA-256 of a token, lowercase hex.
pub fn digest(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(out, "{b:02x}").expect("a write to a String");
    }
    out
}

/// Compares two digests without an early exit on the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Why a join was refused. The reply is the same for every variant, so a
/// caller learns nothing; the reason goes to the log, without the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// No `[secrets]`, or no ready `host-boot` and `host-join` bundle.
    Off,
    /// No stored token for this host.
    Unknown,
    /// The pool's `boot_timeout` passed.
    Expired,
    /// The token is not the one that the create made.
    Wrong,
    /// Another request spent the token first.
    Spent,
    /// The store failed.
    Failed,
}

impl Refusal {
    fn reason(self) -> &'static str {
        match self {
            Self::Off => "join is off",
            Self::Unknown => "no token for the host",
            Self::Expired => "the token expired",
            Self::Wrong => "the token does not match",
            Self::Spent => "the token was already spent",
            Self::Failed => "the store failed",
        }
    }
}

impl Hub {
    /// The vars of bundle `host-join` for one host and token, once. Every
    /// refusal is the same to the caller.
    pub(crate) async fn redeem_join(
        &self,
        host: &HostName,
        token: &str,
    ) -> Result<BTreeMap<String, String>, Refusal> {
        let Some(secrets) = &self.cfg.secrets else {
            return Err(refuse(host, Refusal::Off));
        };
        if !secrets.joins() {
            return Err(refuse(host, Refusal::Off));
        }
        let key = keys::join(host);
        let stored = match self.store.get::<JoinToken>(&key).await {
            Ok(stored) => stored,
            Err(e) => {
                tracing::warn!(%host, "the join key did not read: {e}");
                return Err(refuse(host, Refusal::Failed));
            }
        };
        let Some(doc) = stored else {
            return Err(refuse(host, Refusal::Unknown));
        };
        if doc.value.host != *host {
            return Err(refuse(host, Refusal::Wrong));
        }
        if doc.value.expires <= self.now() {
            // A create that left no host behind leaves the key; the reap
            // would take it later, and this request takes it now. Both use
            // the revision, so a fresh token for the name survives.
            let _ = self.store.delete_at(&key, doc.revision).await;
            return Err(refuse(host, Refusal::Expired));
        }
        if !same(&doc.value.digest, &digest(token)) {
            return Err(refuse(host, Refusal::Wrong));
        }
        // Single use, also under a race: two requests that read the same
        // document try to delete the same revision, and one gets `Conflict`.
        match self.store.delete_at(&key, doc.revision).await {
            Ok(()) => {}
            Err(e) if e.code == ApiErrorCode::Conflict => {
                return Err(refuse(host, Refusal::Spent));
            }
            Err(e) => {
                tracing::warn!(%host, "the join key did not delete: {e}");
                return Err(refuse(host, Refusal::Failed));
            }
        }
        // The reply is the only place a value leaves the hub: no log line and
        // no error carries one.
        match secrets.vars(&host_join()).await {
            Ok(vars) => Ok(vars),
            Err(e) => {
                tracing::warn!(%host, "the join resolved no host-join vars: {e}");
                Err(refuse(host, Refusal::Failed))
            }
        }
    }
}

fn refuse(host: &HostName, reason: Refusal) -> Refusal {
    if reason == Refusal::Failed {
        tracing::warn!(%host, reason = reason.reason(), "join failed");
    } else {
        tracing::info!(%host, reason = reason.reason(), "join refused");
    }
    reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_256_bits_of_hex_and_its_digest_is_64() {
        let one = token().expect("entropy");
        assert_eq!(one.len(), 64);
        assert!(one.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(one, token().unwrap(), "two tokens differ");
        let digest = digest(&one);
        assert_eq!(digest.len(), 64);
        assert!(!digest.contains(one.as_str()), "the digest is not the token");
        assert!(same(&digest, &digest));
        assert!(!same(&digest, "0"));
    }

    /// The digest of a known token, so a change of the encoding shows.
    #[test]
    fn the_digest_is_sha256_of_the_token_bytes() {
        assert_eq!(
            digest("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
