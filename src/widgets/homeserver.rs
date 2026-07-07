use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::consumed;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::button::Button;
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, Focusable, focus_next, focus_prev, get_margin};

pub struct Homeserver {
    homeserver: TextInput,
    continue_button: Button,
}

impl Default for Homeserver {
    fn default() -> Self {
        Self {
            homeserver: TextInput::new("Homeserver".to_string(), true, false),
            continue_button: Button::new("Continue".to_string(), false),
        }
    }
}

impl Homeserver {
    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![
            Box::new(&mut self.homeserver),
            Box::new(&mut self.continue_button),
        ]
    }

    pub fn widget(&self) -> HomeserverWidget<'_> {
        HomeserverWidget { homeserver: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.homeserver.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.continue_button.key_event(input) {
            let homeserver = self.homeserver.value().trim().to_string();

            return Consumed(Box::new(move |app| {
                app.matrix.start_login(homeserver);
            }));
        }

        match input.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus_next(self.focus_order()),
            KeyCode::BackTab | KeyCode::Up => focus_prev(self.focus_order()),
            _ => Ignored,
        }
    }
}

pub struct HomeserverWidget<'a> {
    pub homeserver: &'a Homeserver,
}

impl Widget for HomeserverWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 13))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(3)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Homeserver")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);
        self.homeserver.homeserver.widget().render(splits[0], buf);

        let continue_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[2])[1];

        self.homeserver
            .continue_button
            .widget()
            .render(continue_area, buf);
    }
}
