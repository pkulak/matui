use image::DynamicImage;
use log::{info, warn};
use matrix_sdk::ruma::OwnedMxcUri;
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::message::MessageType::{self, Image, Video};
use ratatui::layout::Size;
use ratatui_image::Resize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use std::collections::HashMap;
use std::sync::OnceLock;

// the tallest a picture gets, in rows
const MAX_ROWS: u16 = 10;

// originals bigger than this are too much to download just to look at
const MAX_ORIGINAL_SIZE: u64 = 2 * 1024 * 1024;

static PICKER: OnceLock<Option<Picker>> = OnceLock::new();

/// Asks the terminal what it can draw. Pictures need Kitty's placeholders,
/// so any other answer turns them off for the session. Call this before
/// anything else reads stdin.
pub fn init_picker() {
    let picker = match Picker::from_query_stdio() {
        Ok(p) if p.protocol_type() == ProtocolType::Kitty => Some(p),
        Ok(p) => {
            info!("pictures off: {:?} terminal", p.protocol_type());
            None
        }
        Err(err) => {
            warn!("pictures off: could not query the terminal: {}", err);
            None
        }
    };

    let _ = PICKER.set(picker);
}

pub fn picker() -> Option<&'static Picker> {
    PICKER.get().and_then(Option::as_ref)
}

/// Which picture, if any, to show for a message.
pub fn picture_source(body: &MessageType) -> Option<MediaSource> {
    match body {
        Image(content) => {
            let info = content.info.as_deref();

            if let Some(thumbnail) = info.and_then(|i| i.thumbnail_source.clone()) {
                return Some(thumbnail);
            }

            let size = info.and_then(|i| i.size)?;
            (u64::from(size) <= MAX_ORIGINAL_SIZE).then(|| content.source.clone())
        }
        Video(content) => content
            .info
            .as_deref()
            .and_then(|i| i.thumbnail_source.clone()),
        _ => None,
    }
}

pub fn source_uri(source: &MediaSource) -> &OwnedMxcUri {
    match source {
        MediaSource::Plain(uri) => uri,
        MediaSource::Encrypted(file) => &file.url,
    }
}

pub enum Picture {
    Loading,
    Failed,
    Ready {
        image: DynamicImage,
        // created on first draw, and again whenever the size changes
        protocol: Option<(Size, SlicedProtocol)>,
    },
}

/// A chat's pictures, keyed by source.
#[derive(Default)]
pub struct Pictures {
    entries: HashMap<OwnedMxcUri, Picture>,
}

impl Pictures {
    /// Store a fetch result; `None` means it failed.
    pub fn insert(&mut self, uri: OwnedMxcUri, image: Option<DynamicImage>) {
        let picture = match image {
            Some(image) => Picture::Ready {
                image,
                protocol: None,
            },
            None => Picture::Failed,
        };

        self.entries.insert(uri, picture);
    }

    /// Marks a picture as loading, returning whether it still needed fetching.
    pub fn request(&mut self, uri: &OwnedMxcUri) -> bool {
        if self.entries.contains_key(uri) {
            return false;
        }

        self.entries.insert(uri.clone(), Picture::Loading);
        true
    }

    /// The rows and columns a ready picture takes, within the given width.
    pub fn size(&self, uri: &OwnedMxcUri, picker: &Picker, width: u16) -> Option<Size> {
        let Some(Picture::Ready { image, .. }) = self.entries.get(uri) else {
            return None;
        };

        Some(Resize::Fit(None).size_for(image, picker.font_size(), Size::new(width, MAX_ROWS)))
    }

    /// The picture, ready to draw at the given size.
    pub fn protocol(
        &mut self,
        uri: &OwnedMxcUri,
        picker: &Picker,
        size: Size,
    ) -> Option<&SlicedProtocol> {
        let entry = self.entries.get_mut(uri)?;

        let Picture::Ready { image, protocol } = entry else {
            return None;
        };

        if protocol.as_ref().is_none_or(|(s, _)| *s != size) {
            match SlicedProtocol::new_with_resize(picker, image.clone(), size, Resize::Fit(None)) {
                Ok(p) => *protocol = Some((size, p)),
                Err(err) => {
                    warn!("could not draw picture {}: {}", uri, err);
                    *entry = Picture::Failed;
                    return None;
                }
            }
        }

        let Picture::Ready { protocol, .. } = entry else {
            return None;
        };

        protocol.as_ref().map(|(_, p)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::exports::serde_json::{self, Value, json};
    use matrix_sdk::ruma::mxc_uri;

    fn content(value: Value) -> MessageType {
        serde_json::from_value(value).unwrap()
    }

    fn image(info: Value) -> MessageType {
        content(json!({
            "msgtype": "m.image",
            "body": "dog.jpg",
            "url": "mxc://example.org/original",
            "info": info
        }))
    }

    fn video(info: Value) -> MessageType {
        content(json!({
            "msgtype": "m.video",
            "body": "dog.mp4",
            "url": "mxc://example.org/original",
            "info": info
        }))
    }

    fn uri(source: Option<MediaSource>) -> Option<OwnedMxcUri> {
        source.as_ref().map(source_uri).cloned()
    }

    #[test]
    fn thumbnails_win() {
        let thumbnail = Some(mxc_uri!("mxc://example.org/thumbnail").to_owned());

        let body = image(json!({ "thumbnail_url": "mxc://example.org/thumbnail" }));
        assert_eq!(uri(picture_source(&body)), thumbnail);

        // even over an original that's too big
        let body = image(json!({
            "size": 10_000_000,
            "thumbnail_url": "mxc://example.org/thumbnail"
        }));
        assert_eq!(uri(picture_source(&body)), thumbnail);

        let body = video(json!({ "thumbnail_url": "mxc://example.org/thumbnail" }));
        assert_eq!(uri(picture_source(&body)), thumbnail);
    }

    #[test]
    fn small_originals_show_without_a_thumbnail() {
        let original = Some(mxc_uri!("mxc://example.org/original").to_owned());

        let body = image(json!({ "size": 1000 }));
        assert_eq!(uri(picture_source(&body)), original);

        let body = image(json!({ "size": 2_097_152 }));
        assert_eq!(uri(picture_source(&body)), original);
    }

    #[test]
    fn big_or_unknown_originals_show_nothing() {
        let body = image(json!({ "size": 2_097_153 }));
        assert_eq!(uri(picture_source(&body)), None);

        let body = image(json!({}));
        assert_eq!(uri(picture_source(&body)), None);

        let body = content(json!({
            "msgtype": "m.image",
            "body": "dog.jpg",
            "url": "mxc://example.org/original"
        }));
        assert_eq!(uri(picture_source(&body)), None);
    }

    #[test]
    fn videos_need_a_thumbnail() {
        let body = video(json!({ "size": 1000 }));
        assert_eq!(uri(picture_source(&body)), None);
    }

    #[test]
    fn other_messages_have_no_picture() {
        for msgtype in ["m.audio", "m.file"] {
            let body = content(json!({
                "msgtype": msgtype,
                "body": "thing",
                "url": "mxc://example.org/original",
                "info": { "size": 1000 }
            }));

            assert_eq!(uri(picture_source(&body)), None);
        }

        let body = content(json!({ "msgtype": "m.text", "body": "hello" }));
        assert_eq!(uri(picture_source(&body)), None);
    }
}
