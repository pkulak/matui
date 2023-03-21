use std::sync::mpsc::Sender;

use crossterm::event::{KeyCode, KeyEvent};
use tui::buffer::Buffer;
use tui::layout::{Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, Borders, Paragraph, Widget};

use crate::widgets::{get_margin, Focusable, KeyEventing};

pub struct Button {
    label: String,
    focused: bool,
    sender: Option<Sender<ButtonEvent>>,
}

pub enum ButtonEvent {
    Submit(String),
}

impl Focusable for &mut Button {
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

impl KeyEventing for &mut Button {
    fn input(&mut self, input: &KeyEvent) -> bool {
        match &self.sender {
            Some(sender) if self.focused && input.code == KeyCode::Enter => {
                sender.send(ButtonEvent::Submit(self.label.clone())).ok();
                true
            }
            _ => false,
        }
    }
}

impl Button {
    pub fn new(label: String, focused: bool, sender: Option<Sender<ButtonEvent>>) -> Button {
        Button {
            label,
            focused,
            sender,
        }
    }

    pub fn widget(&self) -> ButtonWidget {
        ButtonWidget { button: self }
    }
}

pub struct ButtonWidget<'a> {
    button: &'a Button,
}

impl Widget for ButtonWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let color = if self.button.focused {
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

        let area = Layout::default()
            .vertical_margin(1)
            .horizontal_margin(get_margin(area.width, self.button.label.len() as u16))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        Paragraph::new(self.button.label.clone())
            .style(Style::default().fg(color))
            .render(area, buf);
    }
}
