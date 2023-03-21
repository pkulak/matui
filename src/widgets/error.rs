use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::widgets::button::Button;

use super::get_margin;

pub struct Error {
    message: String,
    button: Button,
}

impl Error {
    pub fn new(message: String) -> Self {
        Self {
            message,
            button: Button::new("OK".to_string(), true, None),
        }
    }

    pub fn widget(&self) -> ErrorWidget {
        ErrorWidget { error: self }
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
            .title("Error")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Black));

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
