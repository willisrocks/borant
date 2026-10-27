//! The board (spec: sagas and epics, "API, MCP, and CLI"). It reads the
//! projects, the channels, every task, and the live view, then calls
//! `borant_core::board::build`. It writes nothing, and it must not run behind
//! the hub request mutex (review focus 7).

use borant_core::board::{BoardInput, BoardView, build};
use borant_core::model::{Channel, Project};
use borant_core::proto::ApiError;
use borant_core::{CollectionName, keys};
use serde_json::Value;

use crate::Hub;

/// Who reads the board. Choice C16: an agent sees only its own collection,
/// which may be none.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Caller<'a> {
    /// A person: every collection, or the one that the call names.
    Person,
    /// An agent, with the collection of its record.
    Agent(Option<&'a CollectionName>),
}

impl Hub {
    /// The board of `collection`, or of every collection that the caller may
    /// see. An agent that names another collection is `denied`.
    pub(crate) async fn board(
        &self,
        caller: Caller<'_>,
        collection: Option<CollectionName>,
    ) -> Result<BoardView, ApiError> {
        let now = self.now();
        let projects: Vec<Project> = self
            .store
            .list::<Project>(keys::PROJECT_PREFIX)
            .await?
            .into_iter()
            .map(|doc| doc.value)
            .collect();
        let channels: Vec<Channel> = self
            .store
            .list::<Value>(keys::CHANNEL_PREFIX)
            .await?
            .into_iter()
            .filter(|doc| doc.key.matches('.').count() == 1)
            .filter_map(|doc| serde_json::from_value::<Channel>(doc.value).ok())
            .collect();
        let tasks = self.all_tasks().await?;
        let agents = self.view().await?.agents;

        let wanted = match caller {
            Caller::Agent(mine) => {
                if let Some(want) = &collection
                    && Some(want) != mine
                {
                    return Err(ApiError::denied(format!(
                        "#{want} is in another collection"
                    )));
                }
                vec![mine.cloned()]
            }
            Caller::Person => match collection {
                Some(one) => vec![Some(one)],
                None => every_collection(self, &projects, &channels),
            },
        };
        Ok(build(
            &BoardInput {
                at: now,
                projects: &projects,
                channels: &channels,
                tasks: &tasks,
                agents: &agents,
                collections: &self.cfg.collections,
            },
            &wanted,
        ))
    }
}

/// C16: every collection of `hub.toml`, of a project, or of a channel, plus
/// one board with no collection when a project has none.
fn every_collection(
    hub: &Hub,
    projects: &[Project],
    channels: &[Channel],
) -> Vec<Option<CollectionName>> {
    let mut wanted: Vec<Option<CollectionName>> = hub
        .cfg
        .collections
        .keys()
        .cloned()
        .map(Some)
        .chain(
            projects
                .iter()
                .filter_map(|p| p.collection.clone())
                .map(Some),
        )
        .chain(
            channels
                .iter()
                .filter_map(|c| c.collection.clone())
                .map(Some),
        )
        .collect();
    wanted.sort();
    wanted.dedup();
    if projects.iter().any(|project| project.collection.is_none()) {
        wanted.push(None);
    }
    wanted
}
