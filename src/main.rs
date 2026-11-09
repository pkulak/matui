use crossterm::{
    event::{DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange},
    execute,
};
use log::LevelFilter;
use matui::app::App;
use matui::event::{Event, EventHandler};
use matui::handler::{
    handle_app_event, handle_blur_event, handle_focus_event, handle_key_event, handle_paste_event,
};
use matui::pictures::init_picker;
use matui::settings::watch_settings_forever;
use std::io::stdout;

fn main() -> anyhow::Result<()> {
    // debug builds always log; MATUI_LOG turns logging on for any build
    let log =
        std::env::var_os("MATUI_LOG").or_else(|| cfg!(debug_assertions).then(|| "test.log".into()));

    if let Some(path) = log {
        simple_logging::log_to_file(path, LevelFilter::Info)?;
        log_panics::init();
    }

    watch_settings_forever();

    // Initialize the terminal user interface
    let mut terminal = ratatui::init();
    execute!(stdout(), EnableFocusChange, EnableBracketedPaste)?;

    // this reads the terminal's answers from stdin, so it has to finish
    // before the event thread starts reading
    init_picker();

    // and the event system.
    let events = EventHandler::new(250);
    let sender = events.sender();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();

    let handle = runtime.handle().clone();

    // Create an application.
    let mut app = App::new(sender, handle);

    // Start the main loop.
    while app.running {
        // Handle events.
        let mut render = true;

        match events.next()? {
            Event::Tick => {
                render = app.tick();
            }
            Event::Key(key_event) => handle_key_event(key_event, &mut app, &events)?,
            Event::Paste(value) => handle_paste_event(value, &mut app),
            Event::Matui(app_event) => handle_app_event(app_event, &mut app),
            Event::Focus => handle_focus_event(&mut app),
            Event::Blur => handle_blur_event(&mut app),
            Event::Redraw => {
                let _ = terminal.clear();
                render = true;
            }
        }

        if render {
            terminal.draw(|f| app.render(f))?;
        }
    }

    // Exit the user interface.
    let _ = execute!(stdout(), DisableBracketedPaste, DisableFocusChange);
    ratatui::restore();

    Ok(())
}
