use crate::app::App;
use crate::event::Event;
use crate::matrix::matrix::Matrix;
use crate::spawn::get_text;
use crate::widgets::Action::Typing;
use crate::widgets::EventResult::Consumed;
use crate::widgets::{get_margin, EventResult};
use anyhow::bail;
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::{Joined, RoomMember};
use ruma::events::room::message::MessageType::Text;
use ruma::events::room::message::TextMessageEventContent;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyTimelineEvent;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent::Original;
use ruma::OwnedEventId;
use std::cell::Cell;
use std::collections::HashMap;
use tui::buffer::Buffer;
use tui::layout::{Constraint, Corner, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{List, ListItem, ListState, StatefulWidget, Widget};

pub struct Chat {
    matrix: Matrix,
    room: Option<Joined>,
    events: Vec<AnyTimelineEvent>,
    messages: Vec<Message>,
    members: HashMap<String, String>,
    list_state: Cell<ListState>,
}

#[allow(dead_code)]
pub struct Message {
    id: OwnedEventId,
    body: String,
    sender: String,
}

impl Message {
    // can we make a brand-new message, just from this event?
    fn try_from(event: &AnyTimelineEvent) -> Option<Self> {
        if let MessageLike(RoomMessage(Original(c))) = event.clone() {
            let body = match c.content.msgtype {
                Text(TextMessageEventContent { body, .. }) => body,
                _ => return None,
            };

            return Some(Message {
                id: c.event_id,
                body,
                sender: c.sender.to_string(),
            });
        }

        None
    }

    fn to_list_item(&self, width: usize) -> ListItem {
        use tui::text::Text;

        let spans = vec![Span::styled(
            self.sender.clone(),
            Style::default().fg(Color::Green),
        )];

        let mut lines = Text::from(Spans::from(spans));

        let wrapped = textwrap::wrap(&self.body, width);

        for l in wrapped {
            lines.extend(Text::from(l))
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
            events: vec![],
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

    pub fn timeline_event(&mut self, event: &AnyTimelineEvent) {
        if self.room.is_none() || event.room_id() != self.room.as_ref().unwrap().room_id() {
            return;
        }

        self.events.push(event.clone());
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
            .vertical_margin(2)
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
    timeline: &Vec<AnyTimelineEvent>,
    members: &HashMap<String, String>,
) -> Vec<Message> {
    let mut messages = vec![];

    for event in timeline {
        if let Some(mut message) = Message::try_from(event) {
            if members.contains_key(&message.sender) {
                message.sender = members.get(&message.sender).unwrap().to_string();
            }

            messages.push(message);
        }
    }

    // our message list is reversed because we start at the bottom of the
    // window and move up, like any good chat list
    messages.reverse();

    messages
}
