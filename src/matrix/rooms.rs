use log::{error, info};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::api::client::directory::get_public_rooms_filtered;
use matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset;
use matrix_sdk::ruma::api::client::room::{Visibility, create_room};
use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
use matrix_sdk::ruma::events::{EmptyStateKey, InitialStateEvent};
use matrix_sdk::ruma::{OwnedRoomOrAliasId, OwnedUserId, RoomOrAliasId};

use super::Matrix;
use crate::app::App;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::{Error, ProgressComplete, ProgressStarted};

#[derive(Clone, Copy)]
pub enum Moderation {
    Ban,
    Unban,
    Kick,
}

impl Matrix {
    pub fn fetch_room_member(&self, room: Room, id: OwnedUserId) {
        App::spawn(async move {
            match room.get_member(&id).await {
                Ok(Some(member)) => App::send(MatuiEvent::RoomMember(room, member)),
                Ok(None) => info!("no such room member: {}", id),
                Err(err) => error!("could not fetch room member {}: {}", id, err),
            }
        });
    }

    pub fn join_room(&self, room: Room) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            let result = room.join().await.map(|_| room);
            matrix.open_room(result).await;
        });
    }

    pub fn join_room_by_target(&self, target: OwnedRoomOrAliasId) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            let result = matrix.client().join_room_by_id_or_alias(&target, &[]).await;
            matrix.open_room(result).await;
        });
    }

    pub fn join_room_by_name(&self, name: String) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            // a single token might be an alias on our own server
            if !name.contains(char::is_whitespace)
                && let Ok(alias) =
                    RoomOrAliasId::parse(format!("#{}:{}", name, matrix.me().server_name()))
                && let Ok(room) = matrix.client().join_room_by_id_or_alias(&alias, &[]).await
            {
                matrix.open_room(Ok(room)).await;
                return;
            }

            // otherwise, let the server search its public room directory
            let mut request = get_public_rooms_filtered::v3::Request::new();
            request.limit = Some(1u32.into());
            request.filter.generic_search_term = Some(name.clone());

            match matrix.client().public_rooms_filtered(request).await {
                Ok(response) => match response.chunk.into_iter().next() {
                    Some(chunk) => {
                        App::send(ProgressComplete);
                        App::send(MatuiEvent::RoomFound(chunk));
                    }
                    None => {
                        App::send(ProgressComplete);
                        App::send(Error(format!(
                            "No public room found matching \"{}\".",
                            name
                        )));
                    }
                },
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn leave_room(&self, room: Room, forget: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Leaving.".to_string(), 500));

            match room.leave().await {
                Ok(_) => {
                    matrix.room_cache.remove_room(&room);
                    App::send(ProgressComplete);

                    if forget && let Err(err) = room.forget().await {
                        App::send(Error(err.to_string()));
                    }

                    App::send(MatuiEvent::RoomLeft(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn create_room(
        &self,
        name: Option<String>,
        topic: Option<String>,
        alias: Option<String>,
        encrypted: bool,
        private: bool,
    ) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Creating.".to_string(), 500));

            let mut request = create_room::v3::Request::new();
            request.name = name;
            request.topic = topic;
            request.room_alias_name = alias;

            if private {
                request.preset = Some(RoomPreset::PrivateChat);
            } else {
                request.preset = Some(RoomPreset::PublicChat);
                request.visibility = Visibility::Public;
            }

            if encrypted {
                request.initial_state = vec![
                    InitialStateEvent::new(
                        EmptyStateKey,
                        RoomEncryptionEventContent::with_recommended_defaults(),
                    )
                    .to_raw_any(),
                ];
            }

            let result = matrix.client().create_room(request).await;
            matrix.open_room(result).await;
        });
    }

    pub fn create_dm(&self, user_id: OwnedUserId, encrypted: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            // if we already have a DM with this user, just open it
            if let Some(room) = matrix.client().get_dm_room(&user_id) {
                App::send(MatuiEvent::RoomSelected(room));
                return;
            }

            App::send(ProgressStarted("Creating.".to_string(), 500));

            let result = if encrypted {
                matrix.client().create_dm(&user_id).await
            } else {
                // create_dm, minus the encryption state event
                let mut request = create_room::v3::Request::new();
                request.invite = vec![user_id.clone()];
                request.is_direct = true;
                request.preset = Some(RoomPreset::TrustedPrivateChat);

                matrix.client().create_room(request).await
            };

            matrix.open_room(result).await;
        });
    }

    pub fn invite_user(&self, room: Room, user_id: OwnedUserId) {
        App::spawn(async move {
            App::send(ProgressStarted("Inviting.".to_string(), 500));

            match room.invite_user_by_id(&user_id).await {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        "Invited".to_string(),
                        format!("{} has been invited.", user_id),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn moderate(
        &self,
        room: Room,
        action: Moderation,
        user_id: OwnedUserId,
        reason: Option<String>,
    ) {
        App::spawn(async move {
            let (verb, past) = match action {
                Moderation::Ban => ("Banning", "Banned"),
                Moderation::Unban => ("Unbanning", "Unbanned"),
                Moderation::Kick => ("Kicking", "Kicked"),
            };

            App::send(ProgressStarted(format!("{}.", verb), 500));

            let result = match action {
                Moderation::Ban => room.ban_user(&user_id, reason.as_deref()).await,
                Moderation::Unban => room.unban_user(&user_id, reason.as_deref()).await,
                Moderation::Kick => room.kick_user(&user_id, reason.as_deref()).await,
            };

            match result {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn ignore_user(&self, user_id: OwnedUserId, ignore: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            let (verb, past) = if ignore {
                ("Ignoring", "Ignored")
            } else {
                ("Unignoring", "Unignored")
            };

            App::send(ProgressStarted(format!("{}.", verb), 500));

            let account = matrix.client().account();

            let result = if ignore {
                account.ignore_user(&user_id).await
            } else {
                account.unignore_user(&user_id).await
            };

            match result {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    /// Finish joining or creating a room: add it to the room list and open
    /// it, or show why it failed.
    async fn open_room(&self, result: matrix_sdk::Result<Room>) {
        match result {
            Ok(room) => {
                self.room_cache.add_room(room.clone()).await;
                App::send(ProgressComplete);
                App::send(MatuiEvent::RoomSelected(room));
            }
            Err(err) => {
                App::send(ProgressComplete);
                App::send(Error(err.to_string()));
            }
        }
    }
}
