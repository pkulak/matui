use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{handle_app_event, handle_key_event};
use matui::tui::Tui;
use std::io;
use std::sync::mpsc::channel;
use tui::backend::CrosstermBackend;
use tui::Terminal;

fn main() -> anyhow::Result<()> {
    simple_logging::log_to_file("test.log", LevelFilter::Info)?;

    // Channel for app events.
    let (send, recv) = channel();

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
