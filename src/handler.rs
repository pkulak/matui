use crate::app::{App, Popup};
use crate::matrix::matrix::format_emojis;
use crate::widgets::confirm::{Confirm, ConfirmBehavior};
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::{sort_rooms, Rooms};
use crate::widgets::signin::Signin;
use crate::widgets::EventResult;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ruma::events::receipt::ReceiptEventContent;
use ruma::OwnedUserId;

use crate::event::EventHandler;
use matrix_sdk::encryption::verification::{Emoji, SasVerification};
use matrix_sdk::room::{Joined, Room, RoomMember};
use ruma::events::AnyTimelineEvent;

#[derive(Clone, Debug)]
pub enum MatuiEvent {
    Confirm(String, String),
    Error(String),
    LoginComplete,
    LoginRequired,
    LoginStarted,
    ProgressStarted(String, u64),
    ProgressComplete,
    Receipt(Joined, ReceiptEventContent),
    RoomMember(Joined, RoomMember),
    RoomSelected(Joined),
    SyncComplete,
    SyncStarted(SyncType),
    Timeline(AnyTimelineEvent),
    TimelineBatch(Batch),
    Typing(Joined, Vec<OwnedUserId>),
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
        MatuiEvent::Confirm(header, msg) => {
            app.set_popup(Popup::Error(Error::with_heading(header, msg)));
        }
        MatuiEvent::Error(msg) => {
            app.set_popup(Popup::Error(Error::new(msg)));
        }
        MatuiEvent::LoginRequired => {
            app.set_popup(Popup::Signin(Signin::default()));
        }
        MatuiEvent::LoginStarted => {
            app.set_popup(Popup::Progress(Progress::new("Logging in", 0)));
        }
        MatuiEvent::LoginComplete => {
            app.popup = None;
        }
        MatuiEvent::ProgressStarted(msg, delay) => {
            app.set_popup(Popup::Progress(Progress::new(&msg, delay)))
        }
        MatuiEvent::ProgressComplete => app.popup = None,

        // Let the chat update when we learn about room membership
        MatuiEvent::RoomMember(room, member) => {
            if let Some(c) = &mut app.chat {
                c.room_member_event(room, member);
            }
        }
        MatuiEvent::RoomSelected(room) => app.select_room(room),
        MatuiEvent::SyncStarted(st) => {
            match st {
                SyncType::Initial => app.set_popup(Popup::Progress(Progress::new(
                    "Performing initial sync.",
                    0,
                ))),
                SyncType::Latest => app.set_popup(Popup::Progress(Progress::new("Syncing", 0))),
            };
        }
        MatuiEvent::SyncComplete => {
            app.popup = None;

            // now we can sync forever
            app.matrix.sync();

            // and show the first room
            let mut rooms = app.matrix.fetch_rooms();
            sort_rooms(&mut rooms);

            if let Some(room) = rooms.first() {
                app.select_room(room.inner.clone())
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
        MatuiEvent::Typing(joined, ids) => {
            if let Some(c) = &mut app.chat {
                c.typing_event(joined, ids);
            }
        }
        MatuiEvent::Receipt(joined, content) => {
            if let Some(c) = &mut app.chat {
                c.receipt_event(&joined, &content);
            }

            app.receipts.push_back((joined, content));

            if app.receipts.len() > 500 {
                app.receipts.pop_front();
            }
        }
        MatuiEvent::VerificationStarted(sas, emoji) => {
            app.sas = Some(sas);

            app.set_popup(Popup::Confirm(Confirm::new(
                "Verify".to_string(),
                format!(
                    "Do these emojis match your other session?\n\n{}",
                    format_emojis(emoji)
                ),
                "Yes".to_string(),
                "No".to_string(),
                ConfirmBehavior::Verification,
            )));
        }
        MatuiEvent::VerificationCompleted => {
            app.popup = None;
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

    // consider any key event also a sign of "focus"
    handle_focus_event(app);

    // give the popup first crack at the event
    let result = if let Some(w) = &mut app.popup {
        w.key_event(&key_event)
    } else {
        EventResult::Ignored
    };

    if let EventResult::Consumed(f) = result {
        f(app);
        return Ok(());
    }

    // we own a few key events
    match key_event.code {
        KeyCode::Char(' ') => {
            let current = app.chat.as_ref().map(|c| c.room());

            app.set_popup(Popup::Rooms(Rooms::new(app.matrix.clone(), current)));

            return Ok(());
        }
        KeyCode::Char('q') => {
            app.running = false;
            return Ok(());
        }
        KeyCode::Char('?') => {
            app.set_popup(Popup::Help(Help));
            return Ok(());
        }
        _ => {}
    }

    // and now pass it on to the chat.
    let result = if let Some(w) = &mut app.chat {
        match w.key_event(&key_event, handler) {
            Ok(r) => r,
            Err(err) => {
                app.set_popup(Popup::Error(Error::new(err.to_string())));
                return Ok(());
            }
        }
    } else {
        EventResult::Ignored
    };

    if let EventResult::Consumed(f) = result {
        f(app);
    }

    Ok(())
}

pub fn handle_focus_event(app: &mut App) {
    app.matrix.focus_event();

    // we consider it a room "visit" if you come back to the app and view a
    // room
    if let Some(chat) = &mut app.chat {
        app.matrix
            .clone()
            .room_visit_event(Room::Joined(chat.room()));
        chat.focus_event();
    }
}

pub fn handle_blur_event(app: &mut App) {
    app.matrix.blur_event();

    if let Some(chat) = &mut app.chat {
        chat.blur_event();
    }
}
