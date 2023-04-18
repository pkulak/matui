use log::error;
use ruma::UserId;
use ruma::{events::AnyTimelineEvent, OwnedRoomId};
use std::fs::OpenOptions;
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Cursor},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use image::imageops::FilterType;

use matrix_sdk::{
    media::MediaFormat,
    room::{Room, RoomMember},
    Client,
};
use notify_rust::{CloseReason, Hint};

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
            if message.sender == *client.user_id().unwrap() {
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
        image: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let mut notification = notify_rust::Notification::new();

        notification.summary(summary).body(body);

        if let Some(path) = image {
            notification.hint(Hint::ImagePath(path.to_str().unwrap().to_string()));
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
                move |_: CloseReason| {
                    if let Room::Joined(joined) = room.clone() {
                        Matrix::send(MatuiEvent::RoomSelected(joined));
                    }
                }
            });
        });

        Ok(())
    }

    fn get_cache_path(key: &str) -> PathBuf {
        let mut path = dirs::cache_dir().expect("no cache directory");
        path.push("matui");
        fs::create_dir_all(&path).unwrap();
        path.push(&key);
        path
    }

    fn write_image_to_file(img: Vec<u8>, path: &PathBuf) -> anyhow::Result<()> {
        let data = Cursor::new(img);
        let reader = image::io::Reader::new(data).with_guessed_format()?;

        let img = reader
            .decode()?
            .resize_to_fill(250, 250, FilterType::Lanczos3);

        let file = OpenOptions::new().create_new(true).write(true).open(path)?;

        img.write_to(&mut BufWriter::new(file), image::ImageOutputFormat::Png)?;

        Ok(())
    }

    async fn get_room_image(room: &Room) -> Option<PathBuf> {
        let path = Notify::get_cache_path(room.room_id().as_str());

        if path.exists() {
            return Some(path);
        }

        let avatar = match room.avatar(MediaFormat::File).await {
            Ok(Some(a)) => a,
            _ => return None,
        };

        if let Err(e) = Notify::write_image_to_file(avatar, &path) {
            error!("could not write image: {}", e);
        }

        return Some(path);
    }

    async fn get_user_image(user: &RoomMember) -> Option<PathBuf> {
        let path = Notify::get_cache_path(user.user_id().as_str());

        if path.exists() {
            return Some(path);
        }

        let avatar = match user.avatar(MediaFormat::File).await {
            Ok(Some(a)) => a,
            _ => return None,
        };

        if let Err(e) = Notify::write_image_to_file(avatar, &path) {
            error!("could not write image: {}", e);
        }

        return Some(path);
    }

    async fn get_image(room: Room, user: RoomMember) -> Option<PathBuf> {
        if let Some(path) = Notify::get_user_image(&user).await {
            return Some(path);
        }

        Notify::get_room_image(&room).await
    }
}
