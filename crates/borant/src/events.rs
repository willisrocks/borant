//! `borant events`: the swarm event stream, read from the local host daemon.
//!
//! The host serves `GET /v1/events` on `host.sock` as server-sent events: one
//! JSON `SwarmEvent` in each `data:` line, a blank line after each event, and
//! keep-alive comments in between (phase 2 spec, "borant events"). This module
//! builds the query from the flags, reads the stream over the Unix socket, and
//! prints one line per event in plain text, or the event JSON as it arrived
//! with `--json`.
//!
//! The text line is `HH:MM:SS severity kind subject detail`, in UTC, so the
//! output is the same on every host and in a test. The stream contract says a
//! reader ignores unknown kinds, so text mode skips an event that this build
//! cannot read; `--json` prints it as it arrived instead, so a newer host
//! stays readable.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use borant_core::events::{Severity, SwarmEvent, SwarmEventBody, kind_of};
use borant_core::{AgentId, ChannelName, Clock, HostName, SystemClock, Timestamp};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// How long a connection to the local daemon may take.
const CONNECT: Duration = Duration::from_secs(2);

/// The flags of `borant events`, once parsed.
#[derive(Debug, Clone, Default)]
pub struct Args {
    /// Wait for events published from now on, instead of reading and ending.
    pub follow: bool,
    /// Only events at or after this time.
    pub since: Option<Timestamp>,
    /// Only these kinds; an entry may end in `.*`.
    pub kinds: Vec<String>,
    pub agent: Option<AgentId>,
    pub channel: Option<ChannelName>,
    pub host: Option<HostName>,
    /// Only this severity and worse.
    pub min_severity: Option<Severity>,
    /// Print each event's JSON, as it arrived, instead of a text line.
    pub json: bool,
}

impl Args {
    /// The query of `GET /v1/events`, without the leading `?`.
    ///
    /// The host route reads `since` in Unix milliseconds, `follow=1`,
    /// `kinds` as one comma-separated value, one each of `agent`, `channel`,
    /// and `host`, and `min_severity`. It writes one JSON event per `data:`
    /// line and closes the stream when `follow` is absent.
    pub fn query(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(since) = self.since {
            parts.push(format!("since={}", since.0));
        }
        if self.follow {
            parts.push("follow=1".to_owned());
        }
        if !self.kinds.is_empty() {
            let kinds: Vec<String> = self.kinds.iter().map(|k| encode(k)).collect();
            parts.push(format!("kinds={}", kinds.join(",")));
        }
        if let Some(agent) = &self.agent {
            parts.push(format!("agent={}", encode(agent.as_str())));
        }
        if let Some(channel) = &self.channel {
            parts.push(format!("channel={}", encode(channel.as_str())));
        }
        if let Some(host) = &self.host {
            parts.push(format!("host={}", encode(host.as_str())));
        }
        if let Some(severity) = self.min_severity {
            parts.push(format!("min_severity={}", word(&severity)));
        }
        parts.join("&")
    }
}

/// Percent-encodes everything outside the characters that a query allows.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'*' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The `--severity` value: `info`, `warn`, or `error`.
pub fn severity_arg(text: &str) -> Result<Severity, String> {
    match text {
        "info" => Ok(Severity::Info),
        "warn" => Ok(Severity::Warn),
        "error" => Ok(Severity::Error),
        other => Err(format!("{other} is not info, warn, or error")),
    }
}

/// The current time, for [`parse_since`].
pub fn now() -> Timestamp {
    SystemClock.now()
}

/// Parses `--since`: a duration before `now` (`30s`, `5m`, `2h`, `3d`, `1w`),
/// or an RFC 3339 time such as `2026-10-25T09:00:00Z`.
pub fn parse_since(text: &str, now: Timestamp) -> anyhow::Result<Timestamp> {
    if let Some(duration) = parse_duration(text) {
        return Ok(now.minus(duration));
    }
    crate::cli::rfc3339(text).map_err(|_| {
        anyhow!("{text} is not a duration such as 30m or 2d, or an RFC 3339 time such as 2026-10-25T09:00:00Z")
    })
}

/// A whole count of seconds, minutes, hours, days, or weeks.
pub(crate) fn parse_duration(text: &str) -> Option<Duration> {
    let (count, unit) = text.split_at(text.len().checked_sub(1)?);
    let count: u64 = count.parse().ok()?;
    let secs = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        "w" => 7 * 24 * 60 * 60,
        _ => return None,
    };
    Some(Duration::from_secs(count.checked_mul(secs)?))
}

/// Reads the event stream and prints each event to standard output.
pub async fn run(socket: &Path, args: &Args) -> anyhow::Result<()> {
    let stdout = std::io::stdout();
    stream(socket, args, &mut stdout.lock()).await
}

/// Reads the event stream and prints each event to `out`. Split from [`run`]
/// so that a test can read the output.
pub async fn stream(
    socket: &Path,
    args: &Args,
    out: &mut (impl Write + ?Sized),
) -> anyhow::Result<()> {
    let mut events = EventStream::open(socket, &args.query()).await?;
    while let Some(data) = events.next_event().await? {
        if args.json {
            writeln!(out, "{data}")?;
        } else if let Ok(event) = serde_json::from_str::<SwarmEvent>(&data) {
            writeln!(out, "{}", render(&event))?;
        }
        out.flush()?;
    }
    Ok(())
}

/// One line for people: the UTC time, severity, kind, subject, and detail,
/// then the event's note in parentheses when it has one.
pub fn render(event: &SwarmEvent) -> String {
    let subject = event
        .agent
        .as_ref()
        .map(ToString::to_string)
        .or_else(|| event.host.as_ref().map(ToString::to_string))
        .or_else(|| event.channel.as_ref().map(ToString::to_string))
        .unwrap_or_else(|| "-".to_owned());
    let mut line = format!(
        "{} {:<5} {:<22} {:<14} {}",
        utc_time(event.at),
        word(&event.severity),
        kind_of(event),
        subject,
        detail(event),
    );
    if let Some(note) = &event.note {
        line.push_str(&format!(" ({note})"));
    }
    line.trim_end().to_owned()
}

/// The typed part of an event, as one readable phrase.
fn detail(event: &SwarmEvent) -> String {
    use SwarmEventBody as B;
    match &event.body {
        B::HubStarted {
            instance,
            token,
            quiet_secs,
        } => format!("{instance} token {} quiet {quiet_secs}s", token),
        B::HubSteppedDown { instance, token } => format!("{instance} token {}", token),
        B::HubMaintenance { instance, on } => format!("{instance} on={on}"),
        B::HostRegistered {
            last_seen,
            always_on,
            agents,
        }
        | B::HostOnline {
            last_seen,
            always_on,
            agents,
        }
        | B::HostOffline {
            last_seen,
            always_on,
            agents,
        } => format!(
            "{agents} agents always_on={always_on} last seen {}",
            utc_time(*last_seen)
        ),
        B::HostDraining { agents } => format!("{agents} agents"),
        B::HubUnreachable { since } => format!("since {}", utc_time(*since)),
        B::LeaseAcquired { session, token }
        | B::LeaseReleased { session, token }
        | B::LeaseReclaimed { session, token }
        | B::LeaseExpired { session, token } => format!("session {session} token {}", token.0),
        B::LeaseReserved { reason, until } | B::LeaseReservationEnded { reason, until } => {
            format!("{} until {}", word(reason), utc_time(*until))
        }
        B::SessionStarted { session, harness } => {
            format!("session {session} {}", word(harness))
        }
        B::SessionEnded {
            session,
            harness,
            reason,
        } => format!("session {session} {} ({reason})", word(harness)),
        B::SessionPaused { why } | B::SessionFenced { why } => why.clone(),
        B::SessionResumed {} => String::new(),
        B::AgentSpawned { by, harness } => format!("by {by} {}", word(harness)),
        B::SpawnQueued { key, priority } => match key {
            Some(key) => format!("key {key}, priority {priority}"),
            None => format!("priority {priority}"),
        },
        B::SpawnCancelled => String::new(),
        B::AgentRetired => String::new(),
        B::LaunchStuck { attempts, screen } => {
            let last = screen.lines().last().unwrap_or_default();
            format!("try {attempts}: {last}")
        }
        B::AgentPruned { by, idle_days } => match by {
            Some(by) => format!("idle {idle_days}d by {by}"),
            None => format!("idle {idle_days}d"),
        },
        B::AgentParked { session, git_ref } => match git_ref {
            Some(git_ref) => format!("session {session} at {git_ref}"),
            None => format!("session {session}"),
        },
        B::AgentPaused { by, why } => format!("by {by}: {why}"),
        B::AgentResumed => String::new(),
        B::ChangeUpdated {
            state,
            head,
            checks,
            review,
        } => format!(
            "{} at {} checks={} review={}",
            word(state),
            head,
            checks_word(checks),
            word(review)
        ),
        B::PlacementChosen { harness, reason } => {
            let host = event.host.as_ref().map(ToString::to_string);
            format!(
                "{} ({}, {})",
                host.as_deref().unwrap_or("-"),
                word(reason),
                word(harness)
            )
        }
        B::PlacementRefused { reason, rejected } => {
            format!("{} ({} rejected)", word(reason), rejected.len())
        }
        B::MoveRequested {
            from_host,
            to_host,
            reason,
        }
        | B::MoveLaunched {
            from_host,
            to_host,
            reason,
        }
        | B::MoveReady {
            from_host,
            to_host,
            reason,
        } => format!("{} -> {} ({})", from(from_host), to_host, word(reason)),
        B::MoveFailed {
            from_host,
            to_host,
            reason,
            error,
        } => format!(
            "{} -> {} ({}): {error}",
            from(from_host),
            to_host,
            word(reason)
        ),
        B::MoveStaleSaved { git_ref } => format!("saved {git_ref}"),
        B::ClaimsReleased { tasks, after_secs } => {
            format!("{tasks} tasks after {after_secs}s")
        }
        B::TaskStatus {
            from: was,
            to,
            claimed_by,
            attention_msg,
        } => {
            let mut detail = format!("{} -> {}", word(was), word(to));
            if let Some(by) = claimed_by {
                detail.push_str(&format!(" by {by}"));
            }
            if let Some(msg) = attention_msg {
                detail.push_str(&format!(" ({msg})"));
            }
            detail
        }
        B::DeliveryAckFailed {
            message,
            error,
            count,
        }
        | B::DeliveryAppendFailed {
            message,
            error,
            count,
        } => format!("{message}: {error} x{count}/min"),
        B::DeliveryRedelivered {
            message,
            deliveries,
            count,
        } => format!("{message} deliveries {deliveries} x{count}/min"),
        B::DeliveryWakeLimited { message, count } => format!("{message} x{count}/min"),
        B::DeliveryBadEnvelope { error, count } => format!("{error} x{count}/min"),
        B::MemberLimited {
            bundle,
            member,
            until,
        } => match until {
            Some(until) => format!("{bundle}/{member} until {}", utc_time(*until)),
            None => format!("{bundle}/{member}"),
        },
        B::MemberReset { bundle, member } => format!("{bundle}/{member}"),
        B::ElasticCreated { pool } | B::ElasticDeleted { pool } => format!("pool {pool}"),
        B::AlertFired { rule, subject } | B::AlertResolved { rule, subject } => {
            format!("{rule} {subject}")
        }
    }
}

/// A missing host, as a move line shows it.
fn from(host: &Option<HostName>) -> String {
    host.as_ref().map(ToString::to_string).unwrap_or("-".into())
}

/// The `HH:MM:SS` of a Unix time, in UTC.
pub(crate) fn utc_time(at: Timestamp) -> String {
    let second_of_day = at.0.div_euclid(1000).rem_euclid(24 * 60 * 60);
    format!(
        "{:02}:{:02}:{:02}",
        second_of_day / 3600,
        second_of_day % 3600 / 60,
        second_of_day % 60
    )
}

/// A serde name, such as `takeover` for a `MoveReason`. The events carry
/// typed values, and the wire names are the short ones that a reader can
/// grep.
fn word<T: serde::Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

/// The checks of a change, as one phrase: `pass`, `fail: ci, lint`,
/// `pending`, or `none`.
fn checks_word(checks: &borant_core::ports::CheckState) -> String {
    use borant_core::ports::CheckState;
    match checks {
        CheckState::Pass {} => "pass".to_owned(),
        CheckState::Fail { failed } => format!("fail: {}", failed.join(", ")),
        CheckState::Pending {} => "pending".to_owned(),
        CheckState::None {} => "none".to_owned(),
    }
}

/// How the body of an HTTP reply is framed.
#[derive(Debug, Clone, Copy)]
enum Framing {
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// `Content-Length: n`.
    Length(u64),
    /// Until the connection closes.
    Close,
}

/// The server-sent event stream of `GET /v1/events`.
struct EventStream {
    lines: Lines,
    /// The `data:` lines of the event that is being collected.
    data: Vec<String>,
}

impl EventStream {
    /// Connects, writes the request, and reads the response head.
    async fn open(socket: &Path, query: &str) -> anyhow::Result<Self> {
        let connect = tokio::time::timeout(CONNECT, UnixStream::connect(socket)).await;
        let conn = match connect {
            Err(_) => bail!("timed out connecting to {}", socket.display()),
            Ok(Err(e)) => {
                bail!("the Borant host daemon is not running ({e}). Start it with `borant host`.")
            }
            Ok(Ok(conn)) => conn,
        };
        let mut reader = BufReader::new(conn);
        let request = format!(
            "GET /v1/events?{query} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Accept: text/event-stream\r\n\
             Connection: close\r\n\r\n"
        );
        reader
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .context("cannot send the events request")?;
        reader
            .get_mut()
            .flush()
            .await
            .context("cannot send the events request")?;
        let (status, framing) = read_head(&mut reader).await?;
        if status != 200 {
            let mut body = Vec::new();
            tokio::io::AsyncReadExt::take(&mut reader, 64 * 1024)
                .read_to_end(&mut body)
                .await
                .ok();
            let body = String::from_utf8_lossy(&body);
            bail!(
                "the host refused the events request ({status}): {}",
                body.trim()
            );
        }
        Ok(Self {
            lines: Lines::new(Body::new(reader, framing)),
            data: Vec::new(),
        })
    }

    /// The next event's JSON, or `None` when the stream ended. A stream that
    /// ends without the final blank line still yields its last event.
    async fn next_event(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            match self.lines.line().await? {
                None => {
                    if self.data.is_empty() {
                        return Ok(None);
                    }
                    return Ok(Some(std::mem::take(&mut self.data).join("\n")));
                }
                Some(line) if line.is_empty() => {
                    if !self.data.is_empty() {
                        return Ok(Some(std::mem::take(&mut self.data).join("\n")));
                    }
                }
                Some(line) => {
                    if line.starts_with(':') {
                        // A comment, such as the keep-alive.
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        self.data
                            .push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
                    }
                    // `event:`, `id:`, and `retry:` carry nothing here.
                }
            }
        }
    }
}

/// Reads the status line and the headers of the reply.
async fn read_head(reader: &mut BufReader<UnixStream>) -> anyhow::Result<(u16, Framing)> {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .context("cannot read the reply of the host")?;
    if status_line.is_empty() {
        bail!("the host closed the connection without a reply");
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .with_context(|| format!("bad status line {status_line:?}"))?;
    let mut chunked = false;
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            bail!("the host closed the connection inside the headers");
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "transfer-encoding" if value.to_ascii_lowercase().contains("chunked") => {
                    chunked = true;
                }
                "content-length" => {
                    length = Some(
                        value
                            .parse()
                            .with_context(|| format!("bad content length {value:?}"))?,
                    );
                }
                _ => {}
            }
        }
    }
    let framing = if chunked {
        Framing::Chunked
    } else {
        match length {
            Some(length) => Framing::Length(length),
            None => Framing::Close,
        }
    };
    Ok((status, framing))
}

/// The body of a reply, without its framing.
struct Body {
    reader: BufReader<UnixStream>,
    framing: Framing,
    /// The bytes left in the current chunk.
    chunk_left: u64,
    /// The CRLF that ends the previous chunk is still unread.
    chunk_crlf: bool,
    done: bool,
}

impl Body {
    fn new(reader: BufReader<UnixStream>, framing: Framing) -> Self {
        Self {
            reader,
            framing,
            chunk_left: 0,
            chunk_crlf: false,
            done: false,
        }
    }

    /// The next body bytes; `Ok(0)` at the end of the body.
    async fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<usize> {
        if self.done {
            return Ok(0);
        }
        match self.framing {
            Framing::Close => self.read_into(buf).await,
            Framing::Length(left) => {
                if left == 0 {
                    self.done = true;
                    return Ok(0);
                }
                let want = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
                let read = self.read_into(&mut buf[..want]).await?;
                if read > 0 {
                    self.framing = Framing::Length(left - read as u64);
                }
                Ok(read)
            }
            Framing::Chunked => {
                if self.chunk_crlf {
                    let mut crlf = [0u8; 2];
                    self.reader.read_exact(&mut crlf).await.ok();
                    self.chunk_crlf = false;
                }
                if self.chunk_left == 0 {
                    let size = self.chunk_size().await?;
                    if size == 0 {
                        // The trailers end with a blank line.
                        loop {
                            let mut line = String::new();
                            if self.reader.read_line(&mut line).await? == 0
                                || line.trim().is_empty()
                            {
                                break;
                            }
                        }
                        self.done = true;
                        return Ok(0);
                    }
                    self.chunk_left = size;
                }
                let want = buf
                    .len()
                    .min(usize::try_from(self.chunk_left).unwrap_or(usize::MAX));
                let read = self.read_into(&mut buf[..want]).await?;
                if read > 0 {
                    self.chunk_left -= read as u64;
                    self.chunk_crlf = self.chunk_left == 0;
                }
                Ok(read)
            }
        }
    }

    /// One read that treats a closed connection as the end of the body.
    async fn read_into(&mut self, buf: &mut [u8]) -> anyhow::Result<usize> {
        let read = self.reader.read(buf).await?;
        if read == 0 {
            self.done = true;
        }
        Ok(read)
    }

    /// Reads the size line of the next chunk, in hex.
    async fn chunk_size(&mut self) -> anyhow::Result<u64> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            self.done = true;
            return Ok(0);
        }
        let size = line.split(';').next().unwrap_or("").trim();
        if size.is_empty() {
            self.done = true;
            return Ok(0);
        }
        u64::from_str_radix(size, 16)
            .with_context(|| format!("the stream sent a bad chunk size {size:?}"))
    }
}

/// Body bytes split into lines, across chunk boundaries and UTF-8 splits.
struct Lines {
    body: Body,
    /// Body bytes read but not yet split into lines.
    pending: Vec<u8>,
}

impl Lines {
    fn new(body: Body) -> Self {
        Self {
            body,
            pending: Vec::new(),
        }
    }

    /// The next line, without its terminator. `None` at the end of the body;
    /// a last line without a terminator is still yielded.
    async fn line(&mut self) -> anyhow::Result<Option<String>> {
        loop {
            if let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            let mut buf = [0u8; 4096];
            let read = self.body.read(&mut buf).await?;
            if read == 0 {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.pending);
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            self.pending.extend_from_slice(&buf[..read]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use borant_core::events::RejectedHost;
    use borant_core::model::{MoveReason, TaskStatus};
    use borant_core::{FencingToken, SessionId};

    use super::*;

    const AT: Timestamp = Timestamp(1_792_929_600_000); // 2026-10-25T12:00:00Z

    fn event(body: SwarmEventBody) -> SwarmEvent {
        SwarmEvent::hub(AT, Severity::Info, body)
    }

    #[test]
    fn events_query_from_flags() {
        let args = Args {
            follow: true,
            since: Some(Timestamp(1_792_929_600_000)),
            kinds: vec!["lease.*".to_owned(), "task.status".to_owned()],
            agent: Some("w1".parse().unwrap()),
            channel: Some("api".parse().unwrap()),
            host: Some("borant-1".parse().unwrap()),
            min_severity: Some(Severity::Warn),
            json: false,
        };
        assert_eq!(
            args.query(),
            "since=1792929600000&follow=1&kinds=lease.*,task.status&agent=w1&channel=api&host=borant-1&min_severity=warn"
        );
        assert_eq!(Args::default().query(), "");
    }

    #[test]
    fn events_query_encodes_a_kind_that_needs_it() {
        let args = Args {
            kinds: vec!["a b".to_owned()],
            ..Args::default()
        };
        assert_eq!(args.query(), "kinds=a%20b");
    }

    #[test]
    fn events_since_parses_durations_and_times() {
        let day = 24 * 60 * 60 * 1000;
        assert_eq!(
            parse_since("30s", AT).unwrap(),
            AT.minus(Duration::from_secs(30))
        );
        assert_eq!(
            parse_since("5m", AT).unwrap(),
            AT.minus(Duration::from_secs(300))
        );
        assert_eq!(
            parse_since("2h", AT).unwrap(),
            AT.minus(Duration::from_secs(7200))
        );
        assert_eq!(parse_since("2d", AT).unwrap(), Timestamp(AT.0 - 2 * day));
        assert_eq!(parse_since("1w", AT).unwrap(), Timestamp(AT.0 - 7 * day));
        assert_eq!(
            parse_since("2026-10-25T12:00:00Z", AT).unwrap(),
            Timestamp(1_792_929_600_000)
        );
        for bad in ["", "soon", "5x", "m", "-5m"] {
            assert!(parse_since(bad, AT).is_err(), "{bad}");
        }
    }

    #[test]
    fn events_text_three_kinds() {
        let session: SessionId = "01M5S14CYTAYYF48BJ1C2E1HQH".parse().unwrap();
        let expired = SwarmEvent::host(
            AT,
            &"borant-2".parse().unwrap(),
            Severity::Warn,
            SwarmEventBody::LeaseExpired {
                session,
                token: FencingToken(3),
            },
        )
        .about_agent("pi-worker-4".parse().unwrap())
        .about_host("borant-2".parse().unwrap())
        .with_note("no heartbeat for 30s");
        assert_eq!(
            render(&expired),
            "12:00:00 warn  lease.expired          pi-worker-4    session 01M5S14CYTAYYF48BJ1C2E1HQH token 3 (no heartbeat for 30s)"
        );

        let launched = event(SwarmEventBody::MoveLaunched {
            from_host: Some("borant-2".parse().unwrap()),
            to_host: "borant-1".parse().unwrap(),
            reason: MoveReason::Takeover,
        })
        .about_agent("pi-worker-4".parse().unwrap());
        assert_eq!(
            render(&launched),
            "12:00:00 info  move.launched          pi-worker-4    borant-2 -> borant-1 (takeover)"
        );

        let task = event(SwarmEventBody::TaskStatus {
            from: TaskStatus::InProgress,
            to: TaskStatus::NeedsHuman,
            claimed_by: Some("w1".parse().unwrap()),
            attention_msg: Some("decide the storage".to_owned()),
        })
        .about_agent("w1".parse().unwrap())
        .about_channel("api".parse().unwrap());
        assert_eq!(
            render(&task),
            "12:00:00 info  task.status            w1             in_progress -> needs_human by w1 (decide the storage)"
        );
    }

    #[test]
    fn events_text_shows_pauses_and_changes() {
        let paused = event(SwarmEventBody::AgentPaused {
            by: borant_core::model::Sender::Human,
            why: "cap: 200 turns today".to_owned(),
        })
        .about_agent("w1".parse().unwrap());
        assert_eq!(
            render(&paused),
            "12:00:00 info  agent.paused           w1             by human: cap: 200 turns today"
        );
        let resumed = event(SwarmEventBody::AgentResumed).about_agent("w1".parse().unwrap());
        assert_eq!(render(&resumed), "12:00:00 info  agent.resumed          w1");

        let changed = event(SwarmEventBody::ChangeUpdated {
            state: borant_core::ports::ChangeState::Open,
            head: "abc123".to_owned(),
            checks: borant_core::ports::CheckState::Fail {
                failed: vec!["ci".to_owned(), "lint".to_owned()],
            },
            review: borant_core::ports::ReviewState::ChangesRequested,
        })
        .about_task(borant_core::TaskId::new());
        assert!(
            render(&changed).contains("open at abc123 checks=fail: ci, lint review=changes_requested"),
            "{}",
            render(&changed)
        );
    }

    #[test]
    fn events_text_without_a_subject() {
        let mut e = event(SwarmEventBody::HostDraining { agents: 2 });
        e.severity = Severity::Warn;
        assert_eq!(
            render(&e),
            "12:00:00 warn  host.draining          -              2 agents"
        );
        // A kind with no fields of its own still names its subject.
        let resumed = event(SwarmEventBody::SessionResumed {}).about_agent("w1".parse().unwrap());
        assert_eq!(render(&resumed), "12:00:00 info  session.resumed        w1");
    }

    #[test]
    fn events_text_shows_placement_and_alerts() {
        let chosen = event(SwarmEventBody::PlacementChosen {
            harness: borant_core::model::Harness::Omp,
            reason: MoveReason::Spawn,
        })
        .about_agent("w2".parse().unwrap())
        .about_host("borant-1".parse().unwrap());
        assert_eq!(
            render(&chosen),
            "12:00:00 info  placement.chosen       w2             borant-1 (spawn, omp)"
        );
        let refused = event(SwarmEventBody::PlacementRefused {
            reason: MoveReason::Spawn,
            rejected: vec![RejectedHost {
                host: "laptop-a".parse().unwrap(),
                why: "draining".to_owned(),
            }],
        });
        assert_eq!(
            render(&refused),
            "12:00:00 info  placement.refused      -              spawn (1 rejected)"
        );
        let fired = event(SwarmEventBody::AlertFired {
            rule: "host_offline".to_owned(),
            subject: "borant-2".to_owned(),
        });
        assert_eq!(
            render(&fired),
            "12:00:00 info  alert.fired            -              host_offline borant-2"
        );
    }

    #[test]
    fn events_text_uses_the_time_of_day_in_utc() {
        assert_eq!(utc_time(Timestamp(0)), "00:00:00");
        assert_eq!(utc_time(Timestamp(1_792_929_600_000)), "12:00:00");
        assert_eq!(utc_time(Timestamp(1_792_929_661_000)), "12:01:01");
    }

    /// A stub host on a fresh Unix socket: it reads one request head and
    /// writes `reply`. The task yields the request head.
    async fn serve(socket: PathBuf, reply: Vec<u8>) -> tokio::task::JoinHandle<String> {
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while conn.read(&mut byte).await.unwrap() == 1 {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            conn.write_all(&reply).await.unwrap();
            conn.flush().await.unwrap();
            conn.shutdown().await.unwrap();
            String::from_utf8(head).unwrap()
        })
    }

    fn sse(data: &[&str]) -> String {
        data.iter().map(|d| format!("data: {d}\r\n\r\n")).collect()
    }

    fn chunked(body: &str) -> Vec<u8> {
        let mut reply = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
        );
        for part in body.as_bytes().chunks(7) {
            reply.push_str(&format!("{:x}\r\n", part.len()));
            reply.push_str(std::str::from_utf8(part).unwrap());
            reply.push_str("\r\n");
        }
        reply.push_str("0\r\n\r\n");
        reply.into_bytes()
    }

    fn counted(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn temp_socket(dir: &Path) -> PathBuf {
        dir.join("host.sock")
    }

    fn json_line(e: &SwarmEvent) -> String {
        serde_json::to_string(e).unwrap()
    }

    #[tokio::test]
    async fn events_stream_reads_chunked_sse_with_keep_alives() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        let first = event(SwarmEventBody::HostDraining { agents: 2 });
        let second = event(SwarmEventBody::ClaimsReleased {
            tasks: 3,
            after_secs: 600,
        })
        .about_agent("w1".parse().unwrap());
        let body = format!(
            ": keep-alive\r\n\r\n{}{}",
            sse(&[&json_line(&first)]),
            sse(&[&json_line(&second)])
        );
        let server = serve(socket.clone(), chunked(&body)).await;
        let args = Args {
            follow: true,
            agent: Some("w1".parse().unwrap()),
            ..Args::default()
        };
        let mut out = Vec::new();
        stream(&socket, &args, &mut out).await.unwrap();
        let head = server.await.unwrap();
        assert!(
            head.starts_with("GET /v1/events?follow=1&agent=w1 HTTP/1.1\r\n"),
            "{head}"
        );
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "12:00:00 info  host.draining          -              2 agents\n\
             12:00:00 info  claims.released        w1             3 tasks after 600s\n"
        );
    }

    #[tokio::test]
    async fn events_stream_reads_a_counted_body_and_ends() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        let e = event(SwarmEventBody::HostRegistered {
            last_seen: AT,
            always_on: true,
            agents: 4,
        })
        .about_host("borant-1".parse().unwrap());
        let server = serve(socket.clone(), counted(&sse(&[&json_line(&e)]))).await;
        let args = Args::default();
        let mut out = Vec::new();
        stream(&socket, &args, &mut out).await.unwrap();
        let head = server.await.unwrap();
        assert!(head.starts_with("GET /v1/events? HTTP/1.1\r\n"), "{head}");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "12:00:00 info  host.registered        borant-1       4 agents always_on=true last seen 12:00:00\n"
        );
    }

    #[tokio::test]
    async fn events_json_passes_events_through() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        // The second event has a kind and a field that this build does not know.
        let known = event(SwarmEventBody::AgentPruned {
            by: None,
            idle_days: 30.0,
        })
        .about_agent("old".parse().unwrap());
        let future = String::from(
            r#"{"id":"01M5S14CYTAYYF48BJ1C2E1HQH","at":1792929600000,"source":"hub","severity":"info","kind":"swarm.tuned","knob":4}"#,
        );
        let body = sse(&[&json_line(&known), &future]);
        let server = serve(socket.clone(), chunked(&body)).await;
        let args = Args {
            json: true,
            ..Args::default()
        };
        let mut out = Vec::new();
        stream(&socket, &args, &mut out).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{}\n{future}\n", json_line(&known))
        );
    }

    #[tokio::test]
    async fn events_text_skips_an_unknown_kind() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        let future = String::from(
            r#"{"id":"01M5S14CYTAYYF48BJ1C2E1HQH","at":1792929600000,"source":"hub","severity":"info","kind":"swarm.tuned","knob":4}"#,
        );
        let known = event(SwarmEventBody::HostDraining { agents: 1 });
        let body = sse(&[&future, &json_line(&known)]);
        let server = serve(socket.clone(), chunked(&body)).await;
        let mut out = Vec::new();
        stream(&socket, &Args::default(), &mut out).await.unwrap();
        server.await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "12:00:00 info  host.draining          -              1 agents\n"
        );
    }

    #[tokio::test]
    async fn events_stream_reports_a_refused_request() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        let body =
            r#"{"ok":false,"error":{"code":"service_unavailable","message":"no event log"}}"#;
        let reply = format!(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes();
        let server = serve(socket.clone(), reply).await;
        let err = stream(&socket, &Args::default(), &mut Vec::new())
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(
            err.to_string(),
            format!("the host refused the events request (503): {body}")
        );
    }

    #[tokio::test]
    async fn events_stream_reports_a_daemon_that_is_not_running() {
        let dir = tempfile::tempdir().unwrap();
        let socket = temp_socket(dir.path());
        let err = stream(&socket, &Args::default(), &mut Vec::new())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("the Borant host daemon is not running"),
            "{err}"
        );
    }

    #[test]
    fn events_severity_arg_takes_three_names() {
        assert_eq!(severity_arg("warn"), Ok(Severity::Warn));
        assert_eq!(severity_arg("error"), Ok(Severity::Error));
        assert_eq!(severity_arg("info"), Ok(Severity::Info));
        assert!(severity_arg("WARN").is_err());
        assert!(severity_arg("critical").is_err());
    }
}
