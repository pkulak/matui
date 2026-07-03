use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::consumed;
use crate::widgets::{get_margin, Focusable};

use super::EventResult;

pub struct Checkbox {
    label: String,
    pub checked: bool,
    focused: bool,
}

impl Focusable for &mut Checkbox {
    fn focused(&self) -> bool {
        self.focused
    }

    fn focus(&mut self) {
        self.focused = true;
    }

    fn defocus(&mut self) {
        self.focused = false;
    }
}

impl Checkbox {
    pub fn new(label: String, checked: bool, focused: bool) -> Checkbox {
        Checkbox {
            label,
            checked,
            focused,
        }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if self.focused && input.code == KeyCode::Char(' ') {
            self.checked = !self.checked;
            consumed!()
        } else {
            EventResult::Ignored
        }
    }

    pub fn widget(&self) -> CheckboxWidget<'_> {
        CheckboxWidget { checkbox: self }
    }
}

pub struct CheckboxWidget<'a> {
    checkbox: &'a Checkbox,
}

impl Widget for CheckboxWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = if self.checkbox.focused {
            Color::LightGreen
        } else {
            Color::DarkGray
        };

        let area = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(color))
            .render(area, buf);

        let label = format!(
            "[{}] {}",
            if self.checkbox.checked { "x" } else { " " },
            self.checkbox.label
        );

        let area = Layout::default()
            .vertical_margin(1)
            .horizontal_margin(get_margin(area.width, label.len() as u16))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        Paragraph::new(label)
            .style(Style::default().fg(color))
            .render(area, buf);
    }
}
