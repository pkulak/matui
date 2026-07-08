use crossterm::event::{KeyCode, KeyEvent};
use log::error;
use matrix_sdk::authentication::oauth::qrcode::CheckCodeSender;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::app::App;
use crate::event::EventHandler;
use crate::handler::MatuiEvent;
use crate::spawn::spawn_editor;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, get_margin};
use crate::{close, consumed};

pub enum Qr {
    Code(String),
    Check {
        input: TextInput,
        sender: CheckCodeSender,
    },
    Auth(String),
}

impl Qr {
    pub fn code(code: String) -> Self {
        Self::Code(code)
    }

    pub fn check(sender: CheckCodeSender) -> Self {
        Self::Check {
            input: TextInput::new(String::new(), true, false),
            sender,
        }
    }

    pub fn auth(url: String) -> Self {
        Self::Auth(url)
    }

    pub fn widget(&self) -> QrWidget<'_> {
        QrWidget { qr: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        match self {
            Qr::Code(_) => match input.code {
                KeyCode::Esc => close!(),
                _ => consumed!(),
            },
            Qr::Check {
                input: field,
                sender,
            } => {
                if let Consumed(_) = field.key_event(input) {
                    return consumed!();
                }

                match input.code {
                    KeyCode::Esc => close!(),
                    KeyCode::Enter => {
                        let Ok(code) = field.value().trim().parse::<u8>() else {
                            return consumed!();
                        };

                        let sender = sender.clone();
                        Consumed(Box::new(move |_| {
                            App::send(MatuiEvent::ProgressStarted(
                                "Checking QR login code.".to_string(),
                                0,
                            ));

                            App::spawn(async move {
                                if let Err(err) = sender.send(code).await {
                                    App::send(MatuiEvent::Error(err.to_string()));
                                }
                            });
                        }))
                    }
                    _ => Ignored,
                }
            }
            Qr::Auth(url) => match input.code {
                KeyCode::Char('o') => {
                    if let Err(err) = open::that_detached(url.as_str()) {
                        error!("could not open link: {}", err);
                    }

                    consumed!()
                }
                KeyCode::Char('e') => {
                    let _ = spawn_editor(handler, None, Some(url.as_str()), None);
                    consumed!()
                }
                KeyCode::Esc => close!(),
                _ => consumed!(),
            },
        }
    }
}

pub struct QrWidget<'a> {
    pub qr: &'a Qr,
}

impl Widget for QrWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self.qr {
            Qr::Code(code) => render_code(code, area, buf),
            Qr::Check { input, .. } => render_check(input, area, buf),
            Qr::Auth(url) => render_auth(url, area, buf),
        }
    }
}

fn frame(area: Rect, buf: &mut Buffer, width: u16, height: u16) -> Vec<Rect> {
    let area = Layout::default()
        .horizontal_margin(get_margin(area.width, width))
        .vertical_margin(get_margin(area.height, height))
        .constraints([Constraint::Percentage(100)].as_ref())
        .split(area)[0];

    buf.merge(&Buffer::empty(area));

    let block = Block::default()
        .title("QR Login")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .style(Style::default().bg(Color::Reset));

    block.render(area, buf);

    Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .vertical_margin(1)
        .constraints(
            [
                Constraint::Length(2),
                Constraint::Percentage(100),
                Constraint::Length(1),
            ]
            .as_ref(),
        )
        .split(area)
        .to_vec()
}

fn render_code(code: &str, area: Rect, buf: &mut Buffer) {
    let width = code.lines().map(str::len).max().unwrap_or(60) as u16 + 4;
    let height = code.lines().count() as u16 + 6;
    let splits = frame(area, buf, width, height);

    Paragraph::new("Scan this QR code on the new device to let it log in.")
        .alignment(Alignment::Center)
        .render(splits[0], buf);
    Paragraph::new(code.to_string())
        .alignment(Alignment::Center)
        .render(splits[1], buf);
    Paragraph::new("Esc closes this popup.")
        .alignment(Alignment::Center)
        .render(splits[2], buf);
}

fn render_check(input: &TextInput, area: Rect, buf: &mut Buffer) {
    let splits = frame(area, buf, 60, 10);

    Paragraph::new("Enter the check code shown on the new device.")
        .alignment(Alignment::Center)
        .render(splits[0], buf);

    let input_row = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Percentage(100)].as_ref())
        .split(splits[1])[0];

    let input_area = Layout::default()
        .horizontal_margin(get_margin(input_row.width, 13))
        .constraints([Constraint::Percentage(100)].as_ref())
        .split(input_row)[0];

    let input_splits = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(7), Constraint::Length(6)].as_ref())
        .split(input_area);

    Paragraph::new("Code:")
        .alignment(Alignment::Right)
        .style(Style::default().fg(Color::LightGreen))
        .render(input_splits[0], buf);

    input.widget().render(input_splits[1], buf);

    Paragraph::new("Enter submits, Esc cancels.")
        .alignment(Alignment::Center)
        .render(splits[2], buf);
}

fn render_auth(url: &str, area: Rect, buf: &mut Buffer) {
    let splits = frame(area, buf, 70, 10);

    Paragraph::new("Confirm the new login in your browser.")
        .alignment(Alignment::Center)
        .render(splits[0], buf);
    Paragraph::new(url.to_string()).render(splits[1], buf);
    Paragraph::new("o reopen, e view URL, Esc close.")
        .alignment(Alignment::Center)
        .render(splits[2], buf);
}
