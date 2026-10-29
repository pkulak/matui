use crate::app::Popup;
use crate::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::widgets::compose::Compose;
use crate::{close, consumed};
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::RoomMember;
use matrix_sdk::ruma::{OwnedEventId, OwnedUserId, UserId};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget,
};
use std::cell::Cell;

use crate::widgets::EventResult::Consumed;
use crate::widgets::get_margin;
use crate::widgets::textinput::TextInput;

use super::EventResult;

/// Picks a room member to mention, then opens Compose with their ID.
pub struct Members {
    textinput: TextInput,
    members: Vec<(Option<String>, OwnedUserId)>,
    list_state: Cell<ListState>,
    room: DecoratedRoom,
    matrix: Matrix,
    thread: Option<OwnedEventId>,
}

impl Members {
    pub fn new(
        room: DecoratedRoom,
        matrix: Matrix,
        thread: Option<OwnedEventId>,
        members: Vec<RoomMember>,
        me: &UserId,
    ) -> Self {
        let mut members: Vec<(Option<String>, OwnedUserId)> = members
            .iter()
            .filter(|m| m.user_id() != me)
            .map(|m| (m.display_name().map(String::from), m.user_id().to_owned()))
            .collect();

        members
            .sort_by_cached_key(|(name, id)| name.as_deref().unwrap_or(id.as_str()).to_lowercase());

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            textinput: TextInput::new("Search".to_string(), true, false),
            members,
            list_state: Cell::new(list_state),
            room,
            matrix,
            thread,
        }
    }

    pub fn widget(&self) -> MembersWidget<'_> {
        MembersWidget { members: self }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        if let Consumed(_) = self.textinput.paste_event(value) {
            self.select(0);
            consumed!()
        } else {
            EventResult::Ignored
        }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        let len = self.filtered().len();
        let selected = self.selected();

        match input.code {
            KeyCode::Esc => close!(),
            KeyCode::Down if len > 0 => {
                self.select((selected + 1) % len);
                consumed!()
            }
            KeyCode::Up if len > 0 => {
                self.select((selected + len - 1) % len);
                consumed!()
            }
            KeyCode::Enter => {
                let Some((_, id)) = self.filtered().get(selected).copied() else {
                    return EventResult::Ignored;
                };

                let compose = Compose::new(
                    self.room.clone(),
                    self.matrix.clone(),
                    self.thread.clone(),
                    &format!("{} ", id),
                );

                Consumed(Box::new(|app| app.set_popup(Popup::Compose(compose))))
            }
            _ => {
                if let Consumed(_) = self.textinput.key_event(input) {
                    self.select(0);
                    consumed!()
                } else {
                    EventResult::Ignored
                }
            }
        }
    }

    fn selected(&self) -> usize {
        let state = self.list_state.take();
        let selected = state.selected().unwrap_or(0);
        self.list_state.set(state);
        selected
    }

    fn select(&self, i: usize) {
        let mut state = self.list_state.take();
        state.select(Some(i));
        self.list_state.set(state);
    }

    fn filtered(&self) -> Vec<&(Option<String>, OwnedUserId)> {
        let pattern = self.textinput.value.to_lowercase();

        self.members
            .iter()
            .filter(|(name, id)| {
                id.as_str().to_lowercase().contains(&pattern)
                    || name
                        .as_ref()
                        .is_some_and(|n| n.to_lowercase().contains(&pattern))
            })
            .collect()
    }
}

pub struct MembersWidget<'a> {
    pub members: &'a Members,
}

impl Widget for MembersWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(2)
            .horizontal_margin(get_margin(area.width, 60))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let block = Block::default()
            .title("Mention")
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

        self.members.textinput.widget().render(splits[0], buf);

        let items: Vec<ListItem> = self
            .members
            .filtered()
            .into_iter()
            .map(|(name, id)| {
                let spans = match name {
                    Some(name) => vec![
                        Span::from(name.as_str()),
                        Span::styled(format!("  {}", id), Style::default().fg(Color::DarkGray)),
                    ],
                    None => vec![Span::from(id.as_str())],
                };

                ListItem::new(Line::from(spans))
            })
            .collect();

        let area = Layout::default()
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[1])[0];

        let mut list_state = self.members.list_state.take();
        let list = List::new(items).highlight_symbol("> ");
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.members.list_state.set(list_state)
    }
}
