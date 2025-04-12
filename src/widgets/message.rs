use crate::widgets::message::MessageType::File;
use chrono::TimeZone;
use human_bytes::human_bytes;
use std::cell::Cell;
use std::cmp;
use std::collections::BinaryHeap;
use std::time::{Duration, SystemTime};

use crate::matrix::matrix::{pad_emoji, AfterDownload, Matrix};
use crate::matrix::username::Username;
use crate::spawn::view_text;
use crate::{limit_list, pretty_list};
use chrono::offset::Local;
use matrix_sdk::room::RoomMember;
use once_cell::unsync::OnceCell;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;
use ruma::events::relation::{InReplyTo, Replacement};
use ruma::events::room::message::MessageType::{self, Image, Text, Video};
use ruma::events::room::message::{
    FileMessageEventContent, ImageMessageEventContent, Relation, TextMessageEventContent,
    VideoMessageEventContent,
};
use ruma::events::room::redaction::{OriginalRoomRedactionEvent, RoomRedactionEvent};
use ruma::events::AnyMessageLikeEvent::Reaction as Rctn;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyMessageLikeEvent::RoomRedaction;
use ruma::events::AnyTimelineEvent;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent;
use ruma::{MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId, OwnedUserId};

use super::receipts::Receipt;

// A Message is a line in the chat window; what a user would generally
// consider a "message". It has reactions, edits, and is generally in a state
// of constant mutation, as opposed to "events", which come in, in order.
pub struct Message {
    pub id: OwnedEventId,
    pub in_reply_to: Option<OwnedEventId>,
    pub room_id: OwnedRoomId,
    pub sent: MilliSecondsSinceUnixEpoch,
    pub body: MessageType,
    pub history: Vec<MessageType>,
    pub sender: Username,
    pub reactions: Vec<Reaction>,
    pub replies: Vec<Message>,
    pub receipts: Vec<Username>,

    last_height: Cell<LastHeight>,
}

#[derive(PartialEq, Eq)]
pub enum MergeResult {
    Consumed,
    Missed,
    Ignored,
}

// We need to calculate the message hight a lot, but it rarely changes;
// keep it around.
#[derive(Copy, Clone, Default)]
struct LastHeight {
    width: usize,
    height: usize,
}

impl Message {
    pub fn sort_order(&self) -> &MilliSecondsSinceUnixEpoch {
        if self.replies.is_empty() {
            &self.sent
        } else {
            &self.replies.last().unwrap().sent
        }
    }

    fn display_body(body: &MessageType) -> String {
        match body {
            Text(TextMessageEventContent { body, .. }) => body.to_string(),
            Image(ImageMessageEventContent { body, info, .. }) => {
                if let Some(info) = info {
                    if let Some(size) = info.size {
                        format!("Image: {} ({})", body, human_bytes(size))
                    } else {
                        body.to_string()
                    }
                } else {
                    body.to_string()
                }
            }
            Video(VideoMessageEventContent { body, info, .. }) => {
                if let Some(info) = info {
                    if let Some(size) = info.size {
                        format!("Video: {} ({})", body, human_bytes(size))
                    } else {
                        "no size".to_string()
                    }
                } else {
                    "no info".to_string()
                }
            }
            File(FileMessageEventContent { body, info, .. }) => {
                if let Some(info) = info {
                    if let Some(size) = info.size {
                        format!("File: {} ({})", body, human_bytes(size))
                    } else {
                        body.to_string()
                    }
                } else {
                    body.to_string()
                }
            }
            _ => "unknown".to_string(),
        }
    }

    pub fn display(&self) -> String {
        Message::display_body(&self.body).trim().to_string()
    }

    pub fn display_full(&self) -> String {
        let date = Local.timestamp_opt(self.sent.as_secs().into(), 0).unwrap();

        let mut ret = format!(
            "Sent {} by {} ({})\n\n",
            date.format("%Y-%m-%d at %I:%M:%S %p"),
            self.sender,
            self.sender.id
        );

        ret.push_str(&self.display());
        ret.push_str("\n\n");

        if !self.reactions.is_empty() {
            ret.push_str("### Reactions\n\n");

            for r in &self.reactions {
                for re in &r.events {
                    ret.push_str(
                        format!("* {} by {} ({})\n", r.display(), re.sender, re.sender.id).as_str(),
                    );
                }
            }

            ret.push('\n');
        }

        if !self.history.is_empty() {
            let mut reversed_history = self.history.clone();
            reversed_history.reverse();

            ret.push_str("### History\n\n");

            for h in reversed_history.into_iter() {
                ret.push_str("* ");
                ret.push_str(&Message::display_body(&h));
                ret.push('\n');
            }
        }

        ret
    }

    pub fn pretty_elapsed(&self) -> String {
        let formatter = timeago::Formatter::new();

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let then: u64 = self.sent.as_secs().into();

        formatter.convert(Duration::from_secs(now.saturating_sub(then)))
    }

    pub fn style(&self) -> Style {
        match &self.body {
            Text(_) => Style::default(),
            _ => Style::default().fg(Color::Blue),
        }
    }

    pub fn open(&self, matrix: Matrix) {
        match &self.body {
            Image(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            Video(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            File(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Text(_) => view_text(&self.display()),
            _ => {}
        }
    }

    pub fn save(&self, matrix: Matrix) {
        match &self.body {
            Image(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Video(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            File(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            _ => {}
        }
    }

    pub fn edit(&mut self, new_body: MessageType) {
        let old = std::mem::replace(&mut self.body, new_body);
        self.history.push(old);
    }

    // can we make a brand-new message, just from this event?
    pub fn try_from(event: &AnyTimelineEvent, force: bool) -> Option<Self> {
        if let MessageLike(RoomMessage(MessageLikeEvent::Original(c))) = event {
            let c = c.clone();

            let body = match c.content.msgtype {
                Text(_) | Image(_) | Video(_) | File(_) => c.content.msgtype,
                _ => return None,
            };

            // skip replacements
            if let Some(Relation::Replacement(_)) = c.content.relates_to {
                return None;
            }

            // and replies (sometimes)
            let in_reply_to = if let Some(Relation::Reply {
                in_reply_to: InReplyTo { event_id: id, .. },
            }) = c.content.relates_to
            {
                if !force {
                    return None;
                }

                Some(id)
            } else {
                None
            };

            return Some(Message {
                id: c.event_id,
                in_reply_to,
                room_id: c.room_id,
                sent: c.origin_server_ts,
                body,
                history: vec![],
                sender: Username::new(c.sender),
                reactions: Vec::new(),
                replies: Vec::new(),
                receipts: Vec::new(),
                last_height: Cell::new(LastHeight::default()),
            });
        }

        None
    }

    // if not, we should send the event here, to possibly act on existing
    // events
    pub fn apply_timeline_event(
        messages: &mut Vec<Message>,
        event: &AnyTimelineEvent,
        depth: usize,
    ) -> MergeResult {
        let mut reply_result = MergeResult::Ignored;

        // replacements and replies
        if let MessageLike(RoomMessage(MessageLikeEvent::Original(c))) = event {
            let event_content = c.clone().content;

            if let Some(Relation::Replacement(Replacement {
                event_id: id,
                new_content: content,
                ..
            })) = event_content.relates_to.clone()
            {
                for message in messages.iter_mut() {
                    if message.id == id {
                        message.edit(content.msgtype);
                        return MergeResult::Consumed;
                    }
                }
            }

            if let Some(Relation::Reply {
                in_reply_to: InReplyTo { event_id: id, .. },
            }) = event_content.relates_to
            {
                let mut found_index = None;
                let mut sibling = None;

                for (i, message) in messages.iter_mut().enumerate() {
                    if message.id == id {
                        if let Some(reply) = Message::try_from(event, true) {
                            if depth > 3 {
                                sibling = Some(reply)
                            } else {
                                message.replies.push(reply);
                                found_index = Some(i);
                            }
                            break;
                        }
                    }
                }

                // we found a message with a new reply, so move it to the end
                if let Some(i) = found_index {
                    let found = messages.remove(i);
                    messages.push(found);
                    return MergeResult::Consumed;
                }

                // we found a new reply, but can't nest any deeper
                if let Some(message) = sibling {
                    messages.push(message);
                    return MergeResult::Consumed;
                }

                // we found a reply, but can't put it here
                reply_result = MergeResult::Missed;
            }
        }

        // reactions
        if let MessageLike(Rctn(MessageLikeEvent::Original(c))) = event {
            let relates = c.content.relates_to.clone();

            let body = relates.key;
            let relates_id = relates.event_id;

            for message in messages.iter_mut() {
                if message.id == relates_id {
                    let reaction_event =
                        ReactionEvent::new(event.event_id().into(), c.sender.clone());

                    message.reactions.push(Reaction {
                        body,
                        events: vec![reaction_event],
                        list_view: OnceCell::new(),
                    });

                    return MergeResult::Consumed;
                }
            }
        }

        // redactions (don't track the result)
        if let MessageLike(RoomRedaction(RoomRedactionEvent::Original(
            OriginalRoomRedactionEvent {
                redacts: Some(id), ..
            },
        ))) = event
        {
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
        }

        // and finally, continue down the tree, propogating a "missed" result
        for message in messages.iter_mut() {
            if !message.replies.is_empty() {
                let result = Message::apply_timeline_event(&mut message.replies, event, depth + 1);

                if result != MergeResult::Missed {
                    reply_result = result;
                }
            }
        }

        reply_result
    }

    /// Given a binary heap (priority queue) of Receipts, run through the
    /// the messages, popping off receipts and attaching them. This way we
    /// only show a single receipt per user (per reply chain), on the latest
    /// message they have read.
    pub fn apply_receipts(messages: &mut [Message], heap: &mut BinaryHeap<Receipt>) {
        // hold on to a fresh copy of the heap to clone into each reply chain
        let og_heap = heap.clone();

        for message in messages.iter_mut().rev() {
            if !message.replies.is_empty() {
                Message::apply_receipts(&mut message.replies, &mut og_heap.clone());
            }

            while let Some(candidate) = heap.peek() {
                if candidate.timestamp > &message.sent {
                    message
                        .receipts
                        .push(Username::new(candidate.user_id.clone()));

                    heap.pop();
                } else {
                    break;
                }
            }
        }
    }

    pub fn update_senders(&mut self, members: &Vec<RoomMember>) {
        // maybe we use a map, or sorted list at some point to avoid looping
        for member in members {
            self.sender.update(member);

            for reaction in self.reactions.iter_mut() {
                for event in reaction.events.iter_mut() {
                    event.sender.update(member)
                }
            }

            for username in self.receipts.iter_mut() {
                username.update(member);
            }
        }

        for reply in self.replies.iter_mut() {
            reply.update_senders(members);
        }
    }

    // try our best to remove the fomatting that Matrix adds to the top of
    // message reply bodies
    fn remove_reply_header(body: &str) -> &str {
        let mut marker = 0;

        for line in body.split('\n') {
            if line.trim().is_empty() || line.starts_with("> ") {
                marker += line.len() + 1;
            } else {
                break;
            }
        }

        &body[std::cmp::min(marker, body.len())..body.len()]
    }

    pub fn height(&self, width: usize, reply: bool) -> usize {
        let last = self.last_height.get();

        if last.width == width {
            return last.height;
        }

        let message = if reply {
            textwrap::wrap(Message::remove_reply_header(&self.display()), width).len()
        } else {
            textwrap::wrap(&self.display(), width).len()
        };

        // max of 10 lines in a message
        let mut height = cmp::min(message, 10);

        height += 2;

        if !self.receipts.is_empty() {
            height += 1;
        }

        // max of 5 reactions
        height += cmp::min(self.reactions.len(), 5);

        // and then we have to account for the overlfow message
        if message > 10 || self.reactions.len() > 5 {
            height += 1;
        }

        self.last_height.set(LastHeight { width, height });
        height
    }

    // Indent 2 chars.
    fn indent(lines: &mut [Vec<Span>], first: bool) {
        let first_pipe = if first { "╷" } else { "│" };

        for (index, line) in lines.iter_mut().enumerate() {
            let pipe = if index == 0 { first_pipe } else { "│ " };

            line.insert(0, Span::styled(pipe, Style::default().fg(Color::Magenta)));
        }
    }

    pub fn flatten(&self) -> Vec<&Message> {
        let mut messages = vec![self];

        for r in &self.replies {
            messages.append(&mut r.flatten());
        }

        messages
    }

    pub fn to_list_items(&self, width: usize) -> Vec<ListItem> {
        let items: Vec<ratatui::text::Text> = self
            .to_list_items_internal(&self.display(), width)
            .into_iter()
            .map(|spans| ratatui::text::Text::from(Line::from(spans)))
            .collect();

        items.into_iter().rev().map(ListItem::new).collect()
    }

    fn to_list_items_internal(&self, body: &str, width: usize) -> Vec<Vec<Span>> {
        let mut lines = vec![];

        // start with some negative space
        lines.push(vec![Span::from(" ")]);

        // author
        let mut spans = vec![
            Span::styled(self.sender.as_str(), Style::default().fg(Color::Green)),
            Span::from(" "),
            Span::styled(self.pretty_elapsed(), Style::default().fg(Color::DarkGray)),
        ];

        if !self.history.is_empty() {
            spans.push(Span::styled(" (edited)", Style::default().fg(Color::Red)))
        }

        lines.push(spans);

        // the actual message
        let wrapped = textwrap::wrap(body, width);
        let message_overlap = wrapped.len() > 10;

        for l in wrapped.into_iter().take(10) {
            lines.push(vec![Span::styled(l.trim().to_string(), self.style())]);
        }

        // overflow warning
        if message_overlap || self.reactions.len() > 5 {
            lines.push(vec![Span::styled(
                "* overflow: type \"v\" to view entire message",
                Style::default().fg(Color::Red),
            )])
        }

        // receipts
        if !self.receipts.is_empty() {
            let iter = self
                .receipts
                .iter()
                .map(Username::to_string)
                .map(|n| n.split_whitespace().next().unwrap().to_string());

            lines.push(vec![Span::styled(
                format!(
                    "Seen by {}.",
                    pretty_list(limit_list(iter, 4, self.receipts.len(), None))
                ),
                Style::default().fg(Color::DarkGray),
            )])
        }

        // reactions
        for r in self.reactions.iter().take(5) {
            lines.push(vec![Span::styled(
                r.list_view(),
                Style::default().fg(Color::DarkGray),
            )])
        }

        // replies
        for (i, r) in self.replies.iter().enumerate() {
            let reply = r.display();
            let body = Message::remove_reply_header(&reply);
            let mut reply_lines = r.to_list_items_internal(body, width - 2);
            Message::indent(&mut reply_lines, i == 0);
            lines.append(&mut reply_lines);
        }

        lines
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
        let iter = self.events.iter().map(|s| {
            s.sender
                .as_str()
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        });

        pretty_list(limit_list(iter, 5, self.events.len(), None))
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
    pub sender: Username,
}

impl ReactionEvent {
    pub fn new(id: OwnedEventId, sender_id: OwnedUserId) -> ReactionEvent {
        ReactionEvent {
            id,
            sender: Username::new(sender_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::widgets::message::Message;

    #[test]
    fn remove_matrix_headers() {
        let msg = Message::remove_reply_header("> this is a header\n\nAnd this is a message.");
        assert_eq!(msg, "And this is a message.");

        let msg = Message::remove_reply_header("> this is a header\n\n");
        assert_eq!(msg, "");

        let msg = Message::remove_reply_header("> this is a header\n");
        assert_eq!(msg, "");

        let msg = Message::remove_reply_header("> this is a header");
        assert_eq!(msg, "");

        let msg = Message::remove_reply_header("message");
        assert_eq!(msg, "message");
    }
}
