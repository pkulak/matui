use std::collections::HashSet;
use std::sync::Mutex;

use futures::future::join_all;
use futures::{StreamExt, pin_mut};
use log::info;
use matrix_sdk::latest_events::LatestEventValue;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedRoomId, RoomId};
use matrix_sdk::{RoomDisplayName, RoomRecencyStamp};
use matrix_sdk_ui::eyeball_im::{Vector, VectorDiff};
use matrix_sdk_ui::room_list_service::filters::new_filter_joined;
use matrix_sdk_ui::room_list_service::{RoomList, RoomListItem};
use tokio::sync::oneshot;

use crate::matrix::matrix::Matrix;

#[derive(Default)]
pub struct RoomCache {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    /// The room list's entries, in its order, so its diffs apply.
    listed: Vector<OwnedRoomId>,
    rooms: Vec<DecoratedRoom>,
}

impl RoomCache {
    /// Follow the joined rooms in the room list forever. `loaded` fires once
    /// the cache holds every room the list had when this started.
    pub async fn follow(&self, all_rooms: &RoomList, loaded: oneshot::Sender<()>) {
        // the whole list in one page, since the switcher shows it all
        let (entries, controller) = all_rooms.entries_with_dynamic_adapters(usize::MAX);
        controller.set_filter(Box::new(new_filter_joined()));
        pin_mut!(entries);

        // the stream always starts with a reset of the current rooms
        if let Some(diffs) = entries.next().await {
            self.apply(diffs).await;
        }

        info!("room cache populated");
        let _ = loaded.send(());

        while let Some(diffs) = entries.next().await {
            self.apply(diffs).await;
        }
    }

    async fn apply(&self, diffs: Vec<VectorDiff<RoomListItem>>) {
        for diff in diffs {
            let added = join_all(rooms_in(&diff).into_iter().map(DecoratedRoom::from_room)).await;

            let mut inner = self.inner.lock().expect("to unlock rooms");
            let before: HashSet<OwnedRoomId> = inner.listed.iter().cloned().collect();
            diff.map(|room| room.room_id().to_owned())
                .apply(&mut inner.listed);
            let after: HashSet<OwnedRoomId> = inner.listed.iter().cloned().collect();

            // keep rooms added by add_room until the list has them
            inner
                .rooms
                .retain(|r| after.contains(r.room_id()) || !before.contains(r.room_id()));

            for room in added {
                match inner
                    .rooms
                    .iter_mut()
                    .find(|r| r.room_id() == room.room_id())
                {
                    Some(existing) => *existing = room,
                    None => inner.rooms.push(room),
                }
            }
        }
    }

    /// Add a room we just joined or created, before the room list has it.
    pub async fn add_room(&self, room: Room) {
        let decorated = DecoratedRoom::from_room(room).await;
        let mut inner = self.inner.lock().expect("to unlock rooms");

        // the room list may have already added it
        if !inner
            .rooms
            .iter()
            .any(|r| r.room_id() == decorated.room_id())
        {
            inner.rooms.insert(0, decorated);
        }
    }

    /// Drop a room we just left, before the room list does.
    pub fn remove_room(&self, room: &Room) {
        let mut inner = self.inner.lock().expect("to unlock rooms");
        inner.rooms.retain(|r| r.room_id() != room.room_id());
    }

    pub fn get_rooms(&self) -> Vec<DecoratedRoom> {
        self.inner.lock().expect("to unlock rooms").rooms.clone()
    }

    pub fn wrap(&self, room: &Room) -> Option<DecoratedRoom> {
        let inner = self.inner.lock().expect("to unlock rooms");

        inner
            .rooms
            .iter()
            .find(|r| r.room_id() == room.room_id())
            .cloned()
    }

    pub fn room_visit_event(&self, room: Room) {
        let mut inner = self.inner.lock().expect("to unlock rooms");

        if let Some(dec) = inner
            .rooms
            .iter_mut()
            .find(|r| r.room_id() == room.room_id())
        {
            dec.visited = true;
        }
    }
}

/// The rooms a diff adds or replaces.
fn rooms_in(diff: &VectorDiff<RoomListItem>) -> Vec<Room> {
    match diff {
        VectorDiff::Append { values } | VectorDiff::Reset { values } => {
            values.iter().map(|r| Room::clone(r)).collect()
        }
        VectorDiff::PushFront { value }
        | VectorDiff::PushBack { value }
        | VectorDiff::Insert { value, .. }
        | VectorDiff::Set { value, .. } => vec![Room::clone(value)],
        _ => vec![],
    }
}

#[derive(Clone)]
pub struct DecoratedRoom {
    pub inner: Room,
    pub name: RoomDisplayName,
    pub visited: bool,
    pub last_message: Option<String>,
    pub last_sender: Option<String>,
    pub last_ts: Option<MilliSecondsSinceUnixEpoch>,
    pub recency_stamp: Option<RoomRecencyStamp>,
}

impl DecoratedRoom {
    pub fn room_id(&self) -> &RoomId {
        self.inner.room_id()
    }

    pub fn inner(&self) -> Room {
        self.inner.clone()
    }

    pub fn unread_count(&self) -> u64 {
        if self.visited {
            return 0;
        }

        self.inner.unread_notification_counts().notification_count
    }

    pub fn highlight_count(&self) -> u64 {
        if self.visited {
            return 0;
        }

        self.inner.unread_notification_counts().highlight_count
    }

    async fn from_room(room: Room) -> DecoratedRoom {
        let name = room.cached_display_name().unwrap_or(RoomDisplayName::Empty);
        let latest = room.latest_event();

        let (last_sender, last_message) = match preview(&room, &latest).await {
            Some((sender, message)) => (Some(sender), Some(message)),
            None => (None, None),
        };

        DecoratedRoom {
            name,
            visited: false,
            last_message,
            last_sender,
            last_ts: latest.timestamp(),
            recency_stamp: room.recency_stamp(),
            inner: room,
        }
    }
}

/// The sender and body of the latest event, if it's a message.
async fn preview(room: &Room, latest: &LatestEventValue) -> Option<(String, String)> {
    let LatestEventValue::Remote(event) = latest else {
        return None;
    };

    let event = Matrix::get_room_message_event(room, event)?;
    let og = event.as_original()?;

    let body = match &og.content.msgtype {
        MessageType::Text(content) => content.body.clone(),
        _ => "".to_string(),
    };

    let sender = match room.get_member_no_sync(&og.sender).await {
        Ok(Some(member)) => member.name().to_string(),
        _ => og.sender.to_string(),
    };

    Some((sender, body))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use matrix_sdk::ruma::api::client::sync::sync_events::v5;
    use matrix_sdk::ruma::exports::serde_json::{self, Value, json};
    use matrix_sdk::ruma::room_id;
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk::{Client, SlidingSyncList};
    use matrix_sdk_ui::RoomListService;

    use super::*;

    /// A cache that follows the room list of a client on a mock server.
    async fn follow() -> (MatrixMockServer, Client, Arc<RoomCache>) {
        let server = MatrixMockServer::new().await;
        server.mock_room_state_encryption().plain().mount().await;

        let client = server.client_builder().build().await;
        let service = RoomListService::new(client.clone()).await.unwrap();
        let all_rooms = service.all_rooms().await.unwrap();

        let cache = Arc::new(RoomCache::default());
        let follower = cache.clone();
        let (loaded, cache_loaded) = oneshot::channel();

        tokio::spawn(async move {
            let _service = service;
            follower.follow(&all_rooms, loaded).await;
        });

        cache_loaded.await.unwrap();

        (server, client, cache)
    }

    /// Run one sliding sync that returns these rooms.
    async fn sync(server: &MatrixMockServer, client: &Client, rooms: Value) {
        let mut response = v5::Response::new("0".to_string());
        response.rooms = serde_json::from_value(rooms).unwrap();

        // without a list, sliding sync ignores the rooms
        server
            .mock_sliding_sync()
            .ok_and_run(
                client,
                |builder| builder.add_list(SlidingSyncList::builder("rooms")),
                response,
            )
            .await;
    }

    /// Wait for the cache to match, and return its rooms.
    async fn rooms_once(
        cache: &RoomCache,
        matches: impl Fn(&[DecoratedRoom]) -> bool,
    ) -> Vec<DecoratedRoom> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let rooms = cache.get_rooms();

                if matches(&rooms) {
                    return rooms;
                }

                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the room cache never matched")
    }

    fn ids(rooms: &[DecoratedRoom]) -> Vec<&str> {
        let mut ids: Vec<_> = rooms.iter().map(|r| r.room_id().as_str()).collect();
        ids.sort();
        ids
    }

    fn message(id: &str, sender: &str, ts: u64, body: &str) -> Value {
        json!({
            "type": "m.room.message",
            "event_id": id,
            "sender": sender,
            "origin_server_ts": ts,
            "content": { "msgtype": "m.text", "body": body },
        })
    }

    fn member(user: &str, membership: &str, display_name: Option<&str>) -> Value {
        json!({
            "type": "m.room.member",
            "event_id": format!("$member-{}-{}", user, membership),
            "sender": user,
            "state_key": user,
            "origin_server_ts": 1,
            "content": { "membership": membership, "displayname": display_name },
        })
    }

    #[tokio::test]
    async fn adds_and_removes_rooms_from_the_list() {
        let (server, client, cache) = follow().await;

        sync(
            &server,
            &client,
            json!({
                "!a:localhost": { "initial": true, "name": "Alpha" },
                "!b:localhost": { "initial": true, "name": "Bravo" },
            }),
        )
        .await;

        let rooms = rooms_once(&cache, |rooms| rooms.len() == 2).await;
        assert_eq!(ids(&rooms), ["!a:localhost", "!b:localhost"]);

        // left on another device
        sync(
            &server,
            &client,
            json!({
                "!a:localhost": {
                    "required_state": [member("@example:localhost", "leave", None)],
                },
            }),
        )
        .await;

        let rooms = rooms_once(&cache, |rooms| rooms.len() == 1).await;
        assert_eq!(ids(&rooms), ["!b:localhost"]);
    }

    #[tokio::test]
    async fn previews_follow_the_latest_event() {
        let (server, client, cache) = follow().await;

        sync(
            &server,
            &client,
            json!({
                "!a:localhost": {
                    "initial": true,
                    "required_state": [member("@bob:localhost", "join", Some("Bob"))],
                    "timeline": [message("$1", "@bob:localhost", 1000, "hello")],
                },
            }),
        )
        .await;

        let rooms = rooms_once(&cache, |rooms| {
            rooms.first().and_then(|r| r.last_message.as_deref()) == Some("hello")
        })
        .await;

        assert_eq!(rooms[0].last_sender.as_deref(), Some("Bob"));
        assert_eq!(
            rooms[0].last_ts,
            Some(MilliSecondsSinceUnixEpoch(1000u32.into()))
        );

        // a sender with no member event shows as their ID
        sync(
            &server,
            &client,
            json!({
                "!a:localhost": {
                    "timeline": [message("$2", "@carol:localhost", 2000, "again")],
                },
            }),
        )
        .await;

        let rooms = rooms_once(&cache, |rooms| {
            rooms.first().and_then(|r| r.last_message.as_deref()) == Some("again")
        })
        .await;

        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].last_sender.as_deref(), Some("@carol:localhost"));
        assert_eq!(
            rooms[0].last_ts,
            Some(MilliSecondsSinceUnixEpoch(2000u32.into()))
        );
    }

    #[tokio::test]
    async fn sorts_unread_first_then_by_recency() {
        let (server, client, cache) = follow().await;

        sync(
            &server,
            &client,
            json!({
                "!read-old:localhost": { "initial": true, "bump_stamp": 1 },
                "!read-new:localhost": { "initial": true, "bump_stamp": 3 },
                "!read-none:localhost": { "initial": true },
                "!unread-old:localhost": {
                    "initial": true, "bump_stamp": 2, "notification_count": 1,
                },
                "!unread-new:localhost": {
                    "initial": true, "bump_stamp": 5, "notification_count": 1,
                },
                "!unread-none:localhost": { "initial": true, "notification_count": 1 },
            }),
        )
        .await;

        let mut rooms = rooms_once(&cache, |rooms| rooms.len() == 6).await;
        crate::widgets::rooms::sort_rooms(&mut rooms);

        let order: Vec<_> = rooms.iter().map(|r| r.room_id().as_str()).collect();
        assert_eq!(
            order,
            [
                "!unread-new:localhost",
                "!unread-old:localhost",
                "!unread-none:localhost",
                "!read-new:localhost",
                "!read-old:localhost",
                "!read-none:localhost",
            ]
        );
    }

    #[tokio::test]
    async fn add_and_remove_room_are_idempotent() {
        let (server, client, cache) = follow().await;

        sync(
            &server,
            &client,
            json!({ "!a:localhost": { "initial": true, "name": "Alpha" } }),
        )
        .await;

        rooms_once(&cache, |rooms| rooms.len() == 1).await;
        let room = client.get_room(room_id!("!a:localhost")).unwrap();

        // already listed
        cache.add_room(room.clone()).await;
        assert_eq!(cache.get_rooms().len(), 1);

        cache.remove_room(&room);
        cache.remove_room(&room);
        assert!(cache.get_rooms().is_empty());

        cache.add_room(room.clone()).await;
        cache.add_room(room.clone()).await;
        assert_eq!(cache.get_rooms().len(), 1);

        // an update from the list replaces it rather than adding another
        sync(
            &server,
            &client,
            json!({
                "!a:localhost": {
                    "timeline": [message("$1", "@bob:localhost", 1000, "hello")],
                },
            }),
        )
        .await;

        let rooms = rooms_once(&cache, |rooms| {
            rooms.first().and_then(|r| r.last_message.as_deref()) == Some("hello")
        })
        .await;

        assert_eq!(rooms.len(), 1);
    }
}
