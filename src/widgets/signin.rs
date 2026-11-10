use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::authentication::oauth::qrcode::CheckCodeSender;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Widget;
use tokio::sync::Notify;

use crate::app::NoticeId;
use crate::consumed;
use crate::event::EventHandler;
use crate::handler::Login;
use crate::widgets::EventResult;
use crate::widgets::EventResult::{Consumed, Ignored};
use crate::widgets::homeserver::Homeserver;
use crate::widgets::oauth::Oauth;
use crate::widgets::password::Password;
use crate::widgets::progress::Progress;
use crate::widgets::qr::{check_input, render_check, render_code};
use crate::widgets::textinput::TextInput;

/// The screen for signing in, at the step it's reached.
pub enum Signin {
    Homeserver(Homeserver),
    Password(Password),
    Browser(Oauth),
    QrCode {
        code: String,
        cancel: Arc<Notify>,
    },
    QrCheck {
        input: TextInput,
        sender: CheckCodeSender,
        cancel: Arc<Notify>,
    },
    /// Waiting on the other device.
    Waiting(Progress),
}

impl From<Login> for Signin {
    fn from(step: Login) -> Self {
        match step {
            Login::Homeserver => Signin::Homeserver(Homeserver::default()),
            Login::Password { homeserver } => Signin::Password(Password::new(homeserver)),
            Login::Browser { url, cancel } => Signin::Browser(Oauth::new(url, cancel)),
            Login::QrCode { code, cancel } => Signin::QrCode { code, cancel },
            Login::QrCheck { sender, cancel } => Signin::QrCheck {
                input: check_input(),
                sender,
                cancel,
            },
            Login::QrApprove { code } => waiting(&format!("Enter {code} on your other device.")),
            Login::SyncingSecrets => waiting("Syncing secrets."),
        }
    }
}

fn waiting(text: &str) -> Signin {
    Signin::Waiting(Progress::new(NoticeId::next(), text, 0))
}

impl Signin {
    pub fn widget(&self) -> SigninWidget<'_> {
        SigninWidget { signin: self }
    }

    pub fn tick_event(&mut self, timestamp: usize) {
        if let Signin::Waiting(progress) = self {
            progress.tick_event(timestamp);
        }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        match self {
            Signin::Homeserver(w) => w.paste_event(value),
            Signin::Password(w) => w.paste_event(value),
            Signin::QrCheck { input, .. } => input.paste_event(value),
            Signin::Browser(_) | Signin::QrCode { .. } | Signin::Waiting(_) => Ignored,
        }
    }

    pub fn key_event(&mut self, input: &KeyEvent, handler: &EventHandler) -> EventResult {
        match self {
            Signin::Homeserver(w) => w.key_event(input),
            Signin::Password(w) => w.key_event(input),
            Signin::Browser(w) => w.key_event(input, handler),
            Signin::QrCode { cancel, .. } => {
                if input.code == KeyCode::Esc {
                    cancel.notify_one();
                }

                consumed!()
            }
            Signin::QrCheck {
                input: field,
                sender,
                cancel,
            } => {
                if let Consumed(_) = field.key_event(input) {
                    return consumed!();
                }

                match input.code {
                    KeyCode::Esc => {
                        cancel.notify_one();
                        consumed!()
                    }
                    KeyCode::Enter => {
                        let Ok(code) = field.value().trim().parse::<u8>() else {
                            return consumed!();
                        };

                        let sender = sender.clone();
                        *self = waiting("Checking QR login code.");

                        Consumed(Box::new(move |app| {
                            app.matrix.send_check_code(sender, code)
                        }))
                    }
                    _ => Ignored,
                }
            }
            Signin::Waiting(_) => Ignored,
        }
    }
}

pub struct SigninWidget<'a> {
    signin: &'a Signin,
}

impl Widget for SigninWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        match self.signin {
            Signin::Homeserver(w) => w.widget().render(area, buf),
            Signin::Password(w) => w.widget().render(area, buf),
            Signin::Browser(w) => w.widget().render(area, buf),
            Signin::QrCode { code, .. } => render_code(code, true, area, buf),
            Signin::QrCheck { input, .. } => render_check(input, true, area, buf),
            Signin::Waiting(w) => w.widget().render(area, buf),
        }
    }
}
