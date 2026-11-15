use log::{error, info};
use matrix_sdk::notification_settings::NotificationSettings;
use matrix_sdk::notification_settings::RoomNotificationMode::{
    self, AllMessages, MentionsAndKeywordsOnly,
};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::api::client::directory::get_public_rooms_filtered;
use matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset;
use matrix_sdk::ruma::api::client::room::{Visibility, create_room};
use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
use matrix_sdk::ruma::events::{EmptyStateKey, InitialStateEvent};
use matrix_sdk::ruma::{OwnedRoomOrAliasId, OwnedUserId, RoomOrAliasId};

use super::Matrix;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::Error;

#[derive(Clone, Copy)]
pub enum Moderation {
    Ban,
    Unban,
    Kick,
}

impl Matrix {
    pub fn fetch_room_member(&self, room: Room, id: OwnedUserId) {
        let matrix = self.clone();

        self.spawn(async move {
            match room.get_member(&id).await {
                Ok(Some(member)) => matrix.send(MatuiEvent::RoomMember(room, member)),
                Ok(None) => info!("no such room member: {}", id),
                Err(err) => error!("could not fetch room member {}: {}", id, err),
            }
        });
    }

    pub fn join_room(&self, room: Room) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Joining.", 500);

            let result = room.join().await.map(|_| room);
            matrix.open_room(result).await;
        });
    }

    pub fn join_room_by_target(&self, target: OwnedRoomOrAliasId) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Joining.", 500);

            let result = matrix.client().join_room_by_id_or_alias(&target, &[]).await;
            matrix.open_room(result).await;
        });
    }

    pub fn join_room_by_name(&self, name: String) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Joining.", 500);

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
                        matrix.send(MatuiEvent::RoomFound(chunk));
                    }
                    None => {
                        matrix.send(Error(format!(
                            "No public room found matching \"{}\".",
                            name
                        )));
                    }
                },
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn leave_room(&self, room: Room, forget: bool) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Leaving.", 500);

            match room.leave().await {
                Ok(_) => {
                    matrix.room_cache.remove_room(&room);

                    if forget && let Err(err) = room.forget().await {
                        matrix.send(Error(err.to_string()));
                    }

                    matrix.send(MatuiEvent::RoomLeft(room));
                }
                Err(err) => {
                    matrix.send(Error(err.to_string()));
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

        self.spawn(async move {
            let _progress = matrix.progress("Creating.", 500);

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

        self.spawn(async move {
            // if we already have a DM with this user, just open it
            if let Some(room) = matrix.client().get_dm_room(&user_id) {
                matrix.send(MatuiEvent::RoomSelected(room));
                return;
            }

            let _progress = matrix.progress("Creating.", 500);

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
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Inviting.", 500);

            match room.invite_user_by_id(&user_id).await {
                Ok(_) => {
                    matrix.send(MatuiEvent::Confirm(
                        "Invited".to_string(),
                        format!("{} has been invited.", user_id),
                    ));
                }
                Err(err) => {
                    matrix.send(Error(err.to_string()));
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
        let matrix = self.clone();

        self.spawn(async move {
            let (verb, past) = match action {
                Moderation::Ban => ("Banning", "Banned"),
                Moderation::Unban => ("Unbanning", "Unbanned"),
                Moderation::Kick => ("Kicking", "Kicked"),
            };

            let _progress = matrix.progress(&format!("{}.", verb), 500);

            let result = match action {
                Moderation::Ban => room.ban_user(&user_id, reason.as_deref()).await,
                Moderation::Unban => room.unban_user(&user_id, reason.as_deref()).await,
                Moderation::Kick => room.kick_user(&user_id, reason.as_deref()).await,
            };

            match result {
                Ok(_) => {
                    matrix.send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn ignore_user(&self, user_id: OwnedUserId, ignore: bool) {
        let matrix = self.clone();

        self.spawn(async move {
            let (verb, past) = if ignore {
                ("Ignoring", "Ignored")
            } else {
                ("Unignoring", "Unignored")
            };

            let _progress = matrix.progress(&format!("{}.", verb), 500);

            let account = matrix.client().account();

            let result = if ignore {
                account.ignore_user(&user_id).await
            } else {
                account.unignore_user(&user_id).await
            };

            match result {
                Ok(_) => {
                    matrix.send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                }
            }
        });
    }

    /// Mute the room on the server, so only mentions and keywords notify, or
    /// unmute it so everything does.
    pub fn mute_room(&self, room: Room, mute: bool) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress(if mute { "Muting." } else { "Unmuting." }, 500);

            let settings = matrix.notification_settings().await;

            if let Err(err) = set_room_muted(&room, settings, mute).await {
                matrix.send(Error(err.to_string()));
            }
        });
    }

    /// Finish joining or creating a room: add it to the room list and open
    /// it, or show why it failed. The caller's progress closes after this,
    /// so it doesn't bring back the room switcher over the room.
    async fn open_room(&self, result: matrix_sdk::Result<Room>) {
        match result {
            Ok(room) => {
                self.room_cache.add_room(room.clone()).await;
                self.send(MatuiEvent::RoomSelected(room));
            }
            Err(err) => {
                self.send(Error(err.to_string()));
            }
        }
    }
}

async fn set_room_muted(
    room: &Room,
    settings: &NotificationSettings,
    mute: bool,
) -> anyhow::Result<()> {
    // unmuting drops the room's own setting, unless rooms like it are quiet
    // by default
    let mode = if mute {
        Some(MentionsAndKeywordsOnly)
    } else if default_mode(room, settings).await? == AllMessages {
        None
    } else {
        Some(AllMessages)
    };

    // sync would update the room's cached mode too, but not before the
    // header draws
    match mode {
        Some(mode) => {
            settings
                .set_room_notification_mode(room.room_id(), mode)
                .await?;
            room.update_cached_user_defined_notification_mode(mode);
        }
        None => {
            settings
                .delete_user_defined_room_rules(room.room_id())
                .await?;
            room.clear_user_defined_notification_mode();
        }
    }

    Ok(())
}

/// The account's notification mode for rooms like this one, for when the
/// room has no setting of its own.
async fn default_mode(
    room: &Room,
    settings: &NotificationSettings,
) -> anyhow::Result<RoomNotificationMode> {
    let encrypted = room.latest_encryption_state().await?.is_encrypted();
    let one_to_one = room.active_members_count() == 2;

    Ok(settings
        .get_default_room_notification_mode(encrypted.into(), one_to_one.into())
        .await)
}

#[cfg(test)]
mod tests {
    use super::set_room_muted;
    use matrix_sdk::notification_settings::NotificationSettings;
    use matrix_sdk::notification_settings::RoomNotificationMode::{
        AllMessages, MentionsAndKeywordsOnly,
    };
    use matrix_sdk::room::Room;
    use matrix_sdk::ruma::push::{PredefinedUnderrideRuleId, RuleKind, Ruleset};
    use matrix_sdk::ruma::room_id;
    use matrix_sdk::test_utils::mocks::{MatrixMockServer, PushRuleIdSpec};
    use matrix_sdk_test::JoinedRoomBuilder;
    use matrix_sdk_test::event_factory::EventFactory;

    /// An unencrypted group room, where rooms like it notify only for
    /// mentions when `quiet`.
    async fn room(server: &MatrixMockServer, quiet: bool) -> Room {
        let client = server.client_builder().build().await;
        let room_id = room_id!("!room:localhost");
        let mut rules = Ruleset::server_default(client.user_id().unwrap());

        if quiet {
            rules
                .set_actions(
                    RuleKind::Underride,
                    PredefinedUnderrideRuleId::Message,
                    vec![],
                )
                .unwrap();
        }

        server.mock_room_state_encryption().plain().mount().await;
        server
            .mock_sync()
            .ok_and_run(&client, |builder| {
                builder.add_joined_room(JoinedRoomBuilder::new(room_id));
                builder.add_global_account_data(EventFactory::new().push_rules(rules));
            })
            .await;

        client.get_room(room_id).unwrap()
    }

    async fn settings(room: &Room) -> NotificationSettings {
        room.client().notification_settings().await
    }

    #[tokio::test]
    async fn muting_leaves_only_mentions() {
        let server = MatrixMockServer::new().await;
        let room = room(&server, false).await;

        server
            .mock_set_push_rules(RuleKind::Room, PushRuleIdSpec::Any)
            .ok()
            .mock_once()
            .mount()
            .await;

        set_room_muted(&room, &settings(&room).await, true)
            .await
            .unwrap();

        assert_eq!(
            room.cached_user_defined_notification_mode(),
            Some(MentionsAndKeywordsOnly)
        );
    }

    #[tokio::test]
    async fn unmuting_goes_back_to_the_default_when_it_notifies() {
        let server = MatrixMockServer::new().await;
        let room = room(&server, false).await;

        server
            .mock_set_push_rules(RuleKind::Room, PushRuleIdSpec::Any)
            .ok()
            .mount()
            .await;
        server
            .mock_delete_push_rules(RuleKind::Room, PushRuleIdSpec::Any)
            .ok()
            .mock_once()
            .mount()
            .await;

        let settings = settings(&room).await;
        set_room_muted(&room, &settings, true).await.unwrap();
        set_room_muted(&room, &settings, false).await.unwrap();

        assert_eq!(room.cached_user_defined_notification_mode(), None);
    }

    #[tokio::test]
    async fn unmuting_notifies_for_everything_when_the_default_is_quiet() {
        let server = MatrixMockServer::new().await;
        let room = room(&server, true).await;

        server
            .mock_set_push_rules(RuleKind::Room, PushRuleIdSpec::Any)
            .ok()
            .mock_once()
            .mount()
            .await;

        set_room_muted(&room, &settings(&room).await, false)
            .await
            .unwrap();

        assert_eq!(
            room.cached_user_defined_notification_mode(),
            Some(AllMessages)
        );
    }
}
