use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::{close, consumed};
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::Room;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget,
};
use std::cell::Cell;

use crate::widgets::get_margin;
use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::Consumed;

use super::EventResult;

pub struct Rooms {
    pub textinput: TextInput,
    pub room: Vec<DecoratedRoom>,
    pub list_state: Cell<ListState>,
}

impl Rooms {
    pub fn new(matrix: Matrix, current: Option<Room>) -> Self {
        let mut rooms = matrix.fetch_rooms();
        sort_rooms(&mut rooms);

        // if the current room is at the top, put it at the bottom
        if let Some(current) = current {
            if rooms.len() > 1 && rooms.first().unwrap().inner.room_id() == current.room_id() {
                let first = rooms.remove(0);
                rooms.push(first);
            }
        }

        let mut ret = Self {
            textinput: TextInput::new("Search".to_string(), true, false),
            room: rooms,
            list_state: Cell::new(ListState::default()),
        };

        ret.reset();
        ret
    }

    pub fn widget(&self) -> RoomsWidget<'_> {
        RoomsWidget { rooms: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        match input.code {
            KeyCode::Esc => close!(),
            KeyCode::Down => {
                self.next();
                consumed!()
            }
            KeyCode::Up => {
                self.previous();
                consumed!()
            }
            KeyCode::Enter => {
                if let Some(selected_room) = self.selected_room() {
                    let room = selected_room.inner();
                    Consumed(Box::new(|app| {
                        app.select_room(room);
                        app.close_popup();
                    }))
                } else {
                    EventResult::Ignored
                }
            }
            _ => {
                if let Consumed(_) = self.textinput.key_event(input) {
                    self.reset();
                    consumed!()
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn next(&mut self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i >= self.filtered_rooms().len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn previous(&mut self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i == 0 {
                    self.filtered_rooms().len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn reset(&mut self) {
        let mut state = self.list_state.take();
        state.select(Some(0));
        self.list_state.set(state);
    }

    fn filtered_rooms(&self) -> Vec<&DecoratedRoom> {
        let pattern = self.textinput.value.to_lowercase();

        self.room
            .iter()
            .filter(|j| j.name.to_string().to_lowercase().contains(pattern.as_str()))
            .collect()
    }

    fn selected_room(&self) -> Option<DecoratedRoom> {
        let filtered_rooms = self.filtered_rooms();

        if filtered_rooms.is_empty() {
            return None;
        }

        match self.list_state.take().selected() {
            Some(i) => Some(filtered_rooms[i].clone()),
            None => Some(filtered_rooms[0].clone()),
        }
    }
}

pub struct RoomsWidget<'a> {
    pub rooms: &'a Rooms,
}

impl Widget for RoomsWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(2)
            .horizontal_margin(get_margin(area.width, 60))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        // Render the main block
        let block = Block::default()
            .title("Rooms")
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Reset))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        block.render(area, buf);

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(2)
            .horizontal_margin(2)
            .constraints([Constraint::Length(3), Constraint::Percentage(100)].as_ref())
            .split(area);

        self.rooms.textinput.widget().render(splits[0], buf);

        let items: Vec<ListItem> = self
            .rooms
            .filtered_rooms()
            .into_iter()
            .map(make_list_item)
            .collect();

        let area = Layout::default()
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[1])[0];

        let mut list_state = self.rooms.list_state.take();
        let list = List::new(items).highlight_symbol("> ");
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.rooms.list_state.set(list_state)
    }
}

fn make_list_item(room: &DecoratedRoom) -> ListItem<'_> {
    let name = room.name.to_string();
    let unread = room.unread_count();
    let highlights = room.highlight_count();

    let mut spans = vec![Span::from(name)];

    if unread > 0 {
        spans.push(Span::styled(
            format!(" ({})", unread),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if highlights > 0 {
        spans.push(Span::styled(
            format!(" ({})", highlights),
            Style::default().fg(Color::Green),
        ));
    }

    let mut lines = Text::from(Line::from(spans));

    let spans = if room.last_sender.is_none() || room.last_message.is_none() {
        vec![Span::styled("", Style::default().fg(Color::DarkGray))]
    } else {
        vec![Span::styled(
            format!(
                "{}: {}",
                room.last_sender.clone().unwrap_or_default(),
                room.last_message.clone().unwrap_or_default()
            ),
            Style::default().fg(Color::DarkGray),
        )]
    };

    lines.extend(Text::from(Line::from(spans)));

    ListItem::new(lines)
}

pub fn sort_rooms(rooms: &mut [DecoratedRoom]) {
    rooms.sort_by_key(|r| (r.unread_count(), r.last_ts));
    rooms.reverse()
}
