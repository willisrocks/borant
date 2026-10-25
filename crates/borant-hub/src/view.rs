//! The live view of the swarm, for `borant top` and `borant status --json`
//! (phase 2 spec, "Records and protocol" and "Views").

use std::collections::HashMap;

use borant_core::model::{
    Agent, AgentStatus, HostInfo, HostStatus, InboxStatus, Lease, LeaseState, SwarmView,
};
use borant_core::proto::ApiError;
use borant_core::{AgentId, keys};

use crate::Hub;

impl Hub {
    /// The hosts with their latest stats, every agent with its lease and host,
    /// and the counts of its inbox. The memory lock is held only to copy the
    /// seen times and the stats, so the key reads and the inbox calls do not
    /// block a request.
    pub(crate) async fn view(&self) -> Result<SwarmView, ApiError> {
        let now = self.now();
        let (seen, stats) = {
            let mem = self.mem.lock().await;
            (mem.hosts_seen.clone(), mem.host_stats.clone())
        };
        let hosts = self.store.list::<HostInfo>(keys::HOST_PREFIX).await?;
        let leases = self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await?;
        let agents = self.store.list::<Agent>(keys::AGENT_PREFIX).await?;
        let lease_of: HashMap<&AgentId, &Lease> = leases
            .iter()
            .map(|doc| (&doc.value.agent, &doc.value))
            .collect();

        let mut view = SwarmView {
            at: Some(now),
            ..SwarmView::default()
        };
        for doc in &hosts {
            let info = doc.value.clone();
            let last_seen = seen.get(&info.name).copied();
            view.hosts.push(HostStatus {
                online: last_seen.is_some_and(|at| now.since(at) <= self.cfg.lease.lease),
                last_seen,
                stats: stats.get(&info.name).cloned(),
                info,
            });
        }
        for doc in &agents {
            let agent = &doc.value;
            let lease = lease_of.get(&agent.id).copied();
            let host = lease
                .and_then(|lease| match &lease.state {
                    LeaseState::Held { host, .. } | LeaseState::Reserved { host, .. } => {
                        Some(host.clone())
                    }
                    _ => None,
                })
                .or_else(|| agent.host.clone());
            let busy = stats
                .values()
                .flat_map(|host| host.sessions.iter())
                .any(|session| session.agent == agent.id && session.busy);
            view.agents.push(AgentStatus {
                agent: agent.id.clone(),
                harness: Some(agent.preferred_harness),
                host,
                lease: lease.map(|lease| lease.state.clone()),
                session: agent.session,
                busy,
                // The claim needs a scan of every channel's tasks; the top
                // task adds it with its own query.
                claim: None,
            });
            // A bus that cannot report the counts leaves the agent out.
            if let Ok(stats) = self.bus.inbox_stats(&agent.id).await {
                view.inboxes.push(InboxStatus {
                    agent: agent.id.clone(),
                    stats,
                });
            }
        }
        Ok(view)
    }
}
