use log::error;
use matrix_sdk::ruma::api::client::room::get_room_event;
use matrix_sdk::ruma::events::room::message::Relation;
use matrix_sdk::ruma::events::{AnyMessageLikeEvent, MessageLikeEvent};
use matrix_sdk::ruma::exports::serde_json::Value;
use matrix_sdk::ruma::{EventId, MilliSecondsSinceUnixEpoch, UserId};
use matrix_sdk::ruma::{OwnedEventId, OwnedRoomId, events::AnyTimelineEvent};
use std::fs::OpenOptions;
use std::sync::Arc;
use std::{
    collections::HashMap,
    fs,
    io::{BufWriter, Cursor},
    path::PathBuf,
    sync::Mutex,
};

use image::imageops::FilterType;

use super::Matrix;
use crate::settings::{all_muted, is_muted, is_thread_muted, mentions_override_mute};
use crate::widgets::message::Message;
use matrix_sdk::{
    media::MediaFormat,
    room::{Room, RoomMember},
};

pub struct Notify {
    // the notification showing for each room, or for each thread by its
    // root's ID, so clicking one opens the thread
    shown: Arc<Mutex<HashMap<String, u32>>>,
    // newest event timestamp seen in each room
    marks: Mutex<HashMap<OwnedRoomId, MilliSecondsSinceUnixEpoch>>,
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            shown: Arc::new(Mutex::new(HashMap::new())),
            marks: Mutex::new(HashMap::new()),
        }
    }
}

impl Notify {
    pub async fn timeline_event(
        &self,
        matrix: &Matrix,
        event: AnyTimelineEvent,
        sync_started: MilliSecondsSinceUnixEpoch,
    ) -> anyhow::Result<()> {
        let client = matrix.client();

        if let Some(message) = Message::try_from(&event) {
            let me = client.user_id().unwrap();
            let newer_than = self.raise_mark(&event, sync_started);
            let thread = thread_root(&event);

            // a thread mutes on its own, ignoring its room's mute, unless
            // every room is muted and this one wasn't unmuted
            let muted = match thread {
                Some(root) => is_thread_muted(root) || all_muted() && is_muted(event.room_id()),
                None => is_muted(event.room_id()),
            };

            if !should_notify(
                &event,
                me,
                newer_than,
                muted,
                mentions_override_mute(),
                matrix.sees(event.room_id(), thread),
            ) {
                return Ok(());
            }

            let room = client.get_room(event.room_id()).unwrap();

            // a thread only notifies the people in it
            if let Some(root) = thread
                && !mentions_user(&event, me)
                && !participated(&room, root).await?
            {
                return Ok(());
            }

            let user = room
                .get_member(<&UserId>::try_from(message.sender.as_str()).unwrap())
                .await?
                .unwrap();

            let avatar = Notify::get_image(room.clone(), user.clone()).await;
            let body = message.display();

            let thread = thread.map(ToOwned::to_owned);
            self.send_notification(matrix, user.name(), body, room, thread, avatar)?;
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

    // Rooms are visited from Tokio tasks too, where blocking on D-Bus panics.
    // `key` is a room's ID, or a thread root's.
    #[cfg(not(target_os = "macos"))]
    pub fn close_notification(&self, matrix: &Matrix, key: &str) {
        let handle_id = self
            .shown
            .lock()
            .expect("could not lock notifications")
            .remove(key);

        if let Some(handle_id) = handle_id {
            matrix.spawn(async move {
                let notification = notify_rust::Notification::new().id(handle_id).finalize();

                if let Ok(handle) = notification.show_async().await {
                    handle.close_async().await;
                }
            });
        }
    }

    #[cfg(target_os = "macos")]
    pub fn close_notification(&self, _matrix: &Matrix, key: &str) {
        self.shown
            .lock()
            .expect("could not lock notifications")
            .remove(key);
    }

    #[cfg(not(target_os = "macos"))]
    fn send_notification(
        &self,
        matrix: &Matrix,
        summary: &str,
        body: &str,
        room: Room,
        thread: Option<OwnedEventId>,
        image: Option<PathBuf>,
    ) -> anyhow::Result<()> {
        use crate::handler::MatuiEvent;
        use crate::settings::respect_notification_close_reason;
        use log::info;
        use notify_rust::{CloseReason, Hint};

        let mut notification = notify_rust::Notification::new();

        notification.summary(summary).body(body);

        if let Some(path) = image {
            notification.hint(Hint::ImagePath(path.to_str().unwrap().to_string()));
        }

        // the notification showing for the same room or thread is replaced,
        // keeping its close callback
        let key = match &thread {
            Some(root) => root.to_string(),
            None => room.room_id().to_string(),
        };

        let mut map = self.shown.lock().expect("could not lock notifications");
        let mut watch = true; // should we monitor for the close callback?

        if let Some(handle_id) = map.remove(&key) {
            notification.id(handle_id);
            watch = false;
        }

        drop(map);

        // Clone what we need for the async block
        let shown = self.shown.clone();
        let on_close = matrix.clone();

        matrix.spawn(async move {
            match notification.show_async().await {
                Ok(handle) => {
                    let handle_id = handle.id();

                    // Add to the map
                    let mut map = shown.lock().expect("could not lock notifications");
                    map.insert(key, handle_id);
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

                                on_close.send(match &thread {
                                    Some(root) => {
                                        MatuiEvent::ThreadSelected(room.clone(), root.clone())
                                    }
                                    None => MatuiEvent::RoomSelected(room.clone()),
                                });
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
        _matrix: &Matrix,
        summary: &str,
        body: &str,
        _room: Room,
        _thread: Option<OwnedEventId>,
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
// mark from `raise_mark`. `seen` is whether the user is looking at the
// event's room, or its thread if it's in one.
fn should_notify(
    event: &AnyTimelineEvent,
    me: &UserId,
    newer_than: MilliSecondsSinceUnixEpoch,
    muted: bool,
    mentions_override_mute: bool,
    seen: bool,
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

    // not muted rooms or threads, unless the message mentions us
    if muted && !(mentions_override_mute && mentions_user(event, me)) {
        return false;
    }

    // not what we're looking at
    !seen
}

// the root of the thread the event is in, if it's in one
fn thread_root(event: &AnyTimelineEvent) -> Option<&EventId> {
    if let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(
        MessageLikeEvent::Original(c),
    )) = event
        && let Some(Relation::Thread(thread)) = &c.content.relates_to
    {
        return Some(&thread.event_id);
    }

    None
}

// Did we send the thread's root, or a reply in it? The server says so in the
// thread summary it bundles with the root, so this fetches the root fresh.
async fn participated(room: &Room, root: &EventId) -> anyhow::Result<bool> {
    let request = get_room_event::v3::Request::new(room.room_id().to_owned(), root.to_owned());
    let unsigned: Option<Value> = room
        .client()
        .send(request)
        .await?
        .event
        .get_field("unsigned")?;

    Ok(unsigned.is_some_and(|u| u["m.relations"]["m.thread"]["current_user_participated"] == true))
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
    use super::{Notify, mentions_user, participated, should_notify};
    use crate::matrix::{Attention, Matrix};
    use matrix_sdk::ruma::events::AnyTimelineEvent;
    use matrix_sdk::ruma::exports::serde_json::{Value, from_value, json};
    use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, UInt, event_id, room_id, user_id};
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use std::sync::mpsc::channel;
    use tokio::runtime::Handle;
    use wiremock::ResponseTemplate;

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
        assert!(!should_notify(&event, me, started(), false, true, false));

        let event = event_from("@bob:example.org", 1_000, None);
        assert!(!should_notify(&event, me, started(), false, true, false));

        let event = event_from("@bob:example.org", 1_001, None);
        assert!(should_notify(&event, me, started(), false, true, false));
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

    // The timeline visits the room on our own read receipt, from a Tokio
    // task. The test ends before the close it spawns runs, so it doesn't
    // reach D-Bus.
    #[tokio::test]
    async fn it_closes_a_notification_from_a_task() {
        let (sender, _events) = channel();
        let matrix = Matrix::new(sender, Handle::current());
        let server = MatrixMockServer::new().await;
        let client = server.client_builder().build().await;
        let room = server
            .sync_joined_room(&client, room_id!("!room:localhost"))
            .await;

        let shown = matrix.notify.shown.clone();
        shown.lock().unwrap().insert(room.room_id().to_string(), 1);

        matrix.room_visit_event(room);

        assert!(shown.lock().unwrap().is_empty());
    }

    #[test]
    fn it_skips_own_messages() {
        let me = user_id!("@alice:example.org");
        let event = event_from("@alice:example.org", 2_000, None);

        assert!(!should_notify(&event, me, started(), false, true, false));
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

        assert!(!should_notify(&plain, me, started(), true, true, false));
        assert!(should_notify(&mention, me, started(), true, true, false));

        // the override is off
        assert!(!should_notify(&mention, me, started(), true, false, false));
    }

    #[test]
    fn it_skips_what_the_user_sees() {
        let me = user_id!("@alice:example.org");
        let event = event_from("@bob:example.org", 2_000, None);

        assert!(!should_notify(&event, me, started(), false, true, true));
        assert!(should_notify(&event, me, started(), false, true, false));
    }

    #[test]
    fn only_the_open_thread_is_seen_but_the_room_is_seen_under_it() {
        let room = room_id!("!room:example.org");
        let root = event_id!("$root:example.org");
        let mut attention = Attention {
            focused: true,
            room: Some(room.to_owned()),
            thread: None,
        };

        // the room's timeline leaves out its threads
        assert!(attention.sees(room, None));
        assert!(!attention.sees(room, Some(root)));
        assert!(!attention.sees(room_id!("!other:example.org"), None));

        // the room under an open thread counts as read, so as seen
        attention.thread = Some(root.to_owned());
        assert!(attention.sees(room, Some(root)));
        assert!(attention.sees(room, None));
        assert!(!attention.sees(room, Some(event_id!("$other:example.org"))));

        attention.focused = false;
        assert!(!attention.sees(room, Some(root)));
    }

    #[tokio::test]
    async fn it_asks_the_server_whether_we_participated_in_a_thread() {
        let server = MatrixMockServer::new().await;
        let client = server.client_builder().build().await;
        let room = server
            .sync_joined_room(&client, room_id!("!room:localhost"))
            .await;

        let root = |participated: bool| {
            ResponseTemplate::new(200).set_body_json(json!({
                "type": "m.room.message",
                "event_id": "$root:localhost",
                "sender": "@bob:localhost",
                "origin_server_ts": 1_000,
                "room_id": "!room:localhost",
                "content": { "msgtype": "m.text", "body": "root" },
                "unsigned": { "m.relations": { "m.thread": {
                    "count": 1,
                    "current_user_participated": participated,
                }}},
            }))
        };

        server
            .mock_room_event()
            .ok_with_template(root(true))
            .mock_once()
            .mount()
            .await;
        server
            .mock_room_event()
            .ok_with_template(root(false))
            .mock_once()
            .mount()
            .await;

        let id = event_id!("$root:localhost");
        assert!(participated(&room, id).await.unwrap());
        assert!(!participated(&room, id).await.unwrap());
    }
}
