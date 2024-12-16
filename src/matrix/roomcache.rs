use anyhow::Context;
use futures::future::join_all;
use log::info;
use matrix_sdk::room::{MessagesOptions, Room};

use matrix_sdk::{Client, RoomDisplayName, RoomState};
use ruma::api::Direction;
use ruma::events::room::message::MessageType;
use ruma::events::AnyTimelineEvent;
use ruma::{MilliSecondsSinceUnixEpoch, RoomId};
use std::sync::Mutex;

use crate::matrix::matrix::Matrix;

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
            .map(|r| async move { DecoratedRoom::from_room(r.clone()).await });

        let rooms = join_all(rooms).await;

        let mut old_rooms = self.rooms.lock().expect("to unlock rooms");
        *old_rooms = rooms;

        info!("room cache populated")
    }

    pub fn get_rooms(&self) -> Vec<DecoratedRoom> {
        self.rooms.lock().expect("to unlock rooms").clone()
    }

    pub fn wrap(&self, room: &Room) -> Option<DecoratedRoom> {
        let rooms = self.rooms.lock().expect("to unlock rooms");

        for r in rooms.iter() {
            if r.inner.room_id() == room.room_id() {
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
        let room = match client.get_room(event.room_id()) {
            Some(room) => room,
            None => return,
        };

        if room.state() != RoomState::Joined {
            return;
        }

        let decorated = DecoratedRoom::from_room(room).await;

        let mut rooms = self.rooms.lock().expect("to unlock rooms");

        for dec in rooms.iter_mut() {
            if dec.inner.room_id() == event.room_id() {
                *dec = decorated;
                return;
            }
        }

        info!("A wild room has appeared! {}", decorated.name);
        rooms.insert(0, decorated);
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
        let name = room
            .compute_display_name()
            .await
            .unwrap_or(RoomDisplayName::Empty);

        async fn inner(room: Room, name: RoomDisplayName) -> anyhow::Result<DecoratedRoom> {
            let messages = room
                .messages(MessagesOptions::new(Direction::Backward))
                .await?
                .chunk;

            let mut latest_ts: Option<MilliSecondsSinceUnixEpoch> = None;

            for e in &messages {
                if latest_ts.is_none() {
                    if let Ok(event) = Matrix::deserialize_event(e, room.room_id().to_owned()) {
                        latest_ts = Some(event.origin_server_ts());
                    }
                }

                let Some(event) = Matrix::get_room_message_event(&room, e) else {
                    continue;
                };

                let (body, og) = if let Some(og) = event.as_original() {
                    if let MessageType::Text(content) = &og.content.msgtype {
                        (content.body.clone(), og)
                    } else {
                        ("".to_string(), og)
                    }
                } else {
                    continue;
                };

                let member = room.get_member(&og.sender).await?.context("not a member")?;

                return Ok(DecoratedRoom {
                    inner: room,
                    name,
                    visited: false,
                    last_message: Some(body),
                    last_sender: Some(member.name().to_string()),
                    last_ts: latest_ts,
                });
            }

            Ok(DecoratedRoom {
                inner: room,
                name,
                visited: false,
                last_message: None,
                last_sender: None,
                last_ts: latest_ts,
            })
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
