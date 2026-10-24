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
    let last_session = match &lease.state {
        LeaseState::Held { session, .. } => Some(*session),
        _ => None,
    };
    ended.then(|| Lease {
        agent: lease.agent.clone(),
        token: FencingToken(lease.token.0 + 1),
        state: LeaseState::Expired {
            since: now,
            last_session,
        },
    })
}

/// Gives the lease back to the session that held it when the tick expired
/// it, for example after a sleep or a network drop with no takeover. The
/// token stays at the one the expiry set, one above the session's token. A
/// lease that this session already took back (the host lost the reply) gives
/// the same result again.
pub fn reclaim(
    lease: &Lease,
    session: SessionId,
    token: FencingToken,
    host: &HostName,
    now: Timestamp,
    cfg: &LeaseConfig,
) -> Option<Lease> {
    let next = lease.token.0 == token.0 + 1;
    let ours = match &lease.state {
        LeaseState::Expired {
            last_session: Some(s),
            ..
        } => *s == session,
        LeaseState::Held { session: s, .. } => *s == session,
        _ => false,
    };
    (next && ours).then(|| Lease {
        agent: lease.agent.clone(),
        token: lease.token,
        state: LeaseState::Held {
            session,
            host: host.clone(),
            expires: now.plus(cfg.lease),
        },
    })
}

/// After a hub start, held leases get a full lease time from `now`, and
/// reservations a full reservation time, so a hub restart never causes a mass
/// takeover.
pub fn restart_grace(lease: &Lease, now: Timestamp, cfg: &LeaseConfig) -> Lease {
    let mut out = lease.clone();
    match &mut out.state {
        LeaseState::Held { expires, .. } => *expires = now.plus(cfg.lease),
        LeaseState::Reserved { until, .. } => *until = now.plus(cfg.reserve),
        LeaseState::Expired { .. } | LeaseState::Released { .. } => {}
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
    fn reclaim_after_expiry_keeps_the_raised_token() {
        let s = SessionId::new();
        let h = host("h");
        let l = acquire(&agent(), None, s, &h, Timestamp(0), &cfg()).unwrap();
        let expired = tick(&l, Timestamp(30_001), &cfg()).unwrap();
        assert_eq!(
            expired.state,
            LeaseState::Expired {
                since: Timestamp(30_001),
                last_session: Some(s),
            }
        );
        let back = reclaim(&expired, s, l.token, &h, Timestamp(40_000), &cfg()).unwrap();
        assert_eq!(back.token, FencingToken(2));
        assert_eq!(
            back.state,
            LeaseState::Held {
                session: s,
                host: h.clone(),
                expires: Timestamp(70_000),
            }
        );
        // A retry with the old token, after a lost reply, gets the same lease.
        let again = reclaim(&back, s, l.token, &h, Timestamp(41_000), &cfg()).unwrap();
        assert_eq!(again.token, FencingToken(2));
        // The new token renews normally and does not reclaim.
        assert!(renew(&back, s, FencingToken(2), Timestamp(41_000), &cfg()).is_ok());
        assert_eq!(
            reclaim(&back, s, FencingToken(2), &h, Timestamp(41_000), &cfg()),
            None
        );
    }

    #[test]
    fn reclaim_rejects_other_cases() {
        let s = SessionId::new();
        let h = host("h");
        let l = acquire(&agent(), None, s, &h, Timestamp(0), &cfg()).unwrap();
        let expired = tick(&l, Timestamp(30_001), &cfg()).unwrap();
        let t = l.token;
        let now = Timestamp(40_000);
        // Another session.
        assert_eq!(
            reclaim(&expired, SessionId::new(), t, &h, now, &cfg()),
            None
        );
        // No last session, for example a lapsed reservation.
        let anon = Lease {
            state: LeaseState::Expired {
                since: Timestamp(1),
                last_session: None,
            },
            ..expired.clone()
        };
        assert_eq!(reclaim(&anon, s, t, &h, now, &cfg()), None);
        // A token gap of two.
        let gap = Lease {
            token: FencingToken(t.0 + 2),
            ..expired.clone()
        };
        assert_eq!(reclaim(&gap, s, t, &h, now, &cfg()), None);
        // Reserved and released leases.
        let r = reserve(&expired, &host("b"), MoveReason::Takeover, now, &cfg());
        assert_eq!(reclaim(&r, s, t, &h, now, &cfg()), None);
        let rel = Lease {
            token: expired.token,
            ..release(&l, now)
        };
        assert_eq!(reclaim(&rel, s, t, &h, now, &cfg()), None);
        // A held lease of another session.
        let other = acquire(&agent(), Some(&expired), SessionId::new(), &h, now, &cfg()).unwrap();
        let other = Lease {
            token: expired.token,
            ..other
        };
        assert_eq!(reclaim(&other, s, t, &h, now, &cfg()), None);
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
        assert!(matches!(
            ended.unwrap().state,
            LeaseState::Expired {
                last_session: None,
                ..
            }
        ));
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
