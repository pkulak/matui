use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use matrix_sdk::ruma::UserId;

use crate::app::Popup;
use crate::widgets::error::Error;
use crate::widgets::recover::Recover;
use crate::widgets::textinput::TextInput;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::{get_margin, EventResult};
use crate::{close, consumed};

pub struct Command {
    input: TextInput,
}

impl Default for Command {
    fn default() -> Self {
        let input = TextInput::new(":".to_string(), true, false);

        Self { input }
    }
}

impl Command {
    pub fn widget(&self) -> CommandWidget<'_> {
        CommandWidget { command: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.input.key_event(input) {
            return consumed!();
        }

        match input.code {
            KeyCode::Esc => close!(),
            KeyCode::Enter => {
                let value = self.input.value.trim().to_string();

                let (cmd, arg) = match value.split_once(char::is_whitespace) {
                    Some((cmd, arg)) => (cmd, arg.trim()),
                    None => (value.as_str(), ""),
                };

                match (cmd, arg) {
                    ("", _) => close!(),
                    ("verify", _) => Consumed(Box::new(|app| {
                        app.set_popup(Popup::Recover(Recover::default()))
                    })),
                    ("leave", _) => Consumed(Box::new(|app| {
                        if let Some(chat) = &app.chat {
                            app.matrix.leave_room(chat.room(), false);
                        }
                        app.close_popup();
                    })),
                    ("forget", _) => Consumed(Box::new(|app| {
                        if let Some(chat) = &app.chat {
                            app.matrix.leave_room(chat.room(), true);
                        }
                        app.close_popup();
                    })),
                    ("invite", "") => Consumed(Box::new(|app| {
                        app.set_popup(Popup::Error(Error::new(
                            "Usage: :invite <user>".to_string(),
                        )))
                    })),
                    ("invite", user) => {
                        let user = user.to_string();

                        Consumed(Box::new(move |app| {
                            let Some(chat) = &app.chat else {
                                app.close_popup();
                                return;
                            };

                            // a bare name is assumed to be on our homeserver
                            let id = if user.starts_with('@') {
                                UserId::parse(&user)
                            } else {
                                UserId::parse(format!(
                                    "@{}:{}",
                                    user,
                                    app.matrix.me().server_name()
                                ))
                            };

                            match id {
                                Ok(id) => {
                                    app.matrix.invite_user(chat.room(), id);
                                    app.close_popup();
                                }
                                Err(err) => app.set_popup(Popup::Error(Error::new(
                                    format!("Invalid user ID: {}", err),
                                ))),
                            }
                        }))
                    }
                    _ => {
                        let message = format!("Unknown command: {}", value);

                        Consumed(Box::new(move |app| {
                            app.set_popup(Popup::Error(Error::new(message)))
                        }))
                    }
                }
            }
            _ => Ignored,
        }
    }
}

pub struct CommandWidget<'a> {
    pub command: &'a Command,
}

impl Widget for CommandWidget<'_> {
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
            .title("Command")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        self.command.input.widget().render(splits[0], buf);

        Paragraph::new("Esc to cancel, Enter to run").render(splits[1], buf);
    }
}
