use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crossterm::event::{KeyCode, KeyEvent};
use std::cell::Cell;
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::text::{Span, Spans, Text};
use tui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget};

use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{get_margin, Action, EventResult, KeyEventing};

pub struct Rooms {
    pub textinput: TextInput,
    pub joined: Vec<DecoratedRoom>,
    pub list_state: Cell<ListState>,
}

impl Rooms {
    pub fn new(matrix: Matrix) -> Self {
        let mut rooms = matrix.fetch_rooms();
        sort_rooms(&mut rooms);

        let mut ret = Self {
            textinput: TextInput::new("Search".to_string(), true, false),
            joined: rooms,
            list_state: Cell::new(ListState::default()),
        };

        ret.reset();

        ret
    }

    pub fn widget(&self) -> RoomsWidget {
        RoomsWidget { rooms: self }
    }

    pub fn input(&mut self, input: &KeyEvent) -> EventResult {
        match input.code {
            KeyCode::Down => self.next(),
            KeyCode::Up => self.previous(),
            KeyCode::Enter => return Consumed(Action::SelectRoom(self.selected_room().room)),
            _ => {
                if let Consumed(_) = (&mut self.textinput).input(input) {
                    self.reset()
                }
            }
        };

        Ignored
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

        self.joined
            .iter()
            .filter(|j| j.name.to_string().to_lowercase().contains(pattern.as_str()))
            .collect()
    }

    fn selected_room(&self) -> DecoratedRoom {
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

fn make_list_item(joined: &DecoratedRoom) -> ListItem {
    let name = joined.name.to_string();
    let unread = joined.room.unread_notification_counts().notification_count;
    let highlights = joined.room.unread_notification_counts().highlight_count;

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

    let mut lines = Text::from(Spans::from(spans));

    let spans = vec![Span::styled(
        format!(
            "{}: {}",
            joined.last_sender.clone().unwrap_or_default(),
            joined.last_message.clone().unwrap_or_default()
        ),
        Style::default().fg(Color::DarkGray),
    )];

    lines.extend(Text::from(Spans::from(spans)));

    ListItem::new(lines)
}

fn sort_rooms(rooms: &mut [DecoratedRoom]) {
    rooms.sort_by_key(|r| {
        (
            r.room.unread_notification_counts().notification_count,
            r.last_ts,
        )
    });

    rooms.reverse()
}
