use crossterm::clipboard::CopyToClipboard;
use crossterm::execute;
use std::io::stdout;

/// Put the text on the clipboard, with OSC 52. Only the clipboard: libghostty
/// drops a sequence that names more than one destination, such as `cp`.
pub fn copy(text: &str) -> std::io::Result<()> {
    execute!(stdout(), CopyToClipboard::to_clipboard_from(text))
}
