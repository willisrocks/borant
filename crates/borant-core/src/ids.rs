//! Identifiers. Names appear in NATS subjects and KV keys, so they follow one
//! strict rule: 1-63 characters from `a-z`, `0-9`, `-`, `_`, starting with a
//! letter or digit.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;

fn valid_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_')
}

macro_rules! name_type {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = CoreError;
            fn try_from(value: String) -> Result<Self, Self::Error> {
                if valid_name(&value) {
                    Ok(Self(value))
                } else {
                    Err(CoreError::InvalidName { kind: $kind, value })
                }
            }
        }

        impl FromStr for $name {
            type Err = CoreError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::try_from(s.to_owned())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

name_type!(
    /// A durable agent identity, unique in the swarm.
    AgentId,
    "agent ID"
);
name_type!(
    /// A channel name, unique in the swarm.
    ChannelName,
    "channel name"
);
name_type!(
    /// A project name, unique in the swarm.
    ProjectName,
    "project name"
);
name_type!(
    /// A host name, unique in the swarm.
    HostName,
    "host name"
);
name_type!(
    /// A bundle of secrets, for example `github`. Safe to log and to send to
    /// the hub.
    BundleName,
    "bundle name"
);
name_type!(
    /// One account behind a bundle, for example `a`. Safe to log.
    MemberName,
    "member name"
);
name_type!(
    /// A pool of elastic hosts, one `[[pools]]` entry of `hub.toml`.
    PoolName,
    "pool name"
);
name_type!(
    /// A host tag, for the `avoid` rules of later phases.
    Tag,
    "tag"
);
name_type!(
    /// A role from `[roles]` in `hub.toml`, for example for a worker.
    RoleName,
    "role name"
);
name_type!(
    /// A collection of agents and channels, for example one person's work.
    CollectionName,
    "collection name"
);

macro_rules! ulid_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub ulid::Ulid);

        impl $name {
            pub fn new() -> Self {
                Self(ulid::Ulid::generate())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl FromStr for $name {
            type Err = ulid::DecodeError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                ulid::Ulid::from_string(s).map(Self)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

ulid_type!(
    /// One run of an agent in one harness on one host.
    SessionId
);
ulid_type!(
    /// A task in a channel.
    TaskId
);
ulid_type!(
    /// A message or an event on the bus.
    MessageId
);
ulid_type!(
    /// One event in a session log.
    EventId
);

/// A number that rises with each new lease. The hub rejects older tokens.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct FencingToken(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_names() {
        let long = "a".repeat(64);
        for bad in ["a.b", "a b", "*", ">", "Abc", "", "-a", long.as_str()] {
            let err = bad.parse::<AgentId>().unwrap_err();
            assert!(
                matches!(
                    err,
                    CoreError::InvalidName {
                        kind: "agent ID",
                        ..
                    }
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn accepts_valid_names() {
        for good in ["api-owner", "pi_worker-3", "0", &"a".repeat(63)] {
            assert_eq!(good.parse::<ChannelName>().unwrap().as_str(), good);
        }
    }

    #[test]
    fn names_round_trip_through_serde() {
        let id: AgentId = "api-owner".parse().unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"api-owner\"");
        assert!(serde_json::from_str::<AgentId>("\"Bad.Name\"").is_err());
    }
}
