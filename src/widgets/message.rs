use crate::markdown;
use crate::settings;
use crate::widgets::message::MessageType::File;
use chrono::TimeZone;
use human_bytes::human_bytes;
use std::borrow::Cow;
use std::collections::BinaryHeap;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};
use timeago::Formatter;

use crate::matrix::emoji::pad_emoji;
use crate::matrix::mentions::restore_user_ids;
use crate::matrix::username::Username;
use crate::matrix::{AfterDownload, Matrix};
use crate::spawn::view_text;
use crate::{limit_list, pretty_list};
use chrono::offset::Local;
use matrix_sdk::room::RoomMember;
use matrix_sdk::ruma::events::AnyMessageLikeEvent::RoomMessage;
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk::ruma::events::AnyTimelineEvent::MessageLike;
use matrix_sdk::ruma::events::room::encrypted::Relation::{
    self as EncryptedRelation, Annotation, Replacement, Thread,
};
use matrix_sdk::ruma::events::room::message::MessageType::{
    self, Audio, Image, Notice, Text, Video,
};
use matrix_sdk::ruma::events::room::message::{
    MessageFormat, NoticeMessageEventContent, Relation, TextMessageEventContent,
};
use matrix_sdk::ruma::events::{
    AnySyncMessageLikeEvent, AnySyncTimelineEvent, MessageLikeEvent, SyncMessageLikeEvent,
};
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedUserId};
use matrix_sdk_ui::timeline::{
    EventSendState, EventTimelineItem, MsgLikeKind, TimelineEventItemId,
};
use once_cell::unsync::OnceCell;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::receipts::Receipt;

// A Message is a line in the chat window; what a user would generally
// consider a "message". It has reactions, edits, and is generally in a state
// of constant mutation, as opposed to "events", which come in, in order.
pub struct Message {
    /// The event ID, or the transaction ID until it's sent.
    pub id: TimelineEventItemId,
    pub in_reply_to: Option<OwnedEventId>,
    pub thread_root: Option<OwnedEventId>,
    pub sent: MilliSecondsSinceUnixEpoch,
    pub body: MessageType,
    pub edited: bool,
    pub sender: Username,
    pub reactions: Vec<Reaction>,
    pub replies: Vec<Message>,
    pub thread: Vec<Message>,
    pub receipts: Vec<Username>,
    /// Where sending it, or its latest edit, stands, until it's sent.
    pub sending: Option<Sending>,

    body_lower: OnceCell<String>,
    search_term: OnceCell<String>,
    display_cache: OnceCell<String>,
}

static FORMATTER: OnceLock<Formatter> = OnceLock::new();

fn get_formatter() -> &'static Formatter {
    FORMATTER.get_or_init(|| {
        let mut fmtr = timeago::Formatter::new();
        fmtr.min_unit(timeago::TimeUnit::Minutes);
        fmtr
    })
}

const UNABLE_TO_DECRYPT: &str = "Unable to decrypt this message.";

/// The relation an encrypted event keeps in the clear, which says what it is
/// before it decrypts.
fn encrypted_relation(item: &EventTimelineItem) -> Option<EncryptedRelation> {
    let event = item.original_json()?.deserialize().ok()?;

    let AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomEncrypted(
        SyncMessageLikeEvent::Original(event),
    )) = event
    else {
        return None;
    };

    event.content.relates_to
}

/// A message or edit of ours the server doesn't have yet.
#[derive(Clone, Debug, PartialEq)]
pub enum Sending {
    Pending,
    /// Failed, but matui retries it.
    Failed(String),
    /// Failed for good, holding back everything sent after it.
    Stuck(String),
}

impl Sending {
    fn from_state(state: &EventSendState) -> Option<Self> {
        match state {
            EventSendState::NotSentYet { .. } => Some(Sending::Pending),
            EventSendState::SendingFailed {
                error,
                is_recoverable: true,
            } => Some(Sending::Failed(error.to_string())),
            EventSendState::SendingFailed { error, .. } => Some(Sending::Stuck(error.to_string())),
            EventSendState::Sent { .. } => None,
        }
    }

    // shown under the body
    fn failure(&self) -> Option<String> {
        match self {
            Sending::Pending => None,
            Sending::Failed(error) => Some(format!("Not sent: {}", error)),
            Sending::Stuck(error) => Some(format!(
                "Not sent: {}. Delete it to send later messages.",
                error
            )),
        }
    }
}

impl Message {
    fn new(
        id: TimelineEventItemId,
        sent: MilliSecondsSinceUnixEpoch,
        body: MessageType,
        sender: OwnedUserId,
    ) -> Self {
        Message {
            id,
            in_reply_to: None,
            thread_root: None,
            sent,
            body,
            edited: false,
            sender: Username::new(sender),
            reactions: Vec::new(),
            replies: Vec::new(),
            thread: Vec::new(),
            receipts: Vec::new(),
            sending: None,
            body_lower: OnceCell::new(),
            search_term: OnceCell::new(),
            display_cache: OnceCell::new(),
        }
    }

    /// The message an item of the room's timeline shows, if it shows one.
    pub fn from_item(item: &EventTimelineItem) -> Option<Self> {
        let content = item.content().as_msglike()?;

        let mut thread_root = content.thread_root.clone();

        let (body, edited) = match &content.kind {
            MsgLikeKind::Message(m) => match m.msgtype() {
                body @ (Text(_) | Notice(_) | Image(_) | Video(_) | Audio(_) | File(_)) => {
                    (body.clone(), m.is_edited())
                }
                _ => return None,
            },
            MsgLikeKind::UnableToDecrypt(_) => {
                // only messages get a placeholder, in their thread
                match encrypted_relation(item) {
                    Some(Annotation(_) | Replacement(_)) => return None,
                    Some(Thread(thread)) => thread_root = Some(thread.event_id),
                    _ => {}
                }

                (
                    Notice(NoticeMessageEventContent::plain(UNABLE_TO_DECRYPT)),
                    false,
                )
            }
            _ => return None,
        };

        let mut message = Message::new(
            item.identifier(),
            item.timestamp(),
            body,
            item.sender().to_owned(),
        );
        message.edited = edited;
        message.sending = item
            .send_state()
            .or(item.edit_send_state())
            .and_then(Sending::from_state);
        message.in_reply_to = content.in_reply_to.as_ref().map(|r| r.event_id.clone());
        message.thread_root = thread_root;
        message.reactions = content
            .reactions
            .iter()
            .map(|(key, senders)| Reaction {
                body: key.clone(),
                senders: senders.keys().cloned().map(Username::new).collect(),
                list_view: OnceCell::new(),
            })
            .collect();

        Some(message)
    }

    /// The message a new event from sync shows, for its notification.
    pub fn try_from(event: &AnyTimelineEvent) -> Option<Self> {
        let MessageLike(RoomMessage(MessageLikeEvent::Original(c))) = event else {
            return None;
        };

        if let Some(Relation::Replacement(_)) = c.content.relates_to {
            return None;
        }

        match &c.content.msgtype {
            body @ (Text(_) | Notice(_) | Image(_) | Video(_) | Audio(_) | File(_)) => {
                Some(Message::new(
                    TimelineEventItemId::EventId(c.event_id.clone()),
                    c.origin_server_ts,
                    body.clone(),
                    c.sender.clone(),
                ))
            }
            _ => None,
        }
    }

    /// The event ID, once it's sent.
    pub fn event_id(&self) -> Option<&OwnedEventId> {
        match &self.id {
            TimelineEventItemId::EventId(id) => Some(id),
            TimelineEventItemId::TransactionId(_) => None,
        }
    }

    pub fn sort_order(&self) -> &MilliSecondsSinceUnixEpoch {
        let mut ts = &self.sent;

        if let Some(reply) = self.replies.last() {
            ts = ts.max(&reply.sent);
        }

        if let Some(message) = self.thread.last() {
            ts = ts.max(&message.sent);
        }

        ts
    }

    fn display_body(body: &MessageType) -> String {
        match body {
            Text(TextMessageEventContent { body, .. }) => body.to_string(),
            Notice(content) => content.body.to_string(),
            Image(content) => Self::display_media(
                "Image",
                content.filename(),
                content.caption(),
                content
                    .info
                    .as_ref()
                    .and_then(|info| info.size)
                    .map(human_bytes),
            ),
            Video(content) => Self::display_media(
                "Video",
                content.filename(),
                content.caption(),
                content
                    .info
                    .as_ref()
                    .and_then(|info| info.size)
                    .map(human_bytes),
            ),
            Audio(content) => Self::display_media(
                "Audio",
                content.filename(),
                content.caption(),
                content
                    .info
                    .as_ref()
                    .and_then(|info| info.size)
                    .map(human_bytes),
            ),
            File(content) => Self::display_media(
                "File",
                content.filename(),
                content.caption(),
                content
                    .info
                    .as_ref()
                    .and_then(|info| info.size)
                    .map(human_bytes),
            ),
            _ => "unknown".to_string(),
        }
    }

    fn display_media(
        kind: &str,
        filename: &str,
        caption: Option<&str>,
        size: Option<String>,
    ) -> String {
        let description = match size {
            Some(size) => format!("{kind}: {filename} ({size})"),
            None => filename.to_string(),
        };

        match caption {
            Some(caption) => format!("{description}\n{caption}"),
            None => description,
        }
    }

    pub fn contains_search_term(&self, search_term: &str) -> bool {
        if search_term.is_empty() {
            return true;
        }

        let lowered = &self
            .body_lower
            .get_or_init(|| self.body.body().to_lowercase());

        lowered.contains(search_term)
            || self
                .replies
                .iter()
                .any(|r| r.contains_search_term(search_term))
    }

    // what's shown under a picture
    fn caption(&self) -> Option<&str> {
        match &self.body {
            Image(content) => content.caption(),
            Video(content) => content.caption(),
            _ => None,
        }
        .map(str::trim)
    }

    pub fn display(&self) -> &str {
        self.display_cache
            .get_or_init(|| Message::display_body(&self.body).trim().to_string())
    }

    /// What to edit: the body, with mention pills turned back into user IDs.
    pub fn editable(&self) -> String {
        match &self.body {
            Text(TextMessageEventContent {
                formatted: Some(html),
                ..
            }) if html.format == MessageFormat::Html => {
                restore_user_ids(self.display(), &html.body)
            }
            _ => self.display().to_string(),
        }
    }

    /// Everything about the message, with its earlier bodies, oldest first.
    pub fn display_full(&self, history: &[MessageType]) -> String {
        let date = Local.timestamp_opt(self.sent.as_secs().into(), 0).unwrap();

        let mut ret = format!(
            "Sent {} by {} ({})\n\n",
            date.format("%Y-%m-%d at %I:%M:%S %p"),
            self.sender,
            self.sender.id
        );

        ret.push_str(self.display());
        ret.push_str("\n\n");

        if !self.reactions.is_empty() {
            ret.push_str("### Reactions\n\n");

            for r in &self.reactions {
                for sender in &r.senders {
                    ret.push_str(
                        format!("* {} by {} ({})\n", r.display(), sender, sender.id).as_str(),
                    );
                }
            }

            ret.push('\n');
        }

        if !history.is_empty() {
            ret.push_str("### History\n\n");

            for h in history.iter().rev() {
                ret.push_str("* ");
                ret.push_str(&Message::display_body(h));
                ret.push('\n');
            }
        }

        ret
    }

    pub fn pretty_elapsed(&self) -> String {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let then: u64 = self.sent.as_secs().into();

        get_formatter().convert(Duration::from_secs(now.saturating_sub(then)))
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
            Audio(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            File(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Text(_) => view_text(self.display()),
            _ => {}
        }
    }

    pub fn save(&self, matrix: Matrix) {
        match &self.body {
            Image(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Video(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Audio(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            File(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            _ => {}
        }
    }

    pub fn set_search_term(&self, term: &str) {
        let _ = self.search_term.set(term.to_string());
        self.replies.iter().for_each(|m| m.set_search_term(term));
    }

    /// Put a message under its target in this tree: in the target's thread,
    /// or among its replies, until replies nest too deep to show. The target
    /// moves to the end, since it has something new. Hands the message back
    /// if its target isn't here.
    pub fn nest(
        messages: &mut Vec<Message>,
        message: Message,
        target: &OwnedEventId,
        thread: bool,
        depth: usize,
    ) -> Option<Message> {
        if let Some(i) = messages.iter().position(|m| m.event_id() == Some(target)) {
            if !thread && depth > 3 {
                messages.push(message);
            } else {
                let mut found = messages.remove(i);

                match thread {
                    true => found.thread.push(message),
                    false => found.replies.push(message),
                }

                messages.push(found);
            }

            return None;
        }

        let mut message = message;

        for m in messages.iter_mut() {
            message = Message::nest(&mut m.replies, message, target, thread, depth + 1)?;
            message = Message::nest(&mut m.thread, message, target, thread, depth + 1)?;
        }

        Some(message)
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
                for sender in reaction.senders.iter_mut() {
                    sender.update(member)
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

    // Indent 2 chars.
    fn indent(lines: &mut [Vec<Span>], first: bool) {
        let first_pipe = if first { "╷" } else { "│" };

        for (index, line) in lines.iter_mut().enumerate() {
            let pipe = if index == 0 { first_pipe } else { "│ " };

            line.insert(0, Span::styled(pipe, Style::default().fg(Color::Magenta)));
        }
    }

    pub fn contains_id(&self, id: &TimelineEventItemId) -> bool {
        self.find_by_id(id).is_some()
    }

    pub fn find_by_id(&self, id: &TimelineEventItemId) -> Option<&Message> {
        if &self.id == id {
            return Some(self);
        }

        self.replies.iter().find_map(|r| r.find_by_id(id))
    }

    // how many messages are in this tree, including this one?
    fn tree_len(&self) -> usize {
        1 + self.replies.iter().map(Message::tree_len).sum::<usize>()
    }

    fn highlight<'a>(&self, line: Cow<'a, str>) -> Vec<Span<'a>> {
        let term = match self.search_term.get() {
            Some(t) => t,
            None => return vec![Span::styled(line.to_string(), self.style())],
        };

        let mut result = Vec::new();
        let mut last_end = 0;
        let line_lower = line.to_lowercase();

        for (pos, _) in line_lower.match_indices(term) {
            // pre match
            if pos > last_end {
                let style = self.style();
                let text = match &line {
                    Cow::Borrowed(s) => Cow::Borrowed(&s[last_end..pos]),
                    Cow::Owned(s) => Cow::Owned(s[last_end..pos].to_string()),
                };
                result.push(Span::styled(text, style));
            }

            // actual match
            let style = self
                .style()
                .add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
            let text = match &line {
                Cow::Borrowed(s) => Cow::Borrowed(&s[pos..pos + term.len()]),
                Cow::Owned(s) => Cow::Owned(s[pos..pos + term.len()].to_string()),
            };
            result.push(Span::styled(text, style));
            last_end = pos + term.len();
        }

        if last_end < line.len() {
            let style = self.style();
            let text = match &line {
                Cow::Borrowed(s) => Cow::Borrowed(&s[last_end..]),
                Cow::Owned(s) => Cow::Owned(s[last_end..].to_string()),
            };
            result.push(Span::styled(text, style));
        }

        result.retain(|s| !s.content.is_empty());
        result
    }

    // with picture rows, blank rows for the picture replace the title line,
    // straight under the author
    pub fn to_list_items(
        &self,
        width: usize,
        picture_rows: Option<u16>,
        sidecar: &mut Vec<LineType>,
    ) -> Vec<ListItem<'_>> {
        let items: Vec<ratatui::text::Text> = self
            .to_list_items_internal(self.display(), width, picture_rows, sidecar)
            .into_iter()
            .map(|spans| ratatui::text::Text::from(Line::from(spans)))
            .collect();

        sidecar.reverse();
        items.into_iter().rev().map(ListItem::new).collect()
    }

    fn to_list_items_internal<'a>(
        &'a self,
        body: &'a str,
        width: usize,
        picture_rows: Option<u16>,
        sidecar: &mut Vec<LineType>,
    ) -> Vec<Vec<Span<'a>>> {
        let mut lines = vec![];

        // start with some negative space
        lines.push(vec![Span::from(" ")]);
        sidecar.push(LineType::DeadSpace);

        // author
        let mut author = vec![
            Span::styled(self.sender.as_str(), Style::default().fg(Color::Green)),
            Span::from(" "),
            Span::styled(self.pretty_elapsed(), Style::default().fg(Color::DarkGray)),
        ];

        if self.edited {
            author.push(Span::styled(" (edited)", Style::default().fg(Color::Red)))
        }

        if self.sending == Some(Sending::Pending) {
            author.push(Span::styled(
                " (sending)",
                Style::default().fg(Color::DarkGray),
            ))
        }

        lines.push(author);
        sidecar.push(LineType::MessageStart(self.id.clone()));

        let body = match picture_rows {
            Some(rows) => {
                for _ in 0..rows {
                    lines.push(vec![]);
                    sidecar.push(LineType::MessageContent);
                }

                self.caption().unwrap_or_default()
            }
            None => body,
        };

        // the actual message
        if picture_rows.is_some() && body.is_empty() {
            // a picture with no caption has nothing more to say
        } else if matches!(self.body, Text(_)) && self.search_term.get().is_none() {
            let md_lines = markdown::md_body_to_spans(body, width);
            let limit = settings::message_line_limit();
            let message_overlap = md_lines.len() > limit;

            for line_spans in md_lines.into_iter().take(limit) {
                lines.push(line_spans);
                sidecar.push(LineType::MessageContent);
            }

            if message_overlap || self.reactions.len() > 5 {
                lines.push(vec![Span::styled(
                    "* overflow: type \"v\" to view entire message",
                    Style::default().fg(Color::Red),
                )]);

                sidecar.push(LineType::MessageContent);
            }
        } else {
            let wrapped = textwrap::wrap(body, width);
            let limit = settings::message_line_limit();
            let message_overlap = wrapped.len() > limit;

            for l in wrapped.into_iter().take(limit) {
                let trimmed = match l {
                    Cow::Borrowed(s) => Cow::Borrowed(s.trim()),
                    Cow::Owned(s) => Cow::Owned(s.trim().to_string()),
                };

                lines.push(self.highlight(trimmed));
                sidecar.push(LineType::MessageContent);
            }

            if message_overlap || self.reactions.len() > 5 {
                lines.push(vec![Span::styled(
                    "* overflow: type \"v\" to view entire message",
                    Style::default().fg(Color::Red),
                )]);

                sidecar.push(LineType::MessageContent);
            }
        }

        if let Some(failure) = self.sending.as_ref().and_then(Sending::failure) {
            for line in textwrap::wrap(&failure, width) {
                lines.push(vec![Span::styled(
                    line.into_owned(),
                    Style::default().fg(Color::Red),
                )]);

                sidecar.push(LineType::MessageContent);
            }
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
            )]);

            sidecar.push(LineType::MessageContent);
        }

        // reactions
        for r in self.reactions.iter().take(5) {
            lines.push(vec![Span::styled(
                r.list_view(),
                Style::default().fg(Color::DarkGray),
            )]);

            sidecar.push(LineType::MessageContent);
        }

        // the thread, if this message started one
        if !self.thread.is_empty() {
            let count: usize = self.thread.iter().map(Message::tree_len).sum();
            let noun = if count == 1 { "reply" } else { "replies" };

            lines.push(vec![Span::styled(
                format!("{} {} (enter to view)", count, noun),
                Style::default().fg(Color::DarkGray),
            )]);

            sidecar.push(LineType::MessageContent);
        }

        // replies
        for (i, r) in self.replies.iter().enumerate() {
            let reply = r.display();
            let body = Message::remove_reply_header(reply);
            let mut reply_lines = r.to_list_items_internal(body, width - 2, None, sidecar);
            Message::indent(&mut reply_lines, i == 0);
            lines.append(&mut reply_lines);
        }

        lines
    }
}

// We need to bookkeep exactly what every line represents.
#[derive(PartialEq)]
pub enum LineType {
    MessageStart(TimelineEventItemId),
    MessageContent,
    DeadSpace,
    RoomEvent,
}

// A reaction is a single emoji, from 1 or more users.
#[derive(Clone)]
pub struct Reaction {
    pub body: String,
    pub senders: Vec<Username>,
    pub list_view: OnceCell<String>,
}

impl Reaction {
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
        let iter = self.senders.iter().map(|s| {
            s.as_str()
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string()
        });

        pretty_list(limit_list(iter, 5, self.senders.len(), None))
    }

    // cached to keep allocations out of the render loop
    pub fn list_view(&self) -> &str {
        self.list_view
            .get_or_init(|| format!("{} {}", self.display(), self.pretty_senders()))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::widgets::message::Message;
    use matrix_sdk::ruma::events::room::message::{MessageType::Text, TextMessageEventContent};
    use matrix_sdk::ruma::exports::serde_json::{self, Value, json};
    use matrix_sdk::ruma::{
        MilliSecondsSinceUnixEpoch, OwnedEventId, owned_event_id, owned_user_id,
    };
    use matrix_sdk_ui::timeline::TimelineEventItemId;

    pub(crate) fn text_event(id: &str, ts: u64, body: &str) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": { "msgtype": "m.text", "body": body }
        })
    }

    // a message in a thread; a reply target makes it a genuine reply
    pub(crate) fn thread_event(id: &str, ts: u64, root: &str, reply_to: Option<&str>) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": {
                "msgtype": "m.text",
                "body": "in the thread",
                "m.relates_to": {
                    "rel_type": "m.thread",
                    "event_id": root,
                    "is_falling_back": reply_to.is_none(),
                    "m.in_reply_to": { "event_id": reply_to.unwrap_or(root) }
                }
            }
        })
    }

    // an image from mxc://example.org/image, with whatever info is given
    pub(crate) fn image_event(
        body: &str,
        filename: Option<&str>,
        size: Option<u64>,
        thumbnail: Option<&str>,
        dimensions: Option<(u32, u32)>,
    ) -> Value {
        let mut info = json!({});

        if let Some(size) = size {
            info["size"] = json!(size);
        }

        if let Some(thumbnail) = thumbnail {
            info["thumbnail_url"] = json!(thumbnail);
        }

        if let Some((w, h)) = dimensions {
            info["w"] = json!(w);
            info["h"] = json!(h);
        }

        json!({
            "type": "m.room.message",
            "event_id": "$image:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 1000,
            "content": {
                "msgtype": "m.image",
                "body": body,
                "filename": filename,
                "url": "mxc://example.org/image",
                "info": info
            }
        })
    }

    pub(crate) fn reaction_event(id: &str, ts: u64, target: &str, key: &str) -> Value {
        json!({
            "type": "m.reaction",
            "event_id": id,
            "sender": "@alice:example.org",
            "origin_server_ts": ts,
            "content": {
                "m.relates_to": {
                    "rel_type": "m.annotation",
                    "event_id": target,
                    "key": key
                }
            }
        })
    }

    pub(crate) fn reply_event(id: &str, ts: u64, target: &str, body: &str) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": "@bob:example.org",
            "origin_server_ts": ts,
            "content": {
                "msgtype": "m.text",
                "body": body,
                "m.relates_to": { "m.in_reply_to": { "event_id": target } }
            }
        })
    }

    pub(crate) fn join_event(id: &str, ts: u64, name: &str) -> Value {
        json!({
            "type": "m.room.member",
            "event_id": id,
            "sender": "@bob:example.org",
            "state_key": "@bob:example.org",
            "origin_server_ts": ts,
            "content": { "membership": "join", "displayname": name }
        })
    }

    fn test_message(id: OwnedEventId) -> Message {
        Message::new(
            TimelineEventItemId::EventId(id),
            MilliSecondsSinceUnixEpoch::now(),
            Text(TextMessageEventContent::plain("body")),
            owned_user_id!("@alice:example.org"),
        )
    }

    fn message(mut event: Value) -> Message {
        event["room_id"] = json!("!room:example.org");
        Message::try_from(&serde_json::from_value(event).unwrap()).unwrap()
    }

    #[test]
    fn displays_media_captions_with_the_filename() {
        let captioned = message(image_event(
            "Look at this dog",
            Some("dog.jpg"),
            None,
            None,
            None,
        ));
        assert_eq!(captioned.display(), "dog.jpg\nLook at this dog");

        let plain = message(image_event("dog.jpg", None, None, None, None));
        assert_eq!(plain.display(), "dog.jpg");
    }

    #[test]
    fn displays_notices() {
        let notice = message(json!({
            "type": "m.room.message",
            "event_id": "$notice:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 1000,
            "content": { "msgtype": "m.notice", "body": "hello" }
        }));

        assert_eq!(notice.display(), "hello");
    }

    #[test]
    fn finds_replies_recursively() {
        let reply_id = TimelineEventItemId::EventId(owned_event_id!("$reply:example.org"));
        let mut message = test_message(owned_event_id!("$root:example.org"));
        message
            .replies
            .push(test_message(owned_event_id!("$reply:example.org")));

        assert_eq!(
            message.find_by_id(&reply_id).map(|m| &m.id),
            Some(&reply_id)
        );
        assert!(message.contains_id(&reply_id));
    }

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
