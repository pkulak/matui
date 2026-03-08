use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use matrix_sdk::Room;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use ratatui_textarea::{CursorMove, TextArea};

use crate::app::App;
use crate::consumed;
use crate::event::{Event, EventHandler};
use crate::matrix::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::spawn::get_text;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{get_margin, EventResult};

pub struct Compose {
    input: TextArea<'static>,
    last_width: Cell<u16>,
    room: DecoratedRoom,
    matrix: Matrix,
}

impl Compose {
    pub fn new(room: DecoratedRoom, matrix: Matrix) -> Self {
        let input = TextArea::default();

        Self {
            input,
            last_width: Cell::new(0),
            room,
            matrix,
        }
    }

    /// Converts a 2D cursor position to a flat character offset into the joined text.
    fn cursor_to_flat(lines: &[String], row: usize, col: usize) -> usize {
        let before: usize = lines[..row].iter().map(|l| l.chars().count() + 1).sum();
        let clamped = col.min(lines.get(row).map(|l| l.chars().count()).unwrap_or(0));
        before + clamped
    }

    /// Converts a flat character offset back to a 2D cursor position in wrapped lines.
    fn flat_to_cursor(lines: &[String], mut offset: usize) -> (usize, usize) {
        for (row, line) in lines.iter().enumerate() {
            let len = line.chars().count();
            if offset <= len {
                return (row, offset);
            }
            offset -= len + 1;
        }
        let last = lines.len().saturating_sub(1);
        (last, lines.last().map(|l| l.chars().count()).unwrap_or(0))
    }

    /// Re-wrap the TextArea contents to `last_width` columns using textwrap.
    /// If the wrapped text differs from the current lines, replaces the TextArea
    /// with a new one containing the wrapped lines (cursor reset to the front).
    fn wrap_input(&mut self) {
        let width = self.last_width.get() as usize;
        if width == 0 {
            return;
        }

        let last_line = self.input.lines().last().unwrap_or(&"".to_string());
        let trailing_whitespace = last_line.len() - last_line.trim_end().len();
        
        let (cur_row, cur_col) = self.input.cursor();
        let current_lines = self.input.lines();

        let joined = current_lines.join(" ");

        let wrapped = textwrap::wrap(&joined, width);

        if wrapped.len() == current_lines.len()
            && wrapped
                .iter()
                .zip(current_lines)
                .all(|(w, c)| w.as_ref() == *c)
        {
            return;
        }

        let flat = Self::cursor_to_flat(current_lines, cur_row, cur_col);
        let wrapped_owned: Vec<String> = wrapped.into_iter().map(|c| c.into_owned()).collect();
        let (new_row, new_col) = Self::flat_to_cursor(&wrapped_owned, flat);

        let mut new_input = TextArea::new(wrapped_owned);
        new_input.move_cursor(CursorMove::Jump(new_row as u16, new_col as u16));
        self.input = new_input;
    }

    fn room(&self) -> Room {
        self.room.inner.clone()
    }

    pub fn widget(&self) -> ComposeWidget<'_> {
        ComposeWidget { compose: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        // Handle ^X to edit in external editor
        if input.modifiers.contains(KeyModifiers::CONTROL) && input.code == KeyCode::Char('x') {
            let send = self.matrix.begin_typing(self.room());

            let text = &self.input.lines().join("\n").trim().to_string();

            handler.park();
            let result = get_text(
                Some(text),
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
                    self.input = TextArea::default()
                } else {
                    self.matrix.send_text_message(self.room(), message);

                    return Consumed(Box::new(move |app| {
                        app.close_popup();
                    }));
                }
            }

            return consumed!();
        }

        if self.input.input(*input) {
            self.matrix.typing_notification(self.room(), true);
            self.wrap_input();
            return consumed!();
        }

        self.matrix.typing_notification(self.room(), false);

        match input.code {
            KeyCode::Esc => Consumed(Box::new(move |app| {
                app.close_popup();
            })),
            KeyCode::Enter => {
                let message = self.input.lines().join("\n").trim().to_string();

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

        self.compose.input.render(splits[0], buf);
        self.compose.last_width.set(splits[0].width - 2);

        Paragraph::new("type ^X to edit in external editor").render(splits[1], buf);
    }
}
