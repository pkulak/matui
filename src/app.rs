use matrix_sdk::encryption::verification::SasVerification;
use matrix_sdk::room::Joined;
use once_cell::sync::OnceCell;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::event::{Event, EventHandler};
use crate::matrix::matrix::Matrix;
use crate::widgets::chat::Chat;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::Rooms;
use crate::widgets::signin::Signin;
use tui::backend::Backend;
use tui::terminal::Frame;

static SENDER: OnceCell<Mutex<Sender<Event>>> = OnceCell::new();

/// Application.
pub struct App {
    pub events: EventHandler,

    /// Is the application running?
    pub running: bool,

    /// How many ticks have passed?
    pub timestamp: usize,

    /// Hold on to all our widgets
    pub progress: Option<Progress>,
    pub error: Option<Error>,
    pub signin: Option<Signin>,
    pub confirm: Option<Confirm>,
    pub rooms: Option<Rooms>,
    pub chat: Option<Chat>,

    /// And our single Matrix client and channel
    pub matrix: Matrix,
    pub sender: Sender<Event>,

    /// We'll hold on to any in-progress verifications here
    pub sas: Option<SasVerification>,
}

impl App {
    pub fn new(send: Sender<Event>, events: EventHandler) -> Self {
        let matrix = Matrix::new(send.clone());

        // Save the sender for future threads.
        SENDER
            .set(Mutex::new(send.clone()))
            .expect("could not set sender");

        Self {
            events,
            running: true,
            timestamp: 0,
            progress: None,
            error: None,
            signin: None,
            confirm: None,
            rooms: None,
            chat: None,
            matrix,
            sender: send,
            sas: None,
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
        let mut chat = Chat::new(self.matrix.clone());
        chat.set_room(room);
        self.chat = Some(chat);
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
        if let Some(p) = self.progress.as_mut() {
            p.tick(self.timestamp)
        }

        self.timestamp += 1;
    }

    /// Renders the user interface widgets.
    pub fn render<B: Backend>(&mut self, frame: &mut Frame<'_, B>) {
        if let Some(c) = &self.chat {
            frame.render_widget(c.widget(), frame.size())
        }

        if let Some(w) = &self.error {
            frame.render_widget(w.widget(), frame.size());
            return;
        }

        if let Some(w) = &self.progress {
            frame.render_widget(w.widget(), frame.size());
            return;
        }

        if let Some(w) = &self.signin {
            frame.render_widget(w.widget(), frame.size());
            return;
        }

        if let Some(c) = &self.confirm {
            frame.render_widget(c.widget(), frame.size());
            return;
        }

        if let Some(r) = &self.rooms {
            frame.render_widget(r.widget(), frame.size())
        }
    }
}
