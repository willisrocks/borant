//! The file store: a ref is a file name under `root`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use borant_core::ports::secrets::{Resolved, Secret, SecretCapabilities, SecretRef, SecretStore};
use borant_core::ports::{PortError, PortResult};

/// Reads one file for each ref. A ref must be a single file name: it may not
/// be absolute, may not hold a `..` or a separator, and so it stays under the
/// root. One trailing newline is trimmed, so an editor-written file and a
/// `printf` file give the same value.
#[derive(Debug, Clone)]
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path_for(&self, r: &SecretRef) -> PortResult<PathBuf> {
        let name = Path::new(&r.0);
        let mut parts = name.components();
        match (parts.next(), parts.next()) {
            (Some(Component::Normal(only)), None) => Ok(self.root.join(only)),
            _ => Err(self.outside(r)),
        }
    }

    fn outside(&self, r: &SecretRef) -> PortError {
        PortError::other(format!(
            "file ref {:?} is not a file name under {:?}",
            r.0, self.root
        ))
    }

    fn unreadable(&self, r: &SecretRef, e: std::io::Error) -> PortError {
        if e.kind() == std::io::ErrorKind::NotFound {
            PortError::not_found(format!(
                "file ref {:?} is missing under {:?}",
                r.0, self.root
            ))
        } else {
            PortError::other(format!("file ref {:?} cannot be read: {e}", r.0))
        }
    }
}

/// Trims one trailing newline, so a `\r\n` file loses both characters.
fn trim_one_newline(mut text: String) -> String {
    if text.ends_with('\n') {
        text.pop();
        if text.ends_with('\r') {
            text.pop();
        }
    }
    text
}

#[async_trait]
impl SecretStore for FileStore {
    fn kind(&self) -> &'static str {
        "file"
    }

    fn capabilities(&self) -> SecretCapabilities {
        SecretCapabilities { resolve: true }
    }

    async fn resolve(&self, refs: &[SecretRef]) -> PortResult<Resolved> {
        let mut values = BTreeMap::new();
        for r in refs {
            if values.contains_key(r) {
                continue;
            }
            let path = self.path_for(r)?;
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| self.unreadable(r, e))?;
            let text = String::from_utf8(bytes)
                .map_err(|_| PortError::other(format!("file ref {:?} is not UTF-8", r.0)))?;
            values.insert(r.clone(), Secret::new(trim_one_newline(text)));
        }
        Ok(Resolved { values })
    }
}

#[cfg(test)]
mod tests {
    use super::trim_one_newline;

    #[test]
    fn trims_exactly_one_trailing_newline() {
        assert_eq!(trim_one_newline("v".into()), "v");
        assert_eq!(trim_one_newline("v\n".into()), "v");
        assert_eq!(trim_one_newline("v\r\n".into()), "v");
        assert_eq!(trim_one_newline("v\n\n".into()), "v\n");
        assert_eq!(trim_one_newline("v\nv".into()), "v\nv");
        assert_eq!(trim_one_newline("\n".into()), "");
        assert_eq!(trim_one_newline("".into()), "");
    }
}
