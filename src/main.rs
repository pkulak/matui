use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{handle_app_event, handle_blur_event, handle_focus_event, handle_key_event};
use matui::settings::watch_settings_forever;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        simple_logging::log_to_file("test.log", LevelFilter::Info)?;
        log_panics::init();
    }

    watch_settings_forever();

    // Initialize the terminal user interface
    let mut terminal = ratatui::init();

    // and the event system.
    let events = EventHandler::new(250);
    let sender = events.sender();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();

    // Create an application.
    let mut app = App::new(sender, &runtime);

    // Start the main loop.
    while app.running {
        // Handle events.
        let mut render = true;

        match events.next()? {
            Event::Tick => {
                render = app.tick();
            }
            Event::Key(key_event) => handle_key_event(key_event, &mut app, &events)?,
            Event::Matui(app_event) => handle_app_event(app_event, &mut app),
            Event::Focus => handle_focus_event(&mut app),
            Event::Blur => handle_blur_event(&mut app),
            Event::Redraw => {
                let _ = terminal.clear();
            }
        }

        if render {
            terminal.draw(|f| app.render(f))?;
        }
    }

    // Exit the user interface.
    ratatui::restore();

    // And then the runtime
    runtime.shutdown_timeout(Duration::from_secs(10));

    Ok(())
}
