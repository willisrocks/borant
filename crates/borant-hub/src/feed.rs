//! The person's reads of the event log (person changes feed spec): what
//! changed since a mark, and the history of one task. Both read the log and
//! the records and call `borant_core::changes`; nothing is stored, and the
//! reads must not run behind the hub request mutex.

use borant_core::changes::{
    self, CHANGE_KINDS, ChangeMark, ChangesInput, ChangesView, HISTORY_KINDS, HistoryEntry,
};
use borant_core::events::{EventFilter, SwarmEvent};
use borant_core::model::Channel;
use borant_core::proto::ApiError;
use borant_core::{ChannelName, CollectionName, TaskId};
use futures::StreamExt;
use serde_json::{Value, json};

use crate::Hub;

impl Hub {
    /// The changes of the log, newest first. `mark` is the place of the last
    /// change that the person saw; without it the feed reads the whole log.
    /// An agent reads only the changes of its own collection.
    pub(crate) async fn changes(
        &self,
        mark: Option<ChangeMark>,
        mine: Option<&CollectionName>,
    ) -> Result<ChangesView, ApiError> {
        let events = self.read_log(CHANGE_KINDS, mark).await?;
        let channels: Vec<Channel> = self.open_channels().await?;
        let tasks = self.all_tasks().await?;
        let mut view = changes::build(&ChangesInput {
            at: self.now(),
            events: &events,
            channels: &channels,
            tasks: &tasks,
        });
        if let Some(mine) = mine {
            let visible: Vec<ChannelName> = channels
                .iter()
                .filter(|c| c.collection.as_ref() == Some(mine))
                .map(|c| c.name.clone())
                .collect();
            view.changes
                .retain(|c| c.channel.as_ref().is_some_and(|c| visible.contains(c)));
        }
        Ok(view)
    }

    /// The history of one task, oldest first, at most 50 entries.
    pub(crate) async fn task_history(
        &self,
        channel: &ChannelName,
        id: &TaskId,
    ) -> Result<Value, ApiError> {
        // A task that does not exist is not a history with no entries.
        let ch = self.channel(channel).await?;
        self.task(channel, id).await?;
        let events = self.read_log(HISTORY_KINDS, None).await?;
        let entries: Vec<HistoryEntry> = changes::history(&events, channel, id);
        Ok(json!({
            "at": self.now(),
            "channel": ch.value.name,
            "task": id,
            "entries": entries,
        }))
    }

    /// The events of these kinds, in the window of the mark, oldest first.
    fn filter(kinds: &[&str], mark: Option<ChangeMark>) -> EventFilter {
        let mut filter = EventFilter {
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
            ..EventFilter::default()
        };
        match mark {
            Some(ChangeMark::Seq(seq)) => filter.after_seq = Some(seq),
            Some(ChangeMark::At(at)) => filter.since = Some(at),
            None => {}
        }
        filter
    }

    async fn read_log(
        &self,
        kinds: &[&str],
        mark: Option<ChangeMark>,
    ) -> Result<Vec<SwarmEvent>, ApiError> {
        let Some(log) = &self.events else {
            return Ok(Vec::new());
        };
        let mut stream = log.read(&Self::filter(kinds, mark), false).await?;
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        Ok(events)
    }
}
