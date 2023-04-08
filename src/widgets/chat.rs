use crate::app::App;
use crate::event::Event;
use crate::handler::Batch;
use crate::matrix::matrix::Matrix;
use crate::spawn::get_text;
use crate::widgets::Action::Typing;
use crate::widgets::EventResult::Consumed;
use crate::widgets::{get_margin, EventResult};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::{Joined, RoomMember};
use once_cell::unsync::OnceCell;
use ruma::events::room::message::MessageType::Text;
use ruma::events::room::message::TextMessageEventContent;
use ruma::events::AnyMessageLikeEvent::Reaction as Rctn;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyTimelineEvent;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent::Original;
use ruma::OwnedEventId;
use sorted_vec::SortedVec;
use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::ops::Deref;
use tui::buffer::Buffer;
use tui::layout::{Constraint, Corner, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{List, ListItem, ListState, StatefulWidget, Widget};

pub struct Chat {
    matrix: Matrix,
    room: Option<Joined>,
    events: SortedVec<OrderedEvent>,
    messages: Vec<Message>,
    members: HashMap<String, String>,
    list_state: Cell<ListState>,
}

// a good PR would be to add Ord to AnyTimelineEvent
pub struct OrderedEvent {
    inner: AnyTimelineEvent,
}

impl OrderedEvent {
    pub fn new(inner: AnyTimelineEvent) -> OrderedEvent {
        OrderedEvent { inner }
    }
}

impl Deref for OrderedEvent {
    type Target = AnyTimelineEvent;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl Ord for OrderedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        self.origin_server_ts().cmp(&other.origin_server_ts())
    }
}

impl PartialOrd for OrderedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.origin_server_ts()
            .partial_cmp(&other.origin_server_ts())
    }
}

impl PartialEq for OrderedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.event_id().eq(other.event_id())
    }
}

impl Eq for OrderedEvent {}

pub struct Message {
    id: OwnedEventId,
    body: String,
    sender: String,
    reactions: Vec<Reaction>,
}

#[derive(Clone)]
pub struct Reaction {
    body: String,
    senders: Vec<String>,
    pretty_senders: OnceCell<String>,
}

impl Reaction {
    // drain the given reactions to create a merged version
    pub fn merge(reactions: &mut Vec<Reaction>) -> Vec<Reaction> {
        let mut merged: Vec<Reaction> = vec![];

        for r in reactions {
            let mut added = false;

            for m in merged.iter_mut() {
                if m.body == r.body {
                    m.senders.append(&mut r.senders);
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
                .senders
                .iter()
                .map(|s| s.split_whitespace().next().unwrap_or_default())
                .collect();

            return match all.len() {
                0 => "".to_string(),
                1 => all.get(0).unwrap().to_string(),
                _ => format!(
                    "{} and {}",
                    all[0..all.len() - 1].join(", "),
                    all.last().unwrap()
                ),
            };
        })
    }
}

impl Message {
    // can we make a brand-new message, just from this event?
    fn try_from(event: &AnyTimelineEvent) -> Option<Self> {
        if let MessageLike(RoomMessage(Original(c))) = event {
            let c = c.clone();

            let body = match c.content.msgtype {
                Text(TextMessageEventContent { body, .. }) => body,
                _ => return None,
            };

            return Some(Message {
                id: c.event_id,
                body,
                sender: c.sender.to_string(),
                reactions: Vec::new(),
            });
        }

        None
    }

    fn merge_into_message_list(messages: &mut Vec<Message>, event: &AnyTimelineEvent) {
        if let MessageLike(Rctn(Original(c))) = event {
            let relates = c.content.relates_to.clone();

            let sender = c.sender.to_string();
            let body = relates.key;
            let event_id = relates.event_id;

            for message in messages.iter_mut() {
                if message.id == event_id {
                    message.reactions.push(Reaction {
                        body,
                        senders: vec![sender],
                        pretty_senders: OnceCell::new(),
                    });
                    return;
                }
            }
        }
    }

    fn update_senders(&mut self, map: &HashMap<String, String>) {
        if let Some(sender) = map.get(&self.sender) {
            self.sender = sender.clone();
        }

        for reaction in self.reactions.iter_mut() {
            for sender in reaction.senders.iter_mut() {
                if let Some(s) = map.get(sender) {
                    *sender = s.clone();
                }
            }
        }
    }

    fn to_list_item(&self, width: usize) -> ListItem {
        use tui::text::Text;

        // author
        let spans = vec![Span::styled(
            self.sender.clone(),
            Style::default().fg(Color::Green),
        )];

        // message
        let mut lines = Text::from(Spans::from(spans));

        let wrapped = textwrap::wrap(&self.body, width);

        for l in wrapped {
            lines.extend(Text::from(l))
        }

        // reactions
        for r in &self.reactions {
            let line = if let Some(emoji) = emojis::get(&r.body) {
                if let Some(shortcode) = emoji.shortcode() {
                    format!("{} ({})", emoji.as_str(), shortcode)
                } else {
                    r.body.clone()
                }
            } else {
                r.body.clone()
            };

            let line = format!("{} {}", line, r.pretty_senders());

            lines.extend(Text::styled(line, Style::default().fg(Color::DarkGray)))
        }

        lines.extend(Text::from(" ".to_string()));

        ListItem::new(lines)
    }
}

impl Chat {
    pub fn new(matrix: Matrix) -> Self {
        Self {
            matrix,
            room: None,
            events: SortedVec::new(),
            messages: vec![],
            members: HashMap::new(),
            list_state: Cell::new(ListState::default()),
        }
    }

    pub fn set_room(&mut self, room: Joined) {
        self.matrix.fetch_messages(room.clone());
        self.room = Some(room);
    }

    fn reset(&mut self) {
        if self.messages.is_empty() {
            return;
        }

        let mut state = self.list_state.take();
        state.select(Some(0));
        self.list_state.set(state);
    }

    pub fn input(&self, app: &App, input: &KeyEvent) -> anyhow::Result<EventResult> {
        match input.code {
            KeyCode::Char('i') => {
                app.events.park();
                let result = get_text();
                app.events.unpark();

                // make sure we redraw the whole app when we come back
                app.sender.send(Event::Redraw)?;

                if let Ok(input) = result {
                    if let Some(input) = input {
                        self.matrix
                            .send_text_message(self.room.clone().unwrap(), input);
                        return Ok(Consumed(Typing));
                    } else {
                        bail!("Ignoring blank message.")
                    }
                } else {
                    bail!("Couldn't read from editor.")
                }
            }
            _ => return Ok(EventResult::Ignored),
        };
    }

    pub fn timeline_event(&mut self, event: AnyTimelineEvent) {
        if self.room.is_none() || event.room_id() != self.room.as_ref().unwrap().room_id() {
            return;
        }

        self.events.push(OrderedEvent::new(event));
        self.messages = make_message_list(&self.events, &self.members);
        self.reset();
    }

    pub fn batch_event(&mut self, batch: Batch) {
        if self.room.is_none() || batch.room.room_id() != self.room.as_ref().unwrap().room_id() {
            return;
        }

        for event in batch.events {
            self.events.push(OrderedEvent::new(event));
        }

        self.messages = make_message_list(&self.events, &self.members);
        self.reset();
    }

    pub fn room_member_event(&mut self, member: RoomMember) {
        self.members
            .insert(member.user_id().into(), member.name().to_string());
        self.messages = make_message_list(&self.events, &self.members);
    }

    pub fn widget(&self) -> ChatWidget {
        ChatWidget { chat: self }
    }
}

pub struct ChatWidget<'a> {
    pub chat: &'a Chat,
}

impl Widget for ChatWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(get_margin(area.width, 80))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let items: Vec<ListItem> = self
            .chat
            .messages
            .iter()
            .map(|m| m.to_list_item(area.width as usize))
            .collect();

        let mut list_state = self.chat.list_state.take();

        let list = List::new(items)
            .highlight_symbol("> ")
            .start_corner(Corner::BottomLeft);

        StatefulWidget::render(list, area, buf, &mut list_state);
        self.chat.list_state.set(list_state)
    }
}

fn make_message_list(
    timeline: &SortedVec<OrderedEvent>,
    members: &HashMap<String, String>,
) -> Vec<Message> {
    let mut messages = vec![];
    let mut modifiers = vec![];

    // split everything into either a starting message, or something that
    // modifies an existing message
    for event in timeline.iter() {
        if let Some(message) = Message::try_from(event) {
            messages.push(message);
        } else {
            modifiers.push(event);
        }
    }

    // now apply all the modifiers to the message list
    for event in modifiers {
        Message::merge_into_message_list(&mut messages, event)
    }

    // update senders to friendly names
    messages.iter_mut().for_each(|m| m.update_senders(members));

    // merge all the reactions
    for m in messages.iter_mut() {
        m.reactions = Reaction::merge(&mut m.reactions);
    }

    // our message list is reversed because we start at the bottom of the
    // window and move up, like any good chat
    messages.reverse();

    messages
}
