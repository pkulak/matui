use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use matrix_sdk::ruma::{IdParseError, OwnedUserId, RoomOrAliasId, UserId};

use crate::app::{App, Popup};
use crate::matrix::matrix::Moderation;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::create::Create;
use crate::widgets::error::Error;
use crate::widgets::recover::Recover;
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, get_margin};
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
    /// Prefill with a user ID, cursor at the front, so all that's left
    /// to type is the command name.
    pub fn with_user(user: &UserId) -> Self {
        let mut command = Self::default();
        command.input.value = format!(" {}", user);
        command
    }

    pub fn widget(&self) -> CommandWidget<'_> {
        CommandWidget { command: self }
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
                let value = self.input.value.trim().to_string();

                let (cmd, arg) = match value.split_once(char::is_whitespace) {
                    Some((cmd, arg)) => (cmd, arg.trim()),
                    None => (value.as_str(), ""),
                };

                match (cmd, arg) {
                    ("", _) => close!(),
                    ("q", _) => Consumed(Box::new(|app| app.running = false)),
                    ("logout", _) => Consumed(Box::new(|app| {
                        app.matrix.logout();
                        app.close_popup();
                    })),
                    ("qr", _) => Consumed(Box::new(|app| {
                        app.matrix.grant_login_with_qr();
                        app.close_popup();
                    })),
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
                    ("create", _) => Consumed(Box::new(|app| {
                        app.set_popup(Popup::Create(Create::default()))
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

                            match resolve_user(app, &user) {
                                Ok(id) => {
                                    app.matrix.invite_user(chat.room(), id);
                                    app.close_popup();
                                }
                                Err(err) => app.set_popup(Popup::Error(Error::new(format!(
                                    "Invalid user ID: {}",
                                    err
                                )))),
                            }
                        }))
                    }
                    ("dm", "") => Consumed(Box::new(|app| {
                        app.set_popup(Popup::Error(Error::new(
                            "Usage: :dm <user> [nocrypt]".to_string(),
                        )))
                    })),
                    ("dm", arg) => {
                        let (user, flag) = match arg.split_once(char::is_whitespace) {
                            Some((user, flag)) => (user, flag.trim()),
                            None => (arg, ""),
                        };

                        if !flag.is_empty() && flag != "nocrypt" {
                            let message = format!("Unknown flag: {}", flag);

                            Consumed(Box::new(move |app| {
                                app.set_popup(Popup::Error(Error::new(message)))
                            }))
                        } else {
                            let encrypted = flag.is_empty();
                            let user = user.to_string();

                            Consumed(Box::new(move |app| match resolve_user(app, &user) {
                                Ok(id) => {
                                    app.matrix.create_dm(id, encrypted);
                                    app.close_popup();
                                }
                                Err(err) => app.set_popup(Popup::Error(Error::new(format!(
                                    "Invalid user ID: {}",
                                    err
                                )))),
                            }))
                        }
                    }
                    (action @ ("ban" | "unban" | "kick"), "") => {
                        let message = format!("Usage: :{} <user> [reason]", action);

                        Consumed(Box::new(move |app| {
                            app.set_popup(Popup::Error(Error::new(message)))
                        }))
                    }
                    (action @ ("ban" | "unban" | "kick"), arg) => {
                        let action = match action {
                            "ban" => Moderation::Ban,
                            "unban" => Moderation::Unban,
                            _ => Moderation::Kick,
                        };

                        // first token is the user, the rest is an optional reason
                        let (user, reason) = match arg.split_once(char::is_whitespace) {
                            Some((user, reason)) => {
                                (user.to_string(), Some(reason.trim().to_string()))
                            }
                            None => (arg.to_string(), None),
                        };

                        Consumed(Box::new(move |app| {
                            let Some(chat) = &app.chat else {
                                app.close_popup();
                                return;
                            };

                            match resolve_user(app, &user) {
                                Ok(id) => {
                                    app.matrix.moderate(chat.room(), action, id, reason);
                                    app.close_popup();
                                }
                                Err(err) => app.set_popup(Popup::Error(Error::new(format!(
                                    "Invalid user ID: {}",
                                    err
                                )))),
                            }
                        }))
                    }
                    (action @ ("ignore" | "unignore"), "") => {
                        let message = format!("Usage: :{} <user>", action);

                        Consumed(Box::new(move |app| {
                            app.set_popup(Popup::Error(Error::new(message)))
                        }))
                    }
                    (action @ ("ignore" | "unignore"), user) => {
                        let ignore = action == "ignore";
                        let user = user.to_string();

                        Consumed(Box::new(move |app| match resolve_user(app, &user) {
                            Ok(id) => {
                                app.matrix.ignore_user(id, ignore);
                                app.close_popup();
                            }
                            Err(err) => app.set_popup(Popup::Error(Error::new(format!(
                                "Invalid user ID: {}",
                                err
                            )))),
                        }))
                    }
                    ("join", "") => Consumed(Box::new(|app| {
                        app.set_popup(Popup::Error(Error::new(
                            "Usage: :join <#alias, !id, or name>".to_string(),
                        )))
                    })),
                    ("join", arg) if arg.starts_with('#') || arg.starts_with('!') => {
                        match RoomOrAliasId::parse(arg) {
                            Ok(target) => Consumed(Box::new(move |app| {
                                app.matrix.join_room_by_target(target);
                                app.close_popup();
                            })),
                            Err(err) => {
                                let message = format!("Invalid room: {}", err);

                                Consumed(Box::new(move |app| {
                                    app.set_popup(Popup::Error(Error::new(message)))
                                }))
                            }
                        }
                    }
                    ("join", name) => {
                        let name = name.to_string();

                        Consumed(Box::new(move |app| {
                            app.matrix.join_room_by_name(name);
                            app.close_popup();
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

// a bare name is assumed to be on our homeserver
fn resolve_user(app: &App, user: &str) -> Result<OwnedUserId, IdParseError> {
    if user.starts_with('@') {
        UserId::parse(user)
    } else {
        UserId::parse(format!("@{}:{}", user, app.matrix.me().server_name()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn with_user_leaves_the_cursor_at_the_front() {
        let mut command = Command::with_user(user_id!("@bob:example.com"));

        for c in "dm".chars() {
            command.key_event(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        assert_eq!(command.input.value, "dm @bob:example.com");
    }
}
