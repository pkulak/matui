use std::{fs, io::Cursor, path::Path, process::Command, time::Duration};

use matrix_sdk::attachment::{AttachmentInfo, BaseImageInfo, BaseVideoInfo, Thumbnail};
use mime::{Mime, IMAGE_JPEG};
use ruma::UInt;

pub fn get_thumbnail(
    path: &Path,
    mime: &Mime,
) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    match (mime.type_(), mime.subtype()) {
        (mime::VIDEO, _) => get_video_thumbnail(path),
        (mime::IMAGE, subtype) if is_animated_image(subtype) => get_animated_image_thumbnail(path),
        (mime::IMAGE, _) => {
            let (_, info) = get_file_thumbnail(path)?;
            Ok((None, info)) // static images don't need a thumbnail
        }
        _ => anyhow::bail!("unsupported media type: {}", mime),
    }
}

fn is_animated_image(subtype: mime::Name) -> bool {
    matches!(subtype.as_str(), "gif" | "webp")
}

fn get_video_thumbnail(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
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

    let (thumb, _) = get_file_thumbnail(tmpfile.path())?;
    let (width, height, size) = (thumb.width, thumb.height, thumb.size);

    Ok((
        Some(thumb),
        AttachmentInfo::Video(BaseVideoInfo {
            duration: Some(Duration::from_secs_f32(duration)),
            width: Some(width),
            height: Some(height),
            size: Some(size),
            blurhash: None,
        }),
    ))
}

fn get_animated_image_thumbnail(
    path: &Path,
) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    // Use ffmpeg to extract first frame from animated images
    let tmpfile = tempfile::Builder::new().suffix(".jpg").tempfile()?;

    let mut command = Command::new("ffmpeg");
    command.arg("-y");
    command.args(["-loglevel", "error"]);
    command.arg("-i");
    command.arg(path);
    command.args(["-frames:v", "1"]);
    command.arg(tmpfile.path());

    if !command.status()?.success() {
        anyhow::bail!("could not create thumbnail from animated image");
    }

    let (thumb, _) = get_file_thumbnail(tmpfile.path())?;
    let (width, height, size) = (thumb.width, thumb.height, thumb.size);

    Ok((
        Some(thumb),
        AttachmentInfo::Image(BaseImageInfo {
            width: Some(width),
            height: Some(height),
            size: Some(size),
            is_animated: Some(true),
            blurhash: None,
        }),
    ))
}

fn get_file_thumbnail(path: &Path) -> anyhow::Result<(Thumbnail, AttachmentInfo)> {
    let data = fs::read(path)?;
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

    let (width, height, size) = (thumb.width, thumb.height, thumb.size);

    Ok((
        thumb,
        AttachmentInfo::Image(BaseImageInfo {
            width: Some(width),
            height: Some(height),
            size: Some(size),
            is_animated: Some(false),
            blurhash: None,
        }),
    ))
}

fn get_video_duration(path: &Path) -> anyhow::Result<f32> {
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
