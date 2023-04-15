use anyhow::bail;
use image::imageops::FilterType;
use linkify::LinkFinder;
use log::error;
use matrix_sdk::media::MediaFileHandle;
use notify_rust::Hint;
use std::env::var;
use std::io::{Cursor, Read};
use std::process::{Command, Stdio};
use tempfile::Builder;

pub fn get_text(existing: Option<&str>) -> anyhow::Result<Option<String>> {
    let editor = &var("EDITOR")?;
    let mut tmpfile = Builder::new().suffix(".md").tempfile()?;

    if let Some(str) = existing {
        std::fs::write(&tmpfile, str)?;
    }

    let mut command = Command::new(editor);

    // xterm1 is a terminfo that explicitly ignores the alternate screen,
    // which is great for us, because an editor forcing us back to the
    // main screen is not at all ideal
    command.env("TERM", "xterm1");

    // set up vim just right, if that's what we're using
    if editor.ends_with("vim") || editor.ends_with("vi") {
        // if the file is empty, open in insert, and map enter to save and quit
        if existing.is_none() {
            command.arg("+star");
            command.arg("-c");
            command.arg("imap <C-M> <esc>:wq<enter>");
        }

        // but always turn on word wrap
        command.arg("-c");
        command.arg("set wrap linebreak nolist");
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

pub fn view_text(text: &str) {
    let finder = LinkFinder::new();

    for link in finder.links(text) {
        let mut command = open::commands(link.as_str()).into_iter().next().unwrap();
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Err(e) = command.status() {
            error!("could not open link: {} {}", link.as_str(), e.to_string());
        }
    }
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
