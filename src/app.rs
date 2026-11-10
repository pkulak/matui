use crossterm::event::KeyEvent;
use log::warn;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::OwnedRoomId;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use tokio::runtime::Handle;

use crate::event::{Event, EventHandler};
use crate::matrix::Matrix;
use crate::widgets::EventResult;
use crate::widgets::chat::Chat;
use crate::widgets::command::Command;
use crate::widgets::compose::Compose;
use crate::widgets::confirm::Confirm;
use crate::widgets::create::Create;
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::members::Members;
use crate::widgets::progress::Progress;
use crate::widgets::qr::Qr;
use crate::widgets::recover::Recover;
use crate::widgets::rooms::Rooms;
use crate::widgets::search::Search;
use crate::widgets::signin::Signin;
use crate::widgets::upload::Upload;
use ratatui::Frame;

/// Which notices a withdrawal closes: a progress popup, or the popups of
/// one verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NoticeId(u64);

impl NoticeId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NoticeId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// What's underneath the popups.
pub enum Session {
    /// Signed in, or restoring a saved session: the chat.
    SignedIn,
    /// Signing in, at the step the screen shows.
    SignedOut(Signin),
}

/// Application.
pub struct App {
    /// Is the application running?
    pub running: bool,

    /// How many ticks have passed?
    pub timestamp: usize,

    pub session: Session,

    /// Hold on to all our widgets. The chat is always the room; a thread
    /// renders on top of it while both keep receiving events.
    pub popup: Option<Popup>,
    /// Notices waiting for the popup slot, in arrival order. Something is
    /// always showing while any wait.
    pub notices: VecDeque<Popup>,
    pub chat: Option<Chat>,
    pub thread: Option<Chat>,

    /// And our Matrix client
    pub matrix: Matrix,

    /// Every room we've already asked about joining
    pub invites_seen: HashSet<OwnedRoomId>,
}

impl App {
    pub fn new(send: Sender<Event>, handle: Handle) -> Self {
        Self {
            running: true,
            timestamp: 0,
            session: Session::SignedIn,
            popup: None,
            notices: VecDeque::new(),
            chat: None,
            thread: None,
            matrix: Matrix::new(send, handle),
            invites_seen: HashSet::new(),
        }
    }

    pub fn select_room(&mut self, room: Room) {
        // don't re-select the same room
        if let Some(c) = &self.chat
            && c.room().room_id() == room.room_id()
        {
            return;
        }

        // a new room means any open thread is stale
        self.thread = None;

        let chat = Chat::try_new(self.matrix.clone(), room.clone());

        if chat.is_none() {
            warn!("could not switch to room");
            return;
        }

        self.chat = chat;
        self.matrix.subscribe_to_room(&room);
        self.matrix.room_opened_event(&room);
    }

    /// Show a popup the user opened, replacing whatever is showing.
    pub fn set_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
    }

    /// Show a popup matui raised. It replaces the room switcher or a
    /// progress popup, and otherwise waits its turn.
    pub fn show_notice(&mut self, popup: Popup) {
        match &self.popup {
            None | Some(Popup::Rooms(_) | Popup::Progress(_)) => self.popup = Some(popup),
            Some(_) => self.notices.push_back(popup),
        }
    }

    /// Close the popup showing, and show the next notice waiting. A progress
    /// popup gives way to anything behind it, since its work sends any error
    /// before its completion. With nothing waiting and no room open, there's
    /// nothing behind the popups, so go back to the room switcher.
    pub fn close_popup(&mut self) {
        self.popup = self.notices.pop_front();

        while matches!(self.popup, Some(Popup::Progress(_))) && !self.notices.is_empty() {
            self.popup = self.notices.pop_front();
        }

        if self.popup.is_none() && self.chat.is_none() {
            let rooms = Rooms::new(self.matrix.clone(), None);

            if !rooms.room.is_empty() {
                self.popup = Some(Popup::Rooms(rooms));
            }
        }
    }

    /// Close every popup with this ID, whether it's showing or waiting.
    pub fn withdraw(&mut self, id: NoticeId) {
        let is_it = |p: &Popup| p.id() == Some(id);

        self.notices.retain(|p| !is_it(p));

        if self.popup.as_ref().is_some_and(is_it) {
            self.close_popup();
        }
    }

    /// Handles the tick event of the terminal. Returns true if this tick
    /// should trigger a render.
    pub fn tick(&mut self) -> bool {
        // if this is the very first tick, initialize and move on
        if self.timestamp == 0 {
            self.timestamp += 1;
            self.matrix.init();
            return true;
        }

        let mut render = false;

        // send out the ticks
        if let Session::SignedOut(signin) = &mut self.session {
            signin.tick_event(self.timestamp);
            render = true;
        }

        if let Some(w) = self.popup.as_mut() {
            w.tick_event(self.timestamp);
            render = true;
        }

        if self.timestamp.is_multiple_of(240) {
            render = true;
        }

        self.timestamp += 1;
        render
    }

    /// Renders the user interface widgets.
    pub fn render(&mut self, frame: &mut Frame) {
        if let Session::SignedOut(signin) = &self.session {
            frame.render_widget(signin.widget(), frame.area());
        } else if let Some(t) = &self.thread {
            frame.render_widget(t.widget(), frame.area());
        } else if let Some(c) = &self.chat {
            frame.render_widget(c.widget(), frame.area());
        }

        if let Some(w) = &self.popup {
            w.render(frame);
        }
    }
}

// As far as I can tell, there's no way to use dynamic dispatch here, so
// instead we'll use a giant enum. I tried for way too long and just have
// to give up before I lose it. PRs welcome if there's a better way!
#[allow(clippy::large_enum_variant)]
pub enum Popup {
    Command(Command),
    Confirm(Confirm),
    Compose(Compose),
    Create(Create),
    Error(Error),
    Members(Members),
    Progress(Progress),
    Qr(Qr),
    Recover(Recover),
    Rooms(Rooms),
    Search(Search),
    Upload(Upload),
    Help(Help),
}

impl Popup {
    /// The ID that withdraws this popup, if anything can.
    fn id(&self) -> Option<NoticeId> {
        match self {
            Popup::Progress(w) => Some(w.id),
            Popup::Confirm(w) => w.id(),
            _ => None,
        }
    }

    pub fn key_event(&mut self, event: &KeyEvent, handler: &EventHandler) -> EventResult {
        match self {
            Popup::Command(w) => w.key_event(event),
            Popup::Confirm(w) => w.key_event(event),
            Popup::Compose(w) => w.key_event(event, handler),
            Popup::Create(w) => w.key_event(event),
            Popup::Error(w) => w.key_event(event),
            Popup::Members(w) => w.key_event(event),
            Popup::Progress(_) => EventResult::Ignored,
            Popup::Qr(w) => w.key_event(event, handler),
            Popup::Recover(w) => w.key_event(event),
            Popup::Rooms(w) => w.key_event(event),
            Popup::Search(w) => w.key_event(event),
            Popup::Help(w) => w.key_event(event),
            Popup::Upload(w) => w.key_event(event),
        }
    }

    pub fn paste_event(&mut self, value: &str) -> EventResult {
        match self {
            Popup::Command(w) => w.paste_event(value),
            Popup::Compose(w) => w.paste_event(value),
            Popup::Create(w) => w.paste_event(value),
            Popup::Members(w) => w.paste_event(value),
            Popup::Qr(w) => w.paste_event(value),
            Popup::Recover(w) => w.paste_event(value),
            Popup::Rooms(w) => w.paste_event(value),
            Popup::Search(w) => w.paste_event(value),
            Popup::Upload(w) => w.paste_event(value),
            Popup::Confirm(_) | Popup::Error(_) | Popup::Progress(_) | Popup::Help(_) => {
                EventResult::Ignored
            }
        }
    }

    pub fn tick_event(&mut self, timestamp: usize) -> bool {
        if let Popup::Progress(w) = self {
            w.tick_event(timestamp);
            return true;
        };

        false
    }

    pub fn render(&self, frame: &mut Frame) {
        match self {
            Popup::Command(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Confirm(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Compose(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Create(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Error(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Members(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Progress(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Qr(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Recover(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Rooms(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Search(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Help(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Upload(w) => frame.render_widget(w.widget(), frame.area()),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::mpsc::{Receiver, channel};

    use crossterm::event::{KeyCode, KeyModifiers};
    use matrix_sdk::Client;
    use matrix_sdk::ruma::exports::serde_json::json;
    use matrix_sdk::ruma::room_id;
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk_test::{InvitedRoomBuilder, JoinedRoomBuilder};

    use super::*;
    use crate::handler::{MatuiEvent, handle_app_event, handle_blur_event, handle_focus_event};

    /// An app with nothing showing and no client, sending to the returned
    /// receiver.
    pub(crate) fn test_app() -> (App, Receiver<Event>) {
        let (sender, receiver) = channel();
        (App::new(sender, Handle::current()), receiver)
    }

    /// An app signed in to a mock server, with one joined room in its room
    /// list.
    async fn app_with_room() -> (MatrixMockServer, Client, App, Room, Receiver<Event>) {
        let (app, events) = test_app();
        let server = MatrixMockServer::new().await;
        let client = server.client_builder().build().await;
        let room = server
            .sync_joined_room(&client, room_id!("!room:localhost"))
            .await;

        app.matrix.set_client(client.clone());
        app.matrix.add_room(room.clone()).await;

        (server, client, app, room, events)
    }

    #[tokio::test]
    async fn the_switcher_gives_way_to_a_notice_and_comes_back() {
        let (_server, _client, mut app, _room, _events) = app_with_room().await;
        handle_app_event(MatuiEvent::SyncComplete, &mut app);
        assert!(matches!(app.popup, Some(Popup::Rooms(_))));

        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);
        offer_verification(&mut app);
        assert!(matches!(app.popup, Some(Popup::Error(_))));

        // not while anything is queued
        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Rooms(_))));
    }

    #[tokio::test]
    async fn the_room_being_looked_at_counts_as_read() {
        let (mut app, _events) = test_app();
        let server = MatrixMockServer::new().await;
        let client = server.client_builder().build().await;
        let room = server
            .sync_room(
                &client,
                JoinedRoomBuilder::new(room_id!("!room:localhost"))
                    .set_unread_notifications_count(json!({ "notification_count": 2 })),
            )
            .await;

        app.matrix.set_client(client.clone());
        app.matrix.add_room(room.clone()).await;

        // opened from a notification, out of focus
        handle_blur_event(&mut app);
        app.select_room(room);
        assert_eq!(app.matrix.fetch_rooms()[0].unread_count(), 2);

        handle_focus_event(&mut app);
        assert_eq!(app.matrix.fetch_rooms()[0].unread_count(), 0);
    }

    #[tokio::test]
    async fn closing_every_popup_leaves_an_open_room_showing() {
        let (_server, _client, mut app, room, _events) = app_with_room().await;
        app.select_room(room);
        assert!(app.chat.is_some());

        app.set_popup(Popup::Help(Help::default()));
        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        app.close_popup();
        app.close_popup();
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn each_invite_is_asked_about_once() {
        let (server, client, mut app, _room, _events) = app_with_room().await;
        let invited = server
            .sync_room(
                &client,
                InvitedRoomBuilder::new(room_id!("!invite:localhost")),
            )
            .await;
        app.set_popup(Popup::Help(Help::default()));

        for _ in 0..2 {
            handle_app_event(
                MatuiEvent::Invited(invited.clone(), "Join?".to_string()),
                &mut app,
            );
        }

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Rooms(_))));
    }

    #[tokio::test]
    async fn an_error_shows_its_popup() {
        let (mut app, _events) = test_app();

        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        assert!(matches!(app.popup, Some(Popup::Error(_))));
    }

    fn offer_verification(app: &mut App) {
        handle_app_event(
            MatuiEvent::VerificationOffered {
                other_session: true,
            },
            app,
        );
    }

    #[tokio::test]
    async fn a_notice_waits_behind_an_input_popup() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);
        assert!(matches!(app.popup, Some(Popup::Search(_))));

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Error(_))));
    }

    #[tokio::test]
    async fn a_qr_login_failure_replaces_the_popup_waiting_for_it() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Qr(Qr::code("code".to_string())));

        handle_app_event(MatuiEvent::QrLoginFailed("denied".to_string()), &mut app);

        assert!(matches!(app.popup, Some(Popup::Error(_))));
        assert!(app.notices.is_empty());
    }

    #[tokio::test]
    async fn queued_notices_show_in_arrival_order() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);
        offer_verification(&mut app);

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Error(_))));

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        app.close_popup();
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn a_popup_the_user_opens_replaces_a_notice() {
        let (mut app, _events) = test_app();
        offer_verification(&mut app);

        app.set_popup(Popup::Help(Help::default()));

        assert!(matches!(app.popup, Some(Popup::Help(_))));
    }

    #[tokio::test]
    async fn sending_a_message_leaves_the_prompt_behind_it_showing() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));
        offer_verification(&mut app);

        // the compose popup closes as it sends, then the send shows progress
        app.close_popup();
        let sending = start_progress(&mut app);
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        handle_app_event(MatuiEvent::Withdraw(sending), &mut app);
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        app.close_popup();
        assert!(app.popup.is_none());
    }

    fn start_progress(app: &mut App) -> NoticeId {
        let id = NoticeId::next();
        handle_app_event(
            MatuiEvent::ProgressStarted(id, "Working.".to_string(), 0),
            app,
        );
        id
    }

    fn showing_progress(app: &App) -> Option<NoticeId> {
        match &app.popup {
            Some(Popup::Progress(p)) => Some(p.id),
            _ => None,
        }
    }

    #[tokio::test]
    async fn completing_progress_closes_only_its_own_popup() {
        let (mut app, _events) = test_app();

        // one progress popup waits behind another
        let recovering = start_progress(&mut app);
        let downloading = start_progress(&mut app);

        handle_app_event(MatuiEvent::Withdraw(recovering), &mut app);
        assert_eq!(showing_progress(&app), Some(downloading));

        handle_app_event(MatuiEvent::Withdraw(downloading), &mut app);
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn recovering_closes_the_key_popup_and_shows_progress() {
        let (_server, _client, mut app, _room, events) = app_with_room().await;
        app.set_popup(Popup::Recover(Recover::default()));

        let Some(Popup::Recover(recover)) = app.popup.as_mut() else {
            unreachable!()
        };
        let EventResult::Consumed(f) = recover.key_event(&KeyEvent::from(KeyCode::Enter)) else {
            panic!("recover ignored Enter");
        };
        f(&mut app);

        let next = async {
            loop {
                tokio::task::yield_now().await;

                if let Ok(Event::Matui(event)) = events.try_recv() {
                    return event;
                }
            }
        };
        let started = tokio::time::timeout(std::time::Duration::from_secs(5), next)
            .await
            .expect("an event");
        assert!(matches!(started, MatuiEvent::ProgressStarted(..)));

        handle_app_event(started, &mut app);
        assert!(showing_progress(&app).is_some());
    }

    #[tokio::test]
    async fn completing_queued_progress_unqueues_only_it() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        let first = start_progress(&mut app);
        let second = start_progress(&mut app);

        handle_app_event(MatuiEvent::Withdraw(first), &mut app);
        assert!(matches!(app.popup, Some(Popup::Search(_))));

        app.close_popup();
        assert_eq!(showing_progress(&app), Some(second));
    }

    #[tokio::test]
    async fn completing_replaced_progress_leaves_the_notice_alone() {
        let (mut app, _events) = test_app();
        let working = start_progress(&mut app);
        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        handle_app_event(MatuiEvent::Withdraw(working), &mut app);

        assert!(matches!(app.popup, Some(Popup::Error(_))));
    }

    #[tokio::test]
    async fn a_notice_replaces_progress() {
        let (mut app, _events) = test_app();
        start_progress(&mut app);

        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        assert!(matches!(app.popup, Some(Popup::Error(_))));
        assert!(app.notices.is_empty());
    }

    #[tokio::test]
    async fn queued_progress_gives_way_to_a_notice_behind_it() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        // work that failed sends its error before its completion
        start_progress(&mut app);
        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Error(_))));
        assert!(app.notices.is_empty());
    }

    #[tokio::test]
    async fn search_sends_on_the_apps_channel() {
        let (mut app, events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        let Some(Popup::Search(search)) = app.popup.as_mut() else {
            unreachable!()
        };

        let key = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        let EventResult::Consumed(f) = search.key_event(&key) else {
            panic!("search ignored a key");
        };
        f(&mut app);

        assert!(matches!(
            events.try_recv(),
            Ok(Event::Matui(MatuiEvent::Search(term))) if term == "a"
        ));
    }
}
