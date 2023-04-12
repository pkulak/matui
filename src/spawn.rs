use anyhow::bail;
use image::imageops::FilterType;
use matrix_sdk::media::MediaFileHandle;
use notify_rust::Hint;
use std::env::var;
use std::io::{Cursor, Read};
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

pub fn view_file(handle: MediaFileHandle) -> anyhow::Result<()> {
    let status = open::commands(handle.path())[0].status()?;

    // keep the file handle open until the viewer exits
    drop(handle);

    if !status.success() {
        bail!("Invalid status code.")
    }

    Ok(())
}

pub fn send_notification(summary: &str, body: &str, image: Option<Vec<u8>>) -> anyhow::Result<()> {
    if let Some(img) = image {
        let data = Cursor::new(img);
        let reader = image::io::Reader::new(data).with_guessed_format()?;

        let img = reader
            .decode()?
            .resize_to_fill(250, 250, FilterType::Lanczos3);

        notify_rust::Notification::new()
            .summary(summary)
            .body(body)
            .hint(Hint::ImageData(notify_rust::Image::try_from(img)?))
            .show()?;
    } else {
        notify_rust::Notification::new().body(body).show()?;
    }

    Ok(())
}
