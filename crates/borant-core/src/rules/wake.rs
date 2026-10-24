//! Which messages wake an idle session, and the wake limit (spec section 10).

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use crate::ids::{AgentId, ChannelName};
use crate::model::{Envelope, EnvelopeKind, Target, TaskStatus};
use crate::time::Timestamp;

/// Direct messages wake. Broadcasts wake only when urgent. Task events wake
/// only the owner of the channel, and only for closed, stuck, and
/// needs-human tasks.
pub fn should_wake(
    env: &Envelope,
    recipient: &AgentId,
    owner_of: &dyn Fn(&ChannelName) -> Option<AgentId>,
) -> bool {
    match env.kind {
        EnvelopeKind::Direct => true,
        EnvelopeKind::Broadcast => env.urgent,
        EnvelopeKind::Event => {
            let Some(ev) = &env.event else { return false };
            let attention = matches!(
                ev.status,
                TaskStatus::Closed | TaskStatus::Stuck | TaskStatus::NeedsHuman
            );
            let channel = match &env.to {
                Target::Channel(c) => c,
                Target::Agent(_) => &ev.channel,
            };
            attention && owner_of(channel).as_ref() == Some(recipient)
        }
    }
}

/// Allows at most `per_hour` wakes for each agent in any one-hour window.
#[derive(Debug)]
pub struct WakeLimiter {
    per_hour: u32,
    seen: HashMap<AgentId, VecDeque<Timestamp>>,
}

const HOUR: Duration = Duration::from_secs(3600);

impl WakeLimiter {
    pub fn new(per_hour: u32) -> Self {
        Self {
            per_hour,
            seen: HashMap::new(),
        }
    }

    /// Records a wake and returns true if the agent is under the limit.
    pub fn allow(&mut self, agent: &AgentId, now: Timestamp) -> bool {
        let q = self.seen.entry(agent.clone()).or_default();
        while q.front().is_some_and(|t| now.since(*t) >= HOUR) {
            q.pop_front();
        }
        if q.len() >= self.per_hour as usize {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{MessageId, TaskId};
    use crate::model::{Sender, TaskEvent, TaskEventKind};

    fn env(kind: EnvelopeKind, to: Target, urgent: bool, event: Option<TaskEvent>) -> Envelope {
        Envelope {
            id: MessageId::new(),
            from: Sender::Human,
            to,
            kind,
            text: "x".into(),
            urgent,
            event,
            sent: Timestamp(0),
        }
    }

    fn id(s: &str) -> AgentId {
        s.parse().unwrap()
    }

    #[test]
    fn broadcast_wakes_only_when_urgent() {
        let api: ChannelName = "api".parse().unwrap();
        let none = |_: &ChannelName| None;
        let to = Target::Channel(api);
        assert!(!should_wake(
            &env(EnvelopeKind::Broadcast, to.clone(), false, None),
            &id("w"),
            &none
        ));
        assert!(should_wake(
            &env(EnvelopeKind::Broadcast, to, true, None),
            &id("w"),
            &none
        ));
        assert!(should_wake(
            &env(EnvelopeKind::Direct, Target::Agent(id("w")), false, None),
            &id("w"),
            &none
        ));
    }

    #[test]
    fn task_events_wake_owner_only_for_attention() {
        let api: ChannelName = "api".parse().unwrap();
        let owner = |_: &ChannelName| Some(id("boss"));
        let ev = |status| TaskEvent {
            channel: api.clone(),
            task: TaskId::new(),
            title: "t".into(),
            kind: TaskEventKind::Updated,
            status,
            attention_msg: None,
        };
        let to = Target::Channel(api.clone());
        let stuck = env(
            EnvelopeKind::Event,
            to.clone(),
            false,
            Some(ev(TaskStatus::Stuck)),
        );
        let progress = env(
            EnvelopeKind::Event,
            to,
            false,
            Some(ev(TaskStatus::InProgress)),
        );
        assert!(should_wake(&stuck, &id("boss"), &owner));
        assert!(!should_wake(&stuck, &id("worker"), &owner));
        assert!(!should_wake(&progress, &id("boss"), &owner));
    }

    #[test]
    fn limiter_blocks_31st_wake_in_hour() {
        let mut lim = WakeLimiter::new(30);
        let a = id("a");
        for i in 0..30 {
            assert!(lim.allow(&a, Timestamp(i * 1000)), "wake {i}");
        }
        assert!(!lim.allow(&a, Timestamp(31_000)));
        assert!(lim.allow(&id("b"), Timestamp(31_000)));
        assert!(lim.allow(&a, Timestamp(3_600_000)));
    }
}
