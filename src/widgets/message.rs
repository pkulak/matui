use crate::markdown;
use crate::selection::{self, BodyRow};
use crate::settings;
use crate::widgets::message::MessageType::File;
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime};
use human_bytes::human_bytes;
use std::borrow::Cow;
use std::collections::BinaryHeap;
use std::ops::Range;

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
use matrix_sdk::ruma::events::poll::start::PollKind;
use matrix_sdk::ruma::events::room::encrypted::Relation::{
    self as EncryptedRelation, Annotation, Replacement, Thread,
};
use matrix_sdk::ruma::events::room::message::MessageType::{
    self, Audio, Emote, Image, Location, Notice, Text, Video,
};
use matrix_sdk::ruma::events::room::message::{
    ImageMessageEventContent, LocationMessageEventContent, MessageFormat,
    NoticeMessageEventContent, Relation, TextMessageEventContent,
};
use matrix_sdk::ruma::events::sticker::StickerEventContent;
use matrix_sdk::ruma::events::{
    AnySyncMessageLikeEvent, AnySyncTimelineEvent, MessageLikeEvent, SyncMessageLikeEvent,
};
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedUserId};
use matrix_sdk_ui::timeline::{
    EventSendState, EventTimelineItem, MsgLikeKind, PollResult, TimelineEventItemId,
};
use once_cell::unsync::OnceCell;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

/// How far a user has read: the time of the event their receipt is on.
#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Clone)]
pub struct Receipt {
    pub read_to: MilliSecondsSinceUnixEpoch,
    pub user_id: OwnedUserId,
}

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
    /// How many messages are in the thread this message started.
    pub thread_replies: u32,
    pub receipts: Vec<Username>,
    /// Where sending it, or its latest edit, stands, until it's sent.
    pub sending: Option<Sending>,

    body_lower: OnceCell<String>,
    search_term: OnceCell<String>,
    display_cache: OnceCell<String>,
    links_cache: OnceCell<Vec<Range<usize>>>,
    reaction_lines: OnceCell<Vec<String>>,
}

/// A timestamp in local time.
pub(crate) fn local_time(ts: MilliSecondsSinceUnixEpoch) -> DateTime<Local> {
    let time = DateTime::from_timestamp(ts.as_secs().into(), 0).unwrap_or_default();
    time.with_timezone(&Local)
}

/// When a message was sent, under the marker for `day`: the time, with the
/// date too when it's another day, as for a message someone replied to later.
fn sent_label(sent: NaiveDateTime, day: NaiveDate, today: NaiveDate) -> String {
    let time = sent.format("%-I:%M %p");
    let date = sent.date();

    if date == day {
        time.to_string()
    } else if (today - date).num_days() < 7 {
        format!("{} {time}", sent.format("%a"))
    } else if date.year() == today.year() {
        format!("{}, {time}", sent.format("%b %-d"))
    } else {
        format!("{}, {time}", sent.format("%b %-d, %Y"))
    }
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

/// A sticker, as the image it is.
fn sticker_image(sticker: &StickerEventContent) -> MessageType {
    let image = ImageMessageEventContent::new(sticker.body.clone(), sticker.source.clone().into())
        .info(Some(Box::new(sticker.info.clone())));

    Image(image)
}

/// A poll's question, then its answers with their votes, which an
/// undisclosed poll hides until it ends.
fn poll_text(poll: &PollResult) -> String {
    let ended = poll.end_time.is_some();
    let counted = ended || matches!(poll.kind, PollKind::Disclosed);

    let mut text = format!("📊 {}", poll.question);

    for answer in &poll.answers {
        text.push('\n');
        text.push_str(&answer.text);

        if counted {
            let votes = poll.votes.get(&answer.id).map_or(0, Vec::len);
            text.push_str(&format!(" · {votes}"));
        }
    }

    text.push_str(match ended {
        true => "\nEnded",
        false => "\n(open in another client to vote)",
    });

    text
}

/// The latitude and longitude in a geo URI, such as `geo:51.5,-0.12;u=35`,
/// to five places, about a metre.
fn coordinates(uri: &str) -> Option<(f64, f64)> {
    let point = uri.strip_prefix("geo:")?.split(';').next()?;
    let mut parts = point.split(',');
    let round = |c: &str| c.parse::<f64>().ok().map(|c| (c * 1e5).round() / 1e5);

    Some((round(parts.next()?)?, round(parts.next()?)?))
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
            thread_replies: 0,
            receipts: Vec::new(),
            sending: None,
            body_lower: OnceCell::new(),
            search_term: OnceCell::new(),
            display_cache: OnceCell::new(),
            links_cache: OnceCell::new(),
            reaction_lines: OnceCell::new(),
        }
    }

    /// The message an item of the room's timeline shows, if it shows one.
    pub fn from_item(item: &EventTimelineItem) -> Option<Self> {
        let content = item.content().as_msglike()?;

        let mut thread_root = content.thread_root.clone();

        let (body, edited) = match &content.kind {
            MsgLikeKind::Message(m) => match m.msgtype() {
                body @ (Text(_) | Emote(_) | Notice(_) | Image(_) | Video(_) | Audio(_)
                | File(_) | Location(_)) => (body.clone(), m.is_edited()),
                _ => return None,
            },
            MsgLikeKind::Sticker(s) => (sticker_image(s.content()), false),
            MsgLikeKind::Poll(poll) => (
                Notice(NoticeMessageEventContent::plain(poll_text(&poll.results()))),
                poll.is_edit(),
            ),
            MsgLikeKind::UnableToDecrypt(_) => {
                // only messages get a placeholder; a thread's stays out of
                // the room
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
        message.thread_replies = content.thread_summary.as_ref().map_or(0, |s| s.num_replies);
        message.reactions = content
            .reactions
            .iter()
            .map(|(key, senders)| Reaction {
                body: key.clone(),
                senders: senders.keys().cloned().map(Username::new).collect(),
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
            body @ (Text(_) | Emote(_) | Notice(_) | Image(_) | Video(_) | Audio(_) | File(_)
            | Location(_)) => Some(Message::new(
                TimelineEventItemId::EventId(c.event_id.clone()),
                c.origin_server_ts,
                body.clone(),
                c.sender.clone(),
            )),
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

        ts
    }

    fn display_body(body: &MessageType) -> String {
        match body {
            Text(TextMessageEventContent { body, .. }) => body.to_string(),
            Emote(content) => content.body.to_string(),
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
            Location(content) => Self::display_location(content),
            _ => "unknown".to_string(),
        }
    }

    // a pin and the place's name, then the place on a map
    fn display_location(content: &LocationMessageEventContent) -> String {
        let Some((lat, lon)) = coordinates(&content.geo_uri) else {
            return content.body.clone();
        };

        // the body is often just the geo URI and a time
        let name = content
            .location
            .as_ref()
            .and_then(|l| l.description.as_deref())
            .unwrap_or("Location");

        format!(
            "📍 {name}\nhttps://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=16/{lat}/{lon}"
        )
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

    // what's shown under a picture: display() after its title line, so the
    // caption's text can be selected
    fn caption(&self) -> Option<&str> {
        match &self.body {
            Image(_) | Video(_) => self.display().split_once('\n').map(|(_, c)| c.trim()),
            _ => None,
        }
    }

    pub fn display(&self) -> &str {
        self.display_cache
            .get_or_init(|| Message::display_body(&self.body).trim().to_string())
    }

    /// Where display()'s links are.
    pub fn links(&self) -> &[Range<usize>] {
        self.links_cache
            .get_or_init(|| selection::links(self.display()))
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
        let date = local_time(self.sent);

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
                        format!("* {} by {} ({})\n", pad_emoji(&r.body), sender, sender.id)
                            .as_str(),
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

    /// The reactions under the message, a line for each set of people with
    /// every emoji they reacted with, in the order they first came. Cached to
    /// keep allocations out of the render loop.
    fn reaction_lines(&self) -> &[String] {
        self.reaction_lines.get_or_init(|| {
            let mut groups: Vec<(Vec<String>, &[Username])> = Vec::new();

            for r in &self.reactions {
                let same = |senders: &[Username]| {
                    senders.len() == r.senders.len()
                        && r.senders.iter().all(|s| senders.contains(s))
                };

                match groups.iter_mut().find(|(_, senders)| same(senders)) {
                    Some((emojis, _)) => emojis.push(pad_emoji(&r.body)),
                    None => groups.push((vec![pad_emoji(&r.body)], &r.senders)),
                }
            }

            groups
                .into_iter()
                .map(|(emojis, senders)| format!("{} {}", emojis.join(" "), first_names(senders)))
                .collect()
        })
    }

    pub fn style(&self) -> Style {
        match &self.body {
            Text(_) => Style::default(),
            Emote(_) => Style::default().add_modifier(Modifier::ITALIC),
            _ => Style::default().fg(Color::Blue),
        }
    }

    pub fn open(&self, matrix: Matrix) {
        match &self.body {
            Image(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            Video(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            Audio(_) => matrix.download_content(self.body.clone(), AfterDownload::View),
            File(_) => matrix.download_content(self.body.clone(), AfterDownload::Save),
            Text(_) | Emote(_) | Location(_) => view_text(self.display()),
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

    /// Put a message among its target's replies in this tree, until replies
    /// nest too deep to show. The target moves to the end, since it has
    /// something new. Hands the message back if its target isn't here.
    pub fn nest(
        messages: &mut Vec<Message>,
        message: Message,
        target: &OwnedEventId,
        depth: usize,
    ) -> Option<Message> {
        if let Some(i) = messages.iter().position(|m| m.event_id() == Some(target)) {
            if depth > 3 {
                messages.push(message);
            } else {
                let mut found = messages.remove(i);
                found.replies.push(message);
                messages.push(found);
            }

            return None;
        }

        let mut message = message;

        for m in messages.iter_mut() {
            message = Message::nest(&mut m.replies, message, target, depth + 1)?;
        }

        Some(message)
    }

    /// Given a binary heap (priority queue) of Receipts, run through the
    /// the messages, popping off receipts and attaching them. This way we
    /// only show a single receipt per user (per reply chain), on the latest
    /// message they have read. Reply chains move up out of time order, so
    /// each gets its own.
    pub fn apply_receipts(messages: &mut [Message], heap: &mut BinaryHeap<Receipt>) {
        // hold on to a fresh copy of the heap to clone into each reply chain
        let og_heap = heap.clone();

        for message in messages.iter_mut().rev() {
            if !message.replies.is_empty() {
                Message::apply_receipts(&mut message.replies, &mut og_heap.clone());
            }

            while let Some(candidate) = heap.peek() {
                if candidate.read_to >= message.sent {
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

    fn highlight<'a>(&self, line: Cow<'a, str>) -> Vec<Span<'a>> {
        let term = match self.search_term.get() {
            Some(t) => t,
            None => return vec![Span::styled(line, self.style())],
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
    pub fn to_list_items<'a>(
        &'a self,
        width: usize,
        picture_rows: Option<u16>,
        sidecar: &mut Vec<LineType>,
        bodies: &mut Vec<Option<BodyRow<'a, TimelineEventItemId>>>,
    ) -> Vec<ListItem<'a>> {
        // the day of its marker, which is its latest reply's
        let day = local_time(*self.sort_order()).date_naive();
        let lines = self.to_list_items_internal(self.display(), width, picture_rows, day, sidecar);

        // after the gutters go in, so the columns count them
        bodies.extend(lines.iter().map(|spans| self.body_row(spans)));

        sidecar.reverse();
        bodies.reverse();

        lines
            .into_iter()
            .rev()
            .map(|spans| ListItem::new(Line::from(spans)))
            .collect()
    }

    // the row of this message's text, or a reply's, that the spans draw.
    // Every body is a slice of its message's display(), and nothing else on
    // a row is.
    fn body_row(&self, spans: &[Span]) -> Option<BodyRow<'_, TimelineEventItemId>> {
        BodyRow::new(self.id.clone(), self.display(), self.links(), spans)
            .or_else(|| self.replies.iter().find_map(|r| r.body_row(spans)))
    }

    fn to_list_items_internal<'a>(
        &'a self,
        body: &'a str,
        width: usize,
        picture_rows: Option<u16>,
        day: NaiveDate,
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
            Span::styled(
                sent_label(
                    local_time(self.sent).naive_local(),
                    day,
                    Local::now().date_naive(),
                ),
                Style::default().fg(Color::DarkGray),
            ),
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

            if message_overlap || self.reaction_lines().len() > 5 {
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

            if message_overlap || self.reaction_lines().len() > 5 {
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
        for r in self.reaction_lines().iter().take(5) {
            lines.push(vec![Span::styled(
                r.as_str(),
                Style::default().fg(Color::DarkGray),
            )]);

            sidecar.push(LineType::MessageContent);
        }

        // the thread, if this message started one
        if self.thread_replies > 0 {
            let count = self.thread_replies;
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
            let mut reply_lines = r.to_list_items_internal(body, width - 2, None, day, sidecar);
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
}

// up to 5 first names, then how many others
fn first_names(senders: &[Username]) -> String {
    let iter = senders.iter().map(|s| {
        s.as_str()
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    });

    pretty_list(limit_list(iter, 5, senders.len(), None))
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::matrix::username::Username;
    use crate::widgets::message::{Message, Reaction, sent_label};
    use chrono::NaiveDate;
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
    fn sent_labels_add_the_day_when_its_not_the_markers() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        let at = |date: NaiveDate| date.and_hms_opt(14, 41, 0).unwrap();
        let ago = |days| today - chrono::Days::new(days);

        assert_eq!(sent_label(at(today), today, today), "2:41 PM");
        assert_eq!(sent_label(at(ago(3)), ago(3), today), "2:41 PM");

        // replied to on a later day
        assert_eq!(sent_label(at(ago(1)), today, today), "Sat 2:41 PM");
        assert_eq!(sent_label(at(ago(6)), today, today), "Mon 2:41 PM");
        assert_eq!(sent_label(at(ago(7)), today, today), "Nov 8, 2:41 PM");
        assert_eq!(
            sent_label(at(ago(400)), today, today),
            "Oct 11, 2025, 2:41 PM"
        );

        let morning = today.and_hms_opt(9, 5, 0).unwrap();
        assert_eq!(sent_label(morning, today, today), "9:05 AM");
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

    fn location_event(content: Value) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": "$location:example.org",
            "sender": "@alice:example.org",
            "origin_server_ts": 1000,
            "content": content
        })
    }

    #[test]
    fn displays_locations_with_a_map_link() {
        let described = message(location_event(json!({
            "msgtype": "m.location",
            "body": "Location geo:51.5,-0.12;u=35 at 2026-11-15T12:00:00Z",
            "geo_uri": "geo:51.5,-0.12;u=35",
            "org.matrix.msc3488.location": {
                "uri": "geo:51.5,-0.12;u=35",
                "description": "Big Ben"
            }
        })));
        assert_eq!(
            described.display(),
            "📍 Big Ben\nhttps://www.openstreetmap.org/?mlat=51.5&mlon=-0.12#map=16/51.5/-0.12"
        );
        assert_eq!(described.links().len(), 1);

        let pinned = message(location_event(json!({
            "msgtype": "m.location",
            "body": "Location geo:45.5689773824598,-122.631459536459 at 2026-11-15T12:00:00Z",
            "geo_uri": "geo:45.5689773824598,-122.631459536459"
        })));
        assert_eq!(
            pinned.display(),
            "📍 Location\nhttps://www.openstreetmap.org/?mlat=45.56898&mlon=-122.63146#map=16/45.56898/-122.63146"
        );

        let broken = message(location_event(json!({
            "msgtype": "m.location",
            "body": "Somewhere",
            "geo_uri": "geo:nowhere"
        })));
        assert_eq!(broken.display(), "Somewhere");
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
    fn groups_emojis_with_the_same_people() {
        let mut message = test_message(owned_event_id!("$root:example.org"));
        let reaction = |body: &str, senders: &[&str]| Reaction {
            body: body.to_string(),
            senders: senders
                .iter()
                .map(|&s| Username::new(s.try_into().unwrap()))
                .collect(),
        };

        message.reactions = vec![
            reaction("👍", &["@alice:example.org", "@bob:example.org"]),
            reaction("👀", &["@carol:example.org"]),
            reaction("❤️", &["@bob:example.org", "@alice:example.org"]),
            reaction("✅", &["@carol:example.org"]),
            reaction("😆", &["@bob:example.org"]),
        ];

        assert_eq!(
            message.reaction_lines(),
            [
                "👍 ❤️  @alice:example.org and @bob:example.org",
                "👀 ✅ @carol:example.org",
                "😆 @bob:example.org",
            ]
        );
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
