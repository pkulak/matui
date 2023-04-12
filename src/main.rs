use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{handle_app_event, handle_key_event};
use matui::tui::Tui;
use std::io;
use tui::backend::CrosstermBackend;
use tui::Terminal;

fn main() -> anyhow::Result<()> {
    simple_logging::log_to_file("test.log", LevelFilter::Info)?;

    // Initialize the terminal user interface.
    let backend = CrosstermBackend::new(io::stderr());
    let terminal = Terminal::new(backend)?;
    let events = EventHandler::new(250);
    let sender = events.sender();
    let mut tui = Tui::new(terminal);
    tui.init()?;

    // Create an application.
    let mut app = App::new(sender);

    // Start the main loop.
    while app.running {
        tui.draw(&mut app, false)?;

        // Handle events.
        match events.next()? {
            Event::Tick => app.tick(),
            Event::Redraw => tui.draw(&mut app, true)?,
            Event::Key(key_event) => handle_key_event(key_event, &mut app, &events)?,
            Event::Mouse(_) => {}
            Event::Resize(_, _) => {}
            Event::Matui(app_event) => handle_app_event(app_event, &mut app),
        }
    }

    // Exit the user interface.
    tui.exit()?;
    Ok(())
}
