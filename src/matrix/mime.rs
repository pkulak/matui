use std::path::Path;

use mime::{Mime, APPLICATION_OCTET_STREAM};

/// keep around a few important, chat-related formats
/// everything else can be an octet stream
pub static MIME_TYPES: &[(&str, &str)] = &[
    ("avif", "image/avif"),
    ("jpeg", "image/jpeg"),
    ("jpg", "image/jpeg"),
    ("jxl", "image/jxl"),
    ("heic", "image/heic"),
    ("heics", "image/heic-sequence"),
    ("heif", "image/heif"),
    ("heifs", "image/heif-sequence"),
    ("m4a", "audio/m4a"),
    ("mkv", "video/x-matroska"),
    ("mov", "video/quicktime"),
    ("mp3", "audio/mpeg"),
    ("mp4", "video/mp4"),
    ("mp4a", "audio/mp4"),
    ("pdf", "application/pdf"),
    ("png", "image/png"),
    ("tar", "application/x-tar"),
    ("tar.gz", "application/gzip"),
    ("wav", "audio/wav"),
    ("zip", "application/zip"),
];

pub fn mime_from_path(path: &Path) -> Mime {
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_str()
        .unwrap_or_default()
        .to_string()
        .to_lowercase();

    // don't bother with a binary search for now
    for m in MIME_TYPES {
        if m.0 == ext {
            return m.1.parse().unwrap_or(APPLICATION_OCTET_STREAM);
        }
    }

    APPLICATION_OCTET_STREAM
}

#[cfg(test)]
mod tests {
    use anyhow::Context;

    use crate::matrix::mime::mime_from_path;

    #[test]
    fn it_finds_mime_types() -> anyhow::Result<()> {
        let mut path = dirs::home_dir().context("")?;
        path.push("funny_photo.jpg");

        assert_eq!(mime_from_path(&path).to_string(), "image/jpeg");

        Ok(())
    }

    #[test]
    fn it_does_not_find_mime_types() -> anyhow::Result<()> {
        let mut path = dirs::home_dir().context("")?;
        path.push("silly_file.woofy");

        assert_eq!(
            mime_from_path(&path).to_string(),
            "application/octet-stream"
        );

        Ok(())
    }
}
