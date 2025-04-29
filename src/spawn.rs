use anyhow::{bail, Context};
use image::imageops::FilterType;
use lazy_static::lazy_static;
use linkify::LinkFinder;
use log::error;
use matrix_sdk::media::MediaFileHandle;
use native_dialog::DialogBuilder;
use notify_rust::Hint;
use regex::Regex;
use std::env::var;
use std::fs;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tempfile::Builder;

use crate::settings::clean_vim;

lazy_static! {
    static ref FILE_RE: Regex = Regex::new(r"-([0-9]+)(\.|$)").unwrap();
}

pub fn get_file_paths() -> anyhow::Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("no home directory")?;

    let path = DialogBuilder::file()
        .set_location(home.as_path())
        .open_multiple_file()
        .show()?;

    Ok(path)
}

pub fn get_text(existing: Option<&str>, suffix: Option<&str>) -> anyhow::Result<Option<String>> {
    let editor = &var("EDITOR").unwrap_or("/usr/bin/vi".to_string());
    let tmpfile = Builder::new().suffix(".md").tempfile()?;

    let mut to_write = "".to_string();

    if let Some(str) = existing {
        to_write = str.to_string();
    }

    if let Some(str) = suffix {
        to_write = format!("{}\n\n{}\n", to_write, str);
    }

    if !to_write.trim().is_empty() {
        std::fs::write(&tmpfile, to_write)?;
    }

    let mut command = Command::new(editor);

    // xterm1 is a terminfo that explicitly ignores the alternate screen,
    // which is great for us, because an editor forcing us back to the
    // main screen is not at all ideal
    command.env("TERM", "xterm1");

    // set up vim just right, if that's what we're using
    if editor.ends_with("vim") || editor.ends_with("vi") {
        if clean_vim() {
            command.arg("--clean");
        }

        if existing.is_none() {
            // open in insert mode
            command.arg("+star");

            // map Shift+Enter to insert a new line (needs terminal keybindings)
            command.arg("-c");
            command.arg("imap <S-CR> <esc>o");

            // map Enter to save and quit (works with no keybindings)
            command.arg("-c");
            command.arg("imap <C-M> <esc>:wq<enter>");
        }

        // but always turn on word wrap and spellcheck
        command.arg("-c");
        command.arg("set wrap linebreak nolist spell");
    }

    let status = command.arg(tmpfile.path()).status()?;

    if !status.success() {
        bail!("Invalid status code.")
    }

    let mut contents = fs::read_to_string(tmpfile.path())?;

    if let Some(str) = suffix {
        contents = contents.replace(str, "");
    }

    // This should survive through read_to_string, but let's be sure.
    drop(tmpfile);

    if contents.trim().is_empty() {
        return Ok(None);
    }

    Ok(Some(contents.trim().to_string()))
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

pub fn save_file(handle: MediaFileHandle, file_name: &str) -> anyhow::Result<PathBuf> {
    let mut destination = dirs::download_dir().context("no download directory")?;
    destination.push(file_name);
    let destination = make_unique(destination);
    fs::copy(handle.path(), &destination)?;
    Ok(destination)
}

pub fn make_unique(mut path: PathBuf) -> PathBuf {
    loop {
        if !path.exists() {
            return path;
        }

        path.set_file_name(next_file_name(
            path.file_name().expect("no file name").to_str().unwrap(),
        ));
    }
}

fn next_file_name(og: &str) -> String {
    // if there's already a version, increment
    if let Some(cap) = FILE_RE.captures_iter(og).next() {
        if let Ok(version) = cap[1].parse::<usize>() {
            let replacement = format!("-{}$2", version + 1);
            return FILE_RE.replace(og, replacement).to_string();
        }
    }

    // if there's an extension, start a new version just before
    if og.contains('.') {
        let reversed: String = og.chars().rev().collect();
        let replaced = reversed.replacen('.', ".1-", 1);
        return replaced.chars().rev().collect();
    }

    // otherwise, just throw it on the end
    format!("{}-1", og)
}

pub fn view_text(text: &str) {
    let finder = LinkFinder::new();

    for link in finder.links(text) {
        let mut command = open::commands(link.as_str()).into_iter().next().unwrap();
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());

        if let Err(e) = command.status() {
            error!("could not open link: {} {}", link.as_str(), e);
        }
    }
}

pub fn send_notification(summary: &str, body: &str, image: Option<Vec<u8>>) -> anyhow::Result<()> {
    if let Some(img) = image {
        let data = Cursor::new(img);
        let reader = image::ImageReader::new(data).with_guessed_format()?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_file_first() {
        assert_eq!(next_file_name("image.jpg"), "image-1.jpg");
    }

    #[test]
    fn test_next_file_second() {
        assert_eq!(next_file_name("image-1.jpg"), "image-2.jpg");
    }

    #[test]
    fn test_next_file_too_many() {
        assert_eq!(next_file_name("image-375.jpg"), "image-376.jpg");
    }

    #[test]
    fn test_next_no_ext() {
        assert_eq!(next_file_name("image"), "image-1");
        assert_eq!(next_file_name("image-42"), "image-43");
    }
}
