use crossterm::event::{KeyCode, KeyEvent};
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::widgets::button::Button;
use crate::widgets::Action::{ButtonNo, ButtonYes};
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{focus_next, EventResult, Focusable};

use super::get_margin;

pub struct Confirm {
    title: String,
    message: String,
    yes: Button,
    no: Button,
}

impl Confirm {
    pub fn new(title: String, message: String, yes: String, no: String) -> Self {
        Self {
            title,
            message,
            yes: Button::new(yes, true),
            no: Button::new(no, false),
        }
    }

    pub fn widget(&self) -> ConfirmWidget {
        ConfirmWidget { confirm: self }
    }

    pub fn input(&mut self, input: &KeyEvent) -> EventResult {
        match input.code {
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Char('h')
            | KeyCode::Char('j')
            | KeyCode::Char('k')
            | KeyCode::Char('l') => focus_next(self.focus_order()),
            KeyCode::Enter => {
                if (&mut self.yes).focused() {
                    Consumed(ButtonYes)
                } else {
                    Consumed(ButtonNo)
                }
            }
            _ => Ignored,
        }
    }

    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![Box::new(&mut self.yes), Box::new(&mut self.no)]
    }
}

pub struct ConfirmWidget<'a> {
    pub confirm: &'a Confirm,
}

impl Widget for ConfirmWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 10))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let block = Block::default()
            .title(self.confirm.title.clone())
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Black));

        block.render(area, buf);

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(4)
            .vertical_margin(1)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(4),
                    Constraint::Length(3),
                ]
                .as_ref(),
            )
            .split(area);

        Paragraph::new(self.confirm.message.clone()).render(splits[1], buf);

        let splits = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[2]);

        self.confirm.yes.widget().render(splits[0], buf);
        self.confirm.no.widget().render(splits[1], buf);
    }
}
