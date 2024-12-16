use crossterm::event::{KeyCode, KeyEvent};

use matrix_sdk::room::Room;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use ruma::OwnedEventId;

use crate::widgets::button::Button;
use crate::widgets::{focus_next, Focusable};
use crate::{close, consumed};

use super::{get_margin, EventResult};

#[derive(Clone)]
pub enum ConfirmBehavior {
    Verification,
    DeleteMessage(Room, OwnedEventId),
}

pub struct Confirm {
    title: String,
    message: String,
    yes: Button,
    no: Button,
    behavior: ConfirmBehavior,
}

impl Confirm {
    pub fn new(
        title: String,
        message: String,
        yes: String,
        no: String,
        behavior: ConfirmBehavior,
    ) -> Self {
        Self {
            title,
            message,
            yes: Button::new(yes, true),
            no: Button::new(no, false),
            behavior,
        }
    }

    pub fn widget(&self) -> ConfirmWidget {
        ConfirmWidget { confirm: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
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
                consumed!()
            }
            KeyCode::Esc => close!(),
            KeyCode::Enter => self.make_result(),
            _ => EventResult::Ignored,
        }
    }

    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![Box::new(&mut self.yes), Box::new(&mut self.no)]
    }

    fn make_result(&self) -> EventResult {
        let focused = self.yes.focused();

        match self.behavior.clone() {
            ConfirmBehavior::Verification if focused => EventResult::Consumed(Box::new(|app| {
                if let Some(s) = app.sas.clone() {
                    app.matrix.confirm_verification(s);
                    app.close_popup();
                }
            })),
            ConfirmBehavior::Verification => EventResult::Consumed(Box::new(|app| {
                if let Some(s) = app.sas.clone() {
                    app.matrix.mismatched_verification(s);
                    app.close_popup();
                }
            })),
            ConfirmBehavior::DeleteMessage(room, id) if focused => {
                EventResult::Consumed(Box::new(|app| {
                    app.matrix.redact_event(room, id);
                    app.close_popup();
                }))
            }
            ConfirmBehavior::DeleteMessage(_, _) => close!(),
        }
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
