use crate::widgets::get_margin;
use tui::buffer::Buffer;
use tui::layout::Direction::Vertical;
use tui::layout::{Constraint, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

const FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];

pub struct Progress {
    text: String,
    tail: String,
}

impl Progress {
    pub fn new(text: &str) -> Progress {
        Progress {
            text: text.to_string(),
            tail: "".to_string(),
        }
    }

    pub fn widget(&self) -> ProgressWidget {
        ProgressWidget { notification: self }
    }

    pub fn tick(&mut self, timestamp: usize) {
        self.tail = FRAMES[timestamp % FRAMES.len()].to_string();
    }
}

pub struct ProgressWidget<'a> {
    notification: &'a Progress,
}

impl Widget for ProgressWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let value = format!(
            "{} {} {}",
            self.notification.tail, self.notification.text, self.notification.tail
        );

        let area = Layout::default()
            .direction(Vertical)
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 5))
            .constraints([Constraint::Length(5)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Black))
            .render(area, buf);

        let area = Layout::default()
            .horizontal_margin(get_margin(
                area.width,
                (self.notification.text.len() + 4) as u16,
            ))
            .vertical_margin(2)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        Paragraph::new(value).render(area, buf);
    }
}
