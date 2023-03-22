use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{handle_app_event, handle_key_event, MatuiEvent};
use matui::tui::Tui;
use std::io;
use std::sync::mpsc::{channel, Sender};
use std::sync::Mutex;
use tui::backend::CrosstermBackend;
use tui::Terminal;

static mut SENDER: Mutex<Option<Sender<MatuiEvent>>> = Mutex::new(None);

/// Last resort; it's probably better to pass around a Sender like normal,
/// rather than deal with lock contention. If anyone knows a safe way to
/// avoid async hell, please let me know!
///
/// # Safety
///
/// This is set once before the rest of the app starts, so should always
/// be available and never set again.
pub unsafe fn get_sender() -> Sender<MatuiEvent> {
    SENDER
        .lock()
        .expect("could not unlock sender")
        .as_ref()
        .unwrap()
        .clone()
}

fn main() -> anyhow::Result<()> {
    simple_logging::log_to_file("test.log", LevelFilter::Info)?;

    // Channel for app events.
    let (send, recv) = channel();

    // Save the sender for future threads.
    unsafe { SENDER = Mutex::new(Some(send.clone())) }

    // Create an application.
    let mut app = App::new(send);

    // Initialize the terminal user interface.
    let backend = CrosstermBackend::new(io::stderr());
    let terminal = Terminal::new(backend)?;
    let events = EventHandler::new(250);
    let mut tui = Tui::new(terminal, events);
    tui.init()?;

    // Start the main loop.
    while app.running {
        // Render the user interface.
        tui.draw(&mut app)?;

        // Handle events.
        match tui.events.next()? {
            Event::Tick => app.tick(),
            Event::Key(key_event) => handle_key_event(key_event, &mut app)?,
            Event::Mouse(_) => {}
            Event::Resize(_, _) => {}
        }

        while let Ok(event) = recv.try_recv() {
            handle_app_event(event, &mut app);
        }
    }

    // Exit the user interface.
    tui.exit()?;
    Ok(())
}
