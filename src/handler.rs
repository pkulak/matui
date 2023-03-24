use crate::app::App;
use crate::matrix::format_emojis;
use crate::widgets::chat::Chat;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::Rooms;
use crate::widgets::signin::Signin;
use crate::widgets::Action::{ButtonNo, ButtonYes};
use crate::widgets::EventResult::Consumed;
use crossterm::event::{KeyCode, KeyEvent};
use matrix_sdk::encryption::verification::{Emoji, SasVerification};
use matrix_sdk::room::Joined;

pub enum MatuiEvent {
    Error(String),
    LoginComplete,
    LoginRequired,
    LoginStarted,
    RoomSelected(Joined),
    SyncComplete,
    SyncStarted(SyncType),
    VerificationStarted(SasVerification, [Emoji; 7]),
    VerificationCompleted,
}

pub enum SyncType {
    Initial,
    Latest,
}

pub fn handle_app_event(event: MatuiEvent, app: &mut App) {
    match event {
        MatuiEvent::Error(msg) => {
            app.error = Some(Error::new(msg));
            app.progress = None;
        }
        MatuiEvent::LoginRequired => {
            app.signin = Some(Signin::default());
        }
        MatuiEvent::LoginStarted => {
            app.error = None;
            app.progress = Some(Progress::new("Logging in"));
        }
        MatuiEvent::LoginComplete => {
            app.error = None;
            app.progress = None;
        }
        MatuiEvent::SyncStarted(st) => {
            app.error = None;
            match st {
                SyncType::Initial => app.progress = Some(Progress::new("Performing initial sync.")),
                SyncType::Latest => app.progress = Some(Progress::new("Syncing")),
            };
        }
        MatuiEvent::SyncComplete => {
            app.error = None;
            app.progress = None;
            app.signin = None;

            // now we can sync forever
            app.matrix.sync();
        }
        MatuiEvent::VerificationStarted(sas, emoji) => {
            app.sas = Some(sas);
            app.confirm = Some(Confirm::new(
                "Verify".to_string(),
                format!(
                    "Do these emojis match your other session?\n\n{}",
                    format_emojis(emoji)
                ),
                "Yes".to_string(),
                "No".to_string(),
            ));
        }
        MatuiEvent::VerificationCompleted => {
            app.progress = None;
            app.sas = None;
        }
        MatuiEvent::RoomSelected(joined) => {
            app.rooms = None;

            let mut room = Chat::new(app.matrix.clone());
            room.set_room(joined);

            app.chat = Some(room);
        }
    }
}

pub fn handle_key_event(key_event: KeyEvent, app: &mut App) -> anyhow::Result<()> {
    // hide an error message on any key event
    if app.error.is_some() {
        app.error = None;
        return Ok(());
    }

    match key_event.code {
        KeyCode::Esc => {
            if app.rooms.is_some() {
                app.rooms = None;
            } else {
                app.running = false;
            }
        }
        _ => {
            if let Some(w) = &mut app.signin {
                if let Consumed(ButtonYes) = w.input(&key_event) {
                    app.matrix
                        .login(w.id.value().as_str(), w.password.value().as_str());
                }
            }

            if let Some(w) = &mut app.rooms {
                w.input(&key_event)
            }

            if let Some(w) = &mut app.confirm {
                if let Some(sas) = app.sas.clone() {
                    match w.input(&key_event) {
                        Consumed(ButtonYes) => {
                            app.matrix.confirm_verification(sas);
                            app.confirm = None;
                            app.progress =
                                Some(Progress::new("Waiting for your other device to confirm."));
                        }
                        Consumed(ButtonNo) => {
                            app.matrix.mismatched_verification(sas);
                            app.confirm = None;
                        }
                        _ => {}
                    }
                }
            }

            if app.signin.is_none() && app.rooms.is_none() && key_event.code == KeyCode::Char('r') {
                app.rooms = Some(Rooms::new(app.matrix.joined_rooms(), app.send.clone()));
            }
        }
    }

    Ok(())
}
