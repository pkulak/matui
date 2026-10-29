use matrix_sdk::encryption::verification::Emoji;

pub fn pad_emoji(emoji: &str) -> String {
    // These are emojis that need VARIATION-SELECTOR-16 (U+FE0F) so that they are
    // rendered with coloured glyphs. For these, we need to add an extra
    // space after them so that they are rendered properly in terminals.
    const VARIATION_SELECTOR_EMOJIS: [&str; 8] = ["☁️", "❤️", "☂️", "✏️", "✂️", "☎️", "✈️", "‼️"];

    // Hack to make terminals behave properly when one of the above is printed.
    if VARIATION_SELECTOR_EMOJIS.contains(&emoji) {
        format!("{emoji} ")
    } else {
        emoji.to_owned()
    }
}

pub fn center_emoji(emoji: &str) -> String {
    let emoji = pad_emoji(emoji);

    // This is a trick to account for the fact that emojis are wider than other
    // monospace characters.
    let placeholder = ".".repeat(2);

    format!("{placeholder:^6}").replace(&placeholder, &emoji)
}

pub fn format_emojis(emojis: [Emoji; 7]) -> String {
    let emojis: Vec<_> = emojis.iter().map(|e| e.symbol).collect();

    emojis
        .iter()
        .map(|e| center_emoji(e))
        .collect::<Vec<_>>()
        .join("")
}
