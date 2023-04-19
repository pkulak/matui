use std::time::{Duration, Instant};

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
#[macro_use]
pub mod widgets;

/// Matrix
pub mod matrix;

pub mod settings;

/// Using external apps to do our bidding
pub mod spawn;

pub fn pretty_list(names: Vec<&str>) -> String {
    match names.len() {
        0 => "".to_string(),
        1 => names.first().unwrap().to_string(),
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
        Some((idx, _)) => format!("{}…", &s[..(idx - 1)]),
    }
}

struct KeyCombo {
    last: Instant,
    waiting_for: Vec<char>,
    combo: Vec<char>,
    timeframe: Duration,
}

impl KeyCombo {
    fn new(mut combo: Vec<char>) -> Self {
        if combo.is_empty() {
            panic!("combo cannot be empty");
        }

        combo.reverse();

        KeyCombo {
            last: Instant::now(),
            waiting_for: combo.clone(),
            combo,
            timeframe: Duration::from_millis(500),
        }
    }

    pub fn record(&mut self, c: char) -> bool {
        if self.last.elapsed() > self.timeframe {
            self.reset();
        }

        if &c == self.waiting_for.last().unwrap() {
            self.waiting_for.pop();
            self.last = Instant::now();
        } else {
            self.reset();
        }

        if self.waiting_for.is_empty() {
            self.reset();
            return true;
        }

        false
    }

    fn reset(&mut self) {
        self.last = Instant::now();
        self.waiting_for = self.combo.clone();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crate::KeyCombo;

    #[test]
    #[allow(clippy::bool_assert_comparison)]
    fn it_finds_patterns() {
        let mut combo = KeyCombo::new(vec!['a', 'b', 'c']);

        assert_eq!(combo.record('a'), false);
        assert_eq!(combo.record('b'), false);
        assert_eq!(combo.record('c'), true);
        assert_eq!(combo.record('d'), false);
        assert_eq!(combo.record('x'), false);
        assert_eq!(combo.record('a'), false);
        assert_eq!(combo.record('b'), false);
        assert_eq!(combo.record('c'), true);
    }

    #[test]
    #[allow(clippy::bool_assert_comparison)]
    fn it_ignores_after_delay() {
        let mut combo = KeyCombo::new(vec!['a', 'b', 'c']);

        assert_eq!(combo.record('a'), false);
        assert_eq!(combo.record('b'), false);

        combo.last = Instant::now() - Duration::from_secs(5);

        assert_eq!(combo.record('c'), false);

        assert_eq!(combo.record('a'), false);
        assert_eq!(combo.record('b'), false);
        assert_eq!(combo.record('c'), true);
    }

    #[test]
    #[allow(clippy::bool_assert_comparison)]
    fn it_ignores_after_wront_key() {
        let mut combo = KeyCombo::new(vec!['a', 'b', 'c']);

        assert_eq!(combo.record('a'), false);
        assert_eq!(combo.record('b'), false);
        assert_eq!(combo.record('x'), false);
        assert_eq!(combo.record('c'), false);
    }
}
