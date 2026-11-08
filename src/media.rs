use std::{fs, io::Cursor, path::Path, process::Command, time::Duration};

use image::{
    DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbImage, Rgba, RgbaImage, imageops,
};
use matrix_sdk::attachment::{
    AttachmentInfo, BaseAudioInfo, BaseFileInfo, BaseImageInfo, BaseVideoInfo, Thumbnail,
};
use matrix_sdk::ruma::UInt;
use mime::{IMAGE_JPEG, Mime};

const THUMBNAIL_WIDTH: u32 = 800;
const THUMBNAIL_HEIGHT: u32 = 600;

pub fn get_attachment_info(
    path: &Path,
    mime: &Mime,
) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    match (mime.type_(), mime.subtype()) {
        (mime::VIDEO, _) => get_video_info(path),
        (mime::IMAGE, subtype) if is_animated_image(subtype) => get_animated_image_info(path),
        (mime::IMAGE, _) => get_static_image_info(path),
        (mime::AUDIO, _) => get_audio_info(path),
        _ => get_file_info(path),
    }
}

fn is_animated_image(subtype: mime::Name) -> bool {
    matches!(subtype.as_str(), "gif" | "webp")
}

fn get_video_info(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    let duration = get_media_duration(path)?;
    let frame = extract_frame(path, Some(duration / 2.0))?;
    let size = get_file_size(path)?;

    Ok((
        Some(make_thumbnail(&frame)?),
        AttachmentInfo::Video(BaseVideoInfo {
            duration: Some(Duration::from_secs_f32(duration)),
            width: Some(frame.width().into()),
            height: Some(frame.height().into()),
            size: UInt::new(size),
            blurhash: None,
        }),
    ))
}

fn get_animated_image_info(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    let frame = extract_frame(path, None)?;
    let size = get_file_size(path)?;

    Ok((
        Some(make_thumbnail(&frame)?),
        AttachmentInfo::Image(BaseImageInfo {
            width: Some(frame.width().into()),
            height: Some(frame.height().into()),
            size: UInt::new(size),
            is_animated: Some(true),
            blurhash: None,
        }),
    ))
}

fn get_static_image_info(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    // ffmpeg reads formats the image crate can't, such as HEIC
    let img = decode_image(path).or_else(|_| extract_frame(path, None))?;
    let size = get_file_size(path)?;

    Ok((
        Some(make_thumbnail(&img)?),
        AttachmentInfo::Image(BaseImageInfo {
            width: Some(img.width().into()),
            height: Some(img.height().into()),
            size: UInt::new(size),
            is_animated: Some(false),
            blurhash: None,
        }),
    ))
}

fn decode_image(path: &Path) -> anyhow::Result<DynamicImage> {
    let mut decoder = ImageReader::open(path)?
        .with_guessed_format()?
        .into_decoder()?;
    let orientation = decoder.orientation()?;
    let mut img = DynamicImage::from_decoder(decoder)?;
    // clients display the original upright, so the thumbnail must be too
    img.apply_orientation(orientation);
    Ok(img)
}

/// Grabs a full-size frame from a video or animated image, at `seek` seconds
/// or else the first.
fn extract_frame(path: &Path, seek: Option<f32>) -> anyhow::Result<DynamicImage> {
    // PNG keeps any transparency for `make_thumbnail` to flatten
    let tmpfile = tempfile::Builder::new().suffix(".png").tempfile()?;

    let mut command = Command::new("ffmpeg");
    command.arg("-y");
    command.args(["-loglevel", "error"]);
    if let Some(seek) = seek {
        command.arg("-ss");
        command.arg(seek.to_string());
    }
    command.arg("-i");
    command.arg(path);
    command.args(["-frames:v", "1", "-update", "true"]);
    command.arg(tmpfile.path());

    if !command.status()?.success() {
        anyhow::bail!("could not extract a frame");
    }

    Ok(image::open(tmpfile.path())?)
}

/// Shrinks `img` to fit within 800×600, keeping its shape, and encodes it as
/// a JPEG. Smaller images keep their size.
fn make_thumbnail(img: &DynamicImage) -> anyhow::Result<Thumbnail> {
    let img = if img.width() > THUMBNAIL_WIDTH || img.height() > THUMBNAIL_HEIGHT {
        flatten(&img.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT))
    } else {
        flatten(img)
    };

    let mut data = Vec::new();
    img.write_to(&mut Cursor::new(&mut data), ImageFormat::Jpeg)?;

    Ok(Thumbnail {
        size: UInt::new(data.len() as u64).unwrap(),
        data,
        content_type: IMAGE_JPEG,
        width: img.width().into(),
        height: img.height().into(),
    })
}

/// Lays `img` over white, since JPEG has no alpha channel. Dropping the
/// channel instead would show the colour hidden under transparent pixels,
/// usually black.
fn flatten(img: &DynamicImage) -> RgbImage {
    if !img.color().has_alpha() {
        return img.to_rgb8();
    }

    let mut canvas = RgbaImage::from_pixel(img.width(), img.height(), Rgba([255; 4]));
    imageops::overlay(&mut canvas, &img.to_rgba8(), 0, 0);
    DynamicImage::ImageRgba8(canvas).to_rgb8()
}

fn get_audio_info(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    let duration = get_media_duration(path).ok();
    let size = get_file_size(path)?;

    Ok((
        None,
        AttachmentInfo::Audio(BaseAudioInfo {
            duration: duration.map(Duration::from_secs_f32),
            size: UInt::new(size),
            waveform: Option::None,
        }),
    ))
}

fn get_file_info(path: &Path) -> anyhow::Result<(Option<Thumbnail>, AttachmentInfo)> {
    let size = get_file_size(path)?;

    Ok((
        None,
        AttachmentInfo::File(BaseFileInfo {
            size: UInt::new(size),
        }),
    ))
}

fn get_file_size(path: &Path) -> anyhow::Result<u64> {
    Ok(fs::metadata(path)?.len())
}

fn get_media_duration(path: &Path) -> anyhow::Result<f32> {
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

#[cfg(test)]
mod tests {
    use std::{path::Path, process::Command};

    use image::{
        Delay, DynamicImage, Frame, ImageFormat, Rgba, RgbaImage, codecs::gif::GifEncoder,
    };
    use matrix_sdk::attachment::{AttachmentInfo, BaseImageInfo, BaseVideoInfo, Thumbnail};
    use matrix_sdk::ruma::UInt;
    use mime::{IMAGE_GIF, IMAGE_JPEG, IMAGE_PNG, Mime};
    use tempfile::NamedTempFile;

    use super::get_attachment_info;

    fn png(width: u32, height: u32) -> NamedTempFile {
        let file = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        // with an alpha channel, which JPEG can't hold
        DynamicImage::ImageRgba8(RgbaImage::new(width, height))
            .save_with_format(file.path(), ImageFormat::Png)
            .unwrap();
        file
    }

    fn info(path: &Path, mime: &Mime) -> (Thumbnail, AttachmentInfo) {
        let (thumb, info) = get_attachment_info(path, mime).unwrap();
        (thumb.expect("a thumbnail"), info)
    }

    fn assert_thumbnail(thumb: &Thumbnail, width: u32, height: u32) {
        assert_eq!(thumb.content_type, IMAGE_JPEG);
        assert_eq!(image::guess_format(&thumb.data).unwrap(), ImageFormat::Jpeg);
        assert_eq!(thumb.size, UInt::from(thumb.data.len() as u32));

        let decoded = image::load_from_memory(&thumb.data).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (width, height));
        assert_eq!((thumb.width, thumb.height), (width.into(), height.into()));
    }

    fn assert_white(thumb: &Thumbnail) {
        let decoded = image::load_from_memory(&thumb.data).unwrap().to_rgb8();
        // JPEG is lossy, so allow a little noise
        assert!(decoded.pixels().all(|p| p.0.iter().all(|&c| c > 250)));
    }

    fn assert_image_size(info: &AttachmentInfo, width: u32, height: u32) {
        let AttachmentInfo::Image(BaseImageInfo {
            width: w,
            height: h,
            ..
        }) = info
        else {
            panic!("not image info: {info:?}");
        };
        assert_eq!((*w, *h), (Some(width.into()), Some(height.into())));
    }

    #[test]
    fn it_shrinks_large_images() {
        let file = png(1600, 1000);
        let (thumb, info) = info(file.path(), &IMAGE_PNG);

        assert_thumbnail(&thumb, 800, 500);
        assert_image_size(&info, 1600, 1000);
    }

    #[test]
    fn it_does_not_enlarge_small_images() {
        let file = png(300, 200);
        let (thumb, info) = info(file.path(), &IMAGE_PNG);

        assert_thumbnail(&thumb, 300, 200);
        assert_image_size(&info, 300, 200);
    }

    #[test]
    fn it_puts_transparent_images_on_white() {
        let file = png(300, 200);
        let (thumb, _) = info(file.path(), &IMAGE_PNG);

        assert_white(&thumb);
    }

    #[test]
    fn it_puts_transparent_frames_on_white() {
        // goes through ffmpeg, like animated images
        let file = tempfile::Builder::new().suffix(".webp").tempfile().unwrap();
        // transparent, hiding red
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(300, 200, Rgba([255, 0, 0, 0])))
            .save_with_format(file.path(), ImageFormat::WebP)
            .unwrap();

        let (thumb, _) = info(file.path(), &"image/webp".parse().unwrap());

        assert_white(&thumb);
    }

    #[test]
    fn it_thumbnails_images_the_image_crate_cannot_decode() {
        // stands in for HEIC, which ffmpeg can read but not write
        let file = tempfile::Builder::new().suffix(".avif").tempfile().unwrap();
        let status = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi"])
            .args(["-i", "testsrc=size=1600x1000", "-frames:v", "1"])
            .arg(file.path())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(image::open(file.path()).is_err());

        let (thumb, info) = info(file.path(), &"image/avif".parse().unwrap());

        assert_thumbnail(&thumb, 800, 500);
        assert_image_size(&info, 1600, 1000);
    }

    #[test]
    fn it_thumbnails_animated_gifs() {
        let file = tempfile::Builder::new().suffix(".gif").tempfile().unwrap();
        let frames = (0..3).map(|_| {
            Frame::from_parts(
                RgbaImage::new(1200, 900),
                0,
                0,
                Delay::from_numer_denom_ms(100, 1),
            )
        });
        GifEncoder::new(file.reopen().unwrap())
            .encode_frames(frames)
            .unwrap();

        let (thumb, info) = info(file.path(), &IMAGE_GIF);

        assert_thumbnail(&thumb, 800, 600);
        assert_image_size(&info, 1200, 900);
    }

    #[test]
    fn it_thumbnails_videos() {
        let file = tempfile::Builder::new().suffix(".mp4").tempfile().unwrap();
        let status = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi"])
            .args(["-i", "testsrc=size=1280x720:duration=1"])
            .arg(file.path())
            .status()
            .unwrap();
        assert!(status.success());

        let (thumb, info) = info(file.path(), &"video/mp4".parse().unwrap());

        assert_thumbnail(&thumb, 800, 450);
        let AttachmentInfo::Video(BaseVideoInfo { width, height, .. }) = info else {
            panic!("not video info: {info:?}");
        };
        assert_eq!((width, height), (Some(1280u32.into()), Some(720u32.into())));
    }
}
