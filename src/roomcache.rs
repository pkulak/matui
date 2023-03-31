use anyhow::bail;
use futures::future::join_all;
use log::info;
use matrix_sdk::room::{Joined, MessagesOptions};
use matrix_sdk::{Client, DisplayName};
use ruma::api::Direction;
use ruma::events::room::message::MessageType::Text;
use ruma::events::room::message::TextMessageEventContent;
use ruma::events::AnyMessageLikeEvent::RoomMessage;
use ruma::events::AnyTimelineEvent::MessageLike;
use ruma::events::MessageLikeEvent::Original;
use ruma::MilliSecondsSinceUnixEpoch;
use std::sync::Mutex;

pub struct RoomCache {
    rooms: Mutex<Vec<DecoratedRoom>>,
}

#[derive(Clone)]
pub struct DecoratedRoom {
    pub room: Joined,
    pub name: DisplayName,
    pub last_message: Option<String>,
    pub last_sender: Option<String>,
    pub last_ts: Option<MilliSecondsSinceUnixEpoch>,
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
}

impl DecoratedRoom {
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

                    return Ok(DecoratedRoom {
                        room,
                        name,
                        last_message: Some(body),
                        last_sender: Some(c.sender.to_string()),
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
                    room,
                    name,
                    last_message: None,
                    last_sender: None,
                    last_ts: None,
                }
            }
        }
    }
}
