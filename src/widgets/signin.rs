use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Widget};

use crate::consumed;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::button::Button;
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, Focusable, focus_next, focus_prev, get_margin};

pub struct Signin {
    homeserver: String,
    pub username: TextInput,
    pub password: TextInput,
    submit: Button,
}

impl Signin {
    pub fn new(homeserver: String) -> Self {
        let username = TextInput::new("Username".to_string(), true, false);
        let password = TextInput::new("Password".to_string(), false, true);

        let submit = Button::new("Submit".to_string(), false);

        Self {
            homeserver,
            username,
            password,
            submit,
        }
    }

    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        vec![
            Box::new(&mut self.username),
            Box::new(&mut self.password),
            Box::new(&mut self.submit),
        ]
    }

    pub fn widget(&self) -> SigninWidget<'_> {
        SigninWidget { signin: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.username.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.password.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.submit.key_event(input) {
            let homeserver = self.homeserver.clone();
            let username = self.username.value();
            let password = self.password.value();

            return EventResult::Consumed(Box::new(move |app| {
                app.matrix
                    .login(homeserver.as_str(), username.trim(), password.as_str());
                app.close_popup();
            }));
        }

        match input.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => focus_next(self.focus_order()),
            KeyCode::BackTab | KeyCode::Up => focus_prev(self.focus_order()),
            _ => Ignored,
        }
    }
}

pub struct SigninWidget<'a> {
    pub signin: &'a Signin,
}

impl Widget for SigninWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = Layout::default()
            .horizontal_margin(get_margin(area.width, 60))
            .vertical_margin(get_margin(area.height, 17))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        buf.merge(&Buffer::empty(area));

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(2)
            .constraints(
                [
                    Constraint::Length(1),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
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
        Paragraph::new(format!("Homeserver: {}", self.signin.homeserver)).render(splits[0], buf);
        self.signin.username.widget().render(splits[2], buf);
        self.signin.password.widget().render(splits[4], buf);

        let submit_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[6])[1];

        self.signin.submit.widget().render(submit_area, buf);
    }
}
