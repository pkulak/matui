use crate::app::App;
use crate::widgets::rooms::Rooms;
use crossterm::event::{KeyCode, KeyEvent};

pub fn handle_key_events(key_event: KeyEvent, app: &mut App) -> anyhow::Result<()> {
    // hide an error message on any key event
    if app.error.is_some() {
        app.error = None;
        return Ok(());
    }

    match key_event.code {
        KeyCode::Esc => {
            if app.rooms.is_some() {
                app.rooms = None;
            } else {
                app.running = false;
            }
        }
        _ => {
            if let Some(w) = &mut app.signin {
                w.input(&key_event);
            }

            if let Some(w) = &mut app.rooms {
                w.input(&key_event)
            }

            if let Some(w) = &mut app.confirm {
                w.input(&key_event)
            }

            if app.signin.is_none() && app.rooms.is_none() && key_event.code == KeyCode::Char('r') {
                app.rooms = Some(Rooms::new(app.matrix.joined_rooms(), app.send.clone()));
            }
        }
    }

    Ok(())
}
