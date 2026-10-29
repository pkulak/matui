use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, TryRecvError, channel};
use std::time::Duration;
use std::{fs, thread};

use log::{error, info};
use matrix_sdk::attachment::AttachmentConfig;
use matrix_sdk::deserialized_responses::{TimelineEvent, TimelineEventKind};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::room::{MessagesOptions, Receipts, Room};
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::member::MembershipState;
use matrix_sdk::ruma::events::room::message::MessageType::{self, Audio, File, Image, Video};
use matrix_sdk::ruma::events::room::message::{
    AddMentions, ForwardThread, ReplyWithinThread, RoomMessageEventContent, TextMessageEventContent,
};
use matrix_sdk::ruma::events::{AnyMessageLikeEvent, AnyTimelineEvent, MessageLikeEvent};
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, UInt};

use super::Matrix;
use super::mentions;
use super::mime::mime_from_path;
use crate::app::App;
use crate::handler::MatuiEvent::{Error, ProgressComplete, ProgressStarted};
use crate::handler::{Batch, MatuiEvent};
use crate::media::get_attachment_info;
use crate::spawn::{save_file, view_file};

/// What should we do with the file after we download it?
pub enum AfterDownload {
    View,
    Save,
}

impl Matrix {
    pub fn fetch_messages(&self, room: Room, cursor: Option<String>, limit: usize) {
        App::spawn(async move {
            // fetch the actual messages
            let mut options = MessagesOptions::new(Direction::Backward);
            options.limit =
                UInt::new(limit.try_into().unwrap_or(25)).expect("invalid paging limit");
            options.from = cursor;

            let messages = match room.messages(options).await {
                Ok(msg) => msg,
                Err(err) => {
                    App::send(Error(err.to_string()));
                    return;
                }
            };

            let unpacked: Vec<AnyTimelineEvent> = messages
                .chunk
                .iter()
                .filter_map(|te| {
                    Matrix::deserialize_event(te, room.room_id().into())
                        .map_err(|err| error!("could not deserialize event: {}", err))
                        .ok()
                })
                .collect();

            let batch = Batch {
                room: room.clone(),
                events: unpacked,
                cursor: messages.end,
            };

            App::send(MatuiEvent::TimelineBatch(batch));
        });
    }

    pub fn download_content(&self, message: MessageType, after: AfterDownload) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Downloading file.".to_string(), 250));

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
                    App::send(Error("Unknown file type.".to_string()));
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
                    App::send(Error(err.to_string()));
                    return;
                }
                Ok(mfh) => mfh,
            };

            App::send(ProgressComplete);

            match after {
                AfterDownload::View => {
                    tokio::task::spawn_blocking(move || view_file(handle));
                }
                AfterDownload::Save => match save_file(handle, &file_name).await {
                    Err(err) => App::send(Error(err.to_string())),
                    Ok(None) => {}
                    Ok(Some(path)) => App::send(MatuiEvent::Confirm(
                        "Download Complete".to_string(),
                        format!("Saved to {}", path.to_str().unwrap()),
                    )),
                },
            };
        });
    }

    pub fn send_text_message(&self, room: Room, message: String) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            let content = text_content(&room, &message).await;

            if let Err(err) = room.send(without_me(&room, content)).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_reply(&self, room: Room, message: String, in_reply_to: OwnedEventId) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            let in_reply_to = match Matrix::get_room_event(&room, &in_reply_to).await {
                Some(e) => e,
                None => {
                    App::send(Error("Could not find reply event.".to_string()));
                    return;
                }
            };

            let Some(og_in_reply_to) = in_reply_to.as_original() else {
                return;
            };

            let reply = text_content(&room, &message).await.make_reply_to(
                og_in_reply_to,
                ForwardThread::Yes,
                AddMentions::Yes,
            );

            if let Err(err) = room.send(without_me(&room, reply)).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_thread_message(
        &self,
        room: Room,
        message: String,
        target: OwnedEventId,
        is_reply: ReplyWithinThread,
    ) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            let target = match Matrix::get_room_event(&room, &target).await {
                Some(e) => e,
                None => {
                    App::send(Error("Could not find thread event.".to_string()));
                    return;
                }
            };

            let Some(og_target) = target.as_original() else {
                return;
            };

            let reply = text_content(&room, &message).await.make_for_thread(
                og_target,
                is_reply,
                AddMentions::Yes,
            );

            if let Err(err) = room.send(without_me(&room, reply)).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_attachments(&self, room: Room, attachments: Vec<(PathBuf, Option<String>)>) {
        let total = attachments.len();

        App::spawn(async move {
            for (i, (path, caption)) in attachments.into_iter().enumerate() {
                App::send(ProgressStarted(
                    format!("Uploading {} of {}.", i + 1, total),
                    0,
                ));

                let content_type = mime_from_path(&path);

                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default()
                    .to_string();

                let data = match fs::read(path.to_str().unwrap()) {
                    Ok(d) => d,
                    Err(err) => {
                        App::send(Error(err.to_string()));
                        return;
                    }
                };

                // try to grab a thumbnail
                let config = match get_attachment_info(&path, &content_type) {
                    Ok((thumbnail, info)) => {
                        AttachmentConfig::new().thumbnail(thumbnail).info(info)
                    }
                    _ => AttachmentConfig::new(),
                }
                .caption(caption.map(TextMessageEventContent::plain));

                if let Err(err) = room
                    .send_attachment(&name, &content_type, data, config)
                    .await
                {
                    App::send(Error(err.to_string()));
                }

                App::send(ProgressComplete);
            }
        });
    }

    pub fn send_reaction(&self, room: Room, event_id: OwnedEventId, key: String) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending reaction.".to_string(), 500));

            if let Err(err) = room
                .send(ReactionEventContent::new(Annotation::new(event_id, key)))
                .await
            {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn redact_event(&self, room: Room, event_id: OwnedEventId) {
        App::spawn(async move {
            App::send(ProgressStarted("Removing.".to_string(), 500));

            if let Err(err) = room.redact(&event_id, None, None).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    async fn get_room_event(
        room: &Room,
        id: &OwnedEventId,
    ) -> Option<MessageLikeEvent<RoomMessageEventContent>> {
        match room.event(id, Option::None).await {
            Ok(event) => Matrix::get_room_message_event(room, &event),
            Err(err) => {
                error!("could not get room event: {}", err);
                None
            }
        }
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

    pub fn replace_event(
        &self,
        room: Room,
        id: OwnedEventId,
        message: String,
        in_reply_to: Option<OwnedEventId>,
    ) {
        App::spawn(async move {
            App::send(ProgressStarted("Editing message.".to_string(), 500));

            let Some(event) = Matrix::get_room_event(&room, &id).await else {
                return;
            };

            let Some(event) = event.as_original() else {
                return;
            };

            let reply_event = match in_reply_to {
                Some(id) => Matrix::get_room_event(&room, &id).await,
                None => None,
            };

            let reply_event = match reply_event {
                Some(e) => e.as_original().cloned(),
                None => None,
            };

            info!("reply event: {:?}", reply_event);

            let content = without_me(&room, text_content(&room, &message).await);

            if let Err(err) = room.send(content.make_replacement(event)).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn read_to(&self, room: Room, to: OwnedEventId) {
        let receipts = Receipts::new()
            .fully_read_marker(Some(to.clone()))
            .public_read_receipt(Some(to));

        App::spawn(async move {
            if let Err(e) = room.send_multiple_receipts(receipts).await {
                error!("could not send read receipt: {}", e);
            }
        });
    }

    pub fn typing_notification(&self, room: Room, typing: bool) {
        App::spawn(async move {
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
