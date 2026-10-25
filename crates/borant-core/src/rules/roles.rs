//! Roles and collections (phase 1b spec, "Interfaces" and "Roles"). A role
//! names a harness, a model, a placement rule, and what it may spawn; a
//! collection names the bundles and the git author of its agents.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::ids::{BundleName, CollectionName, RoleName, Tag};
use crate::model::{Agent, GitAuthor, Harness};

/// One `[roles.<name>]` entry of `hub.toml`. Every field is optional except
/// `spawns`, which defaults to empty.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleConfig {
    /// The harness of the role, for example a harness for workers.
    pub harness: Option<Harness>,
    /// The model of the role. Another harness gets no model.
    pub model: Option<String>,
    /// Host tags that a host must have.
    #[serde(default)]
    pub require: Vec<Tag>,
    /// Host tags that make a host more suitable.
    #[serde(default)]
    pub prefer: Vec<Tag>,
    /// Host tags that make a host less suitable. A soft rule.
    #[serde(default)]
    pub avoid: Vec<Tag>,
    /// Keep the agent when the hub goes down (phase 0 pinned agents).
    #[serde(default)]
    pub pinned: bool,
    /// Only one agent of this role per collection or per project.
    pub one_per: Option<OnePer>,
    /// The roles that this role may spawn. Empty means it spawns nothing.
    #[serde(default)]
    pub spawns: Vec<RoleName>,
    /// Tools that this role may not call, for example `spawn`.
    #[serde(default)]
    pub tools_deny: Vec<String>,
    /// The skill that the brief tells the agent to load first.
    pub skill: Option<String>,
    /// Run on the host of the parent agent.
    #[serde(default)]
    pub on_parent_host: bool,
    /// Bundles that the role adds to the launch.
    #[serde(default)]
    pub bundles: Vec<BundleName>,
}

/// One `[collections.<name>]` entry of `hub.toml`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionConfig {
    /// The bundles of the collection, before the role's bundles.
    #[serde(default)]
    pub bundles: Vec<BundleName>,
    /// The author and committer of the commits in this collection.
    pub git_author: Option<GitAuthor>,
}

/// `one_per` of a role: one agent of the role in each collection or project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnePer {
    Collection,
    Project,
}

/// What a spawn asks for, before its role fills in the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpawnInput {
    /// The role that the call names.
    pub role: Option<RoleName>,
    /// The harness that the call names. It wins over the role.
    pub harness: Option<Harness>,
    /// The model that the call names. It wins over the role.
    pub model: Option<String>,
}

/// Where a role may run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    pub require: Vec<Tag>,
    pub prefer: Vec<Tag>,
    pub avoid: Vec<Tag>,
    /// Run on the host of the parent agent.
    pub on_parent_host: bool,
}

/// The child of a spawn after its role, ready for the launch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Resolved {
    pub role: Option<RoleName>,
    pub harness: Option<Harness>,
    pub model: Option<String>,
    pub placement: Placement,
    /// The collection of the parent.
    pub collection: Option<CollectionName>,
    pub pinned: bool,
    pub one_per: Option<OnePer>,
    /// The bundles that the role adds to the launch.
    pub bundles: Vec<BundleName>,
    pub skill: Option<String>,
}

/// A spawn that a role cannot explain.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoleError {
    #[error("no role {0} in hub.toml")]
    Unknown(RoleName),
}

/// The role of a child: the fields of its role, with the explicit values of
/// the call winning, and the collection of the parent.
pub fn resolve(
    spec: &SpawnInput,
    parent: Option<&Agent>,
    roles: &BTreeMap<RoleName, RoleConfig>,
) -> Result<Resolved, RoleError> {
    let role = match &spec.role {
        Some(name) => {
            let config = roles.get(name).ok_or(RoleError::Unknown(name.clone()))?;
            Some((name.clone(), config))
        }
        None => None,
    };
    let collection = parent.and_then(|p| p.collection.clone());
    let Some((name, config)) = role else {
        return Ok(Resolved {
            harness: spec.harness,
            model: spec.model.clone(),
            collection,
            ..Resolved::default()
        });
    };
    Ok(Resolved {
        role: Some(name),
        harness: spec.harness.or(config.harness),
        model: spec.model.clone().or_else(|| config.model.clone()),
        placement: Placement {
            require: config.require.clone(),
            prefer: config.prefer.clone(),
            avoid: config.avoid.clone(),
            on_parent_host: config.on_parent_host,
        },
        collection,
        pinned: config.pinned,
        one_per: config.one_per,
        bundles: config.bundles.clone(),
        skill: config.skill.clone(),
    })
}

/// True when the parent may spawn a child with this role. An agent without a
/// role may spawn anything; a role spawns only the roles in its `spawns`, so a
/// role with an empty `spawns` spawns nothing.
pub fn may_spawn(
    parent_role: Option<&RoleName>,
    child: Option<&RoleName>,
    roles: &BTreeMap<RoleName, RoleConfig>,
) -> bool {
    let Some(parent) = parent_role else {
        return true;
    };
    let Some(config) = roles.get(parent) else {
        return false;
    };
    child.is_some_and(|child| config.spawns.contains(child))
}

/// True when the role may call the tool. A role without a config, or an agent
/// without a role, may call anything that `tools_deny` of its role does not
/// name.
pub fn may_call(
    role: Option<&RoleName>,
    tool: &str,
    roles: &BTreeMap<RoleName, RoleConfig>,
) -> bool {
    let Some(config) = role.and_then(|role| roles.get(role)) else {
        return true;
    };
    !config.tools_deny.iter().any(|denied| denied == tool)
}

/// The first prompt of a child: the skill to load first, then the prompt.
pub fn brief(prompt: &str, skill: Option<&str>) -> String {
    match skill {
        Some(skill) if !skill.is_empty() => format!("First load the {skill} skill.\n\n{prompt}"),
        _ => prompt.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(text: &[(&str, RoleConfig)]) -> BTreeMap<RoleName, RoleConfig> {
        text.iter()
            .map(|(name, config)| (name.parse().unwrap(), config.clone()))
            .collect()
    }

    fn role(spawns: &[&str]) -> RoleConfig {
        RoleConfig {
            spawns: spawns.iter().map(|s| s.parse().unwrap()).collect(),
            ..RoleConfig::default()
        }
    }

    /// Rule: an agent without a role may spawn anything.
    #[test]
    fn roles_may_spawn_allows_any_child_without_a_parent_role() {
        let table = roles(&[("worker", role(&[]))]);
        assert!(may_spawn(None, None, &table));
        assert!(may_spawn(None, Some(&"worker".parse().unwrap()), &table));
    }

    /// Rule: a roled parent spawns only the roles in its spawns.
    #[test]
    fn roles_may_spawn_needs_the_child_in_spawns() {
        let table = roles(&[("chief", role(&["orchestrator"]))]);
        let chief: RoleName = "chief".parse().unwrap();
        let orchestrator: RoleName = "orchestrator".parse().unwrap();
        let worker: RoleName = "worker".parse().unwrap();
        assert!(may_spawn(Some(&chief), Some(&orchestrator), &table));
        assert!(!may_spawn(Some(&chief), Some(&worker), &table));
        assert!(!may_spawn(Some(&chief), None, &table), "no role, no spawn");
        // A role that is not in hub.toml spawns nothing.
        assert!(!may_spawn(Some(&worker), Some(&orchestrator), &table));
    }

    /// Rule: a role may not call the tools of its `tools_deny`.
    #[test]
    fn roles_may_call_denies_the_listed_tools() {
        let worker = RoleConfig {
            tools_deny: vec!["spawn".into(), "move".into()],
            ..RoleConfig::default()
        };
        let table = roles(&[("worker", worker)]);
        let worker: RoleName = "worker".parse().unwrap();
        assert!(!may_call(Some(&worker), "spawn", &table));
        assert!(!may_call(Some(&worker), "move", &table));
        assert!(may_call(Some(&worker), "task_list", &table));
        // No role, or a role that hub.toml does not define, may call anything.
        assert!(may_call(None, "spawn", &table));
        assert!(may_call(Some(&"unknown".parse().unwrap()), "spawn", &table));
    }

    /// Rule: the brief adds the skill line only when the role names a skill.
    #[test]
    fn roles_brief_adds_the_skill_line() {
        assert_eq!(brief("Fix the build.", None), "Fix the build.");
        assert_eq!(brief("Fix the build.", Some("")), "Fix the build.");
        assert_eq!(
            brief("Fix the build.", Some("borant:orchestrate")),
            "First load the borant:orchestrate skill.\n\nFix the build."
        );
    }

    /// Rule: the explicit values of the call win over the role, and the child
    /// takes the collection of its parent.
    #[test]
    fn roles_resolve_fills_the_child_from_its_role() {
        let worker = RoleConfig {
            harness: Some(Harness::Omp),
            model: Some("role-model".into()),
            require: vec!["vps".parse().unwrap()],
            avoid: vec!["laptop".parse().unwrap()],
            pinned: true,
            one_per: Some(OnePer::Project),
            spawns: vec![],
            tools_deny: vec![],
            skill: Some("borant:orchestrate".into()),
            on_parent_host: false,
            prefer: vec![],
            bundles: vec!["github-work".parse().unwrap()],
        };
        let table = roles(&[("worker", worker.clone())]);
        let parent = Agent {
            id: "boss".parse().unwrap(),
            collection: Some("work".parse().unwrap()),
            ..agent()
        };
        let all = resolve(
            &SpawnInput {
                role: Some("worker".parse().unwrap()),
                ..SpawnInput::default()
            },
            Some(&parent),
            &table,
        )
        .unwrap();
        assert_eq!(all.role, Some("worker".parse().unwrap()));
        assert_eq!(all.harness, Some(Harness::Omp));
        assert_eq!(all.model.as_deref(), Some("role-model"));
        assert_eq!(all.collection, Some("work".parse().unwrap()));
        assert!(all.pinned);
        assert_eq!(all.one_per, Some(OnePer::Project));
        assert_eq!(all.placement.require, vec!["vps".parse().unwrap()]);
        assert_eq!(all.placement.avoid, vec!["laptop".parse().unwrap()]);
        assert_eq!(all.skill.as_deref(), Some("borant:orchestrate"));
        assert_eq!(all.bundles, vec!["github-work".parse().unwrap()]);

        // The call wins over the role.
        let call = resolve(
            &SpawnInput {
                role: Some("worker".parse().unwrap()),
                harness: Some(Harness::ClaudeCode),
                model: Some("call-model".into()),
            },
            Some(&parent),
            &table,
        )
        .unwrap();
        assert_eq!(call.harness, Some(Harness::ClaudeCode));
        assert_eq!(call.model.as_deref(), Some("call-model"));

        // Without a role the call's values stand alone.
        let plain = resolve(
            &SpawnInput {
                harness: Some(Harness::Pi),
                ..SpawnInput::default()
            },
            Some(&parent),
            &table,
        )
        .unwrap();
        assert_eq!(plain.role, None);
        assert_eq!(plain.harness, Some(Harness::Pi));
        assert_eq!(plain.collection, Some("work".parse().unwrap()));
        assert!(!plain.pinned);
    }

    /// A role that hub.toml does not define is an error, not a silent default.
    #[test]
    fn roles_resolve_refuses_an_unknown_role() {
        let table = roles(&[]);
        let err = resolve(
            &SpawnInput {
                role: Some("nope".parse().unwrap()),
                ..SpawnInput::default()
            },
            None,
            &table,
        )
        .expect_err("an unknown role");
        assert_eq!(err, RoleError::Unknown("nope".parse().unwrap()));
    }

    /// A minimal agent for the tests.
    fn agent() -> Agent {
        Agent {
            id: "a1".parse().unwrap(),
            preferred_harness: Harness::Fake,
            repo: None,
            movable: false,
            channels: vec![],
            session: None,
            host: None,
            harness_session_id: None,
            created: crate::time::Timestamp(1),
            model: None,
            model_harness: None,
            project: None,
            members: BTreeMap::new(),
            pinned: false,
            role: None,
            collection: None,
            parent: None,
        }
    }
}
