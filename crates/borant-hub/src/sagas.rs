//! The approval gate of a saga: who approves it, the message to the
//! approvers, and closing a channel (spec: sagas and epics, rules 2, 3, and 9).

use borant_core::keys;
use borant_core::model::{Agent, Channel, Sender, Task, TaskStatus};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::roles;
use borant_core::{ChannelName, CollectionName, TaskId};

use crate::Hub;
use crate::store::Doc;
use crate::tasks::is_owner_or_person;

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

    /// Rule 9: only the owner or a person closes a channel.
    pub(crate) async fn channel_close(
        &self,
        by: &Sender,
        mine: Option<&CollectionName>,
        name: &ChannelName,
    ) -> Result<Channel, ApiError> {
        let doc = self.channel_for(mine, name).await?;
        if !is_owner_or_person(&doc.value.owner, by) {
            return Err(ApiError::denied(
                "only the owner of the channel or a person closes it",
            ));
        }
        self.close_channel(doc).await
    }

    /// Rule 9 and choice C8: a closed channel is returned unchanged, and an
    /// open task holds the close with `conflict` and its ids.
    pub(crate) async fn close_channel(&self, mut doc: Doc<Channel>) -> Result<Channel, ApiError> {
        if doc.value.closed.is_some() {
            return Ok(doc.value);
        }
        let open: Vec<TaskId> = self
            .tasks(&doc.value.name)
            .await?
            .into_iter()
            .filter(|t| t.status != TaskStatus::Closed)
            .map(|t| t.id)
            .collect();
        if !open.is_empty() {
            let ids = open
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ApiError::new(
                ApiErrorCode::Conflict,
                format!("channel #{} holds open tasks: {ids}", doc.value.name),
            ));
        }
        doc.value.closed = Some(self.now());
        self.store
            .put(&doc.key, &doc.value, Some(doc.revision))
            .await?;
        Ok(doc.value)
    }
}
