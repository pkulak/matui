use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::button::Button;
use crate::widgets::checkbox::Checkbox;
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, Focusable, focus_next, focus_prev, get_margin};
use crate::{close, consumed};

pub struct Create {
    name: TextInput,
    topic: TextInput,
    alias: TextInput,
    encrypted: Checkbox,
    private: Checkbox,
    cancel: Button,
    create: Button,
}

impl Default for Create {
    fn default() -> Self {
        Self {
            name: TextInput::new("Name".to_string(), true, false),
            topic: TextInput::new("Topic".to_string(), false, false),
            alias: TextInput::new("Alias".to_string(), false, false),
            encrypted: Checkbox::new("Encrypted".to_string(), false, false),
            private: Checkbox::new("Private".to_string(), true, false),
            cancel: Button::new("Cancel".to_string(), false),
            create: Button::new("Create".to_string(), false),
        }
    }
}

impl Create {
    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![
            Box::new(&mut self.name),
            Box::new(&mut self.topic),
            Box::new(&mut self.alias),
            Box::new(&mut self.encrypted),
            Box::new(&mut self.private),
            Box::new(&mut self.cancel),
            Box::new(&mut self.create),
        ]
    }

    pub fn widget(&self) -> CreateWidget<'_> {
        CreateWidget { create: self }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        if let Consumed(_) = self.name.paste_event(value) {
            return consumed!();
        }

        if let Consumed(_) = self.topic.paste_event(value) {
            return consumed!();
        }

        self.alias.paste_event(value)
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.name.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.topic.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.alias.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.encrypted.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.private.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.cancel.key_event(input) {
            return close!();
        }

        if let Consumed(_) = self.create.key_event(input) {
            fn opt(value: String) -> Option<String> {
                let value = value.trim();

                if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                }
            }

            let name = opt(self.name.value());
            let topic = opt(self.topic.value());

            // the server adds the domain, so we only want the local part
            let alias = opt(self
                .alias
                .value()
                .trim()
                .trim_start_matches('#')
                .to_string());

            let encrypted = self.encrypted.checked;
            let private = self.private.checked;

            return EventResult::Consumed(Box::new(move |app| {
                app.matrix
                    .create_room(name, topic, alias, encrypted, private);
                app.close_popup();
            }));
        }

        match input.code {
            KeyCode::Esc => close!(),
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus_next(self.focus_order()),
            KeyCode::BackTab | KeyCode::Up => focus_prev(self.focus_order()),
            _ => Ignored,
        }
    }
}

pub struct CreateWidget<'a> {
    pub create: &'a Create,
}

impl Widget for CreateWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 24))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(2)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Percentage(100),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Create Room")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);
        self.create.name.widget().render(splits[0], buf);
        self.create.topic.widget().render(splits[2], buf);
        self.create.alias.widget().render(splits[4], buf);

        let checks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[6]);

        self.create.encrypted.widget().render(checks[0], buf);
        self.create.private.widget().render(checks[1], buf);

        let buttons = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[8]);

        self.create.cancel.widget().render(buttons[0], buf);
        self.create.create.widget().render(buttons[1], buf);
    }
}
