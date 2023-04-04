use anyhow::bail;
use std::env::var;
use std::io::Read;
use std::process::Command;
use tempfile::NamedTempFile;

pub fn get_text() -> anyhow::Result<Option<String>> {
    let editor = &var("EDITOR")?;
    let mut tmpfile = NamedTempFile::new()?;

    let mut command = Command::new(editor);

    // xterm1 is a terminfo that explicitly ignores the alternate screen,
    // which is great for us, because an editor forcing us back to the
    // main screen is not at all ideal
    command.env("TERM", "xterm1");

    if editor.ends_with("vim") || editor.ends_with("vi") {
        // for vim, open in insert, and map enter to save and quit
        command.arg("+star");
        command.arg("-c");
        command.arg("imap <C-M> <esc>:wq<enter>");
    }

    let status = command.arg(tmpfile.path()).status()?;

    if !status.success() {
        bail!("Invalid status code.")
    }

    let mut contents = String::new();
    tmpfile.read_to_string(&mut contents)?;

    if contents.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(contents))
}
