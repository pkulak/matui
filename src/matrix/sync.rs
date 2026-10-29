use std::time::Duration;

use futures::stream::StreamExt;
use log::error;
use matrix_sdk::Client;
use matrix_sdk::RoomState;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::MilliSecondsSinceUnixEpoch;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::{
    AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, SyncEphemeralRoomEvent,
};
use matrix_sdk_ui::room_list_service::State as RoomListState;
use matrix_sdk_ui::sync_service::{State as SyncState, SyncService};

use super::Matrix;
use super::verification::add_verification_handlers;
use crate::app::App;
use crate::event::Event::Matui;
use crate::handler::MatuiEvent::Error;
use crate::handler::{MatuiEvent, SyncType};

impl Matrix {
    /// Start syncing forever, and show the rooms: a restored session's from
    /// the store right away, and a new login's once the room list has synced.
    /// Returns false if the room list never syncs.
    pub(super) async fn start_sync(&self, client: Client, sync_type: SyncType) -> bool {
        if client.available_sliding_sync_versions().await.is_empty() {
            App::send(Error(
                "Matui needs a homeserver that supports sliding sync (MSC4186), \
                 and this one doesn't."
                    .to_string(),
            ));
            return false;
        }

        // handlers must be in place before the first response arrives
        add_default_handlers(client.clone());
        add_verification_handlers(client.clone());

        let sync_service = match SyncService::builder(client.clone())
            .with_offline_mode()
            .build()
            .await
        {
            Ok(service) => self.sync_service.get_or_init(|| service),
            Err(err) => {
                App::send(Error(err.to_string()));
                return false;
            }
        };

        self.watch_sync(sync_service);

        let room_list = sync_service.room_list_service();

        let all_rooms = match room_list.all_rooms().await {
            Ok(all_rooms) => all_rooms,
            Err(err) => {
                App::send(Error(err.to_string()));
                return false;
            }
        };

        let mut room_list_state = room_list.state();

        let mut room_list_synced = async || {
            while room_list_state.get() != RoomListState::Running {
                if room_list_state.next().await.is_none() {
                    return false;
                }
            }

            true
        };

        // anything older than this doesn't notify
        let _ = self.sync_started.set(MilliSecondsSinceUnixEpoch::now());

        // a new login has no rooms stored until the room list syncs them
        let restored = matches!(sync_type, SyncType::Latest);

        if !restored {
            sync_service.start().await;

            if !room_list_synced().await {
                return false;
            }
        }

        let (loaded, cache_loaded) = tokio::sync::oneshot::channel();
        let room_cache = self.room_cache.clone();

        App::spawn(async move { room_cache.follow(&all_rooms, loaded).await });

        if cache_loaded.await.is_err() {
            return false;
        }

        App::send(MatuiEvent::SyncComplete);

        if restored {
            sync_service.start().await;

            if !room_list_synced().await {
                return false;
            }
        }

        // prompt for any invites that arrived while we weren't running
        for room in client.invited_rooms() {
            send_invite_event(room).await;
        }

        true
    }

    /// Restart sync after it fails, and only show an error once it keeps
    /// failing. Losing the network needs nothing here: offline mode waits for
    /// the server to come back by itself. But if the server answers and still
    /// rejects sync, offline mode retries with no pause, so pause it here.
    fn watch_sync(&self, sync_service: &SyncService) {
        const RESTART_DELAY: Duration = Duration::from_secs(5);
        const FAILURES_BEFORE_POPUP: u32 = 3;

        enum Watched {
            Sync(SyncState),
            RoomList(RoomListState),
        }

        let mut states = futures::stream::select(
            sync_service.state().map(Watched::Sync),
            sync_service
                .room_list_service()
                .state()
                .map(Watched::RoomList),
        );

        let cell = self.sync_service.clone();

        App::spawn(async move {
            let mut failures = 0;

            while let Some(state) = states.next().await {
                let offline = matches!(state, Watched::Sync(SyncState::Offline));

                let failure = match state {
                    // the sync service says Running before any request, so
                    // only a room list that synced again proves recovery
                    Watched::RoomList(RoomListState::Running) => {
                        failures = 0;
                        continue;
                    }
                    Watched::Sync(SyncState::Error(err)) => err.to_string(),
                    Watched::Sync(SyncState::Terminated) => "sync stopped".to_string(),
                    Watched::Sync(SyncState::Offline) => "the server rejects sync".to_string(),
                    _ => continue,
                };

                failures += 1;
                error!("sync failed ({} in a row): {}", failures, failure);

                if failures == FAILURES_BEFORE_POPUP {
                    App::send(Error(format!("Sync failed: {}", failure)));
                }

                // a first Offline is usually a lost connection, which the
                // service waits out by itself
                if offline && failures == 1 {
                    continue;
                }

                let Some(sync_service) = cell.get() else {
                    continue;
                };

                // cancels offline mode's own retries; does nothing after an
                // error or termination
                sync_service.stop().await;
                tokio::time::sleep(RESTART_DELAY).await;
                sync_service.start().await;
            }
        });
    }

    /// Get the full timeline for this room, and drop any other room.
    pub fn subscribe_to_room(&self, room: &Room) {
        let Some(sync_service) = self.sync_service.get() else {
            return;
        };

        let room_list = sync_service.room_list_service();
        let room_id = room.room_id().to_owned();
        let lock = self.subscription_lock.clone();

        App::spawn(async move {
            // keep quick room switches from finishing out of order
            let _guard = lock.lock().await;
            room_list.set_room_subscriptions(&[&room_id]).await;
        });
    }
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
