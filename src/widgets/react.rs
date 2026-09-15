use crate::matrix::matrix::center_emoji;
use crate::settings::get_settings;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Text;
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, StatefulWidget, Widget,
};
use std::cell::Cell;

use crate::widgets::get_margin;

pub enum ReactResult {
    SelectReaction(String),
    RemoveReaction(String),
    Exit,
    Consumed,
}

pub struct React {
    reactions: Vec<Reaction>,
    existing: Vec<String>,
    custom_reaction: Option<String>,
    list_state: Cell<ListState>,
}

struct Reaction {
    emoji: String,
    description: String,
}

fn same_emoji(left: &str, right: &str) -> bool {
    match (emojis::get(left), emojis::get(right)) {
        (Some(left), Some(right)) => left.as_str() == right.as_str(),
        _ => left == right,
    }
}

impl Reaction {
    fn new(emoji: String) -> Self {
        let description = if let Some(e) = emojis::get(&emoji) {
            format!(
                "{} {}",
                center_emoji(&emoji),
                e.shortcode().unwrap_or(e.name())
            )
        } else {
            center_emoji(&emoji)
        };

        Self { emoji, description }
    }
}

impl React {
    pub fn new(additions: Vec<String>, existing: Vec<String>) -> Self {
        let configured = get_settings().get("reactions").unwrap_or_default();
        Self::with_reactions(additions, existing, configured)
    }

    fn with_reactions(
        additions: Vec<String>,
        existing: Vec<String>,
        configured: Vec<String>,
    ) -> Self {
        let mut reactions: Vec<String> = Vec::new();

        for emoji in additions.into_iter().chain(configured) {
            if let Some(index) = reactions
                .iter()
                .position(|reaction| same_emoji(reaction, &emoji))
            {
                // Prefer the raw value belonging to the current user so Enter
                // can find and remove the exact Matrix event.
                if existing.contains(&emoji) && !existing.contains(&reactions[index]) {
                    reactions[index] = emoji;
                }
            } else {
                reactions.push(emoji);
            }
        }

        let reactions = reactions.into_iter().map(Reaction::new).collect::<Vec<_>>();
        let mut list_state = ListState::default();
        list_state.select((!reactions.is_empty()).then_some(0));

        React {
            reactions,
            existing,
            custom_reaction: None,
            list_state: Cell::new(list_state),
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
            _ => ReactResult::Consumed,
        }
    }

    pub fn paste_event(&mut self, value: &str) {
        let value = value.trim();
        let Some(emoji) = emojis::get(value) else {
            return;
        };
        let emoji = emoji.as_str().to_string();

        if let Some(custom) = self.custom_reaction.take()
            && let Some(index) = self.reactions.iter().position(|r| r.emoji == custom)
        {
            self.reactions.remove(index);
        }

        let existing_index = self.reactions.iter().position(|reaction| {
            same_emoji(&reaction.emoji, &emoji) && self.existing.contains(&reaction.emoji)
        });
        let matching_index = existing_index.or_else(|| {
            self.reactions
                .iter()
                .position(|reaction| same_emoji(&reaction.emoji, &emoji))
        });

        if let Some(index) = matching_index {
            self.select(index);
            return;
        }

        self.reactions.insert(0, Reaction::new(emoji.clone()));
        self.custom_reaction = Some(emoji);
        self.select(0);
    }

    fn next(&mut self) {
        if self.reactions.is_empty() {
            return;
        }
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
        if self.reactions.is_empty() {
            return;
        }

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

    fn select(&self, index: usize) {
        let mut state = self.list_state.take();
        state.select(Some(index));
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
            .title_bottom("Paste an emoji")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn empty_react() -> React {
        React {
            reactions: vec![],
            existing: vec![],
            custom_reaction: None,
            list_state: Cell::new(ListState::default()),
        }
    }

    #[test]
    fn pastes_trimmed_compound_emoji() {
        let mut react = empty_react();

        react.paste_event("\u{2003}👨‍👩‍👧‍👦\u{2003}");

        assert_eq!(react.selected_reaction().as_deref(), Some("👨‍👩‍👧‍👦"));
        assert_eq!(react.reactions.len(), 1);
    }

    #[test]
    fn ignores_invalid_paste_without_changing_selection() {
        let mut react = empty_react();
        react.paste_event("🫡");

        react.paste_event("not an emoji");

        assert_eq!(react.selected_reaction().as_deref(), Some("🫡"));
        assert_eq!(react.reactions.len(), 1);
    }

    #[test]
    fn replaces_the_previous_custom_emoji() {
        let mut react = empty_react();
        react.paste_event("🫡");
        react.paste_event("🪿");

        assert_eq!(react.selected_reaction().as_deref(), Some("🪿"));
        assert_eq!(react.reactions.len(), 1);
        assert_eq!(react.custom_reaction.as_deref(), Some("🪿"));
    }

    #[test]
    fn canonical_duplicates_prefer_the_current_users_raw_reaction() {
        let mut react = React::with_reactions(
            vec!["🐿️".to_string(), "🐿".to_string()],
            vec!["🐿".to_string()],
            vec!["🐿️".to_string()],
        );
        react.paste_event("🫡");

        react.paste_event("🐿️");

        assert_eq!(react.selected_reaction().as_deref(), Some("🐿"));
        assert_eq!(react.reactions.len(), 1);
        assert_eq!(react.custom_reaction, None);
        assert!(matches!(
            react.key_event(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ReactResult::RemoveReaction(reaction) if reaction == "🐿"
        ));
    }

    #[test]
    fn consumes_unsupported_keys() {
        let mut react = empty_react();

        assert!(matches!(
            react.key_event(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            ReactResult::Consumed
        ));
    }

    #[test]
    fn navigation_over_an_empty_list_is_a_no_op() {
        let mut react = empty_react();

        react.next();
        react.previous();

        assert_eq!(react.selected_reaction(), None);
    }
}
