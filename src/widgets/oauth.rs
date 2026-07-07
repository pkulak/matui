use crossterm::event::{KeyCode, KeyEvent};
use log::error;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::event::EventHandler;
use crate::spawn::spawn_editor;
use crate::{close, consumed};

use super::{EventResult, get_margin};

/// Shown while an OAuth login waits on the user's browser.
pub struct Oauth {
    url: String,
    cancel: Arc<Notify>,
}

impl Oauth {
    pub fn new(url: String, cancel: Arc<Notify>) -> Self {
        Self { url, cancel }
    }

    pub fn widget(&self) -> OauthWidget<'_> {
        OauthWidget { oauth: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        match input.code {
            KeyCode::Char('o') => {
                if let Err(err) = open::that_detached(self.url.as_str()) {
                    error!("could not open link: {}", err);
                }

                consumed!()
            }
            KeyCode::Char('e') => {
                // just for viewing/copying; whatever comes back is noise
                let _ = spawn_editor(handler, None, Some(self.url.as_str()), None);
                consumed!()
            }
            KeyCode::Esc => {
                self.cancel.notify_one();
                close!()
            }
            // swallow everything else: a stray key shouldn't kill a
            // pending login
            _ => consumed!(),
        }
    }
}

pub struct OauthWidget<'a> {
    pub oauth: &'a Oauth,
}

impl Widget for OauthWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 9))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(4)
            .vertical_margin(1)
            .constraints(
                [
                    Constraint::Length(2),
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(1),
                ]
                .as_ref(),
            )
            .split(area);

        let block = Block::default()
            .title("Sign In")
            .title_alignment(Alignment::Center)
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .style(Style::default().bg(Color::Reset));

        block.render(area, buf);

        Paragraph::new("Waiting for you to sign in with your browser.").render(splits[0], buf);
        Paragraph::new("o    Open the link again.").render(splits[1], buf);
        Paragraph::new("e    View the link in your editor.").render(splits[2], buf);
        Paragraph::new("Esc  Cancel.").render(splits[3], buf);
    }
}
