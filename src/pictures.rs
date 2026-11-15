use crate::media::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH};
use crate::settings::images;
use image::{DynamicImage, ImageFormat, imageops};
use log::{info, warn};
use matrix_sdk::ruma::events::room::message::MessageType::{self, Image, Video};
use matrix_sdk::ruma::events::room::{MediaSource, ThumbnailInfo};
use matrix_sdk::ruma::{OwnedMxcUri, UInt};
use ratatui::layout::Size;
use ratatui_image::errors::Errors;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::sliced::SlicedProtocol;
use ratatui_image::{FilterType, FontSize, Resize};
use std::collections::HashMap;
use std::sync::OnceLock;

// the tallest a picture gets, in rows
const MAX_ROWS: u16 = 10;

// originals bigger than this are too much to download just to look at
const MAX_ORIGINAL_SIZE: u64 = 2 * 1024 * 1024;

const PLAY_BUTTON: &[u8] = include_bytes!("../assets/play-button.png");

static PICKER: OnceLock<Option<Picker>> = OnceLock::new();

/// Asks the terminal what it can draw, unless the settings turn images off.
/// Call this before anything else reads stdin.
pub fn init_picker() {
    let _ = PICKER.set(choose_picker(images(), Picker::from_query_stdio));
}

// Pictures are drawn with Kitty's placeholders, or halfblocks anywhere else.
// Popups can cover both, since they're just text cells, but Sixel and iTerm2
// draw over popups.
fn choose_picker(images: bool, query: impl FnOnce() -> Result<Picker, Errors>) -> Option<Picker> {
    if !images {
        return None;
    }

    match query() {
        Ok(p) if p.protocol_type() == ProtocolType::Kitty => Some(p),
        Ok(mut p) => {
            info!("halfblock pictures: {:?} terminal", p.protocol_type());
            p.set_protocol_type(ProtocolType::Halfblocks);
            Some(p)
        }
        Err(err) => {
            warn!("pictures off: could not query the terminal: {}", err);
            None
        }
    }
}

pub fn picker() -> Option<&'static Picker> {
    PICKER.get().and_then(Option::as_ref)
}

/// A picture to show for a message.
#[derive(Clone)]
pub struct PictureSource {
    pub media: MediaSource,
    // its width and height in pixels, if the event says
    pub dimensions: Option<(u32, u32)>,
    // a video's thumbnail, which gets a play button
    pub video: bool,
}

/// Which picture, if any, to show for a message.
pub fn picture_source(body: &MessageType) -> Option<PictureSource> {
    match body {
        Image(content) => {
            let info = content.info.as_deref()?;

            if let Some(thumbnail) = info.thumbnail_source.clone() {
                return Some(thumbnail_source(thumbnail, info.thumbnail_info.as_deref()));
            }

            let dimensions = dimensions(info.width, info.height);

            let small = match info.size {
                Some(size) => u64::from(size) <= MAX_ORIGINAL_SIZE,
                // no bigger than a thumbnail, as stickers often are
                None => {
                    dimensions.is_some_and(|(w, h)| w <= THUMBNAIL_WIDTH && h <= THUMBNAIL_HEIGHT)
                }
            };

            small.then(|| PictureSource {
                media: content.source.clone(),
                dimensions,
                video: false,
            })
        }
        Video(content) => {
            let info = content.info.as_deref()?;
            let thumbnail = info.thumbnail_source.clone()?;

            Some(PictureSource {
                video: true,
                ..thumbnail_source(thumbnail, info.thumbnail_info.as_deref())
            })
        }
        _ => None,
    }
}

fn thumbnail_source(media: MediaSource, info: Option<&ThumbnailInfo>) -> PictureSource {
    PictureSource {
        media,
        dimensions: info.and_then(|i| dimensions(i.width, i.height)),
        video: false,
    }
}

/// Puts a play button in the middle of a video's thumbnail, a third as big
/// as its shorter side, so it still shows in halfblocks.
pub fn add_play_button(image: DynamicImage) -> DynamicImage {
    let button = image::load_from_memory_with_format(PLAY_BUTTON, ImageFormat::Png)
        .expect("the play button is a PNG");

    let side = (image.width().min(image.height()) / 3).max(1);
    let button = imageops::resize(&button, side, side, FilterType::Lanczos3);

    let mut canvas = image.into_rgba8();
    let x = (canvas.width() - side) / 2;
    let y = (canvas.height() - side) / 2;
    imageops::overlay(&mut canvas, &button, x.into(), y.into());

    DynamicImage::ImageRgba8(canvas)
}

fn dimensions(width: Option<UInt>, height: Option<UInt>) -> Option<(u32, u32)> {
    Some((width?.try_into().ok()?, height?.try_into().ok()?))
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

    /// The rows and columns a picture takes, within the given width. The
    /// event's dimensions give its size before it's ready, and keep it the
    /// same after, so the timeline doesn't jump. Without them, there's no
    /// size until it's ready.
    pub fn size(&self, source: &PictureSource, picker: &Picker, width: u16) -> Option<Size> {
        let dimensions = match self.entries.get(source_uri(&source.media)) {
            Some(Picture::Failed) => return None,
            Some(Picture::Ready { image, .. }) => {
                source.dimensions.unwrap_or((image.width(), image.height()))
            }
            _ => source.dimensions?,
        };

        fit(dimensions, picker.font_size(), Size::new(width, MAX_ROWS))
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
            let resize = Resize::Fit(Some(FilterType::Lanczos3));

            match SlicedProtocol::new_with_resize(picker, image.clone(), size, resize) {
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

// the cells a picture fills when shrunk to fit the available ones, keeping
// its shape, the way Resize::Fit sizes it
fn fit((width, height): (u32, u32), font: FontSize, available: Size) -> Option<Size> {
    if width == 0 || height == 0 {
        return None;
    }

    let (width, height) = (f64::from(width), f64::from(height));

    // never enlarged
    let max_width = width.min(f64::from(available.width) * f64::from(font.width));
    let max_height = height.min(f64::from(available.height) * f64::from(font.height));
    let ratio = f64::min(max_width / width, max_height / height);

    let cells = |pixels: f64, cell: u16| {
        ((pixels * ratio).round().max(1.0) / f64::from(cell)).ceil() as u16
    };

    Some(Size::new(
        cells(width, font.width),
        cells(height, font.height),
    ))
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

    fn uri(source: Option<PictureSource>) -> Option<OwnedMxcUri> {
        source.map(|s| source_uri(&s.media).clone())
    }

    fn answering(protocol: ProtocolType) -> impl FnOnce() -> Result<Picker, Errors> {
        move || {
            let mut picker = Picker::halfblocks();
            picker.set_protocol_type(protocol);
            Ok(picker)
        }
    }

    fn chosen(picker: Option<Picker>) -> Option<ProtocolType> {
        picker.map(|p| p.protocol_type())
    }

    #[test]
    fn kitty_terminals_draw_with_kitty() {
        let picker = choose_picker(true, answering(ProtocolType::Kitty));
        assert_eq!(chosen(picker), Some(ProtocolType::Kitty));
    }

    #[test]
    fn other_terminals_draw_with_halfblocks() {
        for protocol in [
            ProtocolType::Sixel,
            ProtocolType::Iterm2,
            ProtocolType::Halfblocks,
        ] {
            let picker = choose_picker(true, answering(protocol));
            assert_eq!(chosen(picker), Some(ProtocolType::Halfblocks));
        }
    }

    #[test]
    fn query_errors_turn_pictures_off() {
        let picker = choose_picker(true, || Err(Errors::Tmux("no")));
        assert_eq!(chosen(picker), None);
    }

    #[test]
    fn the_setting_turns_pictures_off_without_asking() {
        let picker = choose_picker(false, || panic!("queried the terminal"));
        assert_eq!(chosen(picker), None);
    }

    fn dimensions_of(body: MessageType) -> Option<(u32, u32)> {
        picture_source(&body).unwrap().dimensions
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

        // or, without a size, no bigger than a thumbnail
        let body = image(json!({ "w": 800, "h": 600 }));
        assert_eq!(uri(picture_source(&body)), original);
    }

    #[test]
    fn big_or_unknown_originals_show_nothing() {
        let body = image(json!({ "size": 2_097_153 }));
        assert_eq!(uri(picture_source(&body)), None);

        let body = image(json!({}));
        assert_eq!(uri(picture_source(&body)), None);

        let body = image(json!({ "w": 801, "h": 600 }));
        assert_eq!(uri(picture_source(&body)), None);

        // a size over the limit counts, however small it looks
        let body = image(json!({ "size": 2_097_153, "w": 100, "h": 100 }));
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
    fn only_videos_get_a_play_button() {
        let thumbnail = json!({ "size": 1000, "thumbnail_url": "mxc://example.org/thumbnail" });

        assert!(picture_source(&video(thumbnail.clone())).unwrap().video);
        assert!(!picture_source(&image(thumbnail)).unwrap().video);
        assert!(
            !picture_source(&image(json!({ "size": 1000 })))
                .unwrap()
                .video
        );
    }

    #[test]
    fn the_play_button_goes_in_the_middle() {
        let image = add_play_button(DynamicImage::new_rgb8(300, 200)).into_rgba8();

        assert_eq!((image.width(), image.height()), (300, 200));
        assert_eq!(image.get_pixel(0, 0).0, [0, 0, 0, 255]);
        assert!(image.get_pixel(150, 100).0[0] > 200);
    }

    #[test]
    fn dimensions_come_from_the_info_for_the_source() {
        let both = json!({
            "size": 1000,
            "w": 4000,
            "h": 3000,
            "thumbnail_url": "mxc://example.org/thumbnail",
            "thumbnail_info": { "w": 800, "h": 600 }
        });

        assert_eq!(dimensions_of(image(both.clone())), Some((800, 600)));
        assert_eq!(dimensions_of(video(both)), Some((800, 600)));

        let original = image(json!({ "size": 1000, "w": 400, "h": 300 }));
        assert_eq!(dimensions_of(original), Some((400, 300)));

        // a thumbnail without its own dimensions doesn't borrow the original's
        let body = image(json!({
            "w": 4000,
            "h": 3000,
            "thumbnail_url": "mxc://example.org/thumbnail"
        }));
        assert_eq!(dimensions_of(body), None);

        // and both are needed
        let body = image(json!({ "size": 1000, "w": 400 }));
        assert_eq!(dimensions_of(body), None);
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
