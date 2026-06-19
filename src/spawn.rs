use anyhow::{Context, bail};
#[cfg(target_os = "linux")]
use ashpd::{
    PortalError,
    desktop::{ResponseError, file_chooser::SelectedFiles},
};
use crossterm::{event::EnableFocusChange, execute};
use lazy_static::lazy_static;
use linkify::LinkFinder;
use log::error;
use matrix_sdk::media::MediaFileHandle;
#[cfg(not(target_os = "linux"))]
use native_dialog::DialogBuilder;
use regex::Regex;
use std::env::var;
use std::fs;
use std::io::stdout;
use std::path::PathBuf;
use std::process::Command;
use tempfile::Builder;
#[cfg(target_os = "linux")]
use url::Url;

use crate::app::App;
use crate::event::{Event, EventHandler};
use crate::matrix::matrix::Matrix;
use crate::settings::clean_vim;
use matrix_sdk::Room;

lazy_static! {
    static ref FILE_RE: Regex = Regex::new(r"-([0-9]+)(\.|$)").unwrap();
}

#[cfg(target_os = "linux")]
pub fn get_file_paths() -> anyhow::Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("no home directory")?;

    let files = App::get_handle().block_on(async {
        SelectedFiles::open_file()
            .title("Upload files")
            .accept_label("Upload")
            .multiple(true)
            .current_folder(home)?
            .send()
            .await?
            .response()
    });

    match files {
        Ok(files) => files.uris().iter().map(file_uri_to_path).collect(),
        // Some portal backends report an empty/cancelled picker as `Other`.
        Err(ashpd::Error::Response(ResponseError::Cancelled | ResponseError::Other))
        | Err(ashpd::Error::Portal(PortalError::Cancelled(_))) => Ok(vec![]),
        Err(err) => Err(err.into()),
    }
}

#[cfg(target_os = "linux")]
fn file_uri_to_path(uri: &ashpd::Uri) -> anyhow::Result<PathBuf> {
    let url = Url::parse(uri.as_str()).with_context(|| format!("invalid file URI: {uri}"))?;

    if url.scheme() != "file" {
        bail!("portal returned non-file URI: {uri}");
    }

    url.to_file_path()
        .map_err(|_| anyhow::anyhow!("portal returned invalid file URI: {uri}"))
}

#[cfg(not(target_os = "linux"))]
pub fn get_file_paths() -> anyhow::Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("no home directory")?;

    let path = DialogBuilder::file()
        .set_location(home.as_path())
        .open_multiple_file()
        .show()?;

    Ok(path)
}

pub fn spawn_editor(
    handler: &EventHandler,
    matrix: Option<(&Matrix, Room)>,
    existing: Option<&str>,
    suffix: Option<&str>,
) -> anyhow::Result<Option<String>> {
    let mut send = None;

    if let Some((m, r)) = matrix.as_ref() {
        send = Some(m.begin_typing(r.clone()));
    }

    handler.park();
    let result = get_text(existing, suffix);

    // External editors can change terminal modes too. Neovim, for example,
    // enables focus reporting while it runs and disables it again on exit, so
    // restore our own focus-reporting request before the event handler resumes.
    let _ = execute!(stdout(), EnableFocusChange);

    handler.unpark();

    if let Some((m, r)) = matrix
        && let Some(send) = send
    {
        m.end_typing(r, send);
    }

    // make sure we redraw the whole app when we come back
    let _ = App::get_sender().send(Event::Redraw);

    result
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
    command.env("MATUI_EDITOR", "1");

    // set up vim just right, if that's what we're using
    if editor.ends_with("vim") || editor.ends_with("vi") {
        if clean_vim() {
            command.arg("--clean");
        }

        if existing.is_none() || existing.unwrap().is_empty() {
            // open in insert mode
            command.arg("+star");
        }

        // turn on word wrap and spellcheck
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
    open::that(handle.path())?;

    // xdg-open often returns immediately, so if we drop the handle now,
    // the temporary file is deleted before the viewer can open it.
    std::thread::sleep(std::time::Duration::from_secs(5));

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
    if let Some(cap) = FILE_RE.captures_iter(og).next()
        && let Ok(version) = cap[1].parse::<usize>()
    {
        let replacement = format!("-{}$2", version + 1);
        return FILE_RE.replace(og, replacement).to_string();
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
        if let Err(e) = open::that_detached(link.as_str()) {
            error!("could not open link: {} {}", link.as_str(), e);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn send_notification(summary: &str, body: &str, image: Option<Vec<u8>>) -> anyhow::Result<()> {
    use image::imageops::FilterType;
    use notify_rust::Hint;
    use std::io::Cursor;

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

#[cfg(target_os = "macos")]
pub fn send_notification(summary: &str, body: &str, _image: Option<Vec<u8>>) -> anyhow::Result<()> {
    notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .show()?;
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
