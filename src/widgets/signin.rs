use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, BorderType, Borders, Widget};

use crate::consumed;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::button::Button;
use crate::widgets::textinput::TextInput;
use crate::widgets::{EventResult, Focusable, focus_next, focus_prev, get_margin};

pub struct Signin {
    pub homeserver: TextInput,
    pub username: TextInput,
    pub password: TextInput,
    submit: Button,
    sso: Option<Button>,
    probed: Option<String>,
}

impl Default for Signin {
    fn default() -> Self {
        let homeserver = TextInput::new("Homeserver".to_string(), true, false);
        let username = TextInput::new("Username".to_string(), false, false);
        let password = TextInput::new("Password".to_string(), false, true);

        let submit = Button::new("Submit".to_string(), false);

        Self {
            homeserver,
            username,
            password,
            submit,
            sso: None,
            probed: None,
        }
    }
}

impl Signin {
    /// The homeserver we probed supports OIDC; offer to log in with it.
    pub fn oidc_available(&mut self, issuer: &str, server: &str) {
        if self.homeserver.value.trim() == server {
            self.sso = Some(Button::new(format!("Log in with {}", issuer), false));
        }
    }

    fn focus_order(&mut self) -> Vec<Box<dyn Focusable + '_>> {
        let mut order: Vec<Box<dyn Focusable + '_>> = vec![
            Box::new(&mut self.homeserver),
            Box::new(&mut self.username),
            Box::new(&mut self.password),
            Box::new(&mut self.submit),
        ];

        if let Some(sso) = &mut self.sso {
            order.push(Box::new(sso));
        }

        order
    }

    /// When the homeserver field loses focus, see if its server does OIDC.
    fn maybe_probe(&mut self) -> EventResult {
        let server = self.homeserver.value.trim().to_string();

        if !self.homeserver.focused && !server.is_empty() && self.probed.as_deref() != Some(&server)
        {
            self.probed = Some(server.clone());
            self.sso = None;

            return Consumed(Box::new(move |app| app.matrix.check_oidc(server)));
        }

        consumed!()
    }

    pub fn widget(&self) -> SigninWidget<'_> {
        SigninWidget { signin: self }
    }

    pub fn key_event(&mut self, input: &KeyEvent) -> EventResult {
        if let Consumed(_) = self.homeserver.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.username.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.password.key_event(input) {
            return consumed!();
        }

        if let Consumed(_) = self.submit.key_event(input) {
            let homeserver = self.homeserver.value();
            let username = self.username.value();
            let password = self.password.value();

            return EventResult::Consumed(Box::new(move |app| {
                app.matrix
                    .login(homeserver.trim(), username.trim(), password.as_str());
                app.close_popup();
            }));
        }

        if let Some(sso) = &mut self.sso
            && let Consumed(_) = sso.key_event(input)
        {
            let homeserver = self.homeserver.value().trim().to_string();

            return EventResult::Consumed(Box::new(move |app| {
                app.matrix.login_oauth(homeserver);
                app.close_popup();
            }));
        }

        match input.code {
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => {
                focus_next(self.focus_order());
                self.maybe_probe()
            }
            KeyCode::BackTab | KeyCode::Up => {
                focus_prev(self.focus_order());
                self.maybe_probe()
            }
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
            .vertical_margin(get_margin(area.height, 24))
            .constraints([Constraint::Percentage(100)].as_ref())
            .split(area)[0];

        let splits = Layout::default()
            .direction(Direction::Vertical)
            .horizontal_margin(8)
            .vertical_margin(3)
            .constraints(
                [
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                    Constraint::Length(3),
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
        self.signin.homeserver.widget().render(splits[0], buf);
        self.signin.username.widget().render(splits[2], buf);
        self.signin.password.widget().render(splits[4], buf);

        // pop the submit button on the right side
        let submit_area = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
            .split(splits[6])[1];

        self.signin.submit.widget().render(submit_area, buf);

        if let Some(sso) = &self.signin.sso {
            sso.widget().render(splits[7], buf);
        }
    }
}
