use crossterm::event::KeyEvent;
use log::warn;
use matrix_sdk::encryption::verification::SasVerification;
use matrix_sdk::room::{Joined, Room};
use once_cell::sync::OnceCell;
use ruma::events::receipt::ReceiptEventContent;
use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::event::Event;
use crate::matrix::matrix::Matrix;
use crate::widgets::chat::Chat;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::help::Help;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::Rooms;
use crate::widgets::signin::Signin;
use crate::widgets::EventResult;
use ratatui::backend::Backend;
use ratatui::terminal::Frame;

static SENDER: OnceCell<Mutex<Sender<Event>>> = OnceCell::new();

/// Application.
pub struct App {
    /// Is the application running?
    pub running: bool,

    /// How many ticks have passed?
    pub timestamp: usize,

    /// Hold on to all our widgets
    pub popup: Option<Popup>,
    pub chat: Option<Chat>,

    /// And our single Matrix client and channel
    pub matrix: Matrix,
    pub sender: Sender<Event>,

    /// We'll hold on to any in-progress verifications here
    pub sas: Option<SasVerification>,

    /// Keep old read receipts around
    pub receipts: VecDeque<(Joined, ReceiptEventContent)>,
}

impl App {
    pub fn new(send: Sender<Event>) -> Self {
        let matrix = Matrix::default();

        // Save the sender for future threads.
        SENDER
            .set(Mutex::new(send.clone()))
            .expect("could not set sender");

        Self {
            running: true,
            timestamp: 0,
            popup: None,
            chat: None,
            matrix,
            sender: send,
            sas: None,
            receipts: VecDeque::new(),
        }
    }

    pub fn get_sender() -> Sender<Event> {
        SENDER
            .get()
            .expect("could not get sender")
            .lock()
            .expect("could not lock sender")
            .clone()
    }

    pub fn select_room(&mut self, room: Joined) {
        // don't re-select the same room
        if let Some(c) = &self.chat {
            if c.room().room_id() == room.room_id() {
                return;
            }
        }

        let mut chat = Chat::try_new(self.matrix.clone(), room.clone());

        if chat.is_none() {
            warn!("could not switch to room");
            return;
        }

        // feed all the cached read receipts back in
        for (joined, content) in &self.receipts {
            chat.as_mut().unwrap().receipt_event(joined, content);
        }

        self.chat = chat;
        self.matrix.room_visit_event(Room::Joined(room));
    }

    pub fn set_popup(&mut self, popup: Popup) {
        self.popup = Some(popup);
    }

    pub fn close_popup(&mut self) {
        self.popup = None;
    }

    /// Handles the tick event of the terminal.
    pub fn tick(&mut self) {
        // if this is the very first tick, initialize and move on
        if self.timestamp == 0 {
            self.timestamp += 1;
            self.matrix.init();
            return;
        }

        // send out the ticks
        if let Some(w) = self.popup.as_mut() {
            w.tick_event(self.timestamp)
        }

        self.timestamp += 1;
    }

    /// Renders the user interface widgets.
    pub fn render<B: Backend>(&mut self, frame: &mut Frame<'_, B>) {
        if let Some(c) = &self.chat {
            frame.render_widget(c.widget(), frame.size());
        }

        if let Some(w) = &self.popup {
            w.render(frame);
        }
    }
}

// As far as I can tell, there's no way to use dynamic dispatch here, so
// instead we'll use a giant enum. I tried for way too long and just have
// to give up before I lose it. PRs welcome if there's a better way!
pub enum Popup {
    Confirm(Confirm),
    Error(Error),
    Progress(Progress),
    Rooms(Rooms),
    Signin(Signin),
    Help(Help)
}

impl Popup {
    pub fn key_event(&mut self, event: &KeyEvent) -> EventResult {
        match self {
            Popup::Confirm(w) => w.key_event(event),
            Popup::Error(w) => w.key_event(event),
            Popup::Progress(_) => EventResult::Ignored,
            Popup::Rooms(w) => w.key_event(event),
            Popup::Signin(w) => w.key_event(event),
            Popup::Help(w) => w.key_event(event)
        }
    }

    pub fn tick_event(&mut self, timestamp: usize) {
        if let Popup::Progress(w) = self {
            w.tick_event(timestamp);
        };
    }

    pub fn render<B: Backend>(&self, frame: &mut Frame<'_, B>) {
        match self {
            Popup::Confirm(w) => frame.render_widget(w.widget(), frame.size()),
            Popup::Error(w) => frame.render_widget(w.widget(), frame.size()),
            Popup::Progress(w) => frame.render_widget(w.widget(), frame.size()),
            Popup::Rooms(w) => frame.render_widget(w.widget(), frame.size()),
            Popup::Signin(w) => frame.render_widget(w.widget(), frame.size()),
            Popup::Help(w) => frame.render_widget(w.widget(), frame.size()),
        }
    }
}
