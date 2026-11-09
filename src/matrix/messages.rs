use std::collections::HashMap;
use std::future::Future;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Sender, TryRecvError, channel};
use std::thread;
use std::time::Duration;

use image::{DynamicImage, ImageReader, Limits};
use log::error;
use matrix_sdk::deserialized_responses::{TimelineEvent, TimelineEventKind};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::room::edit::EditedContent;
use matrix_sdk::room::reply::{EnforceThread, Reply};
use matrix_sdk::room::{Receipts, Room};
use matrix_sdk::ruma::events::room::MediaSource;
use matrix_sdk::ruma::events::room::member::MembershipState;
use matrix_sdk::ruma::events::room::message::MessageType::{self, Audio, File, Image, Video};
use matrix_sdk::ruma::events::room::message::{
    AddMentions, ReplyWithinThread, RoomMessageEventContent, TextMessageEventContent,
};
use matrix_sdk::ruma::events::{AnyMessageLikeEvent, AnyTimelineEvent, MessageLikeEvent};
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId};
use matrix_sdk_ui::Timeline;
use matrix_sdk_ui::timeline::{AttachmentConfig, TimelineEventItemId};

use super::Matrix;
use super::mentions;
use super::mime::mime_from_path;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::Error;
use crate::media::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, decode_upright, get_attachment_info};
use crate::pictures::source_uri;
use crate::spawn::{save_file, view_file};

/// The widest or tallest picture we'll decode, in pixels.
const MAX_PICTURE_SIDE: u32 = 10_000;

/// The most memory decoding a picture may take.
const MAX_PICTURE_ALLOC: u64 = 256 * 1024 * 1024;

/// What should we do with the file after we download it?
pub enum AfterDownload {
    View,
    Save,
}

impl Matrix {
    pub fn download_content(&self, message: MessageType, after: AfterDownload) {
        let matrix = self.clone();

        self.spawn(async move {
            let progress = matrix.progress("Downloading file.", 250);

            let (file_name, mimetype, source) = match message {
                Image(c) => (
                    c.filename().to_string(),
                    c.info.and_then(|i| i.mimetype),
                    c.source,
                ),
                Video(c) => (
                    c.filename().to_string(),
                    c.info.and_then(|i| i.mimetype),
                    c.source,
                ),
                Audio(c) => (
                    c.filename().to_string(),
                    c.info.and_then(|i| i.mimetype),
                    c.source,
                ),
                File(c) => (
                    c.filename().to_string(),
                    c.info.and_then(|i| i.mimetype),
                    c.source,
                ),
                _ => {
                    matrix.send(Error("Unknown file type.".to_string()));
                    return;
                }
            };

            let content_type = mimetype.unwrap_or_else(|| "application/octet-stream".to_string());

            let request = MediaRequestParameters {
                source,
                format: MediaFormat::File,
            };

            let handle = match matrix
                .client()
                .media()
                .get_media_file(&request, None, &content_type.parse().unwrap(), true, None)
                .await
            {
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                    return;
                }
                Ok(mfh) => mfh,
            };

            drop(progress);

            match after {
                AfterDownload::View => {
                    tokio::task::spawn_blocking(move || view_file(handle));
                }
                AfterDownload::Save => match save_file(handle, &file_name).await {
                    Err(err) => matrix.send(Error(err.to_string())),
                    Ok(None) => {}
                    Ok(Some(path)) => matrix.send(MatuiEvent::Confirm(
                        "Download Complete".to_string(),
                        format!("Saved to {}", path.to_str().unwrap()),
                    )),
                },
            };
        });
    }

    /// Download and decode a picture for a room's chat, in the background.
    pub fn fetch_picture(&self, room_id: OwnedRoomId, source: MediaSource) {
        let matrix = self.clone();

        self.spawn(async move {
            let uri = source_uri(&source).clone();

            let request = MediaRequestParameters {
                source,
                format: MediaFormat::File,
            };

            let image = match matrix
                .client()
                .media()
                .get_media_content(&request, true)
                .await
            {
                Ok(bytes) => tokio::task::spawn_blocking(move || decode_picture(&bytes))
                    .await
                    .ok()
                    .flatten(),
                Err(err) => {
                    error!("could not download picture {}: {}", uri, err);
                    None
                }
            };

            matrix.send(MatuiEvent::Picture(room_id, uri, image));
        });
    }

    /// Run a send in the background, showing any error that keeps it from
    /// being queued. Once queued, the timeline shows how it goes.
    fn queue<F>(&self, send: F)
    where
        F: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        let matrix = self.clone();

        self.spawn(async move {
            if let Err(err) = send.await {
                matrix.send(Error(err.to_string()));
            }
        });
    }

    pub fn send_text_message(&self, timeline: Arc<Timeline>, message: String) {
        self.queue(async move {
            let room = timeline.room();
            let content = without_me(room, text_content(room, &message).await);

            timeline.send(content.into()).await?;
            Ok(())
        });
    }

    pub fn send_reply(&self, timeline: Arc<Timeline>, message: String, in_reply_to: OwnedEventId) {
        self.queue(async move {
            let room = timeline.room();
            let content = without_me(room, text_content(room, &message).await);

            timeline.send_reply(content.into(), in_reply_to).await?;
            Ok(())
        });
    }

    pub fn send_thread_message(
        &self,
        timeline: Arc<Timeline>,
        message: String,
        target: OwnedEventId,
        is_reply: ReplyWithinThread,
    ) {
        self.queue(async move {
            let room = timeline.room();
            let content = without_me(room, text_content(room, &message).await);

            // the room's timeline doesn't know about the thread, so add it
            let reply = Reply {
                event_id: target,
                enforce_thread: EnforceThread::Threaded(is_reply),
                add_mentions: AddMentions::Yes,
            };

            let content = room.make_reply_event(content.into(), reply).await?;
            timeline.send(content.into()).await?;
            Ok(())
        });
    }

    pub fn send_attachments(
        &self,
        timeline: Arc<Timeline>,
        attachments: Vec<(PathBuf, Option<String>)>,
    ) {
        self.queue(async move {
            for (path, caption) in attachments {
                let content_type = mime_from_path(&path);

                let mut config = AttachmentConfig {
                    caption: caption.map(TextMessageEventContent::plain),
                    ..Default::default()
                };

                // try to grab a thumbnail
                if let Ok((thumbnail, info)) = get_attachment_info(&path, &content_type) {
                    config.thumbnail = thumbnail;
                    config.info = Some(info);
                }

                timeline
                    .send_attachment(path, content_type, config)
                    .use_send_queue()
                    .await?;
            }

            Ok(())
        });
    }

    /// Delete a message, or stop sending it if it isn't sent yet.
    pub fn redact_event(&self, timeline: Arc<Timeline>, id: TimelineEventItemId) {
        self.queue(async move {
            timeline.redact(&id, None).await?;
            Ok(())
        });
    }

    pub fn deserialize_event(
        event: &TimelineEvent,
        room_id: OwnedRoomId,
    ) -> anyhow::Result<AnyTimelineEvent> {
        match &event.kind {
            TimelineEventKind::Decrypted(decrypted) => Ok(decrypted.event.deserialize()?),
            TimelineEventKind::PlainText { event } => {
                Ok(event.deserialize()?.into_full_event(room_id))
            }
            TimelineEventKind::UnableToDecrypt { event, .. } => {
                Ok(event.deserialize()?.into_full_event(room_id))
            }
        }
    }

    pub fn get_room_message_event(
        room: &Room,
        event: &TimelineEvent,
    ) -> Option<MessageLikeEvent<RoomMessageEventContent>> {
        let Ok(event) = Matrix::deserialize_event(event, room.room_id().to_owned()) else {
            return None;
        };

        let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(event)) = event else {
            return None;
        };

        Some(event)
    }

    pub fn replace_event(&self, timeline: Arc<Timeline>, id: OwnedEventId, message: String) {
        self.queue(async move {
            let room = timeline.room();
            let content = without_me(room, text_content(room, &message).await);
            let id = TimelineEventItemId::EventId(id);

            timeline
                .edit(&id, EditedContent::RoomMessage(content.into()))
                .await?;
            Ok(())
        });
    }

    pub fn read_to(&self, room: Room, to: OwnedEventId) {
        let receipts = Receipts::new()
            .fully_read_marker(Some(to.clone()))
            .public_read_receipt(Some(to));

        self.spawn(async move {
            if let Err(e) = room.send_multiple_receipts(receipts).await {
                error!("could not send read receipt: {}", e);
            }
        });
    }

    pub fn typing_notification(&self, room: Room, typing: bool) {
        self.spawn(async move {
            if let Err(e) = room.typing_notice(typing).await {
                error!("could not send typing notice: {}", e);
            }
        });
    }

    pub fn begin_typing(&self, room: Room) -> Sender<()> {
        let (send, recv) = channel();
        let matrix = self.clone();

        thread::spawn(move || {
            while let Err(TryRecvError::Empty) = recv.try_recv() {
                matrix.typing_notification(room.clone(), true);
                thread::sleep(Duration::from_millis(1000));
            }
        });

        send
    }

    pub fn end_typing(&self, room: Room, send: Sender<()>) {
        send.send(()).expect("could not stop thread");
        self.typing_notification(room, false);
    }
}

/// Decodes a picture, shrinking an original to thumbnail size so it's
/// cheap to draw.
fn decode_picture(bytes: &[u8]) -> Option<DynamicImage> {
    // the sender picks these bytes and their claimed size, so cap what
    // decoding them can cost
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_SIDE);
    limits.max_image_height = Some(MAX_PICTURE_SIDE);
    limits.max_alloc = Some(MAX_PICTURE_ALLOC);

    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.limits(limits);

    let image = match decode_upright(reader) {
        Ok(image) => image,
        Err(err) => {
            error!("could not decode picture: {}", err);
            return None;
        }
    };

    if image.width() <= THUMBNAIL_WIDTH && image.height() <= THUMBNAIL_HEIGHT {
        return Some(image);
    }

    Some(image.thumbnail(THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT))
}

/// Builds a text message from what the user typed, turning the IDs of joined
/// members into mentions.
async fn text_content(room: &Room, message: &str) -> RoomMessageEventContent {
    let mut names = HashMap::new();

    for (_, id) in mentions::find_user_ids(message) {
        if names.contains_key(&id) {
            continue;
        }

        if let Ok(Some(member)) = room.get_member(&id).await
            && *member.membership() == MembershipState::Join
        {
            let name = member.display_name().unwrap_or(id.as_str()).to_string();
            names.insert(id, name);
        }
    }

    mentions::message_content(message, &names)
}

/// Nobody needs to mention themselves, whether by typing their own ID or by
/// replying to their own message.
fn without_me(room: &Room, mut content: RoomMessageEventContent) -> RoomMessageEventContent {
    if let Some(mentions) = &mut content.mentions {
        mentions.user_ids.remove(room.own_user_id());
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, RgbImage};

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(vec![]);

        DynamicImage::ImageRgb8(RgbImage::new(width, height))
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();

        bytes.into_inner()
    }

    #[test]
    fn pictures_too_big_to_decode_fail() {
        assert!(decode_picture(&png(MAX_PICTURE_SIDE, 1)).is_some());
        assert!(decode_picture(&png(MAX_PICTURE_SIDE + 1, 1)).is_none());
        assert!(decode_picture(&png(1, MAX_PICTURE_SIDE + 1)).is_none());
    }
}
