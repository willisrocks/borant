//! Request dispatch and the channel and agent calls.

use std::time::Duration;

use borant_core::model::{Agent, Channel, Lease, LeaseState, Member, Project, Sender};
use borant_core::proto::{Actor, ApiError, CtlRequest, ToolCall};
use borant_core::rules::schema;
use borant_core::{AgentId, ChannelName, ProjectName, keys, subjects};
use serde_json::{Value, json};

use crate::store::Doc;
use crate::{Hub, Memory};

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::internal(e.to_string()))
}

impl Hub {
    pub(crate) async fn dispatch(
        &self,
        mem: &mut Memory,
        req: CtlRequest,
    ) -> Result<Value, ApiError> {
        match req {
            CtlRequest::RegisterHost { info } => self.register_host(mem, info).await,
            CtlRequest::HostHeartbeat { host, sessions } => {
                to_value(&self.heartbeat(mem, &host, &sessions).await?)
            }
            CtlRequest::RegisterSession(r) => to_value(&self.register_session(mem, r).await?),
            CtlRequest::EndSession {
                session,
                agent,
                token,
                reason,
            } => {
                self.end_session(mem, session, &agent, token, &reason)
                    .await?;
                Ok(Value::Null)
            }
            CtlRequest::Call { actor, call } => self.call(mem, actor, call).await,
            CtlRequest::MoveReady {
                agent,
                session,
                token,
            } => {
                self.move_ready(mem, &agent, session, token).await?;
                Ok(Value::Null)
            }
            CtlRequest::LaunchFailed { agent, host, error } => {
                self.launch_failed(mem, &agent, &host, &error).await?;
                Ok(Value::Null)
            }
            CtlRequest::Maintenance { on } => {
                mem.maintenance = on;
                Ok(json!({ "maintenance": on }))
            }
            // `Hub::handle` routes a snapshot before it takes the request
            // mutex, because the snapshot reads every key and inbox.
            CtlRequest::Snapshot { .. } => Err(ApiError::internal(
                "a snapshot must not run behind the hub request mutex",
            )),
        }
    }

    async fn call(
        &self,
        mem: &mut Memory,
        actor: Option<Actor>,
        call: ToolCall,
    ) -> Result<Value, ApiError> {
        match &actor {
            Some(a) => self.verify_actor(a).await?,
            None if !call.allowed_for_human() => {
                return Err(ApiError::invalid(
                    "this call needs an agent session; run it from a harness",
                ));
            }
            None => {}
        }
        let me = actor.as_ref().map(|a| a.agent.clone());
        let sender = me.clone().map_or(Sender::Human, Sender::Agent);
        match call {
            ToolCall::Whoami => {
                let a = actor.ok_or_else(|| ApiError::invalid("whoami needs a session"))?;
                let agent = self.agent(&a.agent).await?;
                let mut owner_of = Vec::new();
                for c in &agent.value.channels {
                    if let Ok(ch) = self.channel(c).await
                        && ch.value.owner == a.agent
                    {
                        owner_of.push(c.clone());
                    }
                }
                Ok(json!({
                    "agent": agent.value.id,
                    "session": a.session,
                    "host": agent.value.host,
                    "harness": agent.value.preferred_harness,
                    "channels": agent.value.channels,
                    "owner_of": owner_of,
                    "movable": agent.value.movable,
                }))
            }
            ToolCall::ChannelCreate {
                name,
                schema: s,
                project,
                branch,
            } => {
                let me = me.ok_or_else(|| ApiError::invalid("needs a session"))?;
                to_value(&self.channel_create(&me, &name, s, project, branch).await?)
            }
            ToolCall::ChannelJoin { name } => {
                let me = me.ok_or_else(|| ApiError::invalid("needs a session"))?;
                self.channel(&name).await?;
                self.join(&me, &name).await?;
                Ok(json!({ "joined": name }))
            }
            ToolCall::ChannelLeave { name } => {
                let me = me.ok_or_else(|| ApiError::invalid("needs a session"))?;
                self.leave(&me, &name).await?;
                Ok(json!({ "left": name }))
            }
            ToolCall::ChannelList => {
                let chans: Vec<Channel> = self
                    .store
                    .list::<Value>(keys::CHANNEL_PREFIX)
                    .await?
                    .into_iter()
                    .filter(|d| d.key.matches('.').count() == 1)
                    .filter_map(|d| serde_json::from_value(d.value).ok())
                    .collect();
                to_value(&chans)
            }
            ToolCall::ChannelTransfer { name, to } => {
                let mut ch = self.channel(&name).await?;
                if let Some(me) = &me
                    && ch.value.owner != *me
                {
                    return Err(ApiError::invalid("only the owner can transfer a channel"));
                }
                if self
                    .store
                    .get::<Member>(&keys::member(&name, &to))
                    .await?
                    .is_none()
                {
                    return Err(ApiError::invalid(format!(
                        "{to} is not a member of #{name}"
                    )));
                }
                ch.value.owner = to;
                self.store
                    .put(&ch.key, &ch.value, Some(ch.revision))
                    .await?;
                to_value(&ch.value)
            }
            ToolCall::Members { name } => {
                self.channel(&name).await?;
                let mut out = Vec::new();
                for m in self
                    .store
                    .list::<Member>(&keys::members_prefix(&name))
                    .await?
                {
                    let lease = self
                        .store
                        .get::<Lease>(&keys::lease_agent(&m.value.agent))
                        .await?;
                    let (online, host) = match lease.map(|l| l.value.state) {
                        Some(LeaseState::Held { host, .. }) => (true, Some(host)),
                        _ => (false, None),
                    };
                    out.push(json!({ "agent": m.value.agent, "joined": m.value.joined, "online": online, "host": host }));
                }
                Ok(Value::Array(out))
            }
            ToolCall::Send { .. } | ToolCall::SessionHistory { .. } => Err(ApiError::invalid(
                "send and session_history run in the host daemon",
            )),
            ToolCall::TaskCreate { channel, task } => {
                to_value(&self.task_create(&sender, &channel, task).await?)
            }
            ToolCall::TaskGet { channel, id } => to_value(&self.task(&channel, &id).await?.value),
            ToolCall::TaskList {
                channel,
                ready_only,
            } => to_value(&self.task_list(&channel, ready_only).await?),
            ToolCall::TaskUpdate { channel, id, patch } => {
                to_value(&self.task_update(&sender, &channel, &id, patch).await?)
            }
            ToolCall::TaskClaim { channel, id } => {
                let me = me.ok_or_else(|| ApiError::invalid("needs a session"))?;
                to_value(&self.task_claim(&me, &channel, &id).await?)
            }
            ToolCall::TaskClose {
                channel,
                id,
                reason,
                message,
            } => to_value(
                &self
                    .task_close(&sender, &channel, &id, reason, message)
                    .await?,
            ),
            ToolCall::SchemaGet { channel } => Ok(self.channel(&channel).await?.value.schema),
            ToolCall::SchemaExtend {
                channel,
                properties,
            } => {
                let mut ch = self.channel(&channel).await?;
                if let Some(me) = &me
                    && ch.value.owner != *me
                {
                    return Err(ApiError::invalid("only the owner can extend the schema"));
                }
                ch.value.schema = schema::extend(&ch.value.schema, &properties)?;
                self.store
                    .put(&ch.key, &ch.value, Some(ch.revision))
                    .await?;
                Ok(ch.value.schema)
            }
            ToolCall::Spawn {
                channel,
                prompt,
                harness,
                host,
                name,
                model,
            } => {
                let me = me.ok_or_else(|| ApiError::invalid("needs a session"))?;
                let id = self
                    .spawn(mem, &me, &channel, prompt, harness, host, name, model)
                    .await?;
                Ok(json!({ "agent": id }))
            }
            ToolCall::Move { selector, to } => {
                let moved = self.move_agents(mem, &selector, to).await?;
                Ok(json!({ "moving": moved }))
            }
            ToolCall::Drain { host } => {
                let moved = self.drain(mem, &host).await?;
                Ok(json!({ "draining": host, "moving": moved }))
            }
            ToolCall::ListAgents => self.list_agents().await,
            ToolCall::PruneAgents { older_than_secs } => {
                crate::prune::prune_agents(self, Duration::from_secs(older_than_secs)).await
            }
            ToolCall::ListHosts => self.list_hosts(mem).await,
            ToolCall::ProjectAdd { project } => {
                if project.remote_url.trim().is_empty() || project.default_branch.trim().is_empty()
                {
                    return Err(ApiError::invalid(
                        "a project needs a remote URL and a default branch",
                    ));
                }
                self.store
                    .upsert(&keys::project(&project.name), &project)
                    .await?;
                to_value(&project)
            }
            ToolCall::ProjectGet { name } => to_value(&self.project(&name).await?),
            ToolCall::ProjectList => {
                let projects: Vec<Project> = self
                    .store
                    .list::<Project>(keys::PROJECT_PREFIX)
                    .await?
                    .into_iter()
                    .map(|d| d.value)
                    .collect();
                to_value(&projects)
            }
        }
    }

    pub(crate) async fn verify_actor(&self, a: &Actor) -> Result<(), ApiError> {
        let lease = self
            .store
            .get::<Lease>(&keys::lease_agent(&a.agent))
            .await?
            .ok_or_else(|| {
                ApiError::new(borant_core::proto::ApiErrorCode::StaleToken, "no lease")
            })?;
        match lease.value.state {
            LeaseState::Held { session, .. }
                if session == a.session && lease.value.token == a.token =>
            {
                Ok(())
            }
            _ => Err(borant_core::CoreError::StaleToken.into()),
        }
    }

    pub(crate) async fn agent(&self, id: &AgentId) -> Result<Doc<Agent>, ApiError> {
        self.store
            .get::<Agent>(&keys::agent(id))
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no agent {id}")))
    }

    pub(crate) async fn channel(&self, name: &ChannelName) -> Result<Doc<Channel>, ApiError> {
        self.store
            .get::<Channel>(&keys::channel(name))
            .await?
            .ok_or_else(|| ApiError::not_found(format!("no channel #{name}")))
    }

    pub(crate) async fn project(&self, name: &ProjectName) -> Result<Project, ApiError> {
        self.store
            .get::<Project>(&keys::project(name))
            .await?
            .map(|d| d.value)
            .ok_or_else(|| ApiError::not_found(format!("no project {name}")))
    }

    async fn channel_create(
        &self,
        owner: &AgentId,
        name: &ChannelName,
        s: Option<Value>,
        project: Option<ProjectName>,
        branch: Option<String>,
    ) -> Result<Channel, ApiError> {
        let branch = match &project {
            Some(p) => Some(branch.unwrap_or(self.project(p).await?.default_branch)),
            None if branch.is_some() => {
                return Err(ApiError::invalid("a branch needs a project"));
            }
            None => None,
        };
        let schema = match s {
            Some(s) => {
                schema::check_schema(&s)?;
                s
            }
            None => schema::default_schema(),
        };
        let ch = Channel {
            name: name.clone(),
            owner: owner.clone(),
            schema,
            created: self.now(),
            closed: None,
            project,
            branch,
        };
        self.store
            .put(&keys::channel(name), &ch, None)
            .await
            .map_err(|e| match e.code {
                borant_core::proto::ApiErrorCode::Conflict => {
                    ApiError::new(e.code, format!("channel #{name} exists"))
                }
                _ => e,
            })?;
        self.join(owner, name).await?;
        Ok(ch)
    }

    pub(crate) async fn join(&self, agent: &AgentId, name: &ChannelName) -> Result<(), ApiError> {
        let key = keys::member(name, agent);
        if self.store.get::<Member>(&key).await?.is_none() {
            let m = Member {
                agent: agent.clone(),
                joined: self.now(),
            };
            self.store.put(&key, &m, None).await?;
        }
        let mut a = self.agent(agent).await?;
        if !a.value.channels.contains(name) {
            a.value.channels.push(name.clone());
            a.value.channels.sort();
            self.store.put(&a.key, &a.value, Some(a.revision)).await?;
        }
        self.refresh_inbox(&a.value).await
    }

    async fn leave(&self, agent: &AgentId, name: &ChannelName) -> Result<(), ApiError> {
        self.store.delete(&keys::member(name, agent)).await?;
        let mut a = self.agent(agent).await?;
        a.value.channels.retain(|c| c != name);
        self.store.put(&a.key, &a.value, Some(a.revision)).await?;
        self.refresh_inbox(&a.value).await
    }

    pub(crate) async fn refresh_inbox(&self, a: &Agent) -> Result<(), ApiError> {
        self.bus
            .ensure_inbox(&a.id, &subjects::inbox(&a.id, &a.channels))
            .await?;
        Ok(())
    }
}
