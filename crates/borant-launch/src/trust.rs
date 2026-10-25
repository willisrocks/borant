//! Claude Code's folder-trust record, for the directory of a launch.
//!
//! Claude Code asks "Is this a project you created or one you trust?" for a
//! directory that it does not know, and a git repository root is never covered
//! by the trust of its parent. A launch that sits at that prompt never
//! registers, so the host marks the launch directory as trusted in the file
//! that Claude Code keeps before it starts the harness.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

/// The key of the file that says a directory is trusted.
const TRUST_KEY: &str = "hasTrustDialogAccepted";

/// Serializes the merges of this process, so two launches that start at once
/// do not lose each other's entry. The rename keeps the file valid for other
/// processes.
static MERGE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The count in the name of the temporary file, so two merges that follow one
/// another closely in this process cannot share a name.
static TEMP: AtomicU64 = AtomicU64::new(0);

/// The file where Claude Code keeps its per-project settings:
/// `$CLAUDE_CONFIG_DIR/.claude.json` when the variable names a directory, and
/// `~/.claude.json` otherwise. `None` when neither is known.
pub fn config_path(config_dir: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    match (config_dir, home) {
        (Some(dir), _) => Some(dir.join(".claude.json")),
        (None, Some(home)) => Some(home.join(".claude.json")),
        (None, None) => None,
    }
}

/// [`config_path`] from the environment of this process. An empty variable is
/// the same as one that is not set.
pub fn config_path_from_env() -> Option<PathBuf> {
    let var = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    config_path(var("CLAUDE_CONFIG_DIR").as_deref(), var("HOME").as_deref())
}

/// Marks the launch directory `cwd` as trusted for Claude Code, in the file
/// that [`config_path_from_env`] names.
///
/// A failure leaves the launch to the prompt, which is then the visible
/// failure; the caller logs it and starts the harness anyway.
pub fn trust_launch_folder(cwd: &Path) -> io::Result<()> {
    let file = config_path_from_env().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "neither CLAUDE_CONFIG_DIR nor HOME is set, so there is no .claude.json to mark",
        )
    })?;
    trust_folder(&file, cwd)
}

/// Sets `projects[cwd].hasTrustDialogAccepted = true` in `file`, and keeps
/// every other key of the file and of the entry.
///
/// The file is created with just the key when it is missing, and written with
/// a temporary file and a rename in the same directory, with mode 0600. A file
/// that is not a JSON object fails instead of being replaced: it belongs to a
/// person, and Claude Code reads it too.
///
/// `cwd` is the resolved path of the launch directory, the one that Claude
/// Code sees as its working directory.
pub fn trust_folder(file: &Path, cwd: &Path) -> io::Result<()> {
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let key = cwd.to_string_lossy().into_owned();
    let _merge = MERGE.lock().unwrap_or_else(|e| e.into_inner());
    let mut root = match fs::read(file) {
        Ok(bytes) => {
            let value: Value = serde_json::from_slice(&bytes).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is not JSON: {e}", file.display()),
                )
            })?;
            if !value.is_object() {
                return Err(not_an_object(file));
            }
            value
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Value::Object(Map::new()),
        Err(e) => return Err(e),
    };
    let map = root.as_object_mut().ok_or_else(|| not_an_object(file))?;
    let projects = entry(map, file, "projects")?;
    let project = entry(projects, file, &key)?;
    project.insert(TRUST_KEY.to_owned(), Value::Bool(true));
    write_atomic(file, &root)
}

/// The object under `key` of `map`, created empty when it is missing. Any
/// other value there fails, because a merge cannot keep it.
fn entry<'a>(
    map: &'a mut Map<String, Value>,
    file: &Path,
    key: &str,
) -> io::Result<&'a mut Map<String, Value>> {
    let value = map
        .entry(key.to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    value.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{key} in {} is not an object", file.display()),
        )
    })
}

fn not_an_object(file: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} is not a JSON object", file.display()),
    )
}

/// Replaces `file` with `value` in one rename, so a reader sees the old file
/// or the new one and never half of a merge. The new file is mode 0600: it may
/// hold tokens of the harness.
fn write_atomic(file: &Path, value: &Value) -> io::Result<()> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let name = format!(
        ".{}.{}.{}.tmp",
        file.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude.json".to_owned()),
        std::process::id(),
        TEMP.fetch_add(1, Ordering::Relaxed),
    );
    let temp = dir.join(name);
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    bytes.push(b'\n');
    let result = (|| {
        let mut out = create_private(&temp)?;
        out.write_all(&bytes)?;
        out.sync_all()?;
        drop(out);
        fs::rename(&temp, file)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// A new file that only its owner reads and writes.
fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read(file: &Path) -> Value {
        serde_json::from_slice(&fs::read(file).unwrap()).unwrap()
    }

    /// A JSON object from `pairs`, for an expected value whose keys are not
    /// known when the code is written.
    fn object(pairs: &[(&str, Value)]) -> Value {
        let map: Map<String, Value> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect();
        Value::Object(map)
    }

    fn trusted() -> Value {
        object(&[(TRUST_KEY, json!(true))])
    }

    fn write_json(file: &Path, value: &Value) {
        fs::write(file, serde_json::to_vec(value).unwrap()).unwrap();
    }

    #[test]
    fn a_missing_file_gets_the_trust_key() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        let key = cwd.to_str().unwrap();

        trust_folder(&file, &cwd).unwrap();

        assert_eq!(
            read(&file),
            object(&[("projects", object(&[(key, trusted())]))])
        );
    }

    #[test]
    fn a_file_without_projects_keeps_its_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        let key = cwd.to_str().unwrap();
        write_json(&file, &json!({"numStartups": 4, "env": {"FOO": "bar"}}));

        trust_folder(&file, &cwd).unwrap();

        assert_eq!(
            read(&file),
            json!({
                "numStartups": 4,
                "env": {"FOO": "bar"},
                "projects": object(&[(key, trusted())]),
            })
        );
    }

    #[test]
    fn an_existing_project_keeps_its_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        let key = cwd.to_str().unwrap();
        write_json(
            &file,
            &json!({"projects": {(key): {"allowedTools": ["Bash"], "history": [{"display": "hi"}]}}}),
        );

        trust_folder(&file, &cwd).unwrap();

        assert_eq!(
            read(&file)["projects"][key],
            json!({
                "allowedTools": ["Bash"],
                "history": [{"display": "hi"}],
                (TRUST_KEY): true,
            })
        );
    }

    #[test]
    fn other_projects_are_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        let other = dir.path().join("elsewhere");
        let key = cwd.to_str().unwrap();
        let other_key = other.to_str().unwrap();
        let before = json!({
            "projects": {
                (other_key): {"hasTrustDialogAccepted": false, "history": [1]},
            },
            "oauthAccount": {"emailAddress": "someone@example.com"},
        });
        write_json(&file, &before);

        trust_folder(&file, &cwd).unwrap();

        let value = read(&file);
        assert_eq!(value["projects"][other_key], before["projects"][other_key]);
        assert_eq!(value["oauthAccount"], before["oauthAccount"]);
        assert_eq!(value["projects"][key], trusted());
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        fs::write(&file, br#"{"projects": {}}"#).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

        trust_folder(&file, &cwd).unwrap();

        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_launch_dir_is_recorded_as_claude_code_sees_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        trust_folder(&file, &link).unwrap();

        let projects = read(&file)["projects"].clone();
        assert_eq!(projects[real.to_str().unwrap()], trusted());
        assert!(projects.get(link.to_str().unwrap()).is_none());
    }

    #[test]
    fn a_file_that_is_not_json_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        fs::write(&file, b"{\"projects\": [").unwrap();

        assert!(trust_folder(&file, &cwd).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"{\"projects\": [");
    }

    #[test]
    fn a_projects_value_that_is_not_an_object_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let cwd = dir.path().join("work");
        fs::create_dir(&cwd).unwrap();
        fs::write(&file, br#"{"projects": []}"#).unwrap();

        assert!(trust_folder(&file, &cwd).is_err());
        assert_eq!(fs::read(&file).unwrap(), br#"{"projects": []}"#);
    }

    #[test]
    fn a_second_merge_keeps_the_first_project() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".claude.json");
        let one = dir.path().join("one");
        let two = dir.path().join("two");
        fs::create_dir(&one).unwrap();
        fs::create_dir(&two).unwrap();

        trust_folder(&file, &one).unwrap();
        trust_folder(&file, &two).unwrap();

        let projects = read(&file)["projects"].clone();
        assert_eq!(projects[one.to_str().unwrap()], trusted());
        assert_eq!(projects[two.to_str().unwrap()], trusted());
    }

    #[test]
    fn the_config_file_follows_claude_config_dir_and_home() {
        let home = Path::new("/home/a");
        let config = Path::new("/tmp/cfg");
        assert_eq!(
            config_path(Some(config), Some(home)),
            Some(PathBuf::from("/tmp/cfg/.claude.json"))
        );
        assert_eq!(
            config_path(None, Some(home)),
            Some(PathBuf::from("/home/a/.claude.json"))
        );
        assert_eq!(config_path(None, None), None);
    }
}
