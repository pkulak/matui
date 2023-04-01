use crate::matrix::matrix::Matrix;
use crate::widgets::get_margin;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::room::{Joined, RoomMember};
use ruma::events::room::message::MessageType::Text;
use ruma::events::room::message::TextMessageEventContent;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent::Original;
use ruma::OwnedEventId;
use std::cell::Cell;
use tui::buffer::Buffer;
use tui::layout::{Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{List, ListItem, ListState, StatefulWidget, Widget};

pub struct Chat {
    matrix: Matrix,
    room: Option<Joined>,
    messages: Vec<Message>,
    list_state: Cell<ListState>,
}

pub struct Message {
    id: OwnedEventId,
    body: String,
    sender: String,
}

impl Message {
    fn try_from(event: TimelineEvent) -> Option<Self> {
        if let Ok(MessageLike(RoomMessage(Original(c)))) = event.event.deserialize() {
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
            messages: vec![],
            list_state: Cell::new(ListState::default()),
        }
    }

    pub fn set_room(&mut self, room: Joined) {
        self.matrix.fetch_messages(room.clone());
        self.room = Some(room);
    }

    pub fn timeline_event(&mut self, event: TimelineEvent) {
        if let Some(message) = Message::try_from(event) {
            self.messages.push(message);
        }
    }

    pub fn room_member_event(&mut self, member: RoomMember) {
        for msg in self.messages.iter_mut() {
            if &msg.sender == member.user_id() {
                msg.sender = member.name().to_string();
            }
        }
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
        let list = List::new(items).highlight_style(Style::default().bg(Color::DarkGray));
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.chat.list_state.set(list_state)
    }
}
