//! The issue mirror (spec: issue mirror). The hub copies every saga, epic,
//! and task of a mirrored project out to an issue tracker, one way. A sweep
//! builds the record of each item, compares its hash with the KV record, and
//! creates or updates the issue through the [`IssueTracker`] port. It runs on
//! its own task, outside the request lock, so a slow tracker never delays a
//! lease renewal.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use borant_core::model::{Project, TaskEventKind, TaskStatus};
use borant_core::ports::{IssueTracker, IssueWrite, PortError, PortErrorKind};
use borant_core::proto::{ApiError, ApiErrorCode};
use borant_core::rules::mirror::{self, Item, ItemId, Team};
use borant_core::rules::roles;
use borant_core::{AgentId, ChannelName, keys};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::Hub;

/// How often the hub sweeps, and the adapter that writes the issues.
#[derive(Clone)]
pub struct IssueMirror {
    pub tracker: Arc<dyn IssueTracker>,
    /// The sweep interval of `[mirror].every_secs`.
    pub every: Duration,
    /// The most tracker calls of one sweep, from `[mirror].max_writes`.
    pub max_writes: usize,
}

impl fmt::Debug for IssueMirror {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssueMirror")
            .field("every", &self.every)
            .field("max_writes", &self.max_writes)
            .finish()
    }
}

/// Why the mirror stopped for an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stopped {
    /// No team and no parent issue: the item waits for `issue_target`.
    NoTarget,
    /// The linked issue is gone from the tracker.
    NotFound,
    /// The person cleared the link.
    Unlinked,
}

/// What the mirror remembers about one item. Every field is
/// `#[serde(default)]`, so a record that an older hub wrote still parses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MirrorRecord {
    /// The issue that the item is linked to.
    #[serde(default)]
    pub issue: Option<String>,
    /// The hash of the last record that the mirror wrote.
    #[serde(default)]
    pub hash: Option<String>,
    /// True when the mirror created the issue, false for an adopted one.
    #[serde(default)]
    pub owned: bool,
    /// Why the mirror stopped for the item, `None` while it runs.
    #[serde(default)]
    pub stopped: Option<Stopped>,
    /// The status from before the mirror raised `needs_human`.
    #[serde(default)]
    pub was: Option<TaskStatus>,
    /// True once the link write of the issue reached the item. The record goes
    /// first, so a crash between the two never adopts the hub's own issue.
    #[serde(default)]
    pub linked: bool,
    /// The hash that the tracker refused: the item waits for a change.
    #[serde(default)]
    pub refused: Option<String>,
}

/// What one item cost the sweep.
enum Outcome {
    /// No tracker call.
    Nothing,
    /// One tracker call.
    Called,
    /// The tracker denied the key: the mirror pauses until the next start.
    Paused,
}

impl Hub {
    /// The issue mirror loop: one sweep every `every`, on its own task.
    pub async fn mirror_loop(self: Arc<Self>, shutdown: CancellationToken) {
        let Some(mirror) = self.cfg.mirror.clone() else {
            return;
        };
        let mut every = tokio::time::interval(mirror.every);
        // A slow sweep must not line up ticks: the next one waits its turn.
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = every.tick() => {
                    match self.sweep_mirror(&mirror).await {
                        Ok(0) => {}
                        Ok(writes) => tracing::debug!(writes, "the issue mirror wrote"),
                        Err(e) => tracing::warn!("the issue mirror failed: {}", e.message),
                    }
                }
            }
        }
    }

    /// One sweep: every mirrored item of every mirrored project, parents
    /// first, at most `max_writes` tracker calls. Returns the number of calls.
    /// Public for tests.
    pub async fn sweep_mirror(&self, m: &IssueMirror) -> Result<usize, ApiError> {
        if self.mem.lock().await.mirror_refused.is_some() {
            return Ok(0);
        }
        let wanted = self.mirrored_projects().await?;
        let mut channels: BTreeMap<ChannelName, Option<AgentId>> = BTreeMap::new();
        let mut all: Vec<Item> = Vec::new();
        for channel in self.open_channels().await? {
            let Some(project) = channel.project.as_ref() else {
                continue;
            };
            if !wanted.get(&project.to_string()).copied().unwrap_or(false) {
                continue;
            }
            let tasks = self.tasks(&channel.name).await?;
            channels.insert(channel.name.clone(), channel.owner.clone());
            all.extend(mirror::items(&channel, &tasks));
        }
        // Parents before children, the oldest first.
        all.sort_by_key(|item| (item.depth, item.order));
        // An issue that several items link to gets its state from one of them.
        let losers = mirror::shared_link_losers(&all);

        // One read per record, and the issue key of every item that has one,
        // so a child sees the issue of its parent.
        let mut records: HashMap<ItemId, Option<MirrorRecord>> = HashMap::new();
        let mut known: HashMap<ItemId, String> = HashMap::new();
        for item in &all {
            let key = record_key(&item.id);
            let rec = self.store.get::<MirrorRecord>(&key).await?;
            if let Some(issue) = rec.as_ref().and_then(|d| d.value.issue.clone()) {
                known.insert(item.id.clone(), issue);
            } else if losers.contains(&item.id)
                && let Some(link) = item.link.clone()
            {
                // A rolled-up item writes nothing, but its children nest
                // under the issue that it shares.
                known.insert(item.id.clone(), link);
            }
            records.insert(item.id.clone(), rec.map(|d| d.value));
        }

        let mut calls = 0;
        for item in &all {
            if calls >= m.max_writes {
                break;
            }
            if losers.contains(&item.id) {
                continue;
            }
            let rec = records.get(&item.id).cloned().flatten();
            let owner = channels.get(channel_of(&item.id)).cloned().flatten();
            let id = record_key(&item.id);
            match self
                .mirror_item(m, item, rec, &mut known, owner.as_ref())
                .await
            {
                Ok(Outcome::Nothing) => {}
                Ok(Outcome::Called) => calls += 1,
                Ok(Outcome::Paused) => {
                    calls += 1;
                    break;
                }
                Err(e) => {
                    // A failed record write is a store failure, not a tracker
                    // one: the next sweep retries it.
                    tracing::warn!(item = %id, "the mirror cannot write: {}", e.message);
                }
            }
        }
        Ok(calls)
    }

    /// The projects whose `mirror_issues` resolves to true, by name.
    async fn mirrored_projects(&self) -> Result<BTreeMap<String, bool>, ApiError> {
        let mut out = BTreeMap::new();
        for doc in self.store.list::<Project>(keys::PROJECT_PREFIX).await? {
            let project = &doc.value;
            let collection = project
                .collection
                .as_ref()
                .and_then(|name| self.cfg.collections.get(name));
            out.insert(
                project.name.to_string(),
                roles::mirror_issues(project, collection),
            );
        }
        Ok(out)
    }
}

impl Hub {
    /// One item of the sweep, in the order of the spec: the stop, the closed
    /// skip, the adoption, the create, then the update.
    async fn mirror_item(
        &self,
        m: &IssueMirror,
        item: &Item,
        mut rec: Option<MirrorRecord>,
        known: &mut HashMap<ItemId, String>,
        owner: Option<&AgentId>,
    ) -> Result<Outcome, ApiError> {
        let id = record_key(&item.id);
        // A parent that is mirrored but has no issue yet makes the child
        // wait: no call, no stop, and no question (choice M9).
        let parent = match &item.parent {
            None => None,
            Some(parent) => match known.get(parent) {
                Some(key) => Some(key.clone()),
                None => return Ok(Outcome::Nothing),
            },
        };
        let write = IssueWrite {
            key: mirror::issue_key(&item.id.borant_id()),
            target: item.target.clone(),
            parent: parent.clone(),
            title: item.title.clone(),
            body: item.body.clone(),
            state: item.state,
        };

        // 1. A stopped item wakes only when the person sets a new link.
        if let Some(stopped) = rec.as_ref().and_then(|r| r.stopped)
            && matches!(stopped, Stopped::NotFound | Stopped::Unlinked)
        {
            let record = rec.as_mut().expect("a stop has a record");
            let link = item
                .link
                .clone()
                .filter(|link| Some(link) != record.issue.as_ref());
            let Some(link) = link else {
                return Ok(Outcome::Nothing);
            };
            // Adopt the link, then write its state at once.
            record.issue = Some(link);
            record.owned = false;
            record.stopped = None;
            record.linked = true;
            record.hash = None;
        }

        // 2. Closed history is not backfilled.
        if rec.is_none() && item.closed {
            return Ok(Outcome::Nothing);
        }

        // 3. An item that is already linked adopts its issue.
        if rec.is_none()
            && let Some(link) = item.link.clone()
        {
            let adopted = MirrorRecord {
                issue: Some(link.clone()),
                linked: true,
                ..MirrorRecord::default()
            };
            self.store.upsert(&id, &adopted).await?;
            known.insert(item.id.clone(), link);
            rec = Some(adopted);
        }

        let no_issue = rec.as_ref().is_none_or(|r| r.issue.is_none());
        if no_issue {
            // The create hash, which is the one that a refusal stores.
            let hash = mirror::record_hash(&write, true);
            // A refused create waits for the record itself to change.
            if rec.as_ref().and_then(|r| r.refused.as_deref()) == Some(hash.as_str()) {
                return Ok(Outcome::Nothing);
            }
            let stopped = rec.as_ref().and_then(|r| r.stopped);
            match mirror::team(item.target.as_deref(), parent.as_deref()) {
                Team::Missing => {
                    // A question that already waits is not asked twice.
                    if stopped == Some(Stopped::NoTarget) {
                        return Ok(Outcome::Nothing);
                    }
                    self.raise_no_target(item, rec.as_ref(), owner).await?;
                    return Ok(Outcome::Nothing);
                }
                Team::Target(_) | Team::Parent => {}
            }
            if stopped == Some(Stopped::NoTarget) {
                self.restore(item, rec.as_ref()).await?;
            }
            return match m.tracker.create(&write).await {
                Ok(reference) => {
                    // Create, then the record, then the link, then the linked
                    // flag: a crash at any point makes no second issue (M3).
                    let record = MirrorRecord {
                        issue: Some(reference.key.clone()),
                        hash: Some(hash),
                        owned: true,
                        linked: false,
                        ..MirrorRecord::default()
                    };
                    self.store.upsert(&id, &record).await?;
                    self.link(&item.id, &reference.key).await?;
                    self.store
                        .upsert(
                            &id,
                            &MirrorRecord {
                                linked: true,
                                ..record
                            },
                        )
                        .await?;
                    known.insert(item.id.clone(), reference.key);
                    Ok(Outcome::Called)
                }
                Err(e) => self.failed(item, &hash, e, owner, rec.as_ref()).await,
            };
        }

        // 5. A record with an issue: link it, stop it, or update it.
        let record = rec.as_ref().expect("a record");
        let issue = record.issue.clone().expect("an issue");
        if !record.linked && item.link.is_none() {
            // A crash between the record and the link: finish the link only.
            self.link(&item.id, &issue).await?;
            self.store
                .upsert(
                    &id,
                    &MirrorRecord {
                        linked: true,
                        ..record.clone()
                    },
                )
                .await?;
            return Ok(Outcome::Nothing);
        }
        if record.linked && item.link.is_none() {
            // The person cleared the link: the mirror stops for the item.
            self.store
                .upsert(
                    &id,
                    &MirrorRecord {
                        stopped: Some(Stopped::Unlinked),
                        ..record.clone()
                    },
                )
                .await?;
            return Ok(Outcome::Nothing);
        }
        let hash = mirror::record_hash(&write, record.owned);
        if record.hash.as_deref() == Some(hash.as_str()) {
            return Ok(Outcome::Nothing);
        }
        // An adopted issue gets the state only (Review Focus 3).
        let full = record.owned;
        match m.tracker.update(&issue, &write, full).await {
            Ok(_) => {
                self.store
                    .upsert(
                        &id,
                        &MirrorRecord {
                            hash: Some(hash),
                            ..record.clone()
                        },
                    )
                    .await?;
                Ok(Outcome::Called)
            }
            Err(e) => self.failed(item, &hash, e, owner, Some(record)).await,
        }
    }

    /// The link write: revision-checked, up to 5 tries, and only `issue` and
    /// `updated` change, so a concurrent `task_update` is not lost.
    async fn link(&self, id: &ItemId, key: &str) -> Result<(), ApiError> {
        for _ in 0..5 {
            let written = match id {
                ItemId::Task(channel, task) => {
                    let mut doc = self.task(channel, task).await?;
                    if doc.value.issue.as_deref() == Some(key) {
                        return Ok(());
                    }
                    doc.value.issue = Some(key.to_owned());
                    doc.value.updated = self.now();
                    self.store
                        .put(&doc.key, &doc.value, Some(doc.revision))
                        .await
                }
                ItemId::Channel(channel) => {
                    let mut doc = self.channel(channel).await?;
                    if doc.value.issue.as_deref() == Some(key) {
                        return Ok(());
                    }
                    doc.value.issue = Some(key.to_owned());
                    self.store
                        .put(&doc.key, &doc.value, Some(doc.revision))
                        .await
                }
            };
            match written {
                Ok(_) => return Ok(()),
                Err(e) if e.code == ApiErrorCode::Conflict => continue,
                Err(e) => return Err(e),
            }
        }
        // The next sweep finds the record with `linked: false` and links.
        Err(ApiError::internal(format!(
            "the link of {} to {key} lost 5 revision races",
            id.borant_id()
        )))
    }

    /// Raises `needs_human` on the task that answers for the item, with a
    /// revision-checked write, the way the continues limit does.
    async fn raise_no_target(
        &self,
        item: &Item,
        rec: Option<&MirrorRecord>,
        owner: Option<&AgentId>,
    ) -> Result<(), ApiError> {
        let channel = channel_of(&item.id).clone();
        let mut doc = self.task(&channel, &item.ask).await?;
        if doc.value.status == TaskStatus::Closed {
            return Ok(());
        }
        let message = mirror::no_target_message(&channel, &item.ask);
        let was = doc.value.status;
        doc.value.status = TaskStatus::NeedsHuman;
        doc.value.attention_msg = Some(match &doc.value.attention_msg {
            // A gate that already waits for approval keeps its question (M8).
            Some(existing) if was == TaskStatus::NeedsHuman => {
                format!("{existing}\n\n{message}")
            }
            _ => message,
        });
        doc.value.updated = self.now();
        let task = doc.value.clone();
        match self.store.put(&doc.key, &task, Some(doc.revision)).await {
            Ok(_) => {}
            // The next sweep asks again.
            Err(e) if e.code == ApiErrorCode::Conflict => return Ok(()),
            Err(e) => return Err(e),
        }
        let record = MirrorRecord {
            stopped: Some(Stopped::NoTarget),
            was: Some(was),
            ..rec.cloned().unwrap_or_default()
        };
        self.store.upsert(&record_key(&item.id), &record).await?;
        self.task_event(
            &channel,
            &owner.cloned(),
            &task,
            TaskEventKind::Updated,
            "needs an issue team",
        )
        .await?;
        Ok(())
    }

    /// Puts the task back the way it was, when its question is still the
    /// mirror's, and clears the stop whatever the task says now.
    async fn restore(&self, item: &Item, rec: Option<&MirrorRecord>) -> Result<(), ApiError> {
        let channel = channel_of(&item.id).clone();
        let message = mirror::no_target_message(&channel, &item.ask);
        let doc = self.task(&channel, &item.ask).await?;
        let mut next = doc.value.clone();
        let holds = next
            .attention_msg
            .as_deref()
            .is_some_and(|attention| attention.contains(&message));
        let mut changed = false;
        if holds {
            match rec.and_then(|r| r.was) {
                // The gate keeps its own question; only the mirror's text goes.
                Some(TaskStatus::NeedsHuman) => {
                    next.attention_msg = strip(&next.attention_msg, &message);
                }
                Some(was) => {
                    next.status = was;
                    next.attention_msg = None;
                }
                None => {}
            }
            next.updated = self.now();
            changed = true;
        }
        if changed {
            match self.store.put(&doc.key, &next, Some(doc.revision)).await {
                Ok(_) => {}
                Err(e) if e.code == ApiErrorCode::Conflict => return Ok(()),
                Err(e) => return Err(e),
            }
        }
        let record = MirrorRecord {
            stopped: None,
            was: None,
            ..rec.cloned().unwrap_or_default()
        };
        self.store.upsert(&record_key(&item.id), &record).await?;
        Ok(())
    }

    /// What a failed tracker call does: the record, the owner, or the pause.
    async fn failed(
        &self,
        item: &Item,
        hash: &str,
        error: PortError,
        owner: Option<&AgentId>,
        rec: Option<&MirrorRecord>,
    ) -> Result<Outcome, ApiError> {
        let id = record_key(&item.id);
        match error.kind {
            PortErrorKind::Denied => {
                // The key is wrong or was revoked: the mirror pauses until
                // the next hub start, and the alert tells the person once.
                tracing::error!(item = %id, "the issue tracker refused the mirror: {}", error.message);
                self.mem.lock().await.mirror_refused = Some(error.message);
                Ok(Outcome::Paused)
            }
            PortErrorKind::NotFound => {
                let record = MirrorRecord {
                    stopped: Some(Stopped::NotFound),
                    ..rec.cloned().unwrap_or_default()
                };
                self.store.upsert(&id, &record).await?;
                let issue = record.issue.as_deref().unwrap_or("the issue");
                let text = format!(
                    "Issue {issue} of {} is gone from the tracker; the mirror stops \
                     for it. Set issue on the item to link another one.",
                    item.id.borant_id()
                );
                self.say_owner(owner, &text).await;
                Ok(Outcome::Called)
            }
            PortErrorKind::Invalid => {
                // The refused hash keeps the item quiet until it changes (M6).
                let record = MirrorRecord {
                    refused: Some(hash.to_owned()),
                    ..rec.cloned().unwrap_or_default()
                };
                self.store.upsert(&id, &record).await?;
                let text = format!(
                    "The issue tracker refused {}: {}. The mirror skips it until it \
                     changes.",
                    item.id.borant_id(),
                    error.message
                );
                self.say_owner(owner, &text).await;
                Ok(Outcome::Called)
            }
            // The tracker is out of reach, slow, or broke: the next sweep
            // retries, and nothing is recorded.
            PortErrorKind::Unavailable
            | PortErrorKind::Timeout
            | PortErrorKind::Other
            | PortErrorKind::Conflict => {
                tracing::warn!(
                    item = %id,
                    "the issue mirror call failed, the next sweep retries: {}",
                    error.message
                );
                Ok(Outcome::Called)
            }
        }
    }

    /// One direct message from the hub; a failed one is a warn, not a failed
    /// sweep.
    async fn say_owner(&self, to: Option<&AgentId>, text: &str) {
        let Some(agent) = to else {
            return;
        };
        if let Err(e) = self.tell(agent, text).await {
            tracing::warn!(%agent, "cannot deliver a mirror message: {}", e.message);
        }
    }
}

/// The KV key of an item: `mirror.<channel>.<task>`, or `mirror.<channel>`.
fn record_key(id: &ItemId) -> String {
    match id {
        ItemId::Channel(channel) => format!("mirror.{channel}"),
        ItemId::Task(channel, task) => format!("mirror.{channel}.{task}"),
    }
}

/// The channel that holds an item.
fn channel_of(id: &ItemId) -> &ChannelName {
    match id {
        ItemId::Channel(channel) | ItemId::Task(channel, _) => channel,
    }
}

/// The attention text without the mirror's appended question, `None` when
/// nothing else is left.
fn strip(attention: &Option<String>, message: &str) -> Option<String> {
    let attention = attention.as_deref()?;
    let rest = attention.strip_suffix(&format!("\n\n{message}"));
    let rest = rest
        .or_else(|| (attention == message).then_some(""))
        .unwrap_or(attention);
    (!rest.is_empty()).then(|| rest.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_record_is_default_and_parses_an_old_one() {
        let old: MirrorRecord = serde_json::from_str(r#"{"issue":"T-1","hash":"h"}"#).unwrap();
        assert_eq!(old.issue.as_deref(), Some("T-1"));
        assert!(!old.owned);
        assert_eq!(old.stopped, None);
        assert!(!old.linked);
        assert_eq!(old.was, None);
        assert_eq!(old.refused, None);
    }

    #[test]
    fn mirror_record_names_the_stops_on_the_wire() {
        assert_eq!(
            serde_json::to_string(&Stopped::NoTarget).unwrap(),
            r#""no_target""#
        );
        assert_eq!(
            serde_json::to_string(&Stopped::NotFound).unwrap(),
            r#""not_found""#
        );
        assert_eq!(
            serde_json::to_string(&Stopped::Unlinked).unwrap(),
            r#""unlinked""#
        );
    }

    #[test]
    fn mirror_record_key_names_the_task_or_the_channel() {
        let channel: ChannelName = "work-saga".parse().unwrap();
        let raw = format!("01J{:023}", 1);
        let task: borant_core::TaskId = raw.parse().unwrap();
        assert_eq!(
            record_key(&ItemId::Channel(channel.clone())),
            "mirror.work-saga"
        );
        assert_eq!(
            record_key(&ItemId::Task(channel, task)),
            format!("mirror.work-saga.{raw}")
        );
    }

    #[test]
    fn strip_removes_only_the_mirror_question() {
        let message = "Issue team needed: set issue_target on this task.";
        let both = Some(format!("Scope of the saga\n\n{message}"));
        assert_eq!(strip(&both, message).as_deref(), Some("Scope of the saga"));
        assert_eq!(strip(&Some(message.to_owned()), message), None);
        assert_eq!(
            strip(&Some("other".into()), message).as_deref(),
            Some("other")
        );
        assert_eq!(strip(&None, message), None);
    }
}
