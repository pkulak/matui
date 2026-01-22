use crossterm::event::KeyEvent;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::close;
use crate::widgets::button::Button;

use super::{get_margin, EventResult};

pub struct Error {
    heading: String,
    message: String,
    button: Button,
}

impl Error {
    pub fn new(message: String) -> Self {
        Self {
            heading: "Error".to_string(),
            message,
            button: Button::new("OK".to_string(), true),
        }
    }

    pub fn with_heading(heading: String, message: String) -> Self {
        Self {
            heading,
            message,
            button: Button::new("OK".to_string(), true),
        }
    }

    pub fn widget(&self) -> ErrorWidget {
        ErrorWidget { error: self }
    }

    pub fn key_event(&mut self, _: &KeyEvent) -> EventResult {
        // no matter what, close
        close!()
    }
}

pub struct ErrorWidget<'a> {
    pub error: &'a Error,
}

impl Widget for ErrorWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 8))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(4)
            .vertical_margin(1)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(2),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title(&*self.error.heading)
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        Paragraph::new(self.error.message.clone()).render(splits[1], buf);

        // pop the OK button in the middle
        let area = Layout::default()
            .direction(Direction::Horizontal)
            .horizontal_margin(get_margin(splits[1].width, 20))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[2])[0];

        self.error.button.widget().render(area, buf);
    }
}
