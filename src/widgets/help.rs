use crate::{close, consumed};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Row, Table, Widget};

use crate::widgets::get_margin;

use super::EventResult;

#[derive(Default)]
pub struct Help {
    commands: bool,
}

impl Help {
    pub fn widget(&self) -> HelpWidget {
        HelpWidget {
            commands: self.commands,
        }
    }

    pub fn key_event(&mut self, event: &KeyEvent) -> EventResult {
        match event.code {
            KeyCode::Char('?') => {
                self.commands = !self.commands;
                consumed!()
            }
            _ => close!(),
        }
    }
}

pub struct HelpWidget {
    commands: bool,
}

impl Widget for HelpWidget {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let (title, key_width, rows) = if self.commands {
            ("Help — Commands", 9, command_rows())
        } else {
            ("Help — Keys", 7, key_rows())
        };

        // header, header margin, borders and padding
        let height = rows.len() as u16 + 6;

        let area = Layout::default()
            .direction(Direction::Horizontal)
            .vertical_margin(get_margin(area.height, height))
            .horizontal_margin(get_margin(area.width, 70))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        // Render the main block
        let block = Block::default()
            .title(title)
            .title_alignment(Alignment::Center)
            .style(Style::default().bg(Color::Reset))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);

        block.render(area, buf);

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .vertical_margin(2)
            .horizontal_margin(2)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area);

        let area = Layout::default()
            .horizontal_margin(1)
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(splits[0])[0];

        let widths = &[Constraint::Length(key_width), Constraint::Percentage(90)];

        Table::new(rows, widths)
            .header(
                Row::new(vec!["Key", "Description"])
                    .style(Style::default().fg(Color::Green))
                    .bottom_margin(1),
            )
            .column_spacing(1)
            .render(area, buf)
    }
}

fn key_rows() -> Vec<Row<'static>> {
    vec![
        Row::new(vec!["Space", "Show the room switcher"]),
        Row::new(vec!["j*", "Select one line down."]),
        Row::new(vec!["k*", "Select one line up."]),
        Row::new(vec!["Ctrl+d", "Select half a page down."]),
        Row::new(vec!["Ctrl+u", "Select half a page up."]),
        Row::new(vec!["G", "Select latest message."]),
        Row::new(vec!["i", "Compose a new message."]),
        Row::new(vec![
            "I",
            "Create a new message directly in the external editor.",
        ]),
        Row::new(vec![
            "Enter",
            "Open the selected message, or its thread if it has one.",
        ]),
        Row::new(vec!["Esc", "Leave the thread view."]),
        Row::new(vec![
            "s",
            "Save the selected message (images, videos and audio).",
        ]),
        Row::new(vec![
            "c",
            "Edit the selected message in the external editor.",
        ]),
        Row::new(vec!["r", "React to the selected message."]),
        Row::new(vec!["R", "Reply to the selected message."]),
        Row::new(vec![
            "T",
            "Start (or open) a thread on the selected message.",
        ]),
        Row::new(vec![
            "U",
            "Open the command prompt with the sender filled in.",
        ]),
        Row::new(vec![
            "v",
            "View the selected message in the external editor.",
        ]),
        Row::new(vec![
            "V",
            "View the room and its members in the external editor.",
        ]),
        Row::new(vec!["u", "Upload a file."]),
        Row::new(vec![
            "m",
            "Mute or unmute the current room (until restart).",
        ]),
        Row::new(vec!["/", "Search the current room"]),
        Row::new(vec![":", "Open the command prompt."]),
        Row::new(vec!["?", "Show this help; press again for commands."]),
        Row::new(vec!["", "* arrow keys are fine too."]),
    ]
}

fn command_rows() -> Vec<Row<'static>> {
    vec![
        Row::new(vec![":verify", "Verify this client with your passphrase."]),
        Row::new(vec![":leave", "Leave the current room."]),
        Row::new(vec![
            ":forget",
            "Leave the current room and forget its history.",
        ]),
        Row::new(vec![":invite", "Invite a user (bob, or @bob:example.com)."]),
        Row::new(vec![":create", "Create a new room."]),
        Row::new(vec![
            ":join",
            "Join a room: #alias, !id, or a name to search for.",
        ]),
        Row::new(vec![
            ":dm",
            "Open a DM with a user (\"nocrypt\" skips encryption).",
        ]),
        Row::new(vec![
            ":ban",
            "Ban a user from the room, with an optional reason.",
        ]),
        Row::new(vec![":unban", "Lift a user's ban."]),
        Row::new(vec![
            ":kick",
            "Kick a user from the room, with an optional reason.",
        ]),
        Row::new(vec![
            ":ignore",
            "Hide a user's messages, everywhere, from now on.",
        ]),
        Row::new(vec![":unignore", "Stop ignoring a user."]),
        Row::new(vec![":q", "Quit Matui."]),
        Row::new(vec![":logout", "Log out, remove this session, and quit."]),
        Row::new(vec!["?", "Back to key bindings."]),
    ]
}
