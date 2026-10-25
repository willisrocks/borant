//! Member rules (spec "Member rules"). The pick itself lands with the hub
//! assignment; this file holds the configuration type only.

use serde::{Deserialize, Serialize};

/// The rules for bundle members, from `[secrets.members]` in `hub.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberRules {
    /// The reset time to assume when a limit report gives none.
    pub unknown_reset_mins: u64,
    /// How often one agent may restart on a new member in an hour, then it
    /// waits for the member to reset.
    pub max_restarts_per_hour: u32,
}

impl Default for MemberRules {
    fn default() -> Self {
        Self {
            unknown_reset_mins: 60,
            max_restarts_per_hour: 3,
        }
    }
}
