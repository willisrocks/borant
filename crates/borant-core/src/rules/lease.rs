//! Agent leases and fencing (spec section 11). The hub applies these rules
//! on its own clock.

use std::time::Duration;

use crate::error::CoreError;
use crate::ids::{AgentId, FencingToken, HostName, SessionId};
use crate::model::{Lease, LeaseState, MoveReason};
use crate::time::Timestamp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseConfig {
    /// How long a lease lasts without a heartbeat.
    pub lease: Duration,
    /// How long the hub waits after a lease ends before a takeover.
    pub grace: Duration,
    /// How long a reservation for a launch on a host lasts.
    pub reserve: Duration,
    /// How long claims survive an agent that no host takes over.
    pub claim_timeout: Duration,
    /// How often host daemons send heartbeats.
    pub heartbeat: Duration,
}

impl Default for LeaseConfig {
    fn default() -> Self {
        Self {
            lease: Duration::from_secs(30),
            grace: Duration::from_secs(120),
            reserve: Duration::from_secs(300),
            claim_timeout: Duration::from_secs(1800),
            heartbeat: Duration::from_secs(10),
        }
    }
}

/// Renews a held lease for the session that holds it.
pub fn renew(
    lease: &Lease,
    session: SessionId,
    token: FencingToken,
    now: Timestamp,
    cfg: &LeaseConfig,
) -> Result<Lease, CoreError> {
    match &lease.state {
        LeaseState::Held {
            session: holder,
            host,
            ..
        } if *holder == session && lease.token == token => Ok(Lease {
            agent: lease.agent.clone(),
            token,
            state: LeaseState::Held {
                session,
                host: host.clone(),
                expires: now.plus(cfg.lease),
            },
        }),
        _ => Err(CoreError::StaleToken),
    }
}

/// Gives the lease to a new session. Each success has a higher token.
pub fn acquire(
    agent: &AgentId,
    lease: Option<&Lease>,
    session: SessionId,
    host: &HostName,
    now: Timestamp,
    cfg: &LeaseConfig,
) -> Result<Lease, CoreError> {
    let token = match lease {
        None => FencingToken(1),
        Some(l) => match &l.state {
            LeaseState::Held { host: h, .. } => {
                return Err(CoreError::LeaseHeld { host: h.clone() });
            }
            LeaseState::Reserved { host: h, .. } if h != host => {
                return Err(CoreError::LeaseHeld { host: h.clone() });
            }
            _ => FencingToken(l.token.0 + 1),
        },
    };
    Ok(Lease {
        agent: agent.clone(),
        token,
        state: LeaseState::Held {
            session,
            host: host.clone(),
            expires: now.plus(cfg.lease),
        },
    })
}

/// Moves time forward for one lease. A held lease past its expiry and a
/// reservation past its end become expired. Expiry raises the token, so the
/// old session cannot act again.
pub fn tick(lease: &Lease, now: Timestamp, _cfg: &LeaseConfig) -> Option<Lease> {
    let ended = match &lease.state {
        LeaseState::Held { expires, .. } => now > *expires,
        LeaseState::Reserved { until, .. } => now > *until,
        LeaseState::Expired { .. } | LeaseState::Released { .. } => false,
    };
    ended.then(|| Lease {
        agent: lease.agent.clone(),
        token: FencingToken(lease.token.0 + 1),
        state: LeaseState::Expired { since: now },
    })
}

/// After a hub start, held leases get a full lease time from `now`, so a hub
/// restart never causes a mass takeover.
pub fn restart_grace(lease: &Lease, now: Timestamp, cfg: &LeaseConfig) -> Lease {
    let mut out = lease.clone();
    if let LeaseState::Held { expires, .. } = &mut out.state {
        *expires = now.plus(cfg.lease);
    }
    out
}

/// Ends a lease on purpose, for example at session end or a planned move.
pub fn release(lease: &Lease, now: Timestamp) -> Lease {
    Lease {
        agent: lease.agent.clone(),
        token: FencingToken(lease.token.0 + 1),
        state: LeaseState::Released { since: now },
    }
}

/// Holds the lease for a launch on one host.
pub fn reserve(
    lease: &Lease,
    host: &HostName,
    reason: MoveReason,
    now: Timestamp,
    cfg: &LeaseConfig,
) -> Lease {
    Lease {
        agent: lease.agent.clone(),
        token: lease.token,
        state: LeaseState::Reserved {
            host: host.clone(),
            until: now.plus(cfg.reserve),
            reason,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> LeaseConfig {
        LeaseConfig::default()
    }

    fn agent() -> AgentId {
        "a".parse().unwrap()
    }

    fn host(s: &str) -> HostName {
        s.parse().unwrap()
    }

    #[test]
    fn stale_token_cannot_renew() {
        let s = SessionId::new();
        let l = acquire(&agent(), None, s, &host("h"), Timestamp(0), &cfg()).unwrap();
        assert_eq!(l.token, FencingToken(1));
        assert!(renew(&l, s, FencingToken(1), Timestamp(10), &cfg()).is_ok());
        assert_eq!(
            renew(&l, s, FencingToken(0), Timestamp(10), &cfg()),
            Err(CoreError::StaleToken)
        );
        assert_eq!(
            renew(&l, SessionId::new(), FencingToken(1), Timestamp(10), &cfg()),
            Err(CoreError::StaleToken)
        );
    }

    #[test]
    fn expiry_bumps_token() {
        let s = SessionId::new();
        let l = acquire(&agent(), None, s, &host("h"), Timestamp(0), &cfg()).unwrap();
        assert_eq!(tick(&l, Timestamp(30_000), &cfg()), None);
        let expired = tick(&l, Timestamp(30_001), &cfg()).unwrap();
        assert_eq!(expired.token, FencingToken(2));
        assert_eq!(
            renew(&expired, s, FencingToken(1), Timestamp(30_002), &cfg()),
            Err(CoreError::StaleToken)
        );
        let again = acquire(
            &agent(),
            Some(&expired),
            SessionId::new(),
            &host("h2"),
            Timestamp(1),
            &cfg(),
        )
        .unwrap();
        assert_eq!(again.token, FencingToken(3));
    }

    #[test]
    fn held_lease_cannot_be_acquired() {
        let l = acquire(
            &agent(),
            None,
            SessionId::new(),
            &host("h"),
            Timestamp(0),
            &cfg(),
        )
        .unwrap();
        assert_eq!(
            acquire(
                &agent(),
                Some(&l),
                SessionId::new(),
                &host("h2"),
                Timestamp(1),
                &cfg()
            ),
            Err(CoreError::LeaseHeld { host: host("h") })
        );
    }

    #[test]
    fn reserved_lease_only_for_target_host() {
        let l = acquire(
            &agent(),
            None,
            SessionId::new(),
            &host("h"),
            Timestamp(0),
            &cfg(),
        )
        .unwrap();
        let r = reserve(&l, &host("b"), MoveReason::Takeover, Timestamp(1), &cfg());
        assert!(
            acquire(
                &agent(),
                Some(&r),
                SessionId::new(),
                &host("c"),
                Timestamp(2),
                &cfg()
            )
            .is_err()
        );
        assert!(
            acquire(
                &agent(),
                Some(&r),
                SessionId::new(),
                &host("b"),
                Timestamp(2),
                &cfg()
            )
            .is_ok()
        );
        let ended = tick(
            &r,
            Timestamp(1)
                .plus(cfg().reserve)
                .plus(Duration::from_millis(1)),
            &cfg(),
        );
        assert!(matches!(ended.unwrap().state, LeaseState::Expired { .. }));
    }

    #[test]
    fn restart_grace_extends_held() {
        let l = acquire(
            &agent(),
            None,
            SessionId::new(),
            &host("h"),
            Timestamp(0),
            &cfg(),
        )
        .unwrap();
        let g = restart_grace(&l, Timestamp(100_000), &cfg());
        assert_eq!(tick(&g, Timestamp(120_000), &cfg()), None);
        assert_eq!(g.token, l.token);
    }
}
