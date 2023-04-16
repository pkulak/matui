use ruma::OwnedRoomId;
use std::{
    collections::HashMap,
    io::Cursor,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use image::imageops::FilterType;
use log::error;
use matrix_sdk::{
    media::{MediaFormat, MediaThumbnailSize},
    room::{Room, RoomMember},
    Client,
};
use notify_rust::{CloseReason, Hint};
use ruma::{
    api::client::media::get_content_thumbnail::v3::Method, events::AnyTimelineEvent, UInt, UserId,
};

use crate::{handler::MatuiEvent, settings::is_muted, widgets::message::Message};

use super::matrix::Matrix;

pub struct Notify {
    focus: AtomicBool,
    room_id: Mutex<Option<OwnedRoomId>>,
    rooms: Mutex<HashMap<String, u32>>,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            focus: AtomicBool::new(false),
            room_id: Mutex::new(None),
            rooms: Mutex::new(HashMap::new()),
        }
    }
}

impl Notify {
    pub async fn timeline_event(
        &self,
        client: Client,
        event: AnyTimelineEvent,
    ) -> anyhow::Result<()> {
        if let Some(message) = Message::try_from(&event) {
            // don't send notifications for our own messages
            if message.sender == client.user_id().unwrap().to_string() {
                return Ok(());
            }

            // or when the room is muted
            if is_muted(message.room_id.as_ref()) {
                return Ok(());
            }

            {
                // don't do anything if the app is focused on our room
                let current_room_id = self.room_id.lock().unwrap();

                if self.focus.load(Ordering::Relaxed)
                    && (*current_room_id).as_ref() == Some(&message.room_id)
                {
                    return Ok(());
                }
            }

            let room = client.get_room(&message.room_id).unwrap();

            let user = room
                .get_member(<&UserId>::try_from(message.sender.as_str()).unwrap())
                .await?
                .unwrap();

            let avatar = Notify::get_image(room.clone(), user.clone()).await;
            let body = message.display();

            self.send_notification(user.name(), body, room, avatar)?;
        }

        Ok(())
    }

    pub fn focus_event(&self) {
        self.focus.store(true, Ordering::Relaxed);
    }

    pub fn blur_event(&self) {
        self.focus.store(false, Ordering::Relaxed);
    }

    pub fn room_visit_event(&self, room: Room) {
        let mut map = self.rooms.lock().expect("could not lock rooms");

        if let Some(handle_id) = map.remove(room.room_id().as_str()) {
            if let Ok(handle) = notify_rust::Notification::new().id(handle_id).show() {
                handle.close();
            }
        }

        *self.room_id.lock().unwrap() = Some(room.room_id().to_owned());
    }

    fn send_notification(
        &self,
        summary: &str,
        body: &str,
        room: Room,
        image: Option<Vec<u8>>,
    ) -> anyhow::Result<()> {
        let mut notification = notify_rust::Notification::new();

        notification.summary(summary).body(body);

        if let Some(img) = image {
            let data = Cursor::new(img);
            let reader = image::io::Reader::new(data).with_guessed_format()?;

            let img = reader
                .decode()?
                .resize_to_fill(250, 250, FilterType::Lanczos3);

            notification.hint(Hint::ImageData(notify_rust::Image::try_from(img)?));
        }

        let mut map = self.rooms.lock().expect("could not lock rooms");
        let mut watch = true; // should we monitor for the close callback?

        if let Some(handle_id) = map.remove(room.room_id().as_str()) {
            notification.id(handle_id);
            watch = false;
        }

        let handle = notification.show()?;
        let handle_id = handle.id();

        map.insert(room.room_id().to_string(), handle_id);

        if !watch {
            return Ok(());
        }

        // spawn a thread to sit around and wait for the notification to close
        std::thread::spawn(move || {
            handle.on_close({
                let room = room.clone();

                move |_: CloseReason| {
                    if let Room::Joined(joined) = room.clone() {
                        Matrix::send(MatuiEvent::RoomSelected(joined));
                    }
                }
            });
        });

        Ok(())
    }

    async fn get_image(room: Room, user: RoomMember) -> Option<Vec<u8>> {
        let format = MediaFormat::Thumbnail(MediaThumbnailSize {
            method: Method::Scale,
            width: UInt::new(250).unwrap(),
            height: UInt::new(250).unwrap(),
        });

        let mut avatar = match room.avatar(format).await {
            Ok(a) => a,
            Err(e) => {
                error!("could not fetch room avatar: {}", e.to_string());
                None
            }
        };

        if avatar.is_none() {
            avatar = match user.avatar(MediaFormat::File).await {
                Ok(a) => a,
                Err(e) => {
                    error!("Could not fetch user avatar: {}", e.to_string());
                    None
                }
            };
        }

        avatar
    }
}
