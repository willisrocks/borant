//! The store config of `[secrets.stores]` and the factory the host uses.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use borant_core::ports::secrets::SecretStore;
use borant_core::ports::{PortError, PortErrorKind, PortResult};
use serde::Deserialize;

use crate::{EnvStore, FileStore};

/// One entry of `[secrets.stores]`, tagged by `kind`.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StoreConfig {
    /// A ref is a variable name of the host process.
    Env {},
    /// A ref is a file name under `root`.
    File { root: PathBuf },
    /// An exec plugin. The exec store task fills this arm.
    Exec {
        command: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        credential: Option<String>,
        #[serde(default = "default_timeout_secs")]
        timeout_secs: u64,
    },
}

fn default_timeout_secs() -> u64 {
    10
}

/// Prints key names and the command, never a value.
impl fmt::Debug for StoreConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Env {} => f.write_str("StoreConfig::Env"),
            Self::File { root } => f.debug_tuple("StoreConfig::File").field(root).finish(),
            Self::Exec {
                command,
                env,
                credential,
                timeout_secs,
            } => f
                .debug_struct("StoreConfig::Exec")
                .field("command", command)
                .field("env_keys", &env.keys().collect::<Vec<_>>())
                .field("credential", credential)
                .field("timeout_secs", timeout_secs)
                .finish(),
        }
    }
}

/// Builds the store of one config entry. A caller that fails a store logs the
/// error and leaves out the harnesses whose bundles use it.
pub async fn open_store(
    cfg: &StoreConfig,
    credentials_dir: Option<&Path>,
) -> PortResult<Arc<dyn SecretStore>> {
    // The exec store reads the directory of the credential file.
    let _ = credentials_dir;
    match cfg {
        StoreConfig::Env {} => Ok(Arc::new(EnvStore::new())),
        StoreConfig::File { root } => Ok(Arc::new(FileStore::new(root.clone()))),
        StoreConfig::Exec { .. } => Err(PortError::new(
            PortErrorKind::Other,
            "exec store not built yet",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{StoreConfig, open_store};

    fn parse(text: &str) -> StoreConfig {
        toml::from_str(text).unwrap_or_else(|e| panic!("parse {text:?}: {e}"))
    }

    #[test]
    fn parses_the_env_kind() {
        assert!(matches!(parse(r#"kind = "env""#), StoreConfig::Env {}));
    }

    #[test]
    fn parses_the_file_kind() {
        match parse("kind = \"file\"\nroot = \"/run/borant/secrets\"") {
            StoreConfig::File { root } => {
                assert_eq!(root, std::path::Path::new("/run/borant/secrets"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_the_exec_kind_with_defaults() {
        let cfg = parse(
            r#"
kind = "exec"
command = ["borant-secrets-1password"]
credential = "store-token"
"#,
        );
        match cfg {
            StoreConfig::Exec {
                command,
                env,
                credential,
                timeout_secs,
            } => {
                assert_eq!(command, ["borant-secrets-1password"]);
                assert!(env.is_empty());
                assert_eq!(credential.as_deref(), Some("store-token"));
                assert_eq!(timeout_secs, 10);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_the_exec_kind_fully() {
        let cfg = parse(
            r#"
kind = "exec"
command = ["plugin", "--flag"]
timeout_secs = 30
[env]
OP_SERVICE_ACCOUNT_TOKEN = "must-not-be-printed"
"#,
        );
        match &cfg {
            StoreConfig::Exec {
                command,
                env,
                credential,
                timeout_secs,
            } => {
                assert_eq!(*command, ["plugin".to_string(), "--flag".to_string()]);
                assert_eq!(env["OP_SERVICE_ACCOUNT_TOKEN"], "must-not-be-printed");
                assert!(credential.is_none());
                assert_eq!(*timeout_secs, 30);
            }
            other => panic!("{other:?}"),
        }
        let printed = format!("{cfg:?}");
        assert!(printed.contains("OP_SERVICE_ACCOUNT_TOKEN"), "{printed}");
        assert!(!printed.contains("must-not-be-printed"), "{printed}");
    }

    #[test]
    fn refuses_an_unknown_kind_and_unknown_fields() {
        assert!(toml::from_str::<StoreConfig>(r#"kind = "vault""#).is_err());
        assert!(toml::from_str::<StoreConfig>("kind = \"env\"\nextra = 1").is_err());
        assert!(
            toml::from_str::<StoreConfig>("kind = \"file\"\nroot = \"/s\"\nextra = 1").is_err()
        );
        assert!(toml::from_str::<StoreConfig>("kind = \"exec\"\ncommand = []\nextra = 1").is_err());
    }

    #[tokio::test]
    async fn opens_env_and_file_and_refuses_exec() {
        let env = open_store(&StoreConfig::Env {}, None).await.unwrap();
        assert_eq!(env.kind(), "env");

        let file = open_store(
            &StoreConfig::File {
                root: std::path::PathBuf::from("/run/borant/secrets"),
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(file.kind(), "file");
        assert!(file.capabilities().resolve);

        let err = open_store(
            &StoreConfig::Exec {
                command: vec!["plugin".into()],
                env: Default::default(),
                credential: Some("store-token".into()),
                timeout_secs: 10,
            },
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, borant_core::ports::PortErrorKind::Other);
        assert_eq!(err.message, "exec store not built yet");
    }
}
