use crate::app::{App, Popup};
use crate::matrix::RoomKey;
use crate::matrix::emoji::format_emojis;
use crate::spawn::pasted_file_paths;
use crate::widgets::EventResult;
use crate::widgets::confirm::{Confirm, ConfirmBehavior};
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::homeserver::Homeserver;
use crate::widgets::oauth::Oauth;
use crate::widgets::progress::Progress;
use crate::widgets::qr::Qr;
use crate::widgets::rooms::{Rooms, sort_rooms};
use crate::widgets::signin::Signin;
use crate::widgets::upload::Upload;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::DynamicImage;
use matrix_sdk::authentication::oauth::qrcode::CheckCodeSender;
use matrix_sdk::ruma::directory::PublicRoomsChunk;
use matrix_sdk::ruma::events::receipt::ReceiptEventContent;
use matrix_sdk::ruma::{OwnedMxcUri, OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::event::EventHandler;
use matrix_sdk::encryption::verification::{
    CancelInfo, Emoji, SasVerification, VerificationRequest,
};
use matrix_sdk::room::{Room, RoomMember};
use matrix_sdk::ruma::events::AnyTimelineEvent;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum MatuiEvent {
    Confirm(String, String),
    /// Events that failed to decrypt before, now decrypted. They're old, so
    /// they don't notify.
    Decrypted(Vec<AnyTimelineEvent>),
    Error(String),
    Invited(Room, String),
    LoggedOut,
    LoginComplete,
    LoginRequired,
    LoginStarted,
    PasswordLoginRequired(String),
    /// The OAuth flow is waiting on the browser at the given URL; the
    /// Notify cancels it.
    OauthStarted(String, Arc<Notify>),
    /// A picture for a room's chat, or `None` if it couldn't be loaded.
    Picture(OwnedRoomId, OwnedMxcUri, Option<DynamicImage>),
    ProgressStarted(String, u64),
    ProgressComplete,
    QrLoginAuth(String),
    QrLoginCode(String),
    QrLoginDone,
    QrLoginScanned(CheckCodeSender),
    QrLoginSyncing,
    Recover(String),
    Receipt(Room, ReceiptEventContent),
    RoomFound(PublicRoomsChunk),
    /// Room keys arrived, or `None` if some arrived but we lost track of
    /// which.
    RoomKeys(Option<Vec<RoomKey>>),
    RoomLeft(Room),
    RoomMember(Room, RoomMember),
    RoomSelected(Room),
    Search(String),
    SyncComplete,
    SyncStarted(SyncType),
    Timeline(AnyTimelineEvent),
    TimelineBatch(Batch),
    Typing(Room, Vec<OwnedUserId>),
    /// Offer to verify this session after login, by asking another session
    /// when there's a verified one to ask, or with the recovery key.
    VerificationOffered {
        other_session: bool,
    },
    /// Another device asks to verify, with a prompt naming it.
    VerificationRequested(VerificationRequest, String),
    /// A verification request or its emoji comparison was cancelled, by
    /// either side.
    VerificationCancelled(CancelInfo),
    /// Another of the user's devices answered a request, so its prompt
    /// closes.
    VerificationAnsweredElsewhere,
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
    pub room: Room,
    pub events: Vec<AnyTimelineEvent>,
    pub cursor: Option<String>,
}

pub fn handle_app_event(event: MatuiEvent, app: &mut App) {
    match event {
        MatuiEvent::Confirm(header, msg) => {
            app.show_notice(Popup::Error(Error::with_heading(header, msg)));
        }
        MatuiEvent::Error(msg) => {
            log::warn!("Error: {msg}");
            app.show_notice(Popup::Error(Error::new(msg)));
        }
        MatuiEvent::Invited(room, msg) => {
            // only prompt once per session, no matter how many times we hear
            // about the invite
            if app.invites_seen.insert(room.room_id().to_owned()) {
                app.show_notice(Popup::Confirm(Confirm::new(
                    "Invite".to_string(),
                    msg,
                    "Yes".to_string(),
                    "No".to_string(),
                    ConfirmBehavior::JoinRoom(room),
                )));
            }
        }
        MatuiEvent::LoggedOut => {
            app.running = false;
        }
        MatuiEvent::LoginRequired => {
            app.set_popup(Popup::Homeserver(Homeserver::default()));
        }
        MatuiEvent::PasswordLoginRequired(homeserver) => {
            app.set_popup(Popup::Signin(Signin::new(homeserver)));
        }
        MatuiEvent::OauthStarted(url, cancel) => {
            app.set_popup(Popup::Oauth(Oauth::new(url, cancel)));
        }
        MatuiEvent::LoginStarted => {
            app.show_notice(Popup::Progress(Progress::new("Logging in", 0)));
        }
        // closes the OAuth popup too
        MatuiEvent::LoginComplete => app.close_popup(),
        MatuiEvent::Picture(room_id, uri, image) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.picture_event(&room_id, uri.clone(), image.clone());
            }
        }
        MatuiEvent::ProgressStarted(msg, delay) => {
            app.show_notice(Popup::Progress(Progress::new(&msg, delay)))
        }
        MatuiEvent::ProgressComplete => app.complete_progress(),
        MatuiEvent::QrLoginAuth(url) => {
            app.set_popup(Popup::Qr(Qr::auth(url)));
        }
        MatuiEvent::QrLoginCode(code) => {
            app.set_popup(Popup::Qr(Qr::code(code)));
        }
        MatuiEvent::QrLoginDone => {
            app.set_popup(Popup::Error(Error::with_heading(
                "QR Login".to_string(),
                "Login granted.".to_string(),
            )));
        }
        MatuiEvent::QrLoginScanned(sender) => {
            app.set_popup(Popup::Qr(Qr::check(sender)));
        }
        MatuiEvent::QrLoginSyncing => {
            app.set_popup(Popup::Progress(Progress::new("Syncing secrets.", 0)));
        }
        MatuiEvent::Recover(key) => {
            app.show_notice(Popup::Progress(Progress::new(
                "Fetching encryption keys.",
                0,
            )));
            app.matrix.recover(&key);
        }

        // if we just left the room we're looking at, move to the top one
        MatuiEvent::RoomLeft(room) => {
            if let Some(t) = &app.thread
                && t.room().room_id() == room.room_id()
            {
                app.thread = None;
            }

            if let Some(c) = &app.chat
                && c.room().room_id() == room.room_id()
            {
                let mut rooms = app.matrix.fetch_rooms();
                sort_rooms(&mut rooms);

                match rooms.first() {
                    Some(next) => app.select_room(next.inner()),
                    None => app.chat = None,
                }
            }
        }

        // Let the chat update when we learn about room membership
        MatuiEvent::RoomMember(room, member) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.room_member_event(room.clone(), member.clone());
            }
        }
        MatuiEvent::RoomFound(chunk) => {
            let name = chunk.name.clone().unwrap_or_else(|| "Unnamed".to_string());
            let target: OwnedRoomOrAliasId = match &chunk.canonical_alias {
                Some(alias) => alias.clone().into(),
                None => chunk.room_id.clone().into(),
            };
            let message = format!(
                "Join {} ({})? {} members.",
                name, target, chunk.num_joined_members
            );

            app.show_notice(Popup::Confirm(Confirm::new(
                "Room Found".to_string(),
                message,
                "Join".to_string(),
                "Cancel".to_string(),
                ConfirmBehavior::JoinPublicRoom(target),
            )));
        }
        MatuiEvent::RoomSelected(room) => app.select_room(room),
        MatuiEvent::Search(search_term) => {
            // only the chat being looked at searches
            if let Some(c) = app.thread.as_mut().or(app.chat.as_mut()) {
                c.search_event(&search_term);
            }
        }
        MatuiEvent::SyncStarted(st) => {
            match st {
                SyncType::Initial => app.show_notice(Popup::Progress(Progress::new(
                    "Performing initial sync.",
                    0,
                ))),
                SyncType::Latest => app.show_notice(Popup::Progress(Progress::new("Syncing", 0))),
            };
        }
        MatuiEvent::SyncComplete => {
            app.complete_progress();

            // start in the room chooser, so no room is open (and read) until
            // someone picks one
            let rooms = Rooms::new(app.matrix.clone(), None);

            if app.popup.is_none() && !rooms.room.is_empty() {
                app.set_popup(Popup::Rooms(rooms));
            }
        }
        MatuiEvent::Timeline(event) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.timeline_event(event.clone());
            }

            // is it weird to send events all the way up here, then right
            // back down?
            app.matrix.timeline_event(event)
        }
        MatuiEvent::RoomKeys(keys) => {
            // the room chat has every event the thread view has, so it asks
            // for both
            if let Some(c) = &app.chat {
                c.room_keys_event(keys);
            }
        }
        MatuiEvent::Decrypted(events) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.decrypted_event(events.clone());
            }
        }
        MatuiEvent::TimelineBatch(batch) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.batch_event(batch.clone());
            }
        }
        MatuiEvent::Typing(room, ids) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.typing_event(room.clone(), ids.clone());
            }
        }
        MatuiEvent::Receipt(room, content) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.receipt_event(&room, &content);
            }

            app.receipts.push_back((room, content));

            if app.receipts.len() > 500 {
                app.receipts.pop_front();
            }
        }
        MatuiEvent::VerificationOffered { other_session } => {
            let confirm = if other_session {
                Confirm::new(
                    "Verify This Session".to_string(),
                    "Verify this session to read encrypted messages. To verify \
                     from another session, open it first, so it can answer."
                        .to_string(),
                    "Ask Other Session".to_string(),
                    "Recovery Key".to_string(),
                    ConfirmBehavior::OfferVerification,
                )
            } else {
                Confirm::new(
                    "Verify This Session".to_string(),
                    "Verify this session with your recovery key to read \
                     encrypted messages."
                        .to_string(),
                    "Recovery Key".to_string(),
                    "Not Now".to_string(),
                    ConfirmBehavior::OfferRecovery,
                )
            };

            app.show_notice(Popup::Confirm(confirm));
        }
        MatuiEvent::VerificationRequested(request, message) => {
            app.show_notice(Popup::Confirm(Confirm::new(
                "Verification Request".to_string(),
                message,
                "Accept".to_string(),
                "Decline".to_string(),
                ConfirmBehavior::VerificationRequest(request),
            )));
        }
        MatuiEvent::VerificationCancelled(info) => {
            app.sas = None;

            let heading = if info.cancelled_by_us() {
                "Verification Cancelled by This Session"
            } else {
                "Verification Cancelled by the Other Side"
            };
            let error = Popup::Error(Error::with_heading(
                heading.to_string(),
                format!("{} Try verifying again.", info.reason()),
            ));

            // the reason takes the place of the prompt it cancels
            if app.showing_verification() {
                app.set_popup(error);
            } else {
                app.show_notice(error);
            }
        }
        MatuiEvent::VerificationAnsweredElsewhere => {
            if app.showing_verification() {
                app.close_popup();
            }
        }
        MatuiEvent::VerificationStarted(sas, emoji) => {
            app.sas = Some(sas);

            app.show_notice(Popup::Confirm(Confirm::new(
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
            if app.showing_verification() {
                app.close_popup();
            }

            app.sas = None;
        }
    }
}

fn handle_chat_key_event(
    key_event: &KeyEvent,
    app: &mut App,
    handler: &EventHandler,
) -> anyhow::Result<()> {
    let Some(chat) = app.thread.as_mut().or(app.chat.as_mut()) else {
        return Ok(());
    };

    let result = match chat.key_event(key_event, handler) {
        Ok(result) => result,
        Err(err) => {
            log::warn!("Error: {err}");
            app.show_notice(Popup::Error(Error::new(err.to_string())));
            return Ok(());
        }
    };

    if let EventResult::Consumed(f) = result {
        f(app);
    }

    Ok(())
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

    // give the popup first crack at the event
    let result = if let Some(w) = &mut app.popup {
        w.key_event(&key_event, handler)
    } else {
        EventResult::Ignored
    };

    if let EventResult::Consumed(f) = result {
        f(app);
        return Ok(());
    }

    // A reaction dialog is part of Chat rather than Popup, but is still modal.
    if app
        .thread
        .as_ref()
        .or(app.chat.as_ref())
        .is_some_and(|chat| chat.reacting())
    {
        handle_chat_key_event(&key_event, app, handler)?;
        return Ok(());
    }

    // we own a few key events
    match key_event.code {
        KeyCode::Char(' ') => {
            let current = app.chat.as_ref().map(|c| c.room());

            app.set_popup(Popup::Rooms(Rooms::new(app.matrix.clone(), current)));

            return Ok(());
        }
        KeyCode::Char('?') => {
            app.set_popup(Popup::Help(Help::default()));
            return Ok(());
        }
        _ => {}
    }

    // and now pass it on to the chat being looked at
    handle_chat_key_event(&key_event, app, handler)
}

pub fn handle_paste_event(value: String, app: &mut App) {
    if let Some(popup) = app.popup.as_mut() {
        if let EventResult::Consumed(f) = popup.paste_event(&value) {
            f(app);
        }
        return;
    }

    if let Some(chat) = app.thread.as_mut().or(app.chat.as_mut())
        && let EventResult::Consumed(f) = chat.paste_event(&value)
    {
        f(app);
        return;
    }

    let paths = pasted_file_paths(&value);

    if paths.is_empty() {
        return;
    }

    let Some(room) = app
        .thread
        .as_ref()
        .or(app.chat.as_ref())
        .map(|chat| chat.room())
    else {
        return;
    };

    app.set_popup(Popup::Upload(Upload::new(app.matrix.clone(), room, paths)));
}

pub fn handle_focus_event(app: &mut App) {
    app.focus = true;
    app.matrix.focus_event();

    // we consider it a room "visit" if you come back to the app and view a
    // room
    if let Some(chat) = &mut app.chat {
        app.matrix.clone().room_visit_event(chat.room());
        chat.focus_event();
    }

    if let Some(thread) = &mut app.thread {
        thread.focus_event();
    }

    // the room chooser could have been open for a long time
    if let Some(Popup::Rooms(rooms)) = &mut app.popup {
        rooms.refresh(app.matrix.clone(), app.chat.as_ref().map(|c| c.room()));
    }
}

pub fn handle_blur_event(app: &mut App) {
    app.focus = false;
    app.matrix.blur_event();

    for chat in app.chat.iter_mut().chain(app.thread.iter_mut()) {
        chat.blur_event();
    }
}
