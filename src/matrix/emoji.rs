use matrix_sdk::encryption::verification::Emoji;
use ratatui::text::Span;

pub fn center_emoji(emoji: &str) -> String {
    let pad = 6usize.saturating_sub(Span::raw(emoji).width());
    let left = pad / 2;
    let right = pad - left;

    format!("{}{emoji}{}", " ".repeat(left), " ".repeat(right))
}

pub fn format_emojis(emojis: [Emoji; 7]) -> String {
    emojis
        .iter()
        .map(|e| center_emoji(e.symbol))
        .collect::<Vec<_>>()
        .join("")
}
