extern crate core;

/// Application.
pub mod app;

/// Terminal events handler.
pub mod event;

/// Terminal user interface.
pub mod tui;

/// Event handler.
pub mod handler;

/// List of rooms we're in.
pub mod widgets;

/// Matrix
pub mod matrix;

/// Using external apps to do our bidding
pub mod spawn;

pub fn pretty_list(names: Vec<&str>) -> String {
    match names.len() {
        0 => "".to_string(),
        1 => names.get(0).unwrap().to_string(),
        _ => format!(
            "{} and {}",
            names[0..names.len() - 1].join(", "),
            names.last().unwrap()
        ),
    }
}

fn truncate(s: String, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        None => s,
        Some((idx, _)) => format!("{}…", s[..(idx - 1)].to_string()),
    }
}
