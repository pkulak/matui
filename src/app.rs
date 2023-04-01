use matrix_sdk::encryption::verification::SasVerification;
use std::sync::mpsc::Sender;
use std::sync::Mutex;

use crate::handler::MatuiEvent;
use crate::matrix::matrix::Matrix;
use crate::widgets::chat::Chat;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::Rooms;
use crate::widgets::signin::Signin;
use tui::backend::Backend;
use tui::terminal::Frame;

static mut SENDER: Mutex<Option<Sender<MatuiEvent>>> = Mutex::new(None);

/// Application.
pub struct App {
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
    pub send: Sender<MatuiEvent>,

    /// We'll hold on to any in-progress verifications here
    pub sas: Option<SasVerification>,
}

impl App {
    pub fn new(send: Sender<MatuiEvent>) -> Self {
        let matrix = Matrix::new(send.clone());

        // Save the sender for future threads.
        // Safety: let's make sure to only have one App
        unsafe { SENDER = Mutex::new(Some(send.clone())) }

        Self {
            running: true,
            timestamp: 0,
            progress: None,
            error: None,
            signin: None,
            confirm: None,
            rooms: None,
            chat: None,
            matrix,
            send,
            sas: None,
        }
    }

    /// Last resort; it's probably better to pass around a Sender like normal,
    /// rather than deal with lock contention. If anyone knows a safe way to
    /// avoid async hell, please let me know!
    ///
    /// # Safety
    ///
    /// This is set once before the rest of the app starts, so should always
    /// be available and never set again.
    pub fn get_sender() -> Sender<MatuiEvent> {
        unsafe {
            SENDER
                .lock()
                .expect("could not unlock sender")
                .as_ref()
                .unwrap()
                .clone()
        }
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
        if let Some(c) = self.chat.as_mut() {
            c.tick()
        }

        if let Some(p) = self.progress.as_mut() {
            p.tick(self.timestamp)
        }

        self.timestamp += 1;
    }

    /// Renders the user interface widgets.
    pub fn render<B: Backend>(&mut self, frame: &mut Frame<'_, B>) {
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

        if let Some(c) = &self.chat {
            frame.render_widget(c.widget(), frame.size())
        }

        if let Some(r) = &self.rooms {
            frame.render_widget(r.widget(), frame.size())
        }
    }
}
