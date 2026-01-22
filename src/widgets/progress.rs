use std::time::{Duration, Instant};

use crate::widgets::get_margin;
use ratatui::buffer::Buffer;
use ratatui::layout::Direction::Vertical;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

const FRAMES: &[&str] = &[
    "⠁", "⠂", "⠄", "⡀", "⡈", "⡐", "⡠", "⣀", "⣁", "⣂", "⣄", "⣌", "⣔", "⣤", "⣥", "⣦", "⣮", "⣶", "⣷",
    "⣿", "⡿", "⠿", "⢟", "⠟", "⡛", "⠛", "⠫", "⢋", "⠋", "⠍", "⡉", "⠉", "⠑", "⠡", "⢁",
];

pub struct Progress {
    text: String,
    tail: String,
    created: Instant,
    delay: u64,
}

impl Progress {
    pub fn new(text: &str, delay: u64) -> Progress {
        Progress {
            text: text.to_string(),
            tail: "".to_string(),
            created: Instant::now(),
            delay,
        }
    }

    pub fn widget(&self) -> ProgressWidget {
        ProgressWidget { progress: self }
    }

    pub fn tick_event(&mut self, timestamp: usize) {
        self.tail = FRAMES[timestamp % FRAMES.len()].to_string();
    }
}

pub struct ProgressWidget<'a> {
    progress: &'a Progress,
}

impl Widget for ProgressWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // don't render until it's been past the delay
        if self.progress.created.elapsed() < Duration::from_millis(self.progress.delay) {
            return;
        }

        let value = format!(
            "{} {} {}",
            self.progress.tail, self.progress.text, self.progress.tail
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
            .style(Style::default().bg(Color::Reset))
            .render(area, buf);

        let area = Layout::default()
            .horizontal_margin(get_margin(
                area.width,
                (self.progress.text.len() + 4) as u16,
            ))
            .vertical_margin(2)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        Paragraph::new(value).render(area, buf);
    }
}
