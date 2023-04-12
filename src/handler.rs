use crate::app::App;
use crate::matrix::matrix::format_emojis;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::{sort_rooms, Rooms};
use crate::widgets::signin::Signin;
use crate::widgets::Action::{ButtonNo, ButtonYes, Exit, SelectRoom};
use crate::widgets::EventResult::Consumed;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::event::EventHandler;
use matrix_sdk::encryption::verification::{Emoji, SasVerification};
use matrix_sdk::room::{Joined, Room, RoomMember};
use ruma::events::AnyTimelineEvent;

#[derive(Clone, Debug)]
pub enum MatuiEvent {
    Error(String),
    LoginComplete,
    LoginRequired,
    LoginStarted,
    ProgressStarted(String),
    ProgressComplete,
    RoomMembers(Joined, Vec<RoomMember>),
    RoomSelected(Joined),
    SyncComplete,
    SyncStarted(SyncType),
    Timeline(AnyTimelineEvent),
    TimelineBatch(Batch),
    VerificationStarted(SasVerification, [Emoji; 7]),
    VerificationCompleted,
}

#[derive(Clone, Debug)]
pub enum SyncType {
    Initial,
    Latest,
}

#[derive(Clone, Debug)]
pub struct Batch {
    pub room: Joined,
    pub events: Vec<AnyTimelineEvent>,
    pub cursor: Option<String>,
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

        // Let the chat update when we learn about room membership
        MatuiEvent::RoomMembers(room, members) => {
            if let Some(c) = &mut app.chat {
                c.room_members_event(room, members);
            }
        }
        MatuiEvent::ProgressStarted(msg) => app.progress = Some(Progress::new(&msg)),
        MatuiEvent::ProgressComplete => app.progress = None,
        MatuiEvent::RoomSelected(room) => app.select_room(room),
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

            // and show the first room
            let mut rooms = app.matrix.fetch_rooms();
            sort_rooms(&mut rooms);

            if let Some(room) = rooms.first() {
                app.select_room(room.room.clone())
            }
        }
        MatuiEvent::Timeline(event) => {
            if let Some(c) = &mut app.chat {
                c.timeline_event(event.clone());
            }

            // is it weird to send events all the way up here, then right
            // back down?
            app.matrix.timeline_event(event)
        }
        MatuiEvent::TimelineBatch(batch) => {
            if let Some(c) = &mut app.chat {
                c.batch_event(batch);
            }
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
    }
}

pub fn handle_key_event(
    key_event: KeyEvent,
    app: &mut App,
    handler: &EventHandler,
) -> anyhow::Result<()> {
    // ctrl-c always quits
    if key_event.modifiers == KeyModifiers::CONTROL && key_event.code == KeyCode::Char('c') {
        app.running = false;
        return Ok(());
    }

    // hide an error message on any key event
    if app.error.is_some() {
        app.error = None;
        return Ok(());
    }

    if let Some(w) = &mut app.signin {
        if let Consumed(ButtonYes) = w.input(&key_event) {
            app.matrix
                .login(w.id.value().as_str(), w.password.value().as_str());
            return Ok(());
        }
    }

    if let Some(w) = &mut app.rooms {
        match w.input(&key_event) {
            Consumed(SelectRoom(joined)) => {
                app.rooms = None;
                app.select_room(joined);
                return Ok(());
            }
            Consumed(Exit) => {
                app.rooms = None;
                return Ok(());
            }
            _ => {}
        }
    }

    if let Some(w) = &mut app.confirm {
        if let Some(sas) = app.sas.clone() {
            match w.input(&key_event) {
                Consumed(ButtonYes) => {
                    app.matrix.confirm_verification(sas);
                    app.confirm = None;
                    app.progress = Some(Progress::new("Waiting for your other device to confirm."));
                    return Ok(());
                }
                Consumed(ButtonNo) => {
                    app.matrix.mismatched_verification(sas);
                    app.confirm = None;
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    // there's probably a more elegant way to do this...
    if app.signin.is_none() && app.rooms.is_none() && app.confirm.is_none() && app.error.is_none() {
        if key_event.code == KeyCode::Char('q') {
            app.running = false;
            return Ok(());
        }

        if app.progress.is_none() {
            if let Some(chat) = &mut app.chat {
                match chat.input(&handler, &key_event) {
                    Err(err) => app.error = Some(Error::new(err.to_string())),
                    Ok(Consumed(_)) => return Ok(()),
                    _ => {}
                }
            }

            match key_event.code {
                KeyCode::Char(' ') => {
                    let current = &app.chat.as_ref().and_then(|c| c.room.clone());
                    app.rooms = Some(Rooms::new(app.matrix.clone(), current.clone()));
                }
                _ => {}
            }
        }
    }

    Ok(())
}

pub fn handle_focus_event(app: &mut App) {
    app.matrix.focus_event();

    // we consider it a room "visit" if you come back to the app and view a
    // room
    if let Some(chat) = &app.chat {
        if let Some(joined) = chat.room.clone() {
            app.matrix.clone().room_visit_event(Room::Joined(joined));
        }
    }
}

pub fn handle_blur_event(app: &mut App) {
    app.matrix.blur_event();
}
