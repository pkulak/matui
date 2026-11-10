use crate::app::{App, NoticeId, Popup, Session};
use crate::matrix::emoji::format_emojis;
use crate::spawn::pasted_file_paths;
use crate::widgets::EventResult;
use crate::widgets::confirm::{Confirm, ConfirmBehavior};
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::progress::Progress;
use crate::widgets::qr::Qr;
use crate::widgets::rooms::{Rooms, sort_rooms};
use crate::widgets::signin::Signin;
use crate::widgets::upload::Upload;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use image::DynamicImage;
use matrix_sdk::authentication::oauth::qrcode::CheckCodeSender;
use matrix_sdk::ruma::directory::PublicRoomsChunk;
use matrix_sdk::ruma::{OwnedMxcUri, OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId};
use std::sync::Arc;
use tokio::sync::Notify;

use crate::event::EventHandler;
use matrix_sdk::encryption::verification::{
    CancelInfo, Emoji, SasVerification, VerificationRequest,
};
use matrix_sdk::room::{Room, RoomMember};
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk_ui::Timeline;
use matrix_sdk_ui::eyeball_im::Vector;
use matrix_sdk_ui::timeline::TimelineItem;

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum MatuiEvent {
    Confirm(String, String),
    Error(String),
    Invited(Room, String),
    LoggedOut,
    /// Signing in reached this step.
    Login(Login),
    LoginComplete,
    /// A picture for a room's chat, or `None` if it couldn't be loaded.
    Picture(OwnedRoomId, OwnedMxcUri, Option<DynamicImage>),
    ProgressStarted(NoticeId, String, u64),
    Withdraw(NoticeId),
    // The QrLogin events are for granting another device a login, with :qr.
    QrLoginAuth(String),
    /// A QR code for the other device to scan.
    QrLoginCode(String),
    QrLoginDone,
    /// Its popup only waits for the flow, so the error replaces it rather
    /// than waiting behind it.
    QrLoginFailed(String),
    /// The other device scanned the code. The ID is the flow's, which
    /// withdraws its progress popups when it ends.
    QrLoginScanned(CheckCodeSender, NoticeId),
    QrLoginSyncing(NoticeId),
    RoomFound(PublicRoomsChunk),
    /// A timeline may have earlier events to page back to again.
    NotAtStart(Arc<Timeline>),
    /// A timeline paged back: whether it reached the start, or why it
    /// couldn't.
    Paginated(Arc<Timeline>, Result<bool, String>),
    RoomLeft(Room),
    RoomMember(Room, RoomMember),
    RoomSelected(Room),
    Search(String),
    SyncComplete,
    /// An event from sync, for notifications.
    Timeline(AnyTimelineEvent),
    /// All the items of a room's timeline, after a change.
    TimelineItems(Arc<Timeline>, Vector<Arc<TimelineItem>>),
    Typing(Room, Vec<OwnedUserId>),
    /// Offer to verify this session after login, by asking another session
    /// when there's a verified one to ask, or with the recovery key.
    VerificationOffered {
        other_session: bool,
    },
    /// Another device asks to verify, with a prompt naming it. The
    /// verification's owner withdraws the prompt by its ID when it ends.
    VerificationRequested(NoticeId, VerificationRequest, String),
    /// A verification request or its emoji comparison was cancelled, by
    /// either side.
    VerificationCancelled(CancelInfo),
    /// The emoji to compare, withdrawn by the verification's ID.
    VerificationStarted(NoticeId, SasVerification, [Emoji; 7]),
}

/// The steps of signing this device in. A Notify cancels the step it's in.
#[derive(Clone, Debug)]
pub enum Login {
    Homeserver,
    Password {
        homeserver: String,
    },
    /// Waiting on the browser at this URL.
    Browser {
        url: String,
        cancel: Arc<Notify>,
    },
    /// A QR code for a signed-in device to scan. Cancelling falls back to
    /// the browser.
    QrCode {
        code: String,
        cancel: Arc<Notify>,
    },
    /// The other device scanned the code, and shows a check code to enter.
    QrCheck {
        sender: CheckCodeSender,
        cancel: Arc<Notify>,
    },
    /// Waiting for the other device to approve, by entering this code.
    QrApprove {
        code: String,
    },
    SyncingSecrets,
}

impl Login {
    /// The step's name, for the log.
    fn name(&self) -> &'static str {
        match self {
            Login::Homeserver => "Homeserver",
            Login::Password { .. } => "Password",
            Login::Browser { .. } => "Browser",
            Login::QrCode { .. } => "QrCode",
            Login::QrCheck { .. } => "QrCheck",
            Login::QrApprove { .. } => "QrApprove",
            Login::SyncingSecrets => "SyncingSecrets",
        }
    }
}

#[derive(Clone, Debug)]
pub enum SyncType {
    Initial,
    Latest,
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
        MatuiEvent::Login(step) => {
            log::info!("sign-in: {}", step.name());
            app.session = Session::SignedOut(Signin::from(step));
        }
        MatuiEvent::LoginComplete => {
            log::info!("sign-in: done");
            app.session = Session::SignedIn;
        }
        MatuiEvent::Picture(room_id, uri, image) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.picture_event(&room_id, uri.clone(), image.clone());
            }
        }
        MatuiEvent::ProgressStarted(id, msg, delay) => {
            app.show_notice(Popup::Progress(Progress::new(id, &msg, delay)))
        }
        MatuiEvent::Withdraw(id) => app.withdraw(id),
        // each step of :qr replaces the one before, assuming it's showing.
        // A notice that showed instead, such as after Esc, is lost.
        MatuiEvent::QrLoginFailed(msg) => {
            log::warn!("QR login failed: {msg}");
            app.replace_popup(Popup::Error(Error::new(msg)));
        }
        MatuiEvent::QrLoginAuth(url) => {
            app.replace_popup(Popup::Qr(Qr::auth(url)));
        }
        MatuiEvent::QrLoginCode(code) => {
            app.replace_popup(Popup::Qr(Qr::code(code)));
        }
        MatuiEvent::QrLoginDone => {
            app.replace_popup(Popup::Error(Error::with_heading(
                "QR Login".to_string(),
                "Login granted.".to_string(),
            )));
        }
        MatuiEvent::QrLoginScanned(sender, id) => {
            app.replace_popup(Popup::Qr(Qr::check(sender, id)));
        }
        MatuiEvent::QrLoginSyncing(id) => {
            app.replace_popup(Popup::Progress(Progress::new(id, "Syncing secrets.", 0)));
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
        MatuiEvent::SyncComplete => {
            // start in the room chooser, so no room is open (and read) until
            // someone picks one
            let rooms = Rooms::new(app.matrix.clone(), None);

            if app.popup.is_none() && !rooms.room.is_empty() {
                app.open_popup(Popup::Rooms(rooms));
            }
        }
        MatuiEvent::Timeline(event) => app.matrix.timeline_event(event),
        MatuiEvent::TimelineItems(timeline, items) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.items_event(&timeline, &items);
            }
        }
        MatuiEvent::Paginated(timeline, result) => {
            if let Err(msg) = &result {
                app.show_notice(Popup::Error(Error::new(msg.clone())));
            }

            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.paginated_event(&timeline, &result);
            }
        }
        MatuiEvent::NotAtStart(timeline) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.not_at_start_event(&timeline);
            }
        }
        MatuiEvent::Typing(room, ids) => {
            for c in app.chat.iter_mut().chain(app.thread.iter_mut()) {
                c.typing_event(room.clone(), ids.clone());
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
        MatuiEvent::VerificationRequested(id, request, message) => {
            app.show_notice(Popup::Confirm(Confirm::new(
                "Verification Request".to_string(),
                message,
                "Accept".to_string(),
                "Decline".to_string(),
                ConfirmBehavior::VerificationRequest(id, request),
            )));
        }
        // the owner withdraws the popup it cancels right after, so the
        // reason takes its place unless something else is waiting
        MatuiEvent::VerificationCancelled(info) => {
            let heading = if info.cancelled_by_us() {
                "Verification Cancelled by This Session"
            } else {
                "Verification Cancelled by the Other Side"
            };
            app.show_notice(Popup::Error(Error::with_heading(
                heading.to_string(),
                format!("{} Try verifying again.", info.reason()),
            )));
        }
        MatuiEvent::VerificationStarted(id, sas, emoji) => {
            app.show_notice(Popup::Confirm(Confirm::new(
                "Verify".to_string(),
                format!(
                    "Do these emojis match your other session?\n\n{}",
                    format_emojis(emoji)
                ),
                "Yes".to_string(),
                "No".to_string(),
                ConfirmBehavior::Verification(id, sas),
            )));
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

    // while signing in, the sign-in screen is all there is behind a popup,
    // and it doesn't take keys a popup ignores
    if let Session::SignedOut(signin) = &mut app.session {
        if app.popup.is_none()
            && let EventResult::Consumed(f) = signin.key_event(&key_event, handler)
        {
            f(app);
        }

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

            app.open_popup(Popup::Rooms(Rooms::new(app.matrix.clone(), current)));

            return Ok(());
        }
        KeyCode::Char('?') => {
            app.open_popup(Popup::Help(Help::default()));
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

    if let Session::SignedOut(signin) = &mut app.session {
        if let EventResult::Consumed(f) = signin.paste_event(&value) {
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

    let Some(timeline) = app
        .thread
        .as_ref()
        .or(app.chat.as_ref())
        .and_then(|chat| chat.timeline())
    else {
        return;
    };

    app.open_popup(Popup::Upload(Upload::new(
        app.matrix.clone(),
        timeline,
        paths,
    )));
}

pub fn handle_focus_event(app: &mut App) {
    app.matrix.focus_event();

    if let Some(chat) = &mut app.chat {
        chat.focus_event();
    }

    // the room chooser could have been open for a long time
    if let Some(Popup::Rooms(rooms)) = &mut app.popup {
        rooms.refresh(app.matrix.clone(), app.chat.as_ref().map(|c| c.room()));
    }
}

pub fn handle_blur_event(app: &mut App) {
    app.matrix.blur_event();
}
