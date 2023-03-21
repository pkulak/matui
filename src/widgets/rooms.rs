use crate::matrix::MatrixEvent;
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::Joined;
use std::cell::Cell;
use std::sync::mpsc::Sender;
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans};
use tui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget};

use crate::widgets::textinput::TextInput;
use crate::widgets::{get_margin, KeyEventing};

pub struct Rooms {
    pub textinput: TextInput,
    pub joined: Vec<Joined>,
    pub list_state: Cell<ListState>,
    pub sender: Sender<MatrixEvent>,
}

impl Rooms {
    pub fn new(joined: Vec<Joined>, sender: Sender<MatrixEvent>) -> Self {
        let mut joined: Vec<Joined> = joined.into_iter().filter(|r| !r.is_tombstoned()).collect();

        sort_rooms(&mut joined);

        Self {
            textinput: TextInput::new("Search".to_string(), true, false),
            joined,
            list_state: Cell::new(ListState::default()),
            sender,
        }
    }

    pub fn widget(&self) -> RoomsWidget {
        RoomsWidget { rooms: self }
    }

    pub fn input(&mut self, input: &KeyEvent) {
        match input.code {
            KeyCode::Down => self.next(),
            KeyCode::Up => self.previous(),
            KeyCode::Enter => self
                .sender
                .send(MatrixEvent::RoomSelected(self.selected_room()))
                .expect("could not send room selected event"),
            _ => {
                if (&mut self.textinput).input(input) {
                    self.unselect()
                }
            }
        }
    }

    fn next(&mut self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i >= self.joined.len() - 1 {
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
                    self.joined.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn unselect(&mut self) {
        let mut state = self.list_state.take();
        state.select(None);
        self.list_state.set(state);
    }

    fn filtered_rooms(&self) -> Vec<&Joined> {
        let pattern = self.textinput.value.to_lowercase();

        self.joined
            .iter()
            .filter(|j| {
                j.name()
                    .unwrap_or_default()
                    .to_lowercase()
                    .contains(pattern.as_str())
            })
            .collect()
    }

    fn selected_room(&self) -> Joined {
        match self.list_state.take().selected() {
            Some(i) => self.filtered_rooms()[i].clone(),
            None => self.filtered_rooms()[0].clone(),
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
            .style(Style::default().bg(Color::Black))
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
            .map(|j| make_list_item(j))
            .collect();

        let area = Layout::default()
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[1])[0];

        let mut list_state = self.rooms.list_state.take();
        let list = List::new(items).highlight_style(Style::default().bg(Color::DarkGray));
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.rooms.list_state.set(list_state)
    }
}

fn make_list_item(joined: &Joined) -> ListItem {
    let name = joined.name().unwrap_or("Unknown".to_string());
    let unread = joined.unread_notification_counts().notification_count;
    let highlights = joined.unread_notification_counts().highlight_count;

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

    ListItem::new(Spans::from(spans))
}

fn sort_rooms(rooms: &mut Vec<Joined>) {
    rooms.sort_by_key(|r| {
        (
            -(r.unread_notification_counts().notification_count as i64),
            r.name().unwrap_or_default(),
        )
    });
}
