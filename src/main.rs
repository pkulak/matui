use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{handle_app_event, handle_blur_event, handle_focus_event, handle_key_event};
use matui::tui::Tui;
use std::{fs, io};
use tui::backend::CrosstermBackend;
use tui::Terminal;

const DEFAULT_CONFIG: &str = "reactions = [ \"❤️\", \"👍\", \"👎\", \"😂\", \"‼️\", \"❓️\"]\n";

fn main() -> anyhow::Result<()> {
    simple_logging::log_to_file("test.log", LevelFilter::Info)?;
    log_panics::init();

    // Create the config if it isn't there.
    let mut path = dirs::config_dir().expect("no config directory");
    path.push("matui");
    path.push("config.toml");

    if !path.exists() {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        fs::write(path, DEFAULT_CONFIG)?;
    }

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
            Event::Matui(app_event) => handle_app_event(app_event, &mut app),
            Event::Focus => handle_focus_event(&mut app),
            Event::Blur => handle_blur_event(&mut app),
        }
    }

    // Exit the user interface.
    tui.exit()?;
    Ok(())
}
