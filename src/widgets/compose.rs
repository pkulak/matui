use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, MouseEvent};
use matrix_sdk::Room;
use matrix_sdk::room::RoomMember;
use matrix_sdk::ruma::{OwnedUserId, UserId};
use matrix_sdk_ui::Timeline;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::event::EventHandler;
use crate::matrix::Matrix;
use crate::matrix::roomcache::DecoratedRoom;
use crate::spawn::spawn_editor;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, get_margin};
use crate::{KeyCombo, consumed};

pub struct Compose {
    input: TextInput,
    room: DecoratedRoom,
    timeline: Arc<Timeline>,
    combo: KeyCombo,
    matrix: Matrix,
}

impl Compose {
    pub fn new(
        room: DecoratedRoom,
        timeline: Arc<Timeline>,
        matrix: Matrix,
        members: Vec<RoomMember>,
        text: &str,
    ) -> Self {
        let members: Vec<_> = members
            .iter()
            .map(|m| (m.display_name().map(String::from), m.user_id().to_owned()))
            .collect();
        let completions = member_completions(&members, room.inner.own_user_id());

        let mut input =
            TextInput::new("Message".to_string(), true, false).with_autocomplete('@', completions);

        // pasting leaves the cursor at the end
        input.paste_event(text);

        Self {
            input,
            room,
            timeline,
            combo: KeyCombo::new(vec!['j', 'j']),
            matrix,
        }
    }

    fn room(&self) -> Room {
        self.room.inner.clone()
    }

    fn send(&self, message: String) {
        self.matrix
            .send_text_message(self.timeline.clone(), message)
    }

    pub fn widget(&self) -> ComposeWidget<'_> {
        ComposeWidget { compose: self }
    }

    pub fn mouse_event(&mut self, event: &MouseEvent) {
        self.input.mouse_event(event);
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        if let Consumed(_) = self.input.paste_event(value) {
            self.matrix.typing_notification(self.room(), true);
            consumed!()
        } else {
            Ignored
        }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        // Handle "jj" to edit in external editor
        if let KeyCode::Char(c) = input.code
            && self.combo.record(c)
        {
            self.input.backspace();

            let result = spawn_editor(
                handler,
                Some((&self.matrix, self.room())),
                Some(&self.input.value),
                Some(&format!(
                    "<!-- The message above will be sent to {}. -->",
                    self.room.name
                )),
            );

            if let Ok(Some(message)) = result {
                if message.trim().is_empty() {
                    self.input.value = "".to_string();
                } else if message != self.input.value.trim() {
                    // Unchanged text means the editor was closed without saving.
                    self.send(message);

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
                    self.send(message);
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
            .constraints([Constraint::Percentage(100)])
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(3)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(1),
                Constraint::Percentage(100),
            ])
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

/// Builds `(label, @user:server)` entries for room members, excluding `me`.
/// Primary labels (display name, or localpart if unset) come first sorted
/// alphabetically, with colliding labels disambiguated by ID; secondary
/// localpart aliases follow so typing a localpart still matches while bare `@`
/// lists each member once by display name.
fn member_completions(
    members: &[(Option<String>, OwnedUserId)],
    me: &UserId,
) -> Vec<(String, String)> {
    let others: Vec<_> = members.iter().filter(|(_, id)| id != me).collect();

    let mut counts: HashMap<String, usize> = HashMap::new();
    for (name, id) in &others {
        let base = name
            .as_deref()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| id.localpart());
        *counts.entry(base.to_lowercase()).or_default() += 1;
    }

    let mut primary = Vec::with_capacity(others.len());
    let mut secondary = Vec::new();

    for (name, id) in others {
        let display = name.as_deref().filter(|n| !n.is_empty());
        let base = display.unwrap_or_else(|| id.localpart());
        let label = if counts[&base.to_lowercase()] > 1 {
            format!("{base} ({id})")
        } else {
            base.to_string()
        };

        primary.push((label, id.to_string()));

        if display.is_some() && id.localpart().to_lowercase() != base.to_lowercase() {
            secondary.push((id.localpart().to_string(), id.to_string()));
        }
    }

    primary.sort_by(|a, b| (a.0.to_lowercase(), &a.1).cmp(&(b.0.to_lowercase(), &b.1)));
    secondary.sort_by(|a, b| (a.0.to_lowercase(), &a.1).cmp(&(b.0.to_lowercase(), &b.1)));
    primary.extend(secondary);
    primary
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::owned_user_id;

    use super::member_completions;

    #[test]
    fn it_builds_member_completions() {
        let me = owned_user_id!("@me:kulak.us");
        let members = vec![
            (Some("Me".to_string()), me.clone()),
            (Some("Zack".to_string()), owned_user_id!("@azack:kulak.us")),
            (Some("Gwen".to_string()), owned_user_id!("@gwen:kulak.us")),
            (Some("Alex".to_string()), owned_user_id!("@alex:b.org")),
            (Some("alex".to_string()), owned_user_id!("@alex:a.org")),
            (None, owned_user_id!("@bot:kulak.us")),
        ];

        assert_eq!(
            member_completions(&members, &me),
            vec![
                ("alex (@alex:a.org)".to_string(), "@alex:a.org".to_string()),
                ("Alex (@alex:b.org)".to_string(), "@alex:b.org".to_string()),
                ("bot".to_string(), "@bot:kulak.us".to_string()),
                ("Gwen".to_string(), "@gwen:kulak.us".to_string()),
                ("Zack".to_string(), "@azack:kulak.us".to_string()),
                ("azack".to_string(), "@azack:kulak.us".to_string()),
            ]
        );
    }
}
