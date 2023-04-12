use crate::handler::MatuiEvent;
use crossterm::event::{self, Event as CrosstermEvent, KeyEvent};
use std::ops::Sub;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// Terminal events.
#[derive(Clone, Debug)]
pub enum Event {
    /// Terminal tick.
    Tick,
    /// Force a clear and full re-draw.
    Redraw,
    /// The window has gained focus
    Focus,
    /// The window has lost focus
    Blur,
    /// Key press.
    Key(KeyEvent),
    /// App event
    Matui(MatuiEvent),
}

/// Terminal event handler.
#[allow(dead_code)]
#[derive(Debug)]
pub struct EventHandler {
    /// Event sender channel.
    sender: Sender<Event>,
    /// Event receiver channel.
    receiver: Receiver<Event>,
    /// Park sender.
    pk_sender: Sender<bool>,
    /// Event handler thread.
    handler: thread::JoinHandle<()>,
}

impl EventHandler {
    pub fn park(&self) {
        self.pk_sender.send(true).expect("could send park event");
    }

    pub fn unpark(&self) {
        self.handler.thread().unpark();
    }

    /// Constructs a new instance of [`EventHandler`].
    pub fn new(tick_rate: u64) -> Self {
        let tick_rate = Duration::from_millis(tick_rate);
        let (sender, receiver) = channel();
        let (pk_sender, pk_receiver) = channel();
        let handler = {
            let sender = sender.clone();
            thread::spawn(move || {
                let mut last_tick = Instant::now();
                let mut last_park = Instant::now().sub(Duration::from_secs(10));

                loop {
                    let timeout = tick_rate
                        .checked_sub(last_tick.elapsed())
                        .unwrap_or(tick_rate);

                    if let Ok(_) = pk_receiver.try_recv() {
                        thread::park();
                        last_park = Instant::now()
                    }

                    if event::poll(timeout).expect("no events available") {
                        let event = event::read().expect("unable to read event");

                        if let Ok(_) = pk_receiver.try_recv() {
                            thread::park();
                            last_park = Instant::now()
                        }

                        // right after we unpark, we can get a stream of
                        // garbage events
                        if last_park.elapsed() > Duration::from_millis(250) {
                            match event {
                                CrosstermEvent::Key(e) => sender.send(Event::Key(e)),
                                CrosstermEvent::FocusGained => sender.send(Event::Focus),
                                CrosstermEvent::FocusLost => sender.send(Event::Blur),
                                _ => Ok(()),
                            }
                            .expect("failed to send terminal event")
                        }
                    }

                    if last_tick.elapsed() >= tick_rate {
                        sender.send(Event::Tick).expect("failed to send tick event");
                        last_tick = Instant::now();
                    }
                }
            })
        };
        Self {
            sender,
            receiver,
            pk_sender,
            handler,
        }
    }

    /// Receive the next event from the handler thread.
    ///
    /// This function will always block the current thread if
    /// there is no data available and it's possible for more data to be sent.
    pub fn next(&self) -> anyhow::Result<Event> {
        Ok(self.receiver.recv()?)
    }

    pub fn sender(&self) -> Sender<Event> {
        self.sender.clone()
    }
}
