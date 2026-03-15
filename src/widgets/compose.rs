use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::Room;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::app::App;
use crate::event::{Event, EventHandler};
use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::spawn::get_text;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, get_margin};
use crate::{KeyCombo, consumed};

pub struct Compose {
    input: TextInput,
    room: DecoratedRoom,
    combo: KeyCombo,
    matrix: Matrix,
}

impl Compose {
    pub fn new(room: DecoratedRoom, matrix: Matrix) -> Self {
        let input = TextInput::new("Message".to_string(), true, false);

        Self {
            input,
            room,
            combo: KeyCombo::new(vec!['j', 'j']),
            matrix,
        }
    }

    fn room(&self) -> Room {
        self.room.inner.clone()
    }

    pub fn widget(&self) -> ComposeWidget<'_> {
        ComposeWidget { compose: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        // Handle "jj" to edit in external editor
        if let KeyCode::Char(c) = input.code
            && self.combo.record(c)
        {
            let send = self.matrix.begin_typing(self.room());
            self.input.backspace();

            handler.park();
            let result = get_text(
                Some(&self.input.value),
                Some(&format!(
                    "<!-- The message above will be sent to {}. -->",
                    self.room.name
                )),
            );
            handler.unpark();

            self.matrix.end_typing(self.room(), send);

            let _ = App::get_sender().send(Event::Redraw);

            if let Ok(Some(message)) = result {
                if message.trim().is_empty() {
                    self.input.value = "".to_string();
                } else {
                    self.matrix.send_text_message(self.room(), message);

                    return Consumed(Box::new(move |app| {
                        app.close_popup();
                    }));
                }
            }

            return consumed!();
        }

        if let Consumed(_) = self.input.key_event(input) {
            self.matrix.typing_notification(self.room(), true);
            return consumed!();
        }

        self.matrix.typing_notification(self.room(), false);

        match input.code {
            KeyCode::Esc => Consumed(Box::new(move |app| {
                app.close_popup();
            })),
            KeyCode::Enter => {
                let message = self.input.value.clone();

                if !message.trim().is_empty() {
                    self.matrix.send_text_message(self.room(), message);
                }

                Consumed(Box::new(move |app| {
                    app.close_popup();
                }))
            }
            _ => Ignored,
        }
    }
}

pub struct ComposeWidget<'a> {
    pub compose: &'a Compose,
}

impl Widget for ComposeWidget<'_> {
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
            .title("Compose")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        self.compose.input.widget().render(splits[0], buf);

        Paragraph::new("type \"jj\" to edit in external editor").render(splits[1], buf);
    }
}
