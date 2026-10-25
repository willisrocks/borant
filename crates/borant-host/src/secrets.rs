//! Resolver mode: the host reads the values of a launch's bundles, writes them
//! into the agent's secret directory, and starts the harness through
//! `borant secret-exec`, so no value reaches the tmux arguments, `ps`, NATS,
//! or the session log. This module also holds the exact-value redactor of the
//! session log.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use borant_core::AgentId;
use borant_core::model::Harness;
use borant_core::ports::PortError;
use borant_core::ports::secrets::{Bundle, BundleName, MemberName, Secret, SecretRef};
use borant_core::proto::LaunchOrder;
use borant_core::rules::redact::redact;

use crate::{Host, HostSecrets, LocalSess};

/// The file that holds the variables, as the shim reads it.
const ENV_FILE: &str = "env";
/// The file that holds the pre commands, as the shim reads it.
const PRE_FILE: &str = "pre.json";
/// Values shorter than this are not matched: they match too much text.
const REDACT_MIN: usize = 8;
/// What the redactor puts in place of a value.
pub(crate) const REDACTED: &str = "[redacted]";

/// What one launch resolved: where the values went on disk, the names that the
/// hub and the session log may see, and the redactor of its text.
#[derive(Debug)]
pub(crate) struct LaunchSecrets {
    pub dir: PathBuf,
    pub bundles: Vec<BundleName>,
    pub members: BTreeMap<BundleName, MemberName>,
    pub redactor: Arc<Redactor>,
}

/// The exact values of one agent. It removes them first, longest first, and
/// then runs the patterns of `borant_core::rules::redact`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Redactor {
    values: Vec<String>,
}

impl Redactor {
    pub(crate) fn new(values: impl IntoIterator<Item = String>) -> Self {
        let set: BTreeSet<String> = values
            .into_iter()
            .filter(|v| v.chars().count() >= REDACT_MIN)
            .collect();
        let mut values: Vec<String> = set.into_iter().collect();
        // Longest first, so a value that holds another one leaves no tail.
        values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        Self { values }
    }

    pub(crate) fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut cleaned: Option<String> = None;
        for value in &self.values {
            let current = cleaned.as_deref().unwrap_or(text);
            if current.contains(value.as_str()) {
                cleaned = Some(current.replace(value.as_str(), REDACTED));
            }
        }
        match cleaned {
            Some(s) => Cow::Owned(redact(&s).into_owned()),
            None => redact(text),
        }
    }
}

impl Host {
    /// Resolves the bundles of one launch and writes its secret directory.
    /// `None` when the host has no `[secrets]` section or the launch harness
    /// has no bundle, so the launch runs as today.
    pub(crate) async fn issue_secrets(
        &self,
        o: &LaunchOrder,
    ) -> anyhow::Result<Option<Arc<LaunchSecrets>>> {
        let Some(secrets) = &self.inner.cfg.secrets else {
            return Ok(None);
        };
        // The harness defaults first, then the bundles of the collection and
        // the role, so a later bundle wins over an earlier one.
        let bundles = launch_bundles(secrets, o.harness, &o.bundles);
        if bundles.is_empty() {
            return Ok(None);
        }
        let planned = plan(secrets, &bundles, &o.members)?;
        let values = resolve(secrets, &planned.by_store).await?;
        let dir = self.secret_dir(&o.agent);
        write_dir(&dir, &planned, &values)?;
        let redactor = Arc::new(Redactor::new(
            values
                .values()
                .map(|s| s.expose().to_owned())
                .collect::<Vec<String>>(),
        ));
        tracing::debug!(
            agent = %o.agent,
            bundles = ?bundles,
            dir = %dir.display(),
            "secrets resolved"
        );
        Ok(Some(Arc::new(LaunchSecrets {
            dir,
            bundles,
            members: planned.members,
            redactor,
        })))
    }

    /// The folder of the secret directories of this host.
    pub(crate) fn secrets_root(&self) -> Option<PathBuf> {
        self.inner.cfg.secrets.as_ref()?;
        Some(
            self.inner
                .cfg
                .secrets_root
                .clone()
                .unwrap_or_else(default_secrets_root),
        )
    }

    /// The secret directory of one agent.
    pub(crate) fn secret_dir(&self, agent: &AgentId) -> PathBuf {
        let root = self.secrets_root().unwrap_or_else(default_secrets_root);
        root.join(agent.as_str())
    }

    /// Deletes every secret directory that no live session of this host uses,
    /// for example one whose session ended while the daemon was down. Public
    /// for tests.
    pub async fn remove_stale_secret_dirs(&self) {
        let Some(root) = self.secrets_root() else {
            return;
        };
        let live: HashSet<AgentId> = {
            let st = self.state();
            let mut live: HashSet<AgentId> = st.launched.keys().cloned().collect();
            live.extend(st.sessions.values().filter_map(LocalSess::agent));
            live
        };
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!("cannot read {:?}: {e}", root.display());
                return;
            }
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // A name that is not an agent id is not ours to delete.
            let Ok(agent) = name.parse::<AgentId>() else {
                continue;
            };
            if live.contains(&agent) {
                continue;
            }
            if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                tracing::warn!(%agent, "cannot delete the secret directory: {e}");
            }
        }
    }

    /// Drops the values of an agent whose last session ended, and deletes its
    /// directory. A directory that another live session of the agent still
    /// uses stays.
    pub(crate) async fn forget_secrets(&self, agent: &AgentId, extra: Option<PathBuf>) {
        let (issued, live) = {
            let mut st = self.state();
            let live: Vec<PathBuf> = st
                .sessions
                .values()
                .filter(|s| s.agent().as_ref() == Some(agent))
                .filter_map(|s| s.secrets_dir.clone())
                .collect();
            if live.is_empty() {
                st.redactors.remove(agent);
                (
                    st.launched.remove(agent).map(|issued| issued.dir.clone()),
                    live,
                )
            } else {
                (None, live)
            }
        };
        for dir in [issued, extra].into_iter().flatten() {
            if live.contains(&dir) {
                continue;
            }
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!(%agent, "cannot delete {:?}: {e}", dir.display());
                }
            }
        }
    }

    /// The shim in front of a harness program: `borant secret-exec <dir> --
    /// <program> <args>`. The shim is this executable.
    pub(crate) fn shim_program(
        &self,
        dir: &Path,
        program: Vec<String>,
    ) -> Result<Vec<String>, PortError> {
        let exe = std::env::current_exe()
            .map_err(|e| PortError::other(format!("cannot find the borant executable: {e}")))?;
        let mut argv = vec![
            exe.to_string_lossy().into_owned(),
            "secret-exec".to_owned(),
            dir.to_string_lossy().into_owned(),
            "--".to_owned(),
        ];
        argv.extend(program);
        Ok(argv)
    }

    /// Removes the exact values of the agent, then the patterns of
    /// `borant_core::rules::redact`. Every text of the session log passes
    /// through here.
    pub(crate) fn redact_text<'a>(&self, agent: &AgentId, text: &'a str) -> Cow<'a, str> {
        match self.state().redactors.get(agent).cloned() {
            Some(redactor) => redactor.redact(text),
            None => redact(text),
        }
    }
}

/// The root of the secret directories: the `/run` tmpfs on Linux, `$TMPDIR`
/// on macOS, where nothing is a tmpfs.
fn default_secrets_root() -> PathBuf {
    if cfg!(target_os = "macos") {
        std::env::temp_dir().join("borant/agents")
    } else {
        PathBuf::from("/run/borant/agents")
    }
}

/// The bundles of a launch: the harness defaults first, then the bundles of
/// the collection and the role, so a later bundle wins on the same variable.
/// A bundle that `host.toml` does not define is left out.
fn launch_bundles(
    secrets: &HostSecrets,
    harness: Harness,
    extra: &[BundleName],
) -> Vec<BundleName> {
    let mut out = secrets.bundles_for(harness);
    for bundle in extra {
        if !secrets.bundles.contains_key(bundle) {
            continue;
        }
        out.retain(|b| b != bundle);
        out.push(bundle.clone());
    }
    out
}

/// The values, files, and pre commands of one launch, before any store runs.
#[derive(Debug, Default)]
struct Planned {
    /// `ENV_NAME -> ref`. A later bundle wins over an earlier one.
    vars: BTreeMap<String, SecretRef>,
    files: BTreeMap<String, SecretRef>,
    pre: Vec<Vec<String>>,
    /// The bundles whose vars came from one member, and that member.
    members: BTreeMap<BundleName, MemberName>,
    /// The refs of each store, without duplicates: one `resolve` each.
    by_store: BTreeMap<String, Vec<SecretRef>>,
}

/// The bundles of a harness with the member vars of a member bundle on top.
fn plan(
    secrets: &HostSecrets,
    bundles: &[BundleName],
    order: &BTreeMap<BundleName, MemberName>,
) -> Result<Planned, PortError> {
    let mut planned = Planned::default();
    for name in bundles {
        let bundle = secrets.bundles.get(name).ok_or_else(|| {
            PortError::invalid(format!("the bundle {name} is not defined in host.toml"))
        })?;
        let mut vars = bundle.vars.clone();
        if let Some(member) = member_for(bundle, name, order)? {
            vars.extend(bundle.members[&member].clone());
            planned.members.insert(name.clone(), member);
        }
        let mut refs: Vec<SecretRef> = vars.values().cloned().collect();
        refs.extend(bundle.files.values().cloned());
        let asked = planned.by_store.entry(bundle.store.clone()).or_default();
        for r in refs {
            if !asked.contains(&r) {
                asked.push(r);
            }
        }
        planned.vars.extend(vars);
        planned.files.extend(bundle.files.clone());
        planned.pre.extend(bundle.pre.clone());
    }
    Ok(planned)
}

/// The member whose vars a bundle uses. A bundle with one member needs no
/// pick; a bundle with several needs the pick of the launch order.
fn member_for(
    bundle: &Bundle,
    name: &BundleName,
    order: &BTreeMap<BundleName, MemberName>,
) -> Result<Option<MemberName>, PortError> {
    match bundle.members.len() {
        0 => Ok(None),
        1 => Ok(bundle.members.keys().next().cloned()),
        _ => {
            let member = order.get(name).ok_or_else(|| {
                PortError::invalid(format!(
                    "the member bundle {name} has no member in the launch order"
                ))
            })?;
            if !bundle.members.contains_key(member) {
                return Err(PortError::invalid(format!(
                    "the member {member} of the bundle {name} is not defined in host.toml"
                )));
            }
            Ok(Some(member.clone()))
        }
    }
}

/// One `resolve` for each store, with all of its refs.
async fn resolve(
    secrets: &HostSecrets,
    by_store: &BTreeMap<String, Vec<SecretRef>>,
) -> Result<BTreeMap<SecretRef, Secret>, PortError> {
    let mut values = BTreeMap::new();
    for (store, refs) in by_store {
        let open = secrets
            .stores
            .get(store)
            .ok_or_else(|| PortError::invalid(format!("the secret store {store} is not open")))?;
        let resolved = open.resolve(refs).await?;
        for (r, value) in resolved.values {
            values.insert(r, value);
        }
    }
    for r in by_store.values().flatten() {
        if !values.contains_key(r) {
            return Err(PortError::not_found(format!(
                "no value was resolved for the ref {:?}",
                r.0
            )));
        }
    }
    Ok(values)
}

/// Writes the directory that the shim reads: `env` (0600), `pre.json` when
/// there are pre commands, and one file for each `files` entry (0600).
fn write_dir(
    dir: &Path,
    planned: &Planned,
    values: &BTreeMap<SecretRef, Secret>,
) -> Result<(), PortError> {
    // A directory of a launch that never registered holds stale values.
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(PortError::other(format!(
                "cannot clear {:?}: {e}",
                dir.display()
            )));
        }
    }
    let root = dir.parent().unwrap_or(dir);
    create_private(root)?;
    create_private(dir)?;
    // A directory that already existed keeps its own mode.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        PortError::other(format!("cannot set the mode of {:?}: {e}", dir.display()))
    })?;

    let mut env = String::new();
    for (name, r) in &planned.vars {
        let value = value_of(values, r)?;
        if value.contains(['\n', '\r']) {
            return Err(PortError::invalid(format!(
                "the value of {name} holds a newline, which the env file cannot carry"
            )));
        }
        env.push_str(name);
        env.push('=');
        env.push_str(value);
        env.push('\n');
    }
    write_private(&dir.join(ENV_FILE), env.as_bytes())?;

    if !planned.pre.is_empty() {
        let pre = serde_json::to_vec(&planned.pre)
            .map_err(|e| PortError::other(format!("cannot encode the pre commands: {e}")))?;
        write_private(&dir.join(PRE_FILE), &pre)?;
    }
    for (name, r) in &planned.files {
        let only = plain_name(name)?;
        write_private(&dir.join(only), value_of(values, r)?.as_bytes())?;
    }
    Ok(())
}

/// The value of a ref. The error names the ref, never a value.
fn value_of<'a>(
    values: &'a BTreeMap<SecretRef, Secret>,
    r: &SecretRef,
) -> Result<&'a str, PortError> {
    values
        .get(r)
        .map(Secret::expose)
        .ok_or_else(|| PortError::not_found(format!("no value was resolved for the ref {:?}", r.0)))
}

/// A `files` key is one file name in the secret directory, never a path.
fn plain_name(name: &str) -> Result<&std::ffi::OsStr, PortError> {
    let mut parts = Path::new(name).components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(only)), None) => Ok(only),
        _ => Err(PortError::invalid(format!(
            "the file {name:?} is not a file name in the secret directory"
        ))),
    }
}

fn create_private(dir: &Path) -> Result<(), PortError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| PortError::other(format!("cannot create {:?}: {e}", dir.display())))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), PortError> {
    use std::io::Write;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| PortError::other(format!("cannot create {:?}: {e}", path.display())))?;
    file.write_all(bytes)
        .map_err(|e| PortError::other(format!("cannot write {:?}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, Redactor};

    #[test]
    fn the_redactor_removes_an_exact_value() {
        let r = Redactor::new(["gh-value-123456".to_owned()]);
        let got = r.redact("the token is gh-value-123456, keep it");
        assert!(!got.contains("gh-value-123456"), "{got}");
        assert!(got.contains(REDACTED), "{got}");
    }

    #[test]
    fn the_redactor_ignores_a_value_shorter_than_eight_characters() {
        let r = Redactor::new(["abc1234".to_owned()]);
        assert_eq!(r.redact("abc1234"), "abc1234");
        let r = Redactor::new(["abc12345".to_owned()]);
        assert_eq!(r.redact("x abc12345 y"), "x [redacted] y");
    }

    #[test]
    fn the_redactor_removes_the_longer_value_first() {
        // The shorter value is a suffix of the longer one: a shorter-first
        // order would leave its head behind.
        let r = Redactor::new(["value-one-long".to_owned(), "long".to_owned()]);
        let got = r.redact("value-one-long");
        assert_eq!(got, REDACTED);
    }

    #[test]
    fn the_redactor_still_runs_the_patterns() {
        let r = Redactor::new(Vec::new());
        let got = r.redact("key sk-ABCDEFGHIJKLMNOPQRSTUV");
        assert!(got.contains("[redacted]"), "{got}");
        let r = Redactor::new(["gh-value-123456".to_owned()]);
        let got = r.redact("gh-value-123456 and sk-ABCDEFGHIJKLMNOPQRSTUV");
        assert!(!got.contains("gh-value-123456"), "{got}");
        assert!(!got.contains("sk-ABCDEF"), "{got}");
    }

    #[test]
    fn the_redactor_keeps_borrowed_text_when_nothing_matches() {
        let r = Redactor::new(["gh-value-123456".to_owned()]);
        assert!(matches!(
            r.redact("plain text"),
            std::borrow::Cow::Borrowed(_)
        ));
    }
}
