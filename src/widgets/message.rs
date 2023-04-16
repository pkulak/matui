use chrono::TimeZone;
use std::time::{Duration, SystemTime};

use crate::matrix::matrix::{pad_emoji, Matrix};
use crate::pretty_list;
use crate::spawn::view_text;
use chrono::offset::Local;
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
    pub sender_id: String,
    pub reactions: Vec<Reaction>,
    pub pretty_elapsed: OnceCell<String>,
}

impl Message {
    pub fn display_body(body: &MessageType) -> &str {
        match body {
            Text(TextMessageEventContent { body, .. }) => body,
            Image(ImageMessageEventContent { body, .. }) => body,
            Video(VideoMessageEventContent { body, .. }) => body,
            _ => "unknown",
        }
    }

    pub fn display(&self) -> &str {
        Message::display_body(&self.body)
    }

    pub fn display_full(&self) -> String {
        let date = Local.timestamp_opt(self.sent.as_secs().into(), 0).unwrap();

        let mut ret = format!(
            "Sent {} by {} ({})\n\n",
            date.format("%Y-%m-%d at %I:%M:%S %p"),
            self.sender,
            self.sender_id
        );

        ret.push_str(self.display());
        ret.push_str("\n\n");

        if !self.reactions.is_empty() {
            ret.push_str("### Reactions\n\n");

            for r in &self.reactions {
                for re in &r.events {
                    ret.push_str(
                        format!(
                            "* {} by {} ({})\n",
                            r.display(),
                            re.sender_name,
                            re.sender_id
                        )
                        .as_str(),
                    );
                }
            }

            ret.push_str("\n");
        }

        if !self.history.is_empty() {
            let mut reversed_history = self.history.clone();
            reversed_history.reverse();

            ret.push_str("### History\n\n");

            for h in reversed_history.into_iter() {
                ret.push_str("* ");
                ret.push_str(Message::display_body(&h));
                ret.push_str("\n");
            }
        }

        ret
    }

    pub fn pretty_elapsed(&self) -> &str {
        self.pretty_elapsed.get_or_init(|| {
            let formatter = timeago::Formatter::new();

            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs();

            let then: u64 = self.sent.as_secs().into();
            let pretty_elapsed = formatter.convert(Duration::from_secs(now - then));
            return format!(" {}", pretty_elapsed);
        })
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
            Text(_) => view_text(self.display()),
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
                sender_id: c.sender.to_string(),
                reactions: Vec::new(),
                pretty_elapsed: OnceCell::new(),
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
                        list_view: OnceCell::new(),
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

        // author
        let mut spans = vec![
            Span::styled(&self.sender, Style::default().fg(Color::Green)),
            Span::styled(self.pretty_elapsed(), Style::default().fg(Color::DarkGray)),
        ];

        if !self.history.is_empty() {
            spans.push(Span::styled(" (edited)", Style::default().fg(Color::Red)))
        }

        // message
        let mut lines = Text::from(Spans::from(spans));

        let wrapped = textwrap::wrap(&self.display(), width);
        let message_overlap = wrapped.len() > 10;

        for l in wrapped.into_iter().take(10) {
            lines.extend(Text::styled(l, self.style()))
        }

        // overflow warning
        if message_overlap || self.reactions.len() > 5 {
            lines.extend(Text::styled(
                "* overflow: type \"O\" to view entire message",
                Style::default().fg(Color::Red),
            ))
        }

        // reactions
        for r in self.reactions.iter().take(5) {
            lines.extend(Text::styled(
                r.list_view(),
                Style::default().fg(Color::DarkGray),
            ))
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
    pub list_view: OnceCell<String>,
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

    pub fn display(&self) -> String {
        if let Some(emoji) = emojis::get(&self.body) {
            if let Some(shortcode) = emoji.shortcode() {
                format!("{} ({})", pad_emoji(&self.body), shortcode)
            } else {
                pad_emoji(&self.body)
            }
        } else {
            pad_emoji(&self.body)
        }
    }

    pub fn pretty_senders(&self) -> String {
        let all: Vec<&str> = self
            .events
            .iter()
            .map(|s| s.sender_name.split_whitespace().next().unwrap_or_default())
            .collect();

        pretty_list(all)
    }

    // cached to keep allocations out of the render loop
    pub fn list_view(&self) -> &str {
        self.list_view
            .get_or_init(|| format!("{} {}", self.display(), self.pretty_senders()))
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
