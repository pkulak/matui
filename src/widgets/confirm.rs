use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::Joined;
use ruma::OwnedEventId;
use tui::buffer::Buffer;
use tui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use tui::style::{Color, Style};
use tui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::widgets::button::Button;
use crate::widgets::{focus_next, Focusable};

use super::get_margin;

#[derive(Clone)]
pub enum ConfirmResult {
    Close,
    Ignored,
    Consumed,
    RedactEvent(Joined, OwnedEventId),
    VerificationConfirm,
    VerificationCancel,
}

pub struct Confirm {
    title: String,
    message: String,
    yes: Button,
    yes_result: ConfirmResult,
    no: Button,
    no_result: ConfirmResult,
}

impl Confirm {
    pub fn new(
        title: String,
        message: String,
        yes: String,
        yes_result: ConfirmResult,
        no: String,
        no_result: ConfirmResult,
    ) -> Self {
        Self {
            title,
            message,
            yes: Button::new(yes, true),
            yes_result,
            no: Button::new(no, false),
            no_result,
        }
    }

    pub fn widget(&self) -> ConfirmWidget {
        ConfirmWidget { confirm: self }
    }

    pub fn input(&mut self, input: &KeyEvent) -> ConfirmResult {
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
            | KeyCode::Char('l') => {
                focus_next(self.focus_order());
                ConfirmResult::Consumed
            }
            KeyCode::Enter => {
                if (&mut self.yes).focused() {
                    self.yes_result.clone()
                } else {
                    self.no_result.clone()
                }
            }
            _ => ConfirmResult::Ignored,
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

        buf.merge(&Buffer::empty(area));

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
