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
        self.view_for(None).await
    }

    /// The view as `caller` may read it. The activity text of an agent can
    /// hold project content, so it goes to a person and to an agent that
    /// shares one of its channels, and no other caller sees it.
    pub(crate) async fn view_for(&self, caller: Option<&AgentId>) -> Result<SwarmView, ApiError> {
        let now = self.now();
        let (seen, stats, activity) = {
            let mem = self.mem.lock().await;
            (
                mem.hosts_seen.clone(),
                mem.host_stats.clone(),
                mem.activity.clone(),
            )
        };
        let hosts = self.store.list::<HostInfo>(keys::HOST_PREFIX).await?;
        let leases = self.store.list::<Lease>(keys::LEASE_AGENT_PREFIX).await?;
        let agents = self.store.list::<Agent>(keys::AGENT_PREFIX).await?;
        let lease_of: HashMap<&AgentId, &Lease> = leases
            .iter()
            .map(|doc| (&doc.value.agent, &doc.value))
            .collect();
        // The channels of an agent caller, when its record still exists. An
        // agent that is not in the store reads no other agent.
        let caller_channels = caller.map(|c| {
            agents
                .iter()
                .find(|doc| &doc.value.id == c)
                .map(|doc| doc.value.channels.clone())
                .unwrap_or_default()
        });

        // The collection of an agent caller: a chief reads the agents of its
        // own collection, as it reads their board, though it shares no channel
        // with each of them.
        let caller_collection = caller.and_then(|c| {
            agents
                .iter()
                .find(|doc| &doc.value.id == c)
                .and_then(|doc| doc.value.collection.clone())
        });

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
            // The last report of the live session: a report of an ended
            // session tells nothing about the agent that runs now.
            let readable = caller.is_none_or(|c| {
                c == &agent.id
                    || caller_channels
                        .as_ref()
                        .is_some_and(|mine| agent.channels.iter().any(|ch| mine.contains(ch)))
                    || (caller_collection.is_some() && caller_collection == agent.collection)
            });
            let report = activity
                .get(&agent.id)
                .filter(|r| agent.session == Some(r.session))
                .filter(|_| readable);
            let (last_tool_at, last_tool, last_text_at, last_text, tool_running_since) =
                match report {
                    Some(r) => (
                        r.activity.last_tool_at,
                        r.activity.last_tool.clone(),
                        r.activity.last_text_at,
                        r.activity.last_text.clone(),
                        r.activity.tool_running_since,
                    ),
                    None => (None, None, None, None, None),
                };
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
                role: agent.role.clone(),
                collection: agent.collection.clone(),
                pinned: agent.pinned,
                retired: agent.retired,
                model: agent.model.clone(),
                last_tool_at,
                last_tool,
                last_text_at,
                last_text,
                tool_running_since,
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
