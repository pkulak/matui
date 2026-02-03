use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::app::App;
use crate::consumed;
use crate::event::Event;
use crate::handler::MatuiEvent;
use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{get_margin, EventResult};

pub struct Recover {
    input: TextInput,
}

impl Default for Recover {
    fn default() -> Self {
        let input = TextInput::new("Recovery Key/Passphrase".to_string(), true, false);

        Self { input }
    }
}

impl Recover {
    pub fn widget(&self) -> RecoverWidget<'_> {
        RecoverWidget { recover: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.input.key_event(input) {
            return consumed!();
        }

        match input.code {
            KeyCode::Esc => EventResult::Consumed(Box::new(move |app| {
                app.close_popup();
            })),
            KeyCode::Enter => {
                App::get_sender()
                    .send(Event::Matui(MatuiEvent::Recover(self.input.value.clone())))
                    .expect("to send recover event");

                consumed!()
            }
            _ => Ignored,
        }
    }
}

pub struct RecoverWidget<'a> {
    pub recover: &'a Recover,
}

impl Widget for RecoverWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 10))
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
                    Constraint::Percentage(100),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Recover")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        self.recover.input.widget().render(splits[0], buf);

        Paragraph::new("Esc to cancel, Enter to submit").render(splits[1], buf);
    }
}
