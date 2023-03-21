use std::sync::mpsc::{channel, Receiver, Sender};

use crate::matrix::{Matrix, MatrixEvent, SyncType};
use crate::widgets::chat::Chat;
use crate::widgets::confirm::Confirm;
use crate::widgets::error::Error;
use crate::widgets::progress::Progress;
use crate::widgets::rooms::Rooms;
use crate::widgets::signin::Signin;
use tui::backend::Backend;
use tui::terminal::Frame;

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
    pub send: Sender<MatrixEvent>,
    pub recv: Receiver<MatrixEvent>,
}

impl Default for App {
    fn default() -> Self {
        let (send, recv) = channel();

        let matrix = Matrix::new(send.clone());

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
            recv,
        }
    }
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles the tick event of the terminal.
    pub fn tick(&mut self) {
        // if this is the very first tick, initialize and move on
        if self.timestamp == 0 {
            self.timestamp += 1;
            self.matrix.init();
            return;
        }

        // check for events from Matrix
        for event in self.recv.try_iter() {
            match event {
                MatrixEvent::Error(msg) => {
                    self.error = Some(Error::new(msg));
                    self.progress = None;
                }
                MatrixEvent::LoginComplete => {
                    self.error = None;
                    self.progress = None;
                }
                MatrixEvent::LoginRequired => {
                    // self.signin = Some(Signin::new(self.matrix.clone()));
                    self.confirm = Some(Confirm::new(
                        "Verify".to_string(),
                        "Would you like to very this session?".to_string(),
                        "Yes".to_string(),
                        "No".to_string(),
                    ));
                }
                MatrixEvent::LoginStarted => {
                    self.error = None;
                    self.progress = Some(Progress::new("Logging in"));
                }
                MatrixEvent::SyncComplete => {
                    self.error = None;
                    self.progress = None;
                    self.signin = None;
                }
                MatrixEvent::SyncStarted(st) => {
                    self.error = None;
                    match st {
                        SyncType::Initial => {
                            self.progress = Some(Progress::new("Performing initial sync."))
                        }
                        SyncType::Latest => self.progress = Some(Progress::new("Syncing")),
                    };
                }
                MatrixEvent::RoomSelected(joined) => {
                    self.rooms = None;

                    let mut room = Chat::new(self.matrix.clone());
                    room.set_room(joined.clone());

                    self.chat = Some(room);
                }
            }
        }

        // send out the ticks
        if let Some(r) = self.chat.as_mut() {
            r.tick()
        }

        if let Some(s) = self.signin.as_mut() {
            s.tick()
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
