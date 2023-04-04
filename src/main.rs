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
    let mut app = App::new(sender, events);

    // Start the main loop.
    while app.running {
        // Handle events.
        match app.events.next()? {
            Event::Tick => {
                app.tick();
                tui.draw(&mut app, false)?;
            }
            Event::Redraw => tui.draw(&mut app, true)?,
            Event::Key(key_event) => {
                handle_key_event(key_event, &mut app)?;
                tui.draw(&mut app, false)?;
            }
            Event::Mouse(_) => {}
            Event::Resize(_, _) => {}

            // these events can come in pretty heavy, so don't render
            Event::Matui(app_event) => handle_app_event(app_event, &mut app),
        }
    }

    // Exit the user interface.
    tui.exit()?;
    Ok(())
}
