//! The hung-session rule of the hub, from the last heartbeat of each host and
//! the activity that it carried. The live view, `borant stop`, and the
//! blocked-work alarm read the same answer, so they never disagree about a
//! session.

use std::collections::HashMap;

use borant_core::rules::hung::{self, Hung};
use borant_core::{AgentId, HostName, SessionId, Timestamp};

use crate::{Hub, Memory};

impl Hub {
    /// The hung flag of every session that a host reported, keyed by agent and
    /// session: the live view keeps the one of the live session of the agent.
    pub(crate) fn hung_sessions(
        &self,
        mem: &Memory,
        now: Timestamp,
    ) -> HashMap<(AgentId, SessionId), Hung> {
        mem.idle
            .iter()
            .filter_map(|(agent, report)| {
                let hung = hung_now(
                    mem,
                    agent,
                    report.session,
                    &report.host,
                    self.cfg.alerts.hung_minutes,
                    now,
                )?;
                Some(((agent.clone(), report.session), hung))
            })
            .collect()
    }

    /// Why the live session of `agent` looks hung at `now`, or `None` while it
    /// is fine. `live` is the session that the hub holds for the agent: a
    /// report of an ended session says nothing about the one that runs now.
    pub(crate) fn hung_of(
        &self,
        mem: &Memory,
        agent: &AgentId,
        live: Option<SessionId>,
        now: Timestamp,
    ) -> Option<Hung> {
        let report = mem.idle.get(agent)?;
        if live.is_some_and(|session| session != report.session) {
            return None;
        }
        hung_now(
            mem,
            agent,
            report.session,
            &report.host,
            self.cfg.alerts.hung_minutes,
            now,
        )
    }
}

/// The hung rule for one session, from its host's last heartbeat and the
/// activity that the host reported for it.
fn hung_now(
    mem: &Memory,
    agent: &AgentId,
    session: SessionId,
    host: &HostName,
    minutes: u64,
    now: Timestamp,
) -> Option<Hung> {
    let stats = mem.host_stats.get(host)?;
    let reported = stats.sessions.iter().find(|s| &s.agent == agent)?;
    let activity = mem
        .activity
        .get(agent)
        .filter(|report| report.session == session)
        .map(|report| &report.activity);
    hung::hung_at(
        reported.busy,
        activity.is_some_and(|a| a.tool_running_since.is_some()),
        hung::last_alive(reported.last_event_at, activity),
        mem.hosts_slept.get(host).copied(),
        minutes,
        now,
    )
}
