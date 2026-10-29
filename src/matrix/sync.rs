use std::path::Path;

use anyhow::bail;
use log::{error, info};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::api::client::filter::{
    FilterDefinition, LazyLoadOptions, RoomEventFilter, RoomFilter,
};
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::{
    AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, SyncEphemeralRoomEvent,
};
use matrix_sdk::{Client, LoopCtrl, RoomState};

use super::Matrix;
use super::session::persist_sync_token;
use super::verification::add_verification_handlers;
use crate::app::App;
use crate::event::Event::Matui;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::Error;

impl Matrix {
    pub fn sync(&self) {
        let client = self.client();

        add_default_handlers(client.clone());
        add_verification_handlers(client.clone());

        // apparently we only need the token for sync_once
        let sync_settings = build_sync_settings(None);

        App::spawn(async move {
            // prompt for any invites that arrived while we weren't running
            for room in client.invited_rooms() {
                send_invite_event(room).await;
            }

            let result = client
                .sync_with_result_callback(sync_settings, |sync_result| async move {
                    let response = match sync_result {
                        Ok(resp) => resp,
                        Err(err) => {
                            error!("no sync result: {}", err);
                            return Ok(LoopCtrl::Continue);
                        }
                    };

                    let (_, session_file) = Matrix::dirs();

                    // We persist the token each time to keep the disk up-to-date
                    if let Err(err) = persist_sync_token(&session_file, response.next_batch) {
                        error!("could not persist sync token {}", err)
                    }

                    Ok(LoopCtrl::Continue)
                })
                .await;

            if let Err(err) = result {
                error!("sync loop ended: {}", err);
                App::send(Error(format!("Sync failed: {}", err)));
            }
        });
    }
}

fn build_sync_settings(sync_token: Option<String>) -> SyncSettings {
    let mut state_filter = RoomEventFilter::empty();
    state_filter.lazy_load_options = LazyLoadOptions::Enabled {
        include_redundant_members: false,
    };

    let mut room_filter = RoomFilter::empty();
    room_filter.state = state_filter;

    let mut filter = FilterDefinition::empty();
    filter.room = room_filter;

    let mut sync_settings = SyncSettings::default().filter(filter.into());

    if let Some(token) = sync_token {
        sync_settings = sync_settings.token(token);
    }

    sync_settings
}

pub(super) async fn sync_once(
    client: Client,
    sync_token: Option<String>,
    session_file: &Path,
) -> anyhow::Result<String> {
    let sync_settings = build_sync_settings(sync_token);

    for _ in 0..10 {
        match client.sync_once(sync_settings.clone()).await {
            Ok(response) => {
                persist_sync_token(session_file, response.next_batch.clone())?;
                return Ok(response.next_batch);
            }
            Err(error) => {
                info!("An error occurred during initial sync: {error}");
                info!("Trying again…");
            }
        }
    }

    bail!("Sync timeout.")
}

fn add_default_handlers(client: Client) {
    client.add_event_handler(|event: AnySyncTimelineEvent, room: Room| async move {
        App::get_sender()
            .send(Matui(MatuiEvent::Timeline(
                event.into_full_event(room.room_id().into()),
            )))
            .expect("could not send timeline event");
    });

    client.add_event_handler(|ev: StrippedRoomMemberEvent, room: Room| async move {
        if ev.content.membership == MembershipState::Invite
            && ev.state_key == room.own_user_id()
            && room.state() == RoomState::Invited
        {
            send_invite_event(room).await;
        }
    });

    client.add_event_handler(|event: AnySyncEphemeralRoomEvent, room: Room| async move {
        if room.state() != RoomState::Joined {
            return;
        }

        match event {
            AnySyncEphemeralRoomEvent::Typing(SyncEphemeralRoomEvent { content: c, .. }) => {
                App::get_sender()
                    .send(Matui(MatuiEvent::Typing(room, c.user_ids)))
                    .expect("could not send typing event");
            }
            AnySyncEphemeralRoomEvent::Receipt(SyncEphemeralRoomEvent { content: c, .. }) => {
                App::get_sender()
                    .send(Matui(MatuiEvent::Receipt(room, c)))
                    .expect("could not send typing event");
            }
            _ => {}
        };
    });
}

async fn send_invite_event(room: Room) {
    let name = match room.display_name().await {
        Ok(name) => name.to_string(),
        Err(_) => room.room_id().to_string(),
    };

    let inviter = match room.invite_details().await {
        Ok(invite) => invite
            .inviter
            .map(|m| m.name().to_string())
            .unwrap_or_else(|| invite.inviter_id.to_string()),
        Err(_) => "Someone".to_string(),
    };

    App::send(MatuiEvent::Invited(
        room,
        format!("{} invited you to {}. Join?", inviter, name),
    ));
}
