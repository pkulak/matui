use crate::matrix::matrix::center_emoji;
use crate::settings::get_settings;
use crossterm::event::{KeyCode, KeyEvent};
use std::cell::Cell;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Text;
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget};

use crate::widgets::get_margin;

pub enum ReactResult {
    SelectReaction(String),
    RemoveReaction(String),
    Exit,
    Consumed,
    Ignored,
}

pub struct React {
    reactions: Vec<Reaction>,
    existing: Vec<String>,
    list_state: Cell<ListState>,
}

struct Reaction {
    emoji: String,
    description: String,
}

impl React {
    pub fn new(additions: Vec<String>, existing: Vec<String>) -> Self {
        let mut reactions: Vec<String> = get_settings().get("reactions").unwrap_or_default();

        // get rid of any dupes
        reactions.retain(|r| {
            for ex in &additions {
                if ex == r {
                    return false;
                }
            }

            true
        });

        let reactions = additions
            .into_iter()
            .chain(reactions)
            .map(|emoji| {
                let description = if let Some(e) = emojis::get(&emoji) {
                    format!(
                        "{} {}",
                        center_emoji(&emoji),
                        e.shortcode().unwrap_or(e.name())
                    )
                } else {
                    center_emoji(&emoji)
                };

                Reaction { emoji, description }
            })
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(0_usize));
        let list_state = Cell::new(list_state);

        React {
            reactions,
            existing,
            list_state,
        }
    }

    pub fn widget(&self) -> ReactWidget<'_> {
        ReactWidget { parent: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> ReactResult {
        match input.code {
            KeyCode::Char('k') | KeyCode::Up => {
                self.previous();
                ReactResult::Consumed
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.next();
                ReactResult::Consumed
            }
            KeyCode::Esc => ReactResult::Exit,
            KeyCode::Enter => {
                if let Some(reaction) = self.selected_reaction() {
                    if self.existing.contains(&reaction) {
                        ReactResult::RemoveReaction(reaction)
                    } else {
                        ReactResult::SelectReaction(reaction)
                    }
                } else {
                    ReactResult::Exit
                }
            }
            _ => ReactResult::Ignored,
        }
    }

    fn next(&mut self) {
        let mut state = self.list_state.take();

        let i = match state.selected() {
            Some(i) => {
                if i >= self.reactions.len() - 1 {
                    self.reactions.len() - 1
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
                    0
                } else {
                    i - 1
                }
            }
            None => 0,
        };

        state.select(Some(i));
        self.list_state.set(state);
    }

    fn selected_reaction(&self) -> Option<String> {
        if self.reactions.is_empty() {
            return None;
        }

        let state = self.list_state.take();
        let selected = state.selected().unwrap_or_default();
        self.list_state.set(state);

        self.reactions.get(selected).map(|r| r.emoji.clone())
    }
}

pub struct ReactWidget<'a> {
    pub parent: &'a React,
}

impl Widget for ReactWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(get_margin(
                area.height,
                (self.parent.reactions.len() + 4) as u16,
            ))
            .horizontal_margin(get_margin(area.width, 40))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let title = 'title: {
            if let Some(selected) = self.parent.selected_reaction() {
                for ex in &self.parent.existing {
                    if ex == &selected {
                        break 'title "Remove Reaction";
                    }
                }
            }

            "Add Reaction"
        };

        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Reset))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        block.render(area, buf);

        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(2)
            .horizontal_margin(2)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let items: Vec<ListItem> = self
            .parent
            .reactions
            .iter()
            .map(|r| ListItem::new(Text::from(r.description.clone())))
            .collect();

        let mut list_state = self.parent.list_state.take();
        let list = List::new(items).highlight_symbol("> ");
        StatefulWidget::render(list, area, buf, &mut list_state);
        self.parent.list_state.set(list_state)
    }
}
