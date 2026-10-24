//! A local queue for writes while the hub is unreachable (spec section 16).
//! It is a SQLite file, so it works with any broker adapter.

use std::path::Path;
use std::sync::Mutex;

use anyhow::Context;
use rusqlite::{Connection, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxKind {
    /// A message: `target` is the subject, `payload` is the envelope JSON.
    Publish,
    /// A session event: `target` is the agent ID, `payload` is the event JSON.
    Append,
}

impl OutboxKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Append => "append",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "publish" => Some(Self::Publish),
            "append" => Some(Self::Append),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxItem {
    pub id: i64,
    pub kind: OutboxKind,
    pub target: String,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
pub struct Outbox(Mutex<Connection>);

impl Outbox {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("open outbox {}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS outbox (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 kind TEXT NOT NULL,
                 target TEXT NOT NULL,
                 payload BLOB NOT NULL
             );",
        )?;
        Ok(Self(Mutex::new(conn)))
    }

    pub fn in_memory() -> anyhow::Result<Self> {
        Self::open(Path::new(":memory:"))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, kind: OutboxKind, target: &str, payload: &[u8]) -> anyhow::Result<()> {
        self.conn().execute(
            "INSERT INTO outbox (kind, target, payload) VALUES (?1, ?2, ?3)",
            params![kind.as_str(), target, payload],
        )?;
        Ok(())
    }

    /// The oldest `n` items, in order.
    pub fn peek(&self, n: usize) -> anyhow::Result<Vec<OutboxItem>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT id, kind, target, payload FROM outbox ORDER BY id LIMIT ?1")?;
        let rows = stmt.query_map(params![n as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Vec<u8>>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, kind, target, payload) = row?;
            if let Some(kind) = OutboxKind::parse(&kind) {
                out.push(OutboxItem {
                    id,
                    kind,
                    target,
                    payload,
                });
            }
        }
        Ok(out)
    }

    pub fn remove(&self, id: i64) -> anyhow::Result<()> {
        self.conn()
            .execute("DELETE FROM outbox WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn len(&self) -> anyhow::Result<usize> {
        let n: i64 = self
            .conn()
            .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))?;
        Ok(n as usize)
    }

    pub fn is_empty(&self) -> anyhow::Result<bool> {
        Ok(self.len()? == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("outbox.db");
        {
            let o = Outbox::open(&path).unwrap();
            o.push(OutboxKind::Publish, "borant.agent.a", b"1").unwrap();
            o.push(OutboxKind::Append, "a", b"2").unwrap();
        }
        let o = Outbox::open(&path).unwrap();
        let items = o.peek(10).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, OutboxKind::Publish);
        assert_eq!(items[1].payload, b"2");
        o.remove(items[0].id).unwrap();
        assert_eq!(o.len().unwrap(), 1);
    }
}
