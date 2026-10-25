use log::error;
use matrix_sdk::ruma::events::{AnyMessageLikeEvent, MessageLikeEvent};
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, RoomId, UserId};
use matrix_sdk::ruma::{OwnedRoomId, events::AnyTimelineEvent};
use std::fs::OpenOptions;
use std::sync::Arc;
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Cursor},
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use image::imageops::FilterType;

use crate::settings::{is_muted, mentions_override_mute};
use crate::widgets::message::Message;
use matrix_sdk::{
    Client,
    media::MediaFormat,
    room::{Room, RoomMember},
};

pub struct Notify {
    focus: AtomicBool,
    room_id: Mutex<Option<OwnedRoomId>>,
    rooms: Arc<Mutex<HashMap<String, u32>>>,
    // newest event timestamp seen in each room
    marks: Mutex<HashMap<OwnedRoomId, MilliSecondsSinceUnixEpoch>>,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            focus: AtomicBool::new(false),
            room_id: Mutex::new(None),
            rooms: Arc::new(Mutex::new(HashMap::new())),
            marks: Mutex::new(HashMap::new()),
        }
    }
}

impl Notify {
    pub async fn timeline_event(
        &self,
        client: Client,
        event: AnyTimelineEvent,
        sync_started: MilliSecondsSinceUnixEpoch,
    ) -> anyhow::Result<()> {
        if let Some(message) = Message::try_from(&event, true) {
            let newer_than = self.raise_mark(&event, sync_started);

            {
                let current_room_id = self.room_id.lock().unwrap();

                let focused_room = if self.focus.load(Ordering::Relaxed) {
                    current_room_id.as_deref()
                } else {
                    None
                };

                if !should_notify(
                    &event,
                    client.user_id().unwrap(),
                    newer_than,
                    is_muted(event.room_id()),
                    mentions_override_mute(),
                    focused_room,
                ) {
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

    // Raise the room's mark to this event and return the old mark, starting
    // from `sync_started`. Room subscriptions re-send recent events, so only
    // events newer than the old mark are new.
    fn raise_mark(
        &self,
        event: &AnyTimelineEvent,
        sync_started: MilliSecondsSinceUnixEpoch,
    ) -> MilliSecondsSinceUnixEpoch {
        let mut marks = self.marks.lock().unwrap();
        let mark = marks
            .entry(event.room_id().to_owned())
            .or_insert(sync_started);

        let old = *mark;
        *mark = old.max(event.origin_server_ts());
        old
    }

    pub fn focus_event(&self) {
        self.focus.store(true, Ordering::Relaxed);
    }

    pub fn blur_event(&self) {
        self.focus.store(false, Ordering::Relaxed);
    }

    pub fn room_visit_event(&self, room: Room) {
        self.close_notification(room.room_id().as_str());
        *self.room_id.lock().unwrap() = Some(room.room_id().to_owned());
    }

    #[cfg(not(target_os = "macos"))]
    fn close_notification(&self, room_id: &str) {
        let mut map = self.rooms.lock().expect("could not lock rooms");
        if let Some(handle_id) = map.remove(room_id)
            && let Ok(handle) = notify_rust::Notification::new().id(handle_id).show()
        {
            handle.close();
        }
    }

    #[cfg(target_os = "macos")]
    fn close_notification(&self, room_id: &str) {
        self.rooms
            .lock()
            .expect("could not lock rooms")
            .remove(room_id);
    }

    #[cfg(not(target_os = "macos"))]
    fn send_notification(
        &self,
        summary: &str,
        body: &str,
        room: Room,
        image: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        use crate::app::App;
        use crate::handler::MatuiEvent;
        use crate::settings::respect_notification_close_reason;
        use log::info;
        use notify_rust::{CloseReason, Hint};

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

        drop(map);

        // Clone what we need for the async block
        let room_id = room.room_id().to_string();
        let rooms = self.rooms.clone();

        App::spawn(async move {
            match notification.show_async().await {
                Ok(handle) => {
                    let handle_id = handle.id();

                    // Add to the map
                    let mut map = rooms.lock().expect("could not lock rooms");
                    map.insert(room_id, handle_id);
                    drop(map); // Release lock

                    if watch {
                        std::thread::spawn(move || {
                            handle.on_close(move |reason: CloseReason| {
                                info!("close reason {:?}", reason);

                                if respect_notification_close_reason()
                                    && !matches!(reason, CloseReason::CloseAction)
                                {
                                    return;
                                }

                                App::send(MatuiEvent::RoomSelected(room.clone()));
                            });
                        });
                    }
                }
                Err(e) => {
                    error!("Failed to show notification: {}", e);
                }
            }
        });

        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn send_notification(
        &self,
        summary: &str,
        body: &str,
        _room: Room,
        image: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        let mut notification = notify_rust::Notification::new();
        notification.summary(summary).body(body);

        if let Some(path) = image {
            notification.image_path(path.to_str().unwrap());
        }

        notification.show()?;
        Ok(())
    }

    fn get_cache_path(key: &str) -> PathBuf {
        let mut path = dirs::cache_dir().expect("no cache directory");
        path.push("matui");
        fs::create_dir_all(&path).unwrap();
        path.push(key);
        path
    }

    fn write_image_to_file(img: Vec<u8>, path: &PathBuf) -> anyhow::Result<()> {
        let data = Cursor::new(img);
        let reader = image::ImageReader::new(data).with_guessed_format()?;

        let img = reader
            .decode()?
            .resize_to_fill(250, 250, FilterType::Lanczos3);

        let file = OpenOptions::new().create_new(true).write(true).open(path)?;

        img.write_to(&mut BufWriter::new(file), image::ImageFormat::Png)?;

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
            return None;
        }

        Some(path)
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
            return None;
        }

        Some(path)
    }

    async fn get_image(room: Room, user: RoomMember) -> Option<PathBuf> {
        if let Some(path) = Notify::get_user_image(&user).await {
            return Some(path);
        }

        Notify::get_room_image(&room).await
    }
}

// Should this event send a desktop notification? `newer_than` is the room's
// mark from `raise_mark`. `focused_room` is the room on screen, or None when
// Matui doesn't have focus.
fn should_notify(
    event: &AnyTimelineEvent,
    me: &UserId,
    newer_than: MilliSecondsSinceUnixEpoch,
    muted: bool,
    mentions_override_mute: bool,
    focused_room: Option<&RoomId>,
) -> bool {
    // only messages we haven't seen; the room list and room subscriptions
    // also deliver older events
    if event.origin_server_ts() <= newer_than {
        return false;
    }

    // not our own messages
    if event.sender() == me {
        return false;
    }

    // not muted rooms, unless the message mentions us
    if muted && !(mentions_override_mute && mentions_user(event, me)) {
        return false;
    }

    // not the room we're looking at
    focused_room != Some(event.room_id())
}

// does the event intentionally mention the given user (or the whole room)?
fn mentions_user(event: &AnyTimelineEvent, me: &UserId) -> bool {
    if let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(
        MessageLikeEvent::Original(c),
    )) = event
        && let Some(mentions) = &c.content.mentions
    {
        return mentions.room || mentions.user_ids.contains(me);
    }

    false
}

#[cfg(test)]
mod tests {
    use super::{Notify, mentions_user, should_notify};
    use matrix_sdk::ruma::events::AnyTimelineEvent;
    use matrix_sdk::ruma::exports::serde_json::{Value, from_value, json};
    use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, UInt, room_id, user_id};

    fn message_event(mentions: Option<Value>) -> AnyTimelineEvent {
        event_from("@bob:example.org", 1_000, mentions)
    }

    fn event_from(sender: &str, ts: u64, mentions: Option<Value>) -> AnyTimelineEvent {
        let mut content = json!({ "msgtype": "m.text", "body": "hey" });

        if let Some(m) = mentions {
            content["m.mentions"] = m;
        }

        from_value(json!({
            "type": "m.room.message",
            "event_id": "$event:example.org",
            "sender": sender,
            "origin_server_ts": ts,
            "room_id": "!room:example.org",
            "content": content,
        }))
        .unwrap()
    }

    #[test]
    fn it_finds_mentions() {
        let me = user_id!("@alice:example.org");

        let event = message_event(Some(json!({ "user_ids": ["@alice:example.org"] })));
        assert!(mentions_user(&event, me));

        let event = message_event(Some(json!({ "user_ids": ["@carol:example.org"] })));
        assert!(!mentions_user(&event, me));

        let event = message_event(Some(json!({ "room": true })));
        assert!(mentions_user(&event, me));

        let event = message_event(None);
        assert!(!mentions_user(&event, me));
    }

    fn started() -> MilliSecondsSinceUnixEpoch {
        MilliSecondsSinceUnixEpoch(UInt::from(1_000u32))
    }

    #[test]
    fn it_only_notifies_after_startup() {
        let me = user_id!("@alice:example.org");

        let event = event_from("@bob:example.org", 999, None);
        assert!(!should_notify(&event, me, started(), false, true, None));

        let event = event_from("@bob:example.org", 1_000, None);
        assert!(!should_notify(&event, me, started(), false, true, None));

        let event = event_from("@bob:example.org", 1_001, None);
        assert!(should_notify(&event, me, started(), false, true, None));
    }

    #[test]
    fn it_raises_the_mark_once_per_event() {
        let notify = Notify::default();
        let event = event_from("@bob:example.org", 2_000, None);
        let older = event_from("@bob:example.org", 1_500, None);

        assert_eq!(notify.raise_mark(&event, started()), started());

        // a subscription re-sends the event, and an older one
        let mark = MilliSecondsSinceUnixEpoch(UInt::from(2_000u32));
        assert_eq!(notify.raise_mark(&event, started()), mark);
        assert_eq!(notify.raise_mark(&older, started()), mark);
    }

    #[test]
    fn it_skips_own_messages() {
        let me = user_id!("@alice:example.org");
        let event = event_from("@alice:example.org", 2_000, None);

        assert!(!should_notify(&event, me, started(), false, true, None));
    }

    #[test]
    fn it_skips_muted_rooms_unless_mentioned() {
        let me = user_id!("@alice:example.org");
        let plain = event_from("@bob:example.org", 2_000, None);
        let mention = event_from(
            "@bob:example.org",
            2_000,
            Some(json!({ "user_ids": ["@alice:example.org"] })),
        );

        assert!(!should_notify(&plain, me, started(), true, true, None));
        assert!(should_notify(&mention, me, started(), true, true, None));

        // the override is off
        assert!(!should_notify(&mention, me, started(), true, false, None));
    }

    #[test]
    fn it_skips_the_focused_room() {
        let me = user_id!("@alice:example.org");
        let event = event_from("@bob:example.org", 2_000, None);
        let this_room = Some(room_id!("!room:example.org"));
        let other_room = Some(room_id!("!other:example.org"));

        assert!(!should_notify(
            &event,
            me,
            started(),
            false,
            true,
            this_room
        ));
        assert!(should_notify(
            &event,
            me,
            started(),
            false,
            true,
            other_room
        ));
    }
}
