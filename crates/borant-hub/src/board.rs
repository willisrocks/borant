//! The board (spec: sagas and epics, "API, MCP, and CLI"). It reads the
//! projects, the channels, every task, and the live view, then calls
//! `borant_core::board::build`. It writes nothing, and it must not run behind
//! the hub request mutex (review focus 7).

use std::collections::HashMap;

use borant_core::board::{BoardInput, BoardView, DEFAULT_DONE_SINCE, TaskLines, build};
use borant_core::events::EventFilter;
use borant_core::model::{Channel, Project, SpawnRecord};
use borant_core::proto::ApiError;
use borant_core::{CollectionName, TaskId, Timestamp, keys};
use futures::StreamExt;
use serde_json::Value;

use crate::Hub;
use crate::changes::PR_URL;

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
    ///
    /// `with_tasks` adds the lines of `--tasks`: the time of each task's
    /// last status change, and the PR watch's last read.
    pub(crate) async fn board(
        &self,
        caller: Caller<'_>,
        collection: Option<CollectionName>,
        with_tasks: bool,
        done_since: Option<Timestamp>,
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
        let all = self.all_tasks().await?;
        let spawns: Vec<SpawnRecord> = self
            .store
            .list::<SpawnRecord>(keys::SPAWN_PREFIX)
            .await?
            .into_iter()
            .map(|doc| doc.value)
            .collect();
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
        // The lines add two reads that the counts do not need: the time of
        // each task's last status change, and the state of each change
        // request that the watch holds (spec: board task lines, "Cost").
        let status_since = if with_tasks {
            self.status_since().await?
        } else {
            Vec::new()
        };
        let mut prs = Vec::new();
        if with_tasks {
            for (channel, task) in &all {
                if !task.properties.contains_key(PR_URL) {
                    continue;
                }
                if let Some(status) = self.change_status(channel, &task.id).await? {
                    prs.push((channel.clone(), task.id, status));
                }
            }
        }
        let lines = with_tasks.then(|| TaskLines {
            done_since: done_since.unwrap_or(now.minus(DEFAULT_DONE_SINCE)),
            status_since: &status_since,
            prs: &prs,
        });

        Ok(build(
            &BoardInput {
                at: now,
                projects: &projects,
                channels: &channels,
                tasks: &all,
                spawns: &spawns,
                agents: &agents,
                collections: &self.cfg.collections,
                lines,
            },
            &wanted,
        ))
    }

    /// The time of the last `task.status` event of each task, for the task
    /// lines. A task that the log predates has none, and its line falls back
    /// to the record's own time.
    ///
    /// The board reads this every few seconds, so a pass scans only the
    /// events that arrived since the last one: the whole history is read
    /// once, on the first pass of the hub.
    async fn status_since(&self) -> Result<Vec<(TaskId, Timestamp)>, ApiError> {
        let Some(log) = &self.events else {
            return Ok(Vec::new());
        };
        // The pass starts here, and the next one reads from a millisecond
        // before it: an event stamped in the same millisecond as this pass
        // arrived after this read, and must still be seen.
        let now = self.now();
        let filter = EventFilter {
            kinds: vec!["task.status".to_owned()],
            since: self
                .status_since
                .lock()
                .expect("the status times")
                .scanned_to
                .map(|at| Timestamp(at.0.saturating_sub(1))),
            ..EventFilter::default()
        };
        let mut read = log.read(&filter, false).await?;
        let mut fresh: HashMap<TaskId, Timestamp> = HashMap::new();
        while let Some(event) = read.next().await {
            let Some(task) = event.task else {
                continue;
            };
            fresh
                .entry(task)
                .and_modify(|at| *at = (*at).max(event.at))
                .or_insert(event.at);
        }
        // Another pass may have read the same window; the times merge by
        // max, so the answer is the same.
        let mut held = self.status_since.lock().expect("the status times");
        for (task, at) in fresh {
            held.since
                .entry(task)
                .and_modify(|had| *had = (*had).max(at))
                .or_insert(at);
        }
        held.scanned_to = Some(held.scanned_to.map_or(now, |had| had.max(now)));
        Ok(held.since.iter().map(|(task, at)| (*task, *at)).collect())
    }
}

/// How far the task lines have read the status changes of the tasks.
#[derive(Debug, Default)]
pub(crate) struct StatusSinces {
    /// The start of the last pass that read the log. `None` reads it all.
    scanned_to: Option<Timestamp>,
    /// The time of the last status change of a task, by task id.
    since: HashMap<TaskId, Timestamp>,
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
