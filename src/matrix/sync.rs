use std::pin::pin;
use std::time::Duration;

use futures::stream::{Stream, StreamExt};
use log::{error, warn};
use matrix_sdk::Client;
use matrix_sdk::RoomState;
use matrix_sdk::encryption::backups::BackupState;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::MilliSecondsSinceUnixEpoch;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::{
    AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, SyncEphemeralRoomEvent,
};
use matrix_sdk_ui::room_list_service::State as RoomListState;
use matrix_sdk_ui::sync_service::{State as SyncState, SyncService};

use super::Matrix;
use super::decryption::RoomKey;
use super::verification::add_verification_handlers;
use crate::handler::MatuiEvent::Error;
use crate::handler::{MatuiEvent, SyncType};

impl Matrix {
    /// Start syncing forever, and show the rooms: a restored session's from
    /// the store right away, and a new login's once the room list has synced.
    /// Returns false if the room list never syncs.
    pub(super) async fn start_sync(&self, client: Client, sync_type: SyncType) -> bool {
        if client.available_sliding_sync_versions().await.is_empty() {
            self.send(Error(
                "Matui needs a homeserver that supports sliding sync (MSC4186), \
                 and this one doesn't."
                    .to_string(),
            ));
            return false;
        }

        // handlers must be in place before the first response arrives
        add_default_handlers(self, &client);
        add_verification_handlers(self);
        follow_room_keys(self, &client).await;
        self.spawn(show_backup_downloads(
            client.encryption().backups().state_stream(),
            self.clone(),
        ));

        let sync_service = match SyncService::builder(client.clone())
            .with_offline_mode()
            .build()
            .await
        {
            Ok(service) => self.sync_service.get_or_init(|| service),
            Err(err) => {
                self.send(Error(err.to_string()));
                return false;
            }
        };

        self.watch_sync(sync_service);

        let room_list = sync_service.room_list_service();

        let all_rooms = match room_list.all_rooms().await {
            Ok(all_rooms) => all_rooms,
            Err(err) => {
                self.send(Error(err.to_string()));
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

        self.spawn(async move { room_cache.follow(&all_rooms, loaded).await });

        if cache_loaded.await.is_err() {
            return false;
        }

        self.send(MatuiEvent::SyncComplete);

        if restored {
            sync_service.start().await;

            if !room_list_synced().await {
                return false;
            }
        }

        // prompt for any invites that arrived while we weren't running
        for room in client.invited_rooms() {
            send_invite_event(self, room).await;
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
        let matrix = self.clone();

        self.spawn(async move {
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
                    matrix.send(Error(format!("Sync failed: {}", failure)));
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

        self.spawn(async move {
            // keep quick room switches from finishing out of order
            let _guard = lock.lock().await;
            room_list.set_room_subscriptions(&[&room_id]).await;
        });
    }
}

fn add_default_handlers(matrix: &Matrix, client: &Client) {
    let handler = matrix.clone();
    client.add_event_handler(move |event: AnySyncTimelineEvent, room: Room| async move {
        handler.send(MatuiEvent::Timeline(
            event.into_full_event(room.room_id().into()),
        ));
    });

    let handler = matrix.clone();
    client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room| async move {
        if ev.content.membership == MembershipState::Invite
            && ev.state_key == room.own_user_id()
            && room.state() == RoomState::Invited
        {
            send_invite_event(&handler, room).await;
        }
    });

    let handler = matrix.clone();
    client.add_event_handler(
        move |event: AnySyncEphemeralRoomEvent, room: Room| async move {
            if room.state() != RoomState::Joined {
                return;
            }

            match event {
                AnySyncEphemeralRoomEvent::Typing(SyncEphemeralRoomEvent {
                    content: c, ..
                }) => {
                    handler.send(MatuiEvent::Typing(room, c.user_ids));
                }
                AnySyncEphemeralRoomEvent::Receipt(SyncEphemeralRoomEvent {
                    content: c, ..
                }) => {
                    handler.send(MatuiEvent::Receipt(room, c));
                }
                _ => {}
            };
        },
    );
}

/// Tell the UI about every room key that arrives, so the open room can
/// decrypt what it couldn't before.
async fn follow_room_keys(matrix: &Matrix, client: &Client) {
    let Some(stream) = client.encryption().room_keys_received_stream().await else {
        return;
    };

    let handler = matrix.clone();

    matrix.spawn(async move {
        let mut stream = pin!(stream);

        while let Some(keys) = stream.next().await {
            let keys = match keys {
                Ok(keys) => Some(
                    keys.into_iter()
                        .map(|k| RoomKey {
                            room_id: k.room_id,
                            session_id: k.session_id,
                        })
                        .collect(),
                ),
                Err(err) => {
                    warn!("lost track of room keys: {}", err);
                    None
                }
            };

            handler.send(MatuiEvent::RoomKeys(keys));
        }
    });
}

/// Show progress while every room key downloads from the backup, such as
/// after verifying. It can take a while, and a room opened meanwhile has
/// nothing it can decrypt yet.
async fn show_backup_downloads<E>(
    states: impl Stream<Item = Result<BackupState, E>>,
    matrix: Matrix,
) {
    let mut states = pin!(states);
    let mut downloading = false;

    while let Some(state) = states.next().await {
        // a missed state is fine: the next one says whether it's still going
        let Ok(state) = state else {
            continue;
        };

        let now = state == BackupState::Downloading;

        if now == downloading {
            continue;
        }

        downloading = now;

        let event = if downloading {
            MatuiEvent::ProgressStarted("Downloading encryption keys.".to_string(), 250)
        } else {
            MatuiEvent::ProgressComplete
        };

        matrix.send(event);
    }
}

async fn send_invite_event(matrix: &Matrix, room: Room) {
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

    matrix.send(MatuiEvent::Invited(
        room,
        format!("{} invited you to {}. Join?", inviter, name),
    ));
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::channel;

    use futures::stream;
    use tokio::runtime::Handle;

    use super::*;
    use crate::event::Event::Matui;

    /// The progress events sent for these backup states, with `None` for
    /// a state the stream missed.
    async fn progress(states: Vec<Option<BackupState>>) -> Vec<&'static str> {
        let (sender, events) = channel();
        let states = states.into_iter().map(|s| s.ok_or(()));

        let matrix = Matrix::new(sender, Handle::current());
        show_backup_downloads(stream::iter(states), matrix).await;

        events
            .try_iter()
            .map(|event| match event {
                Matui(MatuiEvent::ProgressStarted(..)) => "started",
                Matui(MatuiEvent::ProgressComplete) => "complete",
                event => panic!("expected progress, got {:?}", event),
            })
            .collect()
    }

    #[tokio::test]
    async fn shows_progress_while_downloading() {
        use BackupState::*;

        let events = progress(vec![
            Some(Unknown),
            Some(Enabling),
            Some(Downloading),
            Some(Enabled),
        ])
        .await;

        assert_eq!(events, ["started", "complete"]);
    }

    #[tokio::test]
    async fn ends_progress_after_a_missed_state() {
        use BackupState::*;

        let events = progress(vec![Some(Downloading), None, Some(Enabled)]).await;

        assert_eq!(events, ["started", "complete"]);
    }

    #[tokio::test]
    async fn shows_nothing_without_a_download() {
        use BackupState::*;

        let events = progress(vec![Some(Resuming), Some(Enabled)]).await;

        assert!(events.is_empty());
    }
}
