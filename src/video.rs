use std::{fs, io::Cursor, path::Path, process::Command};

use matrix_sdk::attachment::Thumbnail;
use mime::IMAGE_JPEG;
use ruma::UInt;

pub fn get_video_duration(path: &Path) -> anyhow::Result<f32> {
    let mut command = Command::new("ffprobe");

    command.args([
        "-loglevel",
        "error",
        "-of",
        "csv=p=0",
        "-show_entries",
        "format=duration",
    ]);

    command.arg(path);

    let output = command.output()?;
    let output = String::from_utf8(output.stdout)?;

    Ok(output.trim().parse()?)
}

pub fn get_video_thumbnail(path: &Path) -> anyhow::Result<Thumbnail> {
    let duration = get_video_duration(path)?;
    let tmpfile = tempfile::Builder::new().suffix(".jpg").tempfile()?;

    let mut command = Command::new("ffmpeg");

    command.arg("-y");
    command.args(["-loglevel", "error"]);
    command.arg("-ss");
    command.arg((duration / 2.0).to_string());
    command.arg("-i");
    command.arg(path);
    command.args(["-frames:v", "1", "-update", "true"]);
    command.arg(tmpfile.path());

    if !command.status()?.success() {
        anyhow::bail!("could not create thumbnail");
    }

    let data = fs::read(tmpfile.path())?;
    let cursor = Cursor::new(&data);

    let img = image::ImageReader::new(cursor)
        .with_guessed_format()?
        .decode()?;

    let size = data.len() as u64;

    let thumb = Thumbnail {
        data,
        content_type: IMAGE_JPEG,
        size: UInt::new(size).unwrap(),
        width: img.width().into(),
        height: img.height().into(),
    };

    Ok(thumb)
}
