use std::mem;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::room::Room;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::matrix::matrix::Matrix;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, get_margin};
use crate::{close, consumed};

pub struct Upload {
    input: TextInput,
    matrix: Matrix,
    room: Room,
    paths: Vec<PathBuf>,
    captions: Vec<Option<String>>,
}

impl Upload {
    pub fn new(matrix: Matrix, room: Room, paths: Vec<PathBuf>) -> Self {
        debug_assert!(!paths.is_empty());

        Self {
            input: TextInput::new("Caption (optional):".to_string(), true, false),
            matrix,
            room,
            paths,
            captions: Vec::new(),
        }
    }

    pub fn widget(&self) -> UploadWidget<'_> {
        UploadWidget { upload: self }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        self.input.paste_event(value)
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.input.key_event(input) {
            return consumed!();
        }

        match input.code {
            KeyCode::Esc => close!(),
            KeyCode::Enter => {
                let caption = self.input.value();
                self.captions.push(if caption.trim().is_empty() {
                    None
                } else {
                    Some(caption)
                });

                if self.captions.len() < self.paths.len() {
                    self.input = TextInput::new("Caption (optional):".to_string(), true, false);
                    return consumed!();
                }

                let attachments = mem::take(&mut self.paths)
                    .into_iter()
                    .zip(mem::take(&mut self.captions))
                    .collect();
                let matrix = self.matrix.clone();
                let room = self.room.clone();

                EventResult::Consumed(Box::new(move |app| {
                    matrix.send_attachments(room, attachments);
                    app.close_popup();
                }))
            }
            _ => Ignored,
        }
    }

    fn current_filename(&self) -> String {
        self.paths[self.captions.len()]
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

pub struct UploadWidget<'a> {
    upload: &'a Upload,
}

impl Widget for UploadWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 70))
            .vertical_margin(get_margin(area.height, 13))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let title = if self.upload.paths.len() == 1 {
            "Upload".to_string()
        } else {
            format!(
                "Upload {} of {}",
                self.upload.captions.len() + 1,
                self.upload.paths.len()
            )
        };

        Block::default()
            .title(title)
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset))
            .render(area, buf);

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(4)
            .vertical_margin(2)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        Paragraph::new(format!("File: {}", self.upload.current_filename())).render(splits[0], buf);
        self.upload.input.widget().render(splits[2], buf);
        Paragraph::new("* Captions might not be visible to people using older apps.")
            .style(Style::default().fg(Color::DarkGray))
            .render(splits[4], buf);
    }
}
