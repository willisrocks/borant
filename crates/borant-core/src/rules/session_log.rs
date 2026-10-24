//! Rules of the neutral session log (spec section 12).

use std::collections::HashSet;

use crate::model::{Envelope, SessionEvent, SessionEventBody};

/// Default size limit for tool input and output in one event.
pub const MAX_TOOL_BYTES: usize = 8 * 1024;

/// Cuts text to at most `max_bytes` on a char boundary and marks the cut.
pub fn cut(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[cut: {} bytes removed]",
        &text[..end],
        text.len() - end
    )
}

/// The sequence number of the first event of the oldest turn to keep, or
/// `None` if the log has `keep_turns` turns or fewer. A turn starts at a
/// `TurnStart` event.
pub fn tail_start(events: &[(u64, SessionEvent)], keep_turns: usize) -> Option<u64> {
    let starts: Vec<u64> = events
        .iter()
        .filter(|(_, e)| matches!(e.body, SessionEventBody::TurnStart))
        .map(|(seq, _)| *seq)
        .collect();
    if keep_turns == 0 || starts.len() <= keep_turns {
        return None;
    }
    Some(starts[starts.len() - keep_turns])
}

/// Messages that the log received but never delivered, in order.
pub fn undelivered(events: &[SessionEvent]) -> Vec<Envelope> {
    let delivered: HashSet<_> = events
        .iter()
        .filter_map(|e| match &e.body {
            SessionEventBody::Delivered { message } => Some(*message),
            _ => None,
        })
        .collect();
    let mut seen = HashSet::new();
    events
        .iter()
        .filter_map(|e| match &e.body {
            SessionEventBody::SwarmMessage { envelope } => Some(envelope),
            _ => None,
        })
        .filter(|env| !delivered.contains(&env.id) && seen.insert(env.id))
        .cloned()
        .collect()
}

/// True if the log already holds a message with this ID.
pub fn has_message(events: &[SessionEvent], id: crate::ids::MessageId) -> bool {
    events.iter().any(
        |e| matches!(&e.body, SessionEventBody::SwarmMessage { envelope } if envelope.id == id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{EventId, MessageId};
    use crate::model::{EnvelopeKind, Harness, Sender, Target};
    use crate::time::Timestamp;

    fn ev(body: SessionEventBody) -> SessionEvent {
        SessionEvent {
            id: EventId::new(),
            agent: "a".parse().unwrap(),
            session: None,
            harness: Harness::Fake,
            host: "h".parse().unwrap(),
            turn: 0,
            at: Timestamp(0),
            body,
        }
    }

    fn msg() -> Envelope {
        Envelope {
            id: MessageId::new(),
            from: Sender::Human,
            to: Target::Agent("a".parse().unwrap()),
            kind: EnvelopeKind::Direct,
            text: "hi".into(),
            urgent: false,
            event: None,
            sent: Timestamp(0),
        }
    }

    #[test]
    fn cut_marks_removed_bytes() {
        let input = "x".repeat(10_000);
        let got = cut(&input, 8192);
        assert!(got.starts_with(&"x".repeat(8192)));
        assert!(got.ends_with("[cut: 1808 bytes removed]"));
        assert_eq!(cut("short", 8192), "short");
    }

    #[test]
    fn cut_respects_char_boundaries() {
        let got = cut("ééé", 3);
        assert!(got.starts_with('é'), "{got}");
        assert!(got.ends_with("[cut: 4 bytes removed]"), "{got}");
    }

    #[test]
    fn tail_keeps_last_turns() {
        let mut events = vec![];
        let mut seq = 0;
        for _ in 0..4 {
            seq += 1;
            events.push((seq, ev(SessionEventBody::TurnStart)));
            seq += 1;
            events.push((seq, ev(SessionEventBody::UserPrompt { text: "p".into() })));
        }
        assert_eq!(tail_start(&events, 2), Some(5));
        assert_eq!(tail_start(&events, 4), None);
        assert_eq!(tail_start(&events, 9), None);
    }

    #[test]
    fn undelivered_skips_delivered() {
        let m1 = msg();
        let m2 = msg();
        let events = vec![
            ev(SessionEventBody::SwarmMessage {
                envelope: m1.clone(),
            }),
            ev(SessionEventBody::SwarmMessage {
                envelope: m2.clone(),
            }),
            ev(SessionEventBody::Delivered { message: m1.id }),
            ev(SessionEventBody::SwarmMessage {
                envelope: m2.clone(),
            }),
        ];
        assert_eq!(undelivered(&events), vec![m2.clone()]);
        assert!(has_message(&events, m2.id));
        assert!(!has_message(&events, MessageId::new()));
    }
}
