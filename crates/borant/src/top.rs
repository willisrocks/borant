//! `borant top`: a live screen of the swarm, from `ToolCall::View`.

use std::time::{Duration, Instant};

use borant_core::AgentId;
use borant_core::model::{LeaseState, SwarmView};
use borant_core::proto::ToolCall;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};

/// How often the screen reads the view.
pub const POLL: Duration = Duration::from_secs(2);

/// The label of a lease state.
fn lease_label(state: Option<&LeaseState>) -> &'static str {
    match state {
        Some(LeaseState::Held { .. }) => "held",
        Some(LeaseState::Reserved { .. }) => "reserved",
        Some(LeaseState::Expired { .. }) => "expired",
        Some(LeaseState::Released { .. }) => "released",
        None => "-",
    }
}

/// The inbox counts of one agent as `pending/ack_pending/redelivered`.
fn inbox_label(view: &SwarmView, agent: &AgentId) -> String {
    match view.inboxes.iter().find(|inbox| &inbox.agent == agent) {
        Some(inbox) => format!(
            "{}/{}/{}",
            inbox.stats.pending, inbox.stats.ack_pending, inbox.stats.redelivered
        ),
        None => "-".to_owned(),
    }
}

/// Draws the screen: the hosts, the agents with their inbox counts, and the
/// key hint. `note` shows the last read error, if any.
pub fn draw(frame: &mut Frame, view: &SwarmView, note: Option<&str>) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(hosts_height(view)),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(frame.area());
    draw_hosts(frame, chunks[0], view);
    draw_agents(frame, chunks[1], view);
    let footer = match note {
        Some(note) => format!("q quit · view every 2 s · {note}"),
        None => "q quit · view every 2 s".to_owned(),
    };
    frame.render_widget(Paragraph::new(footer), chunks[2]);
}

fn hosts_height(view: &SwarmView) -> u16 {
    // The borders and the header take three lines.
    u16::try_from(view.hosts.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4)
        .clamp(4, 12)
}

fn draw_hosts(frame: &mut Frame, area: Rect, view: &SwarmView) {
    let rows: Vec<Row> = view
        .hosts
        .iter()
        .map(|host| {
            let stats = host.stats.as_ref();
            Row::new(vec![
                Cell::from(host.info.name.to_string()),
                Cell::from(if host.online { "yes" } else { "no" }),
                Cell::from(
                    stats
                        .map(|s| format!("{:.2}", s.load1))
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(
                    stats
                        .map(|s| s.cpus.to_string())
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(
                    stats
                        .and_then(|s| s.last_heartbeat_rtt_ms)
                        .map(|ms| format!("{ms}ms"))
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(format!(
                    "{}/{}",
                    host.info.max_agents,
                    host.info.harnesses.len()
                )),
                Cell::from(
                    stats
                        .map(|s| s.outbox_depth.to_string())
                        .unwrap_or_else(|| "-".into()),
                ),
            ])
        })
        .collect();
    let header = Row::new(vec![
        "HOST", "ONLINE", "LOAD", "CPUS", "RTT", "MAX/HARN", "OUTBOX",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(7),
            Constraint::Length(6),
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(" hosts "));
    frame.render_widget(table, area);
}

fn draw_agents(frame: &mut Frame, area: Rect, view: &SwarmView) {
    let rows: Vec<Row> = view
        .agents
        .iter()
        .map(|agent| {
            Row::new(vec![
                Cell::from(agent.agent.to_string()),
                Cell::from(
                    agent
                        .harness
                        .map(|h| h.as_str().to_owned())
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(
                    agent
                        .host
                        .as_ref()
                        .map(|h| h.to_string())
                        .unwrap_or_else(|| "-".into()),
                ),
                Cell::from(lease_label(agent.lease.as_ref())),
                Cell::from(if agent.busy { "busy" } else { "idle" }),
                Cell::from(inbox_label(view, &agent.agent)),
            ])
        })
        .collect();
    let header = Row::new(vec![
        "AGENT",
        "HARNESS",
        "HOST",
        "LEASE",
        "STATE",
        "INBOX p/a/r",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(12),
            Constraint::Length(16),
            Constraint::Length(9),
            Constraint::Length(6),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title(title(view)));
    frame.render_widget(table, area);
}

fn title(view: &SwarmView) -> String {
    format!(
        " agents {} · hosts {} ",
        view.agents.len(),
        view.hosts.len()
    )
}

/// Runs the screen until the user quits with `q` or Esc.
pub async fn run() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal).await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
    let client = crate::cli::client();
    let mut view = SwarmView::default();
    let mut note: Option<String> = None;
    let mut next = Instant::now();
    loop {
        if Instant::now() >= next {
            match read_view(&client).await {
                Ok(fresh) => {
                    view = fresh;
                    note = None;
                }
                Err(e) => note = Some(format!("{e:#}")),
            }
            next = Instant::now() + POLL;
        }
        terminal.draw(|frame| draw(frame, &view, note.as_deref()))?;
        // A short sleep keeps the loop from spinning; the key check does not
        // block, so a read that waits for the screen never delays one.
        tokio::time::sleep(Duration::from_millis(50)).await;
        if event::poll(Duration::ZERO)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
        {
            return Ok(());
        }
    }
}

/// One read of the view through the host daemon.
async fn read_view(client: &borant_host::LocalClient) -> anyhow::Result<SwarmView> {
    let value = crate::cli::call(client, ToolCall::View).await?;
    Ok(serde_json::from_value(value)?)
}

/// The text of a whole buffer, one line for each row. Tests read the screen
/// with it.
#[cfg(test)]
fn buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
    let mut out = String::new();
    let width = usize::from(buffer.area.width);
    for row in buffer.content().chunks(width.max(1)) {
        for cell in row {
            out.push_str(cell.symbol());
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use borant_core::model::{AgentStatus, HostInfo, HostStatus, InboxStatus};
    use borant_core::ports::InboxStats;
    use borant_core::{HostName, SessionId, Timestamp};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view() -> SwarmView {
        let host: HostName = "borant-1".parse().unwrap();
        let agent: AgentId = "api-owner".parse().unwrap();
        SwarmView {
            hosts: vec![HostStatus {
                info: HostInfo {
                    name: host.clone(),
                    harnesses: vec![borant_core::model::Harness::ClaudeCode],
                    launcher: "tmux".into(),
                    max_agents: 6,
                    always_on: true,
                    draining: false,
                    ssh: None,
                    members: Default::default(),
                    pool: None,
                    tags: vec![],
                    bundles: vec![],
                },
                online: true,
                last_seen: Some(Timestamp(1_000)),
                stats: None,
            }],
            agents: vec![AgentStatus {
                agent: agent.clone(),
                harness: Some(borant_core::model::Harness::ClaudeCode),
                host: Some(host),
                lease: Some(LeaseState::Held {
                    session: SessionId::new(),
                    host: "borant-1".parse().unwrap(),
                    expires: Timestamp(2_000),
                }),
                session: Some(SessionId::new()),
                busy: true,
                claim: None,
            }],
            inboxes: vec![InboxStatus {
                agent,
                stats: InboxStats {
                    pending: 1,
                    ack_pending: 2,
                    redelivered: 3,
                },
            }],
            at: Some(Timestamp(1_000)),
        }
    }

    /// The screen has the columns the spec asks for, and the counts.
    #[test]
    fn top_renders_the_columns_and_the_inbox_counts() {
        let view = view();
        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &view, None)).unwrap();
        let text = buffer_text(terminal.backend().buffer());
        for column in [
            "HOST", "ONLINE", "LOAD", "AGENT", "HARNESS", "LEASE", "STATE",
        ] {
            assert!(text.contains(column), "the screen shows {column}:\n{text}");
        }
        assert!(text.contains("INBOX p/a/r"), "the inbox column:\n{text}");
        assert!(text.contains("api-owner"), "{text}");
        assert!(text.contains("borant-1"), "{text}");
        assert!(text.contains("held"), "{text}");
        assert!(text.contains("busy"), "{text}");
        assert!(
            text.contains("1/2/3"),
            "pending/ack_pending/redelivered:\n{text}"
        );
        assert!(text.contains("q quit"), "{text}");
    }

    /// A read error is on the screen, and an agent without an inbox shows a
    /// dash.
    #[test]
    fn top_shows_the_read_error_and_a_missing_inbox() {
        let mut view = view();
        view.inboxes.clear();
        let backend = TestBackend::new(110, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw(frame, &view, Some("the host daemon is not running")))
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("the host daemon is not running"), "{text}");
        assert!(
            text.contains("  - "),
            "the missing inbox is a dash:\n{text}"
        );
    }

    #[test]
    fn top_lease_labels() {
        assert_eq!(lease_label(None), "-");
        assert_eq!(
            lease_label(Some(&LeaseState::Released {
                since: Timestamp(1),
                parked: false,
            })),
            "released"
        );
        assert_eq!(
            lease_label(Some(&LeaseState::Expired {
                since: Timestamp(1),
                last_session: None,
            })),
            "expired"
        );
    }
}
