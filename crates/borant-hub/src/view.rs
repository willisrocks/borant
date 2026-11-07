//! The live view of the swarm, for `borant top` and `borant status --json`
//! (phase 2 spec, "Records and protocol" and "Views").

use std::collections::HashMap;

use borant_core::AgentId;
use borant_core::model::{AgentStatus, HostStatus, InboxStatus, Lease, LeaseState, SwarmView};
use borant_core::proto::ApiError;

use crate::{Hub, Lists};

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
        let lists = self.lists().await?;
        self.view_of(caller, &lists, true).await
    }

    /// The view of the records of one tick. `inboxes` reads the counts of
    /// every agent's inbox in one call to the bus; a caller that reads the
    /// view for its hosts and agents alone, as the alert pass does, leaves
    /// them out.
    pub(crate) async fn view_of(
        &self,
        caller: Option<&AgentId>,
        lists: &Lists,
        inboxes: bool,
    ) -> Result<SwarmView, ApiError> {
        let now = self.now();
        let (seen, stats, activity, hung) = {
            let mem = self.mem.lock().await;
            (
                mem.hosts_seen.clone(),
                mem.host_stats.clone(),
                mem.activity.clone(),
                self.hung_sessions(&mem, now),
            )
        };
        let lease_of: HashMap<&AgentId, &Lease> = lists
            .leases
            .iter()
            .map(|doc| (&doc.value.agent, &doc.value))
            .collect();
        // The channels of an agent caller, when its record still exists. An
        // agent that is not in the store reads no other agent.
        let caller_channels = caller.map(|c| {
            lists
                .agents
                .iter()
                .find(|doc| &doc.value.id == c)
                .map(|doc| doc.value.channels.clone())
                .unwrap_or_default()
        });

        // The collection of an agent caller: a chief reads the agents of its
        // own collection, as it reads their board, though it shares no channel
        // with each of them.
        let caller_collection = caller.and_then(|c| {
            lists
                .agents
                .iter()
                .find(|doc| &doc.value.id == c)
                .and_then(|doc| doc.value.collection.clone())
        });

        let mut view = SwarmView {
            at: Some(now),
            ..SwarmView::default()
        };
        for doc in &lists.hosts {
            let info = doc.value.clone();
            let last_seen = seen.get(&info.name).copied();
            view.hosts.push(HostStatus {
                online: last_seen.is_some_and(|at| now.since(at) <= self.cfg.lease.lease),
                last_seen,
                stats: stats.get(&info.name).cloned(),
                info,
            });
        }
        for doc in &lists.agents {
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
                // The hung flag of the live session only: a report of an
                // ended session says nothing about the one that runs now.
                hung: agent
                    .session
                    .and_then(|session| hung.get(&(agent.id.clone(), session)).copied()),
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
        }
        if inboxes {
            // One call for the whole swarm: a bus that can ask the server for
            // every inbox at once does, and one that cannot asks in turn.
            let ids: Vec<AgentId> = lists.agents.iter().map(|d| d.value.id.clone()).collect();
            match self.bus.inbox_stats_many(&ids).await {
                Ok(all) => {
                    view.inboxes = all
                        .into_iter()
                        .map(|(agent, stats)| InboxStatus { agent, stats })
                        .collect();
                }
                // A bus that cannot report the counts leaves the agents out.
                Err(e) => tracing::debug!("the inbox counts are unavailable: {e}"),
            }
        }
        Ok(view)
    }
}
