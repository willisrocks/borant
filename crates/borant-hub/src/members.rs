//! Bundle member assignment (spec "Member rules"). The hub picks the member of
//! each bundle with several members at every launch, keeps the latest usage
//! report of each member in memory, and keeps the state of each member in KV at
//! `member.<bundle>.<member>`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use borant_core::events::{Severity, SwarmEventBody};
use borant_core::model::{
    Agent, Envelope, EnvelopeKind, Harness, HostInfo, Lease, LeaseState, MemberState, MoveReason,
    Sender, Target, UsageWindow,
};
use borant_core::proto::{ApiError, HostCommand, MoveTarget};
use borant_core::rules::members::{Pick, REPORT_FRESH_FOR, note_restart, pick};
use borant_core::{
    AgentId, BundleName, FencingToken, HostName, MemberName, MessageId, SessionId, Timestamp, keys,
    subjects,
};
use serde_json::{Value, json};

use crate::{Hub, Memory, PendingMove};

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
                member: member.clone(),
                limited_until: Some(until),
                waiting: Vec::new(),
                restarts: Vec::new(),
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

    /// Counts one limit restart, or one wait, of a member for the last seven
    /// days of `borant members`.
    async fn add_restart(
        &self,
        bundle: &BundleName,
        member: &MemberName,
        now: Timestamp,
    ) -> Result<(), ApiError> {
        let key = key(bundle, member);
        let doc = self.store.get::<MemberState>(&key).await?;
        let mut state = doc
            .as_ref()
            .map(|d| d.value.clone())
            .unwrap_or(MemberState {
                bundle: bundle.clone(),
                member: member.clone(),
                limited_until: None,
                waiting: Vec::new(),
                restarts: Vec::new(),
            });
        note_restart(&mut state, now);
        self.store
            .put(&key, &state, doc.as_ref().map(|d| d.revision))
            .await?;
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
                        // One time for each limit restart or wait in the last
                        // seven days.
                        "restarts": state.map(|s| s.restarts.clone()).unwrap_or_default(),
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

/// The window of the loop guard: the limit restarts of one agent in one hour.
pub(crate) const LIMIT_WINDOW: Duration = Duration::from_secs(60 * 60);

/// What a session gets when the member of its bundle resets.
pub const WAKE_TEXT: &str = "Your account limit reset. Continue your task.";

impl Hub {
    /// A session stopped at the usage limit of one bundle member (spec "Limit
    /// restart"). The member is limited until its reset, and the agent then
    /// either restarts on the same host with the next free member or waits for
    /// the earliest reset.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn member_limited(
        &self,
        mem: &mut Memory,
        agent: AgentId,
        session: SessionId,
        token: FencingToken,
        bundle: BundleName,
        member: MemberName,
        resets_at: Option<Timestamp>,
    ) -> Result<Value, ApiError> {
        // A late hook must not mark a member of a session that already moved.
        let Some((host, info)) = self.held_host(&agent, session, token).await? else {
            return Err(borant_core::CoreError::StaleToken.into());
        };
        let now = self.now();
        let until = resets_at.unwrap_or_else(|| {
            now.plus(Duration::from_secs(
                self.cfg.member_rules.unknown_reset_mins.saturating_mul(60),
            ))
        });
        self.set_member_limited(&bundle, &member, until, None)
            .await?;
        let event = self
            .event(
                now,
                Severity::Warn,
                SwarmEventBody::MemberLimited {
                    bundle: bundle.clone(),
                    member: member.clone(),
                    until: Some(until),
                },
            )
            .about_agent(agent.clone())
            .about_host(host.clone());
        Self::queue(mem, event);
        tracing::info!(%agent, %bundle, %member, until = %until, "the session hit the usage limit of its member");

        // A bundle that the host defines with several members is the only one
        // that can move the agent to another account.
        let Some(known) = info.as_ref().and_then(|i| i.members.get(&bundle)) else {
            return Ok(json!({ "restart": false, "waiting": false }));
        };
        // The states hold the limit that this call just wrote.
        let states = self.member_states(&bundle).await?;
        let reports = self.fresh_reports(mem, &bundle, now);
        let next = match pick(&known.members, Some(&member), &states, &reports, now) {
            Pick::Member(m) if m != member => Some(m),
            _ => None,
        };
        // The loop guard counts the restarts of one agent in one hour.
        let allowed = {
            let restarts = mem.limit_restarts.entry(agent.clone()).or_default();
            restarts.retain(|t| now.since(*t) < LIMIT_WINDOW);
            (restarts.len() as u32) < self.cfg.member_rules.max_restarts_per_hour
        };
        if allowed && let Some(next) = &next {
            mem.limit_restarts
                .entry(agent.clone())
                .or_default()
                .push(now);
            // The old session ends through the planned move path first: a
            // placement refuses a held lease. The move stays on the same host.
            mem.pending_moves.insert(
                agent.clone(),
                PendingMove {
                    target: MoveTarget {
                        host: Some(host.clone()),
                        harness: None,
                    },
                    reason: MoveReason::Limit,
                    prompt: None,
                },
            );
            let cmd = HostCommand::PrepareMove {
                agent: agent.clone(),
                session,
            };
            let payload =
                serde_json::to_vec(&cmd).map_err(|e| ApiError::internal(e.to_string()))?;
            self.bus
                .send(&subjects::host_cmd(&host), payload.into())
                .await?;
            let requested = self
                .event(
                    now,
                    Severity::Info,
                    SwarmEventBody::MoveRequested {
                        from_host: Some(host.clone()),
                        to_host: host.clone(),
                        reason: MoveReason::Limit,
                    },
                )
                .about_agent(agent.clone());
            Self::queue(mem, requested);
            // The member that ran out is the one that is counted, once for
            // this limit restart.
            self.add_restart(&bundle, &member, now).await?;
            tracing::info!(%agent, %bundle, %member, %next, "limit restart on the same host");
            return Ok(json!({ "restart": true, "member": next }));
        }
        // No free member, or the guard holds the agent: it waits for the
        // earliest reset and its session stays idle.
        let earliest = states
            .iter()
            .filter_map(|s| s.limited_until)
            .min()
            .unwrap_or(until);
        let current = self.agent(&agent).await?;
        self.wait_for_member(mem, &current.value, &bundle, &states, earliest)
            .await?;
        // The member that ran out is counted once, whether its limit
        // restarted the agent or made it wait for the reset.
        self.add_restart(&bundle, &member, now).await?;
        if next.is_some() {
            tracing::warn!(
                %agent,
                "the limit restart guard holds the agent until {earliest}"
            );
        }
        Ok(json!({ "restart": false, "waiting": earliest }))
    }

    /// The host of a held lease and its `HostInfo`, when `session` and `token`
    /// still hold it.
    async fn held_host(
        &self,
        agent: &AgentId,
        session: SessionId,
        token: FencingToken,
    ) -> Result<Option<(HostName, Option<HostInfo>)>, ApiError> {
        let Some(doc) = self.store.get::<Lease>(&keys::lease_agent(agent)).await? else {
            return Ok(None);
        };
        let LeaseState::Held {
            session: s, host, ..
        } = &doc.value.state
        else {
            return Ok(None);
        };
        if *s != session || doc.value.token != token {
            return Ok(None);
        }
        let info = self
            .store
            .get::<HostInfo>(&keys::host(host))
            .await?
            .map(|d| d.value);
        Ok(Some((host.clone(), info)))
    }

    /// Marks one member limited until `until`, and keeps its waiting agents
    /// unless `waiting` replaces them.
    async fn set_member_limited(
        &self,
        bundle: &BundleName,
        member: &MemberName,
        until: Timestamp,
        waiting: Option<Vec<AgentId>>,
    ) -> Result<(), ApiError> {
        let key = key(bundle, member);
        let doc = self.store.get::<MemberState>(&key).await?;
        let mut state = doc
            .as_ref()
            .map(|d| d.value.clone())
            .unwrap_or(MemberState {
                bundle: bundle.clone(),
                member: member.clone(),
                limited_until: None,
                waiting: Vec::new(),
                restarts: Vec::new(),
            });
        state.limited_until = Some(until);
        if let Some(waiting) = waiting {
            state.waiting = waiting;
        }
        self.store
            .put(&key, &state, doc.as_ref().map(|d| d.revision))
            .await?;
        Ok(())
    }

    /// Wakes the agents that wait for a member that reset, and clears its
    /// limit. The hub tick calls it (rule 6 of the spec).
    pub(crate) async fn wake_limited_members(&self) -> Result<(), ApiError> {
        let now = self.now();
        for doc in self.store.list::<MemberState>("member.").await? {
            let state = doc.value;
            let Some(until) = state.limited_until else {
                continue;
            };
            if until > now {
                continue;
            }
            for agent in &state.waiting {
                let env = Envelope {
                    id: MessageId::new(),
                    from: Sender::Human,
                    to: Target::Agent(agent.clone()),
                    kind: EnvelopeKind::Direct,
                    text: WAKE_TEXT.to_owned(),
                    urgent: false,
                    event: None,
                    sent: now,
                };
                if let Err(e) = self.bus.publish(&subjects::agent(agent), &env).await {
                    tracing::warn!(%agent, "cannot wake the agent after its limit reset: {e}");
                }
            }
            if !state.waiting.is_empty() {
                tracing::info!(
                    bundle = %state.bundle,
                    member = %state.member,
                    agents = state.waiting.len(),
                    "the member reset: its waiting agents are woken"
                );
            }
            let cleared = MemberState {
                bundle: state.bundle.clone(),
                member: state.member.clone(),
                limited_until: None,
                waiting: Vec::new(),
                // The count of the last seven days outlives one reset.
                restarts: state.restarts.clone(),
            };
            self.store
                .put(
                    &key(&state.bundle, &state.member),
                    &cleared,
                    Some(doc.revision),
                )
                .await?;
        }
        Ok(())
    }
}
