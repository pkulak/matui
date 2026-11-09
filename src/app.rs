use crossterm::event::KeyEvent;
use log::warn;
use matrix_sdk::encryption::verification::SasVerification;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::OwnedRoomId;
use matrix_sdk::ruma::events::receipt::ReceiptEventContent;
use std::collections::{HashSet, VecDeque};
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
use crate::widgets::homeserver::Homeserver;
use crate::widgets::members::Members;
use crate::widgets::oauth::Oauth;
use crate::widgets::progress::{Progress, ProgressId};
use crate::widgets::qr::Qr;
use crate::widgets::recover::Recover;
use crate::widgets::rooms::Rooms;
use crate::widgets::search::Search;
use crate::widgets::signin::Signin;
use crate::widgets::upload::Upload;
use ratatui::Frame;

/// Application.
pub struct App {
    /// Is the application running?
    pub running: bool,

    /// How many ticks have passed?
    pub timestamp: usize,

    /// Does the terminal have focus? Terminals that don't report focus
    /// never change this, so it starts out true.
    pub focus: bool,

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

    /// We'll hold on to any in-progress verifications here
    pub sas: Option<SasVerification>,

    /// Keep old read receipts around
    pub receipts: VecDeque<(Room, ReceiptEventContent)>,

    /// Every room we've already asked about joining
    pub invites_seen: HashSet<OwnedRoomId>,
}

impl App {
    pub fn new(send: Sender<Event>, handle: Handle) -> Self {
        Self {
            running: true,
            timestamp: 0,
            focus: true,
            popup: None,
            notices: VecDeque::new(),
            chat: None,
            thread: None,
            matrix: Matrix::new(send, handle),
            sas: None,
            receipts: VecDeque::new(),
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

        let mut chat = Chat::try_new(self.matrix.clone(), room.clone(), self.focus);

        if chat.is_none() {
            warn!("could not switch to room");
            return;
        }

        // feed all the cached read receipts back in
        for (room, content) in &self.receipts {
            chat.as_mut().unwrap().receipt_event(room, content);
        }

        self.chat = chat;
        self.matrix.subscribe_to_room(&room);
        self.matrix.room_visit_event(room);
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

    /// Some progress finished: close its popup, whether it's showing or
    /// waiting.
    pub fn complete_progress(&mut self, id: ProgressId) {
        let is_it = |p: &Popup| matches!(p, Popup::Progress(w) if w.id == id);

        if self.popup.as_ref().is_some_and(is_it) {
            self.close_popup();
        } else {
            self.notices.retain(|p| !is_it(p));
        }
    }

    /// Whether the popup showing is a verification prompt or emoji
    /// comparison.
    pub fn showing_verification(&self) -> bool {
        matches!(&self.popup, Some(Popup::Confirm(c)) if c.is_verification())
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
        if let Some(t) = &self.thread {
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
    Homeserver(Homeserver),
    Members(Members),
    Progress(Progress),
    Qr(Qr),
    Recover(Recover),
    Rooms(Rooms),
    Oauth(Oauth),
    Signin(Signin),
    Search(Search),
    Upload(Upload),
    Help(Help),
}

impl Popup {
    pub fn key_event(&mut self, event: &KeyEvent, handler: &EventHandler) -> EventResult {
        match self {
            Popup::Command(w) => w.key_event(event),
            Popup::Confirm(w) => w.key_event(event),
            Popup::Compose(w) => w.key_event(event, handler),
            Popup::Create(w) => w.key_event(event),
            Popup::Error(w) => w.key_event(event),
            Popup::Homeserver(w) => w.key_event(event),
            Popup::Members(w) => w.key_event(event),
            Popup::Progress(_) => EventResult::Ignored,
            Popup::Qr(w) => w.key_event(event, handler),
            Popup::Recover(w) => w.key_event(event),
            Popup::Rooms(w) => w.key_event(event),
            Popup::Oauth(w) => w.key_event(event, handler),
            Popup::Signin(w) => w.key_event(event),
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
            Popup::Homeserver(w) => w.paste_event(value),
            Popup::Members(w) => w.paste_event(value),
            Popup::Qr(w) => w.paste_event(value),
            Popup::Recover(w) => w.paste_event(value),
            Popup::Rooms(w) => w.paste_event(value),
            Popup::Signin(w) => w.paste_event(value),
            Popup::Search(w) => w.paste_event(value),
            Popup::Upload(w) => w.paste_event(value),
            Popup::Confirm(_)
            | Popup::Error(_)
            | Popup::Progress(_)
            | Popup::Oauth(_)
            | Popup::Help(_) => EventResult::Ignored,
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
            Popup::Homeserver(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Members(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Progress(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Qr(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Recover(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Rooms(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Oauth(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Signin(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Search(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Help(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Upload(w) => frame.render_widget(w.widget(), frame.area()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};

    use crossterm::event::{KeyCode, KeyModifiers};
    use matrix_sdk::Client;
    use matrix_sdk::ruma::room_id;
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk_test::InvitedRoomBuilder;

    use super::*;
    use crate::handler::{MatuiEvent, handle_app_event};

    fn test_app() -> (App, Receiver<Event>) {
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

        handle_app_event(MatuiEvent::ProgressComplete(sending), &mut app);
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        app.close_popup();
        assert!(app.popup.is_none());
    }

    fn start_progress(app: &mut App) -> ProgressId {
        let id = ProgressId::next();
        handle_app_event(
            MatuiEvent::ProgressStarted(id, "Working.".to_string(), 0),
            app,
        );
        id
    }

    fn showing_progress(app: &App) -> Option<ProgressId> {
        match &app.popup {
            Some(Popup::Progress(p)) => Some(p.id),
            _ => None,
        }
    }

    #[tokio::test]
    async fn completing_progress_closes_only_its_own_popup() {
        let (mut app, _events) = test_app();

        // recovering shows its progress, then the backup download's
        let recovering = start_progress(&mut app);
        let downloading = start_progress(&mut app);

        handle_app_event(MatuiEvent::ProgressComplete(recovering), &mut app);
        assert_eq!(showing_progress(&app), Some(downloading));

        handle_app_event(MatuiEvent::ProgressComplete(downloading), &mut app);
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn completing_queued_progress_unqueues_only_it() {
        let (mut app, _events) = test_app();
        app.set_popup(Popup::Search(Search::default()));

        let first = start_progress(&mut app);
        let second = start_progress(&mut app);

        handle_app_event(MatuiEvent::ProgressComplete(first), &mut app);
        assert!(matches!(app.popup, Some(Popup::Search(_))));

        app.close_popup();
        assert_eq!(showing_progress(&app), Some(second));
    }

    #[tokio::test]
    async fn completing_replaced_progress_leaves_the_notice_alone() {
        let (mut app, _events) = test_app();
        let working = start_progress(&mut app);
        handle_app_event(MatuiEvent::Error("oops".to_string()), &mut app);

        handle_app_event(MatuiEvent::ProgressComplete(working), &mut app);

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
