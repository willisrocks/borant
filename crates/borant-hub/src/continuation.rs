//! Bounded task continuation (phase 11 spec), the hub's half: a heartbeat that
//! reports a bound session idle asks `rules::continuation::decide`, then writes
//! what it says before it acts, so a hub restart never sends a second continue
//! for one idle period.
//!
//! Only the hub writes control-plane state, so the count lives in the agent
//! record and the message comes from here, as a direct message. The host keeps
//! reporting idle in the heartbeat, as it did before phase 11.

use borant_core::model::{Agent, Envelope, EnvelopeKind, Sender, Target, Task, TaskStatus};
use borant_core::proto::{ApiError, SessionLeaseRef};
use borant_core::rules::continuation::{self, Decision};
use borant_core::{AgentId, ChannelName, MessageId, TaskId, keys, subjects};

use crate::waits::WaitRecord;
use crate::{Hub, Memory};

/// The direct message of one continue: it names the bound task and tells the
/// worker to wait on it instead of stopping.
fn continue_text(channel: &ChannelName, task: &TaskId) -> String {
    format!(
        "Continue with task {channel}/{task}. When it is done, call the wait tool on it; \
         do not stop otherwise."
    )
}

impl Hub {
    /// The continues of every session that this heartbeat reports idle. One
    /// agent's failure is a warn: the heartbeat already renewed the leases, so
    /// a continue must not turn it into an error.
    pub(crate) async fn continue_idle(&self, mem: &Memory, sessions: &[SessionLeaseRef]) {
        for s in sessions {
            if let Err(e) = self.continue_one(mem, s).await {
                tracing::warn!(agent = %s.agent, "the continue failed: {e}");
            }
        }
    }

    /// One idle report: read the agent, its task, and its waits, then decide.
    async fn continue_one(&self, mem: &Memory, s: &SessionLeaseRef) -> Result<(), ApiError> {
        // A session in a turn reports no idle end, so it never continues.
        let Some(idle_since) = s.idle_since else {
            return Ok(());
        };
        let Some(doc) = self.store.get::<Agent>(&keys::agent(&s.agent)).await? else {
            return Ok(());
        };
        let Some(state) = &doc.value.continues else {
            return Ok(());
        };
        let agent = &doc.value;
        // The cheap guards first: most heartbeats repeat the idle period that
        // already got its continue, and a spent worker reads nothing.
        if state.exhausted || state.last_idle == Some(idle_since) {
            return Ok(());
        }
        // A session that the hub already replaced (a move, a takeover) reports
        // an idle that belongs to no live worker.
        if agent
            .session
            .as_ref()
            .is_some_and(|live| live != &s.session)
        {
            return Ok(());
        }
        let bound = agent.task.as_ref().map(|t| (t.channel.clone(), t.id));
        let task = match &bound {
            Some((channel, id)) => self.store.get::<Task>(&keys::task(channel, id)).await?,
            None => None,
        };
        let waiting = match &bound {
            Some((channel, id)) => self.waiting_on(&s.agent, channel, id).await?,
            None => false,
        };
        let moving = mem.pending_moves.contains_key(&s.agent);
        let now = self.now();
        let decision = continuation::decide(
            agent,
            task.as_ref().map(|d| &d.value),
            Some(idle_since),
            waiting,
            moving,
            now,
        );
        match decision {
            Decision::Skip(_) => Ok(()),
            Decision::Continue => {
                // The record first: a crash between here and the send costs
                // one continue, never a second one for this idle period.
                let mut next = doc.value.clone();
                let Some(state) = next.continues.as_mut() else {
                    return Ok(());
                };
                state.record_continue(idle_since);
                let used = state.used;
                self.store.put(&doc.key, &next, Some(doc.revision)).await?;
                let Some((channel, id)) = &bound else {
                    return Ok(());
                };
                let env = Envelope {
                    id: MessageId::new(),
                    from: Sender::Human,
                    to: Target::Agent(next.id.clone()),
                    kind: EnvelopeKind::Direct,
                    text: continue_text(channel, id),
                    urgent: false,
                    event: None,
                    sent: now,
                };
                self.bus.publish(&subjects::agent(&next.id), &env).await?;
                tracing::info!(agent = %next.id, used, "continue sent");
                Ok(())
            }
            Decision::Exhaust(why) => {
                // The record first, then the task: a worker whose count ran
                // out never gets another continue, even if the task write
                // fails.
                let mut next = doc.value.clone();
                let Some(state) = next.continues.as_mut() else {
                    return Ok(());
                };
                state.exhausted = true;
                self.store.put(&doc.key, &next, Some(doc.revision)).await?;
                let Some(task_doc) = task else {
                    return Ok(());
                };
                let mut task = task_doc.value;
                task.status = TaskStatus::NeedsHuman;
                task.attention_msg = Some(why.to_string());
                task.updated = now;
                self.store
                    .put(&task_doc.key, &task, Some(task_doc.revision))
                    .await?;
                tracing::warn!(agent = %next.id, task = %task.id, why = %why, "the continues ran out");
                Ok(())
            }
        }
    }

    /// True when the agent stored a wait on this task: the worker is done and
    /// waits for the close, so the continues stop (phase 11 spec).
    async fn waiting_on(
        &self,
        agent: &AgentId,
        channel: &ChannelName,
        task: &TaskId,
    ) -> Result<bool, ApiError> {
        // A wait key names its agent and its id, `wait.<agent>.<id>`, but not
        // its task, so the scan reads the record.
        for doc in self.store.list::<WaitRecord>(keys::WAIT_PREFIX).await? {
            let w = doc.value;
            if &w.agent == agent && &w.channel == channel && &w.task == task {
                return Ok(true);
            }
        }
        Ok(false)
    }
}
