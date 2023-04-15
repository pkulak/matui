use std::time::{Duration, SystemTime};

use crate::matrix::matrix::{pad_emoji, Matrix};
use crate::pretty_list;
use matrix_sdk::room::RoomMember;
use once_cell::unsync::OnceCell;
use ruma::events::relation::Replacement;
use ruma::events::room::message::MessageType::{self, Image, Text, Video};
use ruma::events::room::message::{
    ImageMessageEventContent, Relation, TextMessageEventContent, VideoMessageEventContent,
};
use ruma::events::room::redaction::RoomRedactionEvent;
use ruma::events::AnyMessageLikeEvent::Reaction as Rctn;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyMessageLikeEvent::RoomRedaction;
use ruma::events::AnyTimelineEvent;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent;
use ruma::{MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::ListItem;

// A Message is a line in the chat window; what a user would generally
// consider a "message". It has reactions, edits, and is generally in a state
// of constant mutation, as opposed to "events", which come in, in order.
pub struct Message {
    pub id: OwnedEventId,
    pub room_id: OwnedRoomId,
    pub sent: MilliSecondsSinceUnixEpoch,
    pub body: MessageType,
    pub history: Vec<MessageType>,
    pub sender: String,
    pub reactions: Vec<Reaction>,
}

impl Message {
    pub fn display(&self) -> &str {
        match &self.body {
            Text(TextMessageEventContent { body, .. }) => body,
            Image(ImageMessageEventContent { body, .. }) => body,
            Video(VideoMessageEventContent { body, .. }) => body,
            _ => "unknown",
        }
    }

    pub fn style(&self) -> Style {
        match &self.body {
            Text(_) => Style::default(),
            _ => Style::default().fg(Color::Blue),
        }
    }

    pub fn open(&self, matrix: Matrix) {
        match &self.body {
            Image(_) => matrix.open_content(self.body.clone()),
            Video(_) => matrix.open_content(self.body.clone()),
            _ => {}
        }
    }

    pub fn edit(&mut self, new_body: MessageType) {
        let old = std::mem::replace(&mut self.body, new_body);
        self.history.push(old);
    }

    // can we make a brand-new message, just from this event?
    pub fn try_from(event: &AnyTimelineEvent) -> Option<Self> {
        if let MessageLike(RoomMessage(MessageLikeEvent::Original(c))) = event {
            let c = c.clone();

            let body = match c.content.msgtype {
                Text(_) | Image(_) | Video(_) => c.content.msgtype,
                _ => return None,
            };

            // skip replacements
            if let Some(Relation::Replacement(_)) = c.content.relates_to {
                return None;
            }

            return Some(Message {
                id: c.event_id,
                room_id: c.room_id,
                sent: c.origin_server_ts,
                body,
                history: vec![],
                sender: c.sender.to_string(),
                reactions: Vec::new(),
            });
        }

        None
    }

    // if not, we should send the event here, to possibly act on existing
    // events
    pub fn merge_into_message_list(messages: &mut Vec<Message>, event: &AnyTimelineEvent) {
        // replacements
        if let MessageLike(RoomMessage(MessageLikeEvent::Original(c))) = event {
            let event = c.clone().content;

            if let Some(Relation::Replacement(Replacement {
                event_id: id,
                new_content: content,
                ..
            })) = event.relates_to
            {
                for message in messages.iter_mut() {
                    if message.id == id {
                        message.edit(content);
                        return;
                    }
                }
            }
        }

        // reactions
        if let MessageLike(Rctn(MessageLikeEvent::Original(c))) = event {
            let relates = c.content.relates_to.clone();

            let sender = c.sender.to_string();
            let body = relates.key;
            let relates_id = relates.event_id;

            for message in messages.iter_mut() {
                if message.id == relates_id {
                    let reaction_event = ReactionEvent::new(event.event_id().into(), sender);

                    message.reactions.push(Reaction {
                        body,
                        events: vec![reaction_event],
                        pretty_senders: OnceCell::new(),
                    });
                    return;
                }
            }

            return;
        }

        // redactions
        if let MessageLike(RoomRedaction(RoomRedactionEvent::Original(c))) = event {
            let id = &c.redacts;

            // first look in the reactions
            for message in messages.iter_mut() {
                for r in &mut message.reactions {
                    r.events.retain(|e| &e.id != id)
                }

                // making sure to get rid of reactions that have no events
                message.reactions.retain(|r| !r.events.is_empty());
            }

            // then look at the messages
            messages.retain(|m| &m.id != id);

            return;
        }
    }

    pub fn update_senders(&mut self, members: &Vec<RoomMember>) {
        fn set_name(old: &mut String, new: &RoomMember) {
            *old = new.display_name().unwrap_or_else(|| old).to_string();
        }

        // maybe we use a map, or sorted list at some point to avoid looping
        for member in members {
            let user_id: String = member.user_id().into();

            if self.sender == user_id {
                set_name(&mut self.sender, member);
            }

            for reaction in self.reactions.iter_mut() {
                for event in reaction.events.iter_mut() {
                    if event.sender_id == user_id {
                        set_name(&mut event.sender_name, member);
                    }
                }
            }
        }
    }

    pub fn to_list_item(&self, width: usize) -> ListItem {
        use tui::text::Text;

        let formatter = timeago::Formatter::new();

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let then: u64 = self.sent.as_secs().into();
        let pretty_elapsed = formatter.convert(Duration::from_secs(now - then));
        let pretty_elapsed = format!(" {}", pretty_elapsed);

        // author
        let mut spans = vec![
            Span::styled(self.sender.clone(), Style::default().fg(Color::Green)),
            Span::styled(pretty_elapsed, Style::default().fg(Color::DarkGray)),
        ];

        if !self.history.is_empty() {
            spans.push(Span::styled(" (edited)", Style::default().fg(Color::Red)))
        }

        // message
        let mut lines = Text::from(Spans::from(spans));

        let wrapped = textwrap::wrap(&self.display(), width);

        for l in wrapped {
            lines.extend(Text::styled(l, self.style()))
        }

        // reactions
        for r in &self.reactions {
            let line = if let Some(emoji) = emojis::get(&r.body) {
                if let Some(shortcode) = emoji.shortcode() {
                    format!("{} ({})", pad_emoji(&r.body), shortcode)
                } else {
                    pad_emoji(&r.body)
                }
            } else {
                pad_emoji(&r.body)
            };

            let line = format!("{} {}", line, r.pretty_senders());

            lines.extend(Text::styled(line, Style::default().fg(Color::DarkGray)))
        }

        lines.extend(Text::from(" ".to_string()));

        ListItem::new(lines)
    }
}

// A reaction is a single emoji. I may have 1 or more events, one for each
// user.
#[derive(Clone)]
pub struct Reaction {
    pub body: String,
    pub events: Vec<ReactionEvent>,
    pub pretty_senders: OnceCell<String>,
}

impl Reaction {
    // drain the given reactions to create a merged version
    pub fn merge(reactions: &mut Vec<Reaction>) -> Vec<Reaction> {
        let mut merged: Vec<Reaction> = vec![];

        for r in reactions {
            let mut added = false;

            for m in merged.iter_mut() {
                if m.body == r.body {
                    m.events.append(&mut r.events);
                    added = true;
                }
            }

            if !added {
                merged.push(r.clone());
            }
        }

        merged
    }

    pub fn pretty_senders(&self) -> &str {
        self.pretty_senders.get_or_init(|| {
            let all: Vec<&str> = self
                .events
                .iter()
                .map(|s| s.sender_name.split_whitespace().next().unwrap_or_default())
                .collect();

            return pretty_list(all);
        })
    }
}

#[derive(Clone)]
pub struct ReactionEvent {
    pub id: OwnedEventId,
    pub sender_name: String,
    pub sender_id: String,
}

impl ReactionEvent {
    pub fn new(id: OwnedEventId, sender_id: String) -> ReactionEvent {
        ReactionEvent {
            id,
            sender_name: sender_id.clone(),
            sender_id,
        }
    }
}
