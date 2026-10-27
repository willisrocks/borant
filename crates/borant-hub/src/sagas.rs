//! The approval gate of a saga: who approves it, and the message to the
//! approvers (spec: sagas and epics, rules 2 and 3).

use borant_core::keys;
use borant_core::model::{Agent, Channel, Sender, Task};
use borant_core::proto::ApiError;
use borant_core::rules::roles;

use crate::Hub;

impl Hub {
    /// Rule 3: a person approves, and so does an agent whose role has
    /// `approves = true`. The owner of the channel is not an approver by
    /// being the owner.
    pub(crate) async fn is_approver(&self, by: &Sender) -> Result<bool, ApiError> {
        match by {
            Sender::Human => Ok(true),
            Sender::Agent(id) => {
                let agent = self.agent(id).await?;
                Ok(roles::approves(agent.value.role.as_ref(), &self.cfg.roles))
            }
        }
    }

    /// Rule 2 and choice C6: one direct message for a gate that goes to
    /// `needs_human`, to each approver agent of the saga's collection, and to
    /// no approver of another collection. A channel with no collection tells
    /// the approvers with no collection. Returns how many agents it told.
    pub(crate) async fn notify_approvers(
        &self,
        ch: &Channel,
        gate: &Task,
    ) -> Result<usize, ApiError> {
        let title = ch
            .title
            .clone()
            .unwrap_or_else(|| ch.name.as_str().to_owned());
        let text = format!(
            "Saga #{name} ({title}) waits for approval: {why}. Close task {name}/{gate} as done to approve it, or as wontfix to reject it.",
            name = ch.name,
            why = gate.attention_msg.clone().unwrap_or_default(),
            gate = gate.id,
        );
        let mut told = 0;
        for doc in self.store.list::<Agent>(keys::AGENT_PREFIX).await? {
            let agent = doc.value;
            if agent.retired || agent.collection != ch.collection {
                continue;
            }
            if !roles::approves(agent.role.as_ref(), &self.cfg.roles) {
                continue;
            }
            self.tell(&agent.id, &text).await?;
            told += 1;
        }
        Ok(told)
    }
}
