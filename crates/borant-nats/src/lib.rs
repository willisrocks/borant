//! NATS JetStream adapters for the Borant ports.
//!
//! - State store: one KV bucket, `borant`.
//! - Message bus: stream `BORANT_MSG` for `borant.agent.>` and
//!   `borant.chan.>`, one durable pull consumer `inbox-{agent}` for each
//!   agent, and core NATS for requests and host commands.
//! - Session store: stream `BORANT_SESSION`, one subject for each agent.
//! - Event log: stream `BORANT_EVENTS` for `borant.events.>`, one subject for
//!   each kind, with an ephemeral consumer for each reader.
//!
//! The adapters create the bucket and the streams on first use, so a host
//! daemon can start while the hub is unreachable.

mod bus;
mod events;
mod session;
mod state;

use std::path::PathBuf;
use std::time::Duration;

use async_nats::jetstream;
use borant_core::ports::{PortError, PortErrorKind};

pub use bus::{NatsBus, consumer_name};
pub use events::NatsEventLog;
pub use session::NatsSession;
pub use state::NatsState;

pub const KV_BUCKET: &str = "borant";
pub const MSG_STREAM: &str = "BORANT_MSG";
pub const SESSION_STREAM: &str = "BORANT_SESSION";
pub const EVENT_STREAM: &str = "BORANT_EVENTS";

#[derive(Debug, Clone, Default)]
pub struct NatsConfig {
    pub urls: Vec<String>,
    pub creds_file: Option<PathBuf>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub tls_required: bool,
    /// The connection name that the server shows.
    pub name: String,
    /// Message retention. Defaults to 30 days.
    pub max_age: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct Nats {
    client: async_nats::Client,
    js: jetstream::Context,
    max_age: Duration,
}

/// Connects in the background. The call returns at once, and port calls
/// fail with `Unavailable` until the connection is up.
pub async fn connect(cfg: &NatsConfig) -> Result<Nats, PortError> {
    let mut opts = async_nats::ConnectOptions::new()
        .name(if cfg.name.is_empty() {
            "borant"
        } else {
            &cfg.name
        })
        .retry_on_initial_connect()
        // The client drops a connection after two missed pings. With the
        // default of 60 s, a host that was cut off for a minute or two keeps
        // its dead TCP connection, and TCP retransmit backoff then delays
        // every request for up to a minute after the network returns. 5 s
        // finds a dead connection well inside the 30 s lease.
        .ping_interval(Duration::from_secs(5))
        .request_timeout(Some(Duration::from_secs(10)))
        .require_tls(cfg.tls_required);
    if let Some(path) = &cfg.creds_file {
        opts = opts
            .credentials_file(path)
            .await
            .map_err(|e| PortError::other(format!("read credentials {}: {e}", path.display())))?;
    }
    if let (Some(u), Some(p)) = (&cfg.user, &cfg.password) {
        opts = opts.user_and_password(u.clone(), p.clone());
    }
    let urls = if cfg.urls.is_empty() {
        vec!["nats://127.0.0.1:4222".to_owned()]
    } else {
        cfg.urls.clone()
    };
    let client = opts
        .connect(urls)
        .await
        .map_err(|e| PortError::unavailable(format!("connect to NATS: {e}")))?;
    let js = jetstream::new(client.clone());
    Ok(Nats {
        client,
        js,
        max_age: cfg.max_age.unwrap_or(Duration::from_secs(30 * 24 * 3600)),
    })
}

impl Nats {
    pub fn state(&self) -> NatsState {
        NatsState::new(self.client.clone(), self.js.clone())
    }

    pub fn bus(&self) -> NatsBus {
        NatsBus::new(self.client.clone(), self.js.clone(), self.max_age)
    }

    pub fn session(&self) -> NatsSession {
        NatsSession::new(self.js.clone())
    }

    pub fn events(&self) -> NatsEventLog {
        NatsEventLog::new(self.js.clone())
    }

    pub fn client(&self) -> &async_nats::Client {
        &self.client
    }
}

/// Maps a NATS error to a port error. Connection, timeout, and server
/// errors count as `Unavailable`, so callers can queue work in an outbox.
pub(crate) fn unavailable(context: &str, e: impl std::fmt::Display) -> PortError {
    let message = format!("{context}: {e}");
    let kind = if message.contains("timed out") {
        PortErrorKind::Timeout
    } else {
        PortErrorKind::Unavailable
    };
    PortError::new(kind, message)
}
