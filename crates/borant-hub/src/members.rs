//! Bundle member assignment (spec "Member rules"). The hub picks the member of
//! each bundle with several members at every launch, keeps the latest usage
//! report of each member in memory, and keeps the state of each member in KV at
//! `member.<bundle>.<member>`.

use std::collections::{BTreeMap, BTreeSet};

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{Agent, Harness, HostInfo, MemberState, UsageWindow};
use borant_core::proto::ApiError;
use borant_core::rules::members::{Pick, REPORT_FRESH_FOR, pick};
use borant_core::{BundleName, MemberName, Timestamp, keys};
use serde_json::{Value, json};

use crate::{Hub, Memory};

/// A usage report that a human sent, with the time it arrived.
#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub at: Timestamp,
    pub windows: Vec<UsageWindow>,
}

/// The KV key of one member state.
pub(crate) fn key(bundle: &BundleName, member: &MemberName) -> String {
    format!("member.{bundle}.{member}")
}

/// The KV prefix of the states of one bundle.
pub(crate) fn prefix(bundle: &BundleName) -> String {
    format!("member.{bundle}.")
}

/// What a launch learned about the members of its host.
pub(crate) enum Assign {
    /// One member for each bundle with several members.
    Members(BTreeMap<BundleName, MemberName>),
    /// Every member of a bundle is limited; the agent waits for a reset.
    Wait,
}

impl Hub {
    /// The member of each bundle with several members on `host` for a launch
    /// with `harness`. A bundle whose members are all limited puts the agent in
    /// `waiting` and returns [`Assign::Wait`].
    pub(crate) async fn assign_members(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        host: &HostInfo,
        harness: Harness,
    ) -> Result<Assign, ApiError> {
        let now = self.now();
        let mut members = BTreeMap::new();
        for (bundle, known) in &host.members {
            // A bundle with one member needs no pick and is not announced.
            if known.members.len() < 2 || !known.harnesses.contains(&harness) {
                continue;
            }
            let states = self.member_states(bundle).await?;
            let reports = self.fresh_reports(mem, bundle, now);
            match pick(
                &known.members,
                agent.members.get(bundle),
                &states,
                &reports,
                now,
            ) {
                Pick::Member(member) => {
                    members.insert(bundle.clone(), member);
                }
                Pick::WaitUntil(until) => {
                    self.wait_for_member(mem, agent, bundle, &states, until)
                        .await?;
                    return Ok(Assign::Wait);
                }
            }
        }
        Ok(Assign::Members(members))
    }

    /// The states of the members of one bundle.
    async fn member_states(&self, bundle: &BundleName) -> Result<Vec<MemberState>, ApiError> {
        Ok(self
            .store
            .list::<MemberState>(&prefix(bundle))
            .await?
            .into_iter()
            .map(|d| d.value)
            .collect())
    }

    /// The reports of this bundle that are still fresh.
    fn fresh_reports(
        &self,
        mem: &Memory,
        bundle: &BundleName,
        now: Timestamp,
    ) -> Vec<(MemberName, Vec<UsageWindow>)> {
        mem.reports
            .iter()
            .filter(|((b, _), r)| b == bundle && now.since(r.at) <= REPORT_FRESH_FOR)
            .map(|((_, member), report)| (member.clone(), report.windows.clone()))
            .collect()
    }

    /// Puts the agent in `waiting` of the member with the earliest reset, and
    /// logs the limit of the bundle once.
    async fn wait_for_member(
        &self,
        mem: &mut Memory,
        agent: &Agent,
        bundle: &BundleName,
        states: &[MemberState],
        until: Timestamp,
    ) -> Result<(), ApiError> {
        if mem.limit_warned.insert(bundle.clone()) {
            tracing::warn!("every member of {bundle} is limited until {until}");
        }
        let Some(member) = states
            .iter()
            .filter(|s| s.limited_until == Some(until))
            .map(|s| s.member.clone())
            .min()
        else {
            return Ok(());
        };
        let key = key(bundle, &member);
        let doc = self.store.get::<MemberState>(&key).await?;
        let mut state = doc
            .as_ref()
            .map(|d| d.value.clone())
            .unwrap_or(MemberState {
                bundle: bundle.clone(),
                member,
                limited_until: Some(until),
                waiting: Vec::new(),
            });
        state.limited_until = Some(until);
        if !state.waiting.contains(&agent.id) {
            state.waiting.push(agent.id.clone());
        }
        self.store
            .put(&key, &state, doc.as_ref().map(|d| d.revision))
            .await?;
        let event = self
            .event(
                self.now(),
                Severity::Warn,
                SwarmEventBody::MemberLimited {
                    bundle: state.bundle.clone(),
                    member: state.member.clone(),
                    until: Some(until),
                },
            )
            .about_agent(agent.id.clone());
        Self::queue(mem, event);
        Ok(())
    }

    /// The state and the latest report of the member of each bundle.
    pub(crate) async fn list_members(&self, mem: &Memory) -> Result<Value, ApiError> {
        let now = self.now();
        let mut bundles: BTreeMap<BundleName, Vec<MemberName>> = BTreeMap::new();
        for doc in self.store.list::<HostInfo>(keys::HOST_PREFIX).await? {
            for (bundle, known) in doc.value.members {
                let list = bundles.entry(bundle).or_default();
                for member in known.members {
                    if !list.contains(&member) {
                        list.push(member);
                    }
                }
            }
        }
        // A state can name a member that no host announces any more.
        for doc in self.store.list::<MemberState>("member.").await? {
            let list = bundles.entry(doc.value.bundle).or_default();
            if !list.contains(&doc.value.member) {
                list.push(doc.value.member);
            }
        }
        let mut out = serde_json::Map::new();
        for (bundle, mut members) in bundles {
            members.sort();
            let states = self.member_states(&bundle).await?;
            let list: Vec<Value> = members
                .iter()
                .map(|member| {
                    let state = states.iter().find(|s| &s.member == member);
                    let report = mem.reports.get(&(bundle.clone(), member.clone())).map(|r| {
                        json!({
                            "at": r.at,
                            "fresh": now.since(r.at) <= REPORT_FRESH_FOR,
                            "windows": r.windows,
                        })
                    });
                    json!({
                        "member": member,
                        "limited_until": state.and_then(|s| s.limited_until),
                        "waiting": state.map(|s| s.waiting.clone()).unwrap_or_default(),
                        "report": report,
                    })
                })
                .collect();
            out.insert(bundle.to_string(), Value::Array(list));
        }
        Ok(Value::Object(out))
    }

    /// Keeps the latest usage report of one member, in memory only.
    pub(crate) async fn usage_report(
        &self,
        mem: &mut Memory,
        bundle: BundleName,
        member: MemberName,
        windows: Vec<UsageWindow>,
    ) -> Result<Value, ApiError> {
        for window in &windows {
            if !(0.0..=1.0).contains(&window.used) {
                return Err(ApiError::invalid(format!(
                    "the used share of window {} is not between 0 and 1",
                    window.name
                )));
            }
        }
        let at = self.now();
        let count = windows.len();
        mem.reports.insert(
            (bundle.clone(), member.clone()),
            Report {
                at,
                windows: windows.clone(),
            },
        );
        tracing::info!(%bundle, %member, windows = count, "usage report");
        Ok(json!({ "bundle": bundle, "member": member, "at": at, "windows": windows }))
    }
}

impl Hub {
    /// The bundles whose every member is limited now, for the alert pass. The
    /// members come from the hosts that define the bundle; a member with no
    /// state, or one whose limit has passed, keeps the bundle out.
    pub(crate) async fn limited_bundles(
        &self,
        now: Timestamp,
    ) -> Result<Vec<BundleName>, ApiError> {
        let mut members: BTreeMap<BundleName, BTreeSet<MemberName>> = BTreeMap::new();
        for doc in self.store.list::<HostInfo>(keys::HOST_PREFIX).await? {
            for (bundle, known) in doc.value.members {
                members.entry(bundle).or_default().extend(known.members);
            }
        }
        if members.is_empty() {
            return Ok(Vec::new());
        }
        let states: Vec<MemberState> = self
            .store
            .list::<MemberState>("member.")
            .await?
            .into_iter()
            .map(|d| d.value)
            .collect();
        let mut limited = Vec::new();
        for (bundle, known) in members {
            if known.is_empty() {
                continue;
            }
            let every = known.iter().all(|member| {
                states.iter().any(|state| {
                    state.bundle == bundle
                        && &state.member == member
                        && state.limited_until.is_some_and(|until| until > now)
                })
            });
            if every {
                limited.push(bundle);
            }
        }
        Ok(limited)
    }
}
