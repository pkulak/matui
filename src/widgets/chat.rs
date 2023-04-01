use crate::matrix::matrix::{Matrix, MessageEvent};
use crate::widgets::get_margin;
use log::info;
use matrix_sdk::deserialized_responses::TimelineEvent;
use matrix_sdk::room::{Joined, Messages};
use std::cell::Cell;
use std::sync::mpsc::{channel, Receiver, Sender};
use tui::buffer::Buffer;
use tui::layout::{Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{List, ListItem, ListState, StatefulWidget, Widget};

pub struct Chat {
    matrix: Matrix,
    room: Option<Joined>,
    messages: Option<Messages>,
    list_state: Cell<ListState>,
    sender: Sender<MessageEvent>,
    receiver: Receiver<MessageEvent>,
}

impl Chat {
    pub fn new(matrix: Matrix) -> Self {
        let (sender, receiver) = channel();

        Self {
            matrix,
            room: None,
            messages: None,
            list_state: Cell::new(ListState::default()),
            sender,
            receiver,
        }
    }

    pub fn set_room(&mut self, room: Joined) {
        self.matrix
            .fetch_messages(room.clone(), self.sender.clone());

        self.room = Some(room);
    }

    pub fn tick(&mut self) {
        if let Ok(MessageEvent::FetchCompleted(msg)) = self.receiver.try_recv() {
            let total = msg.chunk.len();
            self.messages = Some(msg);
            info!("loaded {} messages", total);
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

        let items: Vec<ListItem> = if let Some(msg) = &self.chat.messages {
            msg.chunk.iter().map(make_list_item).collect()
        } else {
            vec![]
        };

        let mut list_state = self.chat.list_state.take();
        let list = List::new(items).highlight_style(Style::default().bg(Color::DarkGray));
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.chat.list_state.set(list_state)
    }
}

fn make_list_item(m: &TimelineEvent) -> ListItem {
    ListItem::new(m.event.json().to_string())
}
