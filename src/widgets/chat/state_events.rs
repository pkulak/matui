use super::TimelineItem;
use crate::widgets::message::LineType;
use crate::{limit_list, pretty_list, truncate};
use matrix_sdk::room::RoomMember;
use matrix_sdk::ruma::events::StateEventContentChange;
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, UserId};
use matrix_sdk_ui::timeline::{
    AnyOtherStateEventContentChange, EventTimelineItem, MembershipChange, TimelineItemContent,
};
use ratatui::style::{Color, Style};
use ratatui::text::Span;
use ratatui::widgets::ListItem;

// a group of consecutive room state events, rendered as single gray lines
pub struct RoomEvents {
    pub(super) ts: MilliSecondsSinceUnixEpoch,
    lines: Vec<String>,
}

impl RoomEvents {
    pub(super) fn to_list_items(&self, sidecar: &mut Vec<LineType>) -> Vec<ListItem<'_>> {
        // mirror Message::to_list_items: build top-down, then reverse
        let mut items = vec![ListItem::new(" ")];
        sidecar.push(LineType::DeadSpace);

        for line in &self.lines {
            items.push(ListItem::new(Span::styled(
                line.as_str(),
                Style::default().fg(Color::DarkGray),
            )));

            sidecar.push(LineType::RoomEvent);
        }

        sidecar.reverse();
        items.reverse();
        items
    }
}

#[derive(Clone, Copy, PartialEq)]
enum MemberKind {
    Joined,
    Left,
    Invited,
    Kicked,
    Banned,
    Unbanned,
}

enum StateEntry {
    Member(MemberKind, String),
    Line(String),
}

#[derive(Default)]
pub(super) struct PendingEvents {
    entries: Vec<StateEntry>,
    ts: Option<MilliSecondsSinceUnixEpoch>,
}

impl PendingEvents {
    pub(super) fn push_event(&mut self, event: &EventTimelineItem, members: &[RoomMember]) {
        if let Some(entry) = state_entry(event, members) {
            self.push(entry, event.timestamp());
        }
    }

    fn push(&mut self, entry: StateEntry, ts: MilliSecondsSinceUnixEpoch) {
        self.entries.push(entry);
        self.ts = Some(ts);
    }

    // combine runs of same-kind member entries into single lines
    pub(super) fn flush(&mut self) -> Option<RoomEvents> {
        let ts = self.ts.take()?;
        let mut lines = vec![];
        let mut run: Option<(MemberKind, Vec<String>)> = None;

        for entry in self.entries.drain(..) {
            match entry {
                StateEntry::Member(kind, name) => match &mut run {
                    Some((k, names)) if *k == kind => {
                        if !names.contains(&name) {
                            names.push(name);
                        }
                    }
                    _ => {
                        if let Some((k, names)) = run.take() {
                            lines.push(member_line(k, names));
                        }

                        run = Some((kind, vec![name]));
                    }
                },
                StateEntry::Line(line) => {
                    if let Some((k, names)) = run.take() {
                        lines.push(member_line(k, names));
                    }

                    lines.push(line);
                }
            }
        }

        if let Some((k, names)) = run.take() {
            lines.push(member_line(k, names));
        }

        Some(RoomEvents { ts, lines })
    }
}

fn state_entry(event: &EventTimelineItem, members: &[RoomMember]) -> Option<StateEntry> {
    let line = match event.content() {
        TimelineItemContent::MembershipChange(change) => {
            let kind = match change.change()? {
                MembershipChange::Joined | MembershipChange::InvitationAccepted => {
                    MemberKind::Joined
                }
                MembershipChange::Left => MemberKind::Left,
                MembershipChange::Invited => MemberKind::Invited,
                MembershipChange::Kicked => MemberKind::Kicked,
                MembershipChange::Banned | MembershipChange::KickedAndBanned => MemberKind::Banned,
                MembershipChange::Unbanned => MemberKind::Unbanned,
                // profile changes, knocks, etc are just noise
                _ => return None,
            };

            let name = display_name(change.user_id(), change.display_name().as_deref(), members);
            return Some(StateEntry::Member(kind, name));
        }
        TimelineItemContent::OtherState(state) => match state.content() {
            AnyOtherStateEventContentChange::RoomName(StateEventContentChange::Original {
                content,
                ..
            }) => format!(
                "{} changed the room name to {}.",
                display_name(event.sender(), None, members),
                truncate(content.name.clone(), 48),
            ),
            AnyOtherStateEventContentChange::RoomTopic(StateEventContentChange::Original {
                content,
                ..
            }) => format!(
                "{} changed the topic to {}.",
                display_name(event.sender(), None, members),
                truncate(content.topic.clone(), 48),
            ),
            _ => return None,
        },
        _ => return None,
    };

    Some(StateEntry::Line(line))
}

fn display_name(id: &UserId, fallback: Option<&str>, members: &[RoomMember]) -> String {
    members
        .iter()
        .find(|m| m.user_id() == id)
        .and_then(|m| m.display_name())
        .or(fallback)
        .unwrap_or_else(|| id.localpart())
        .to_string()
}

fn member_line(kind: MemberKind, names: Vec<String>) -> String {
    let total = names.len();
    let subject = pretty_list(limit_list(names.into_iter(), 3, total, None));

    let verb = match (kind, total > 1) {
        (MemberKind::Joined, _) => "joined the room",
        (MemberKind::Left, _) => "left the room",
        (MemberKind::Invited, false) => "was invited to the room",
        (MemberKind::Invited, true) => "were invited to the room",
        (MemberKind::Kicked, false) => "was kicked from the room",
        (MemberKind::Kicked, true) => "were kicked from the room",
        (MemberKind::Banned, false) => "was banned from the room",
        (MemberKind::Banned, true) => "were banned from the room",
        (MemberKind::Unbanned, false) => "was unbanned",
        (MemberKind::Unbanned, true) => "were unbanned",
    };

    format!("{} {}.", subject, verb)
}

// merge adjacent groups; only possible when the message that separated
// them was redacted
pub(super) fn push_group(items: &mut Vec<TimelineItem>, group: RoomEvents) {
    if let Some(TimelineItem::Events(prev)) = items.last_mut() {
        // the incoming group is older, so its lines go on top
        let mut lines = group.lines;
        lines.append(&mut prev.lines);
        prev.lines = lines;
    } else {
        items.push(TimelineItem::Events(group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(kind: MemberKind, name: &str) -> StateEntry {
        StateEntry::Member(kind, name.to_string())
    }

    fn flush(entries: Vec<StateEntry>) -> Vec<String> {
        let mut pending = PendingEvents::default();

        for entry in entries {
            pending.push(entry, MilliSecondsSinceUnixEpoch::now());
        }

        pending.flush().unwrap().lines
    }

    #[test]
    fn combines_runs_of_the_same_kind() {
        let lines = flush(vec![
            member(MemberKind::Joined, "Bob"),
            member(MemberKind::Joined, "Alice"),
            member(MemberKind::Left, "Jeff"),
        ]);

        assert_eq!(
            lines,
            vec!["Bob and Alice joined the room.", "Jeff left the room."]
        );
    }

    #[test]
    fn dedups_names_and_counts_overflow() {
        let lines = flush(vec![
            member(MemberKind::Left, "Bob"),
            member(MemberKind::Left, "Bob"),
            member(MemberKind::Left, "Alice"),
            member(MemberKind::Left, "Jeff"),
            member(MemberKind::Left, "Carol"),
            member(MemberKind::Left, "Dave"),
        ]);

        assert_eq!(lines, vec!["Bob, Alice, Jeff and 2 others left the room."]);
    }

    #[test]
    fn standalone_lines_break_runs() {
        let lines = flush(vec![
            member(MemberKind::Joined, "Bob"),
            StateEntry::Line("Alice changed the topic to Fun.".to_string()),
            member(MemberKind::Joined, "Jeff"),
        ]);

        assert_eq!(
            lines,
            vec![
                "Bob joined the room.",
                "Alice changed the topic to Fun.",
                "Jeff joined the room.",
            ]
        );
    }

    #[test]
    fn singular_verbs_agree() {
        assert_eq!(
            member_line(MemberKind::Kicked, vec!["Bob".to_string()]),
            "Bob was kicked from the room."
        );
    }
}
