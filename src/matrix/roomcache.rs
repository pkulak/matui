use anyhow::{bail, Context};
use futures::future::join_all;
use log::info;
use matrix_sdk::room::{Joined, MessagesOptions, Room};
use matrix_sdk::{Client, DisplayName};
use ruma::api::Direction;
use ruma::events::room::message::MessageType::Text;
use ruma::events::room::message::TextMessageEventContent;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyTimelineEvent;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent::Original;
use ruma::{MilliSecondsSinceUnixEpoch, RoomId};
use std::sync::Mutex;

pub struct RoomCache {
    rooms: Mutex<Vec<DecoratedRoom>>,
}

impl Default for RoomCache {
    fn default() -> Self {
        RoomCache {
            rooms: Mutex::new(vec![]),
        }
    }
}

impl RoomCache {
    pub async fn populate(&self, client: Client) {
        info!("populating room cache");

        let rooms = client
            .joined_rooms()
            .into_iter()
            .map(|r| async move { DecoratedRoom::from_joined(r.clone()).await });

        let rooms = join_all(rooms).await;

        let mut old_rooms = self.rooms.lock().expect("to unlock rooms");
        *old_rooms = rooms;

        info!("room cache populated")
    }

    pub fn get_rooms(&self) -> Vec<DecoratedRoom> {
        self.rooms.lock().expect("to unlock rooms").clone()
    }

    pub fn wrap(&self, joined: &Joined) -> Option<DecoratedRoom> {
        let rooms = self.rooms.lock().expect("to unlock rooms");

        for r in rooms.iter() {
            if r.inner.room_id() == joined.room_id() {
                return Some(r.clone());
            }
        }

        None
    }

    pub fn room_visit_event(&self, room: Room) {
        let mut rooms = self.rooms.lock().expect("to unlock rooms");

        for dec in rooms.iter_mut() {
            if dec.inner.room_id() == room.room_id() {
                dec.visited = true;
                return;
            }
        }
    }

    pub async fn timeline_event(&self, client: Client, event: &AnyTimelineEvent) {
        let joined = match client.get_joined_room(event.room_id()) {
            Some(joined) => joined,
            None => return,
        };

        let decorated = DecoratedRoom::from_joined(joined).await;

        let mut rooms = self.rooms.lock().expect("to unlock rooms");

        for dec in rooms.iter_mut() {
            if dec.inner.room_id() == event.room_id() {
                *dec = decorated;
                return;
            }
        }
    }
}

#[derive(Clone)]
pub struct DecoratedRoom {
    pub inner: Joined,
    pub name: DisplayName,
    pub visited: bool,
    pub last_message: Option<String>,
    pub last_sender: Option<String>,
    pub last_ts: Option<MilliSecondsSinceUnixEpoch>,
}

impl DecoratedRoom {
    pub fn room_id(&self) -> &RoomId {
        self.inner.room_id()
    }

    pub fn inner(&self) -> Joined {
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

    async fn from_joined(room: Joined) -> DecoratedRoom {
        let name = room.display_name().await.unwrap_or(DisplayName::Empty);

        async fn inner(room: Joined, name: DisplayName) -> anyhow::Result<DecoratedRoom> {
            let messages = room
                .messages(MessagesOptions::new(Direction::Backward))
                .await?
                .chunk;

            if messages.is_empty() {
                bail!("no events for room")
            }

            for event in messages {
                if let Ok(MessageLike(RoomMessage(Original(c)))) = event.event.deserialize() {
                    let body = match c.content.msgtype {
                        Text(TextMessageEventContent { body, .. }) => body,
                        _ => "".to_string(),
                    };

                    let member = room.get_member(&c.sender).await?.context("not a member")?;

                    return Ok(DecoratedRoom {
                        inner: room,
                        name,
                        visited: false,
                        last_message: Some(body),
                        last_sender: Some(member.name().to_string()),
                        last_ts: Some(c.origin_server_ts),
                    });
                }
            }

            bail!("no message found");
        }

        match inner(room.clone(), name.clone()).await {
            Ok(r) => r,
            Err(e) => {
                info!("could not fetch room details: {}", e.to_string());
                DecoratedRoom {
                    inner: room,
                    name,
                    visited: false,
                    last_message: None,
                    last_sender: None,
                    last_ts: None,
                }
            }
        }
    }
}
