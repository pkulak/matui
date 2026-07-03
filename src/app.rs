use crossterm::event::KeyEvent;
use log::warn;
use matrix_sdk::encryption::verification::SasVerification;
use matrix_sdk::room::Room;
use once_cell::sync::OnceCell;
use matrix_sdk::ruma::events::receipt::ReceiptEventContent;
use matrix_sdk::ruma::OwnedRoomId;
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::sync::mpsc::Sender;
use tokio::runtime::Handle;

use crate::event::{Event, EventHandler};
use crate::handler::MatuiEvent;
use crate::matrix::matrix::Matrix;
use crate::widgets::EventResult;
use crate::widgets::chat::Chat;
use crate::widgets::command::Command;
use crate::widgets::compose::Compose;
use crate::widgets::confirm::{Confirm, ConfirmBehavior};
use crate::widgets::create::Create;
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::progress::Progress;
use crate::widgets::recover::Recover;
use crate::widgets::rooms::Rooms;
use crate::widgets::search::Search;
use crate::widgets::signin::Signin;
use ratatui::Frame;

static SENDER: OnceCell<Sender<Event>> = OnceCell::new();
static HANDLE: OnceCell<Handle> = OnceCell::new();

/// Application.
pub struct App {
    /// Is the application running?
    pub running: bool,

    /// How many ticks have passed?
    pub timestamp: usize,

    /// Hold on to all our widgets. The chat is always the room; a thread
    /// renders on top of it while both keep receiving events.
    pub popup: Option<Popup>,
    pub chat: Option<Chat>,
    pub thread: Option<Chat>,

    /// And our Matrix client
    pub matrix: Matrix,

    /// We'll hold on to any in-progress verifications here
    pub sas: Option<SasVerification>,

    /// Keep old read receipts around
    pub receipts: VecDeque<(Room, ReceiptEventContent)>,

    /// Invites waiting to be shown, and every room we've already asked about
    pub invites: VecDeque<(Room, String)>,
    pub invites_seen: HashSet<OwnedRoomId>,
}

impl App {
    pub fn new(send: Sender<Event>, handle: Handle) -> Self {
        // Save the sender and handle for future threads.
        SENDER.set(send).expect("could not set sender");
        HANDLE.set(handle).expect("could not set handle");

        let matrix = Matrix::default();

        Self {
            running: true,
            timestamp: 0,
            popup: None,
            chat: None,
            thread: None,
            matrix,
            sas: None,
            receipts: VecDeque::new(),
            invites: VecDeque::new(),
            invites_seen: HashSet::new(),
        }
    }

    pub fn get_sender() -> Sender<Event> {
        SENDER.get().expect("could not lock sender").clone()
    }

    pub fn get_handle() -> Handle {
        HANDLE.get().expect("could not get handle").clone()
    }

    pub fn send(event: MatuiEvent) {
        App::get_sender()
            .send(Event::Matui(event))
            .expect("could not send event");
    }

    pub fn spawn<F>(future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        App::get_handle().spawn(future);
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

        let mut chat = Chat::try_new(self.matrix.clone(), room.clone());

        if chat.is_none() {
            warn!("could not switch to room");
            return;
        }

        // feed all the cached read receipts back in
        for (room, content) in &self.receipts {
            chat.as_mut().unwrap().receipt_event(room, content);
        }

        self.chat = chat;
        self.matrix.room_visit_event(room);
    }

    pub fn set_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
    }

    pub fn close_popup(&mut self) {
        self.popup = None;
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

        // show the next pending invite whenever the popup slot is free; it
        // stays at the front of the queue until answered, so a stomped popup
        // comes right back
        if self.popup.is_none()
            && let Some((room, msg)) = self.invites.front().cloned()
        {
            self.set_popup(Popup::Confirm(Confirm::new(
                "Invite".to_string(),
                msg,
                "Yes".to_string(),
                "No".to_string(),
                ConfirmBehavior::JoinRoom(room),
            )));
            render = true;
        }

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
    Progress(Progress),
    Recover(Recover),
    Rooms(Rooms),
    Signin(Signin),
    Search(Search),
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
            Popup::Progress(_) => EventResult::Ignored,
            Popup::Recover(w) => w.key_event(event),
            Popup::Rooms(w) => w.key_event(event),
            Popup::Signin(w) => w.key_event(event),
            Popup::Search(w) => w.key_event(event),
            Popup::Help(w) => w.key_event(event),
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
            Popup::Progress(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Recover(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Rooms(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Signin(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Search(w) => frame.render_widget(w.widget(), frame.area()),
            Popup::Help(w) => frame.render_widget(w.widget(), frame.area()),
        }
    }
}
