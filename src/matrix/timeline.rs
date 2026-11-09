use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use log::error;
use matrix_sdk::HttpError;
use matrix_sdk::event_cache::EventCacheError;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::room::encrypted;
use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk::ruma::{OwnedEventId, TransactionId};
use matrix_sdk::send_queue::{LocalEchoContent, RoomSendQueueUpdate};
use matrix_sdk::store::SerializableEventContent;
use matrix_sdk_ui::Timeline;
use matrix_sdk_ui::timeline::{
    self, RoomExt, TimelineEventItemId, TimelineFocus, TimelineReadReceiptTracking,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::AbortHandle;

use super::Matrix;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::Error;

impl Matrix {
    /// Follow the room's timeline, without its threads' messages, sending all
    /// its items, with read receipts, after every change. Abort the returned
    /// handle to stop.
    pub fn follow_timeline(&self, room: Room) -> AbortHandle {
        let matrix = self.clone();

        let task = self.spawn(async move {
            let focus = TimelineFocus::Live {
                hide_threaded_events: true,
            };

            let timeline = room
                .timeline_builder()
                .with_focus(focus)
                .track_read_marker_and_receipts(TimelineReadReceiptTracking::MessageLikeEvents)
                .build()
                .await;

            match timeline {
                Ok(timeline) => {
                    let timeline = Arc::new(timeline);

                    tokio::join!(
                        send_items(&matrix, timeline.clone()),
                        visit_on_own_receipts(&matrix, &timeline)
                    );
                }
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });

        task.abort_handle()
    }

    /// A timeline of the thread with this root. Normally a read from the
    /// event cache.
    pub fn thread_timeline(
        &self,
        room: &Room,
        root: OwnedEventId,
    ) -> anyhow::Result<Arc<Timeline>> {
        let focus = TimelineFocus::Thread {
            root_event_id: root,
        };

        let timeline = self
            .handle
            .block_on(room.timeline_builder().with_focus(focus).build())?;

        Ok(Arc::new(timeline))
    }

    /// Follow the timeline, sending all its items after every change. Abort
    /// the returned handle to stop.
    pub fn follow(&self, timeline: Arc<Timeline>) -> AbortHandle {
        let matrix = self.clone();

        let task = self.spawn(async move { send_items(&matrix, timeline).await });

        task.abort_handle()
    }

    /// Page the timeline back by about this many events. Its items arrive
    /// separately, before or after it's done. Abort the returned handle to
    /// stop, including any retries.
    pub fn paginate(&self, timeline: Arc<Timeline>, events: u16) -> AbortHandle {
        const RETRY_DELAY: Duration = Duration::from_secs(3);

        let matrix = self.clone();

        let task = self.spawn(async move {
            loop {
                match timeline.paginate_backwards(events).await {
                    Ok(hit_start) => {
                        matrix.send(MatuiEvent::Paginated(timeline, hit_start));
                        return;
                    }
                    // a lost connection comes back by itself, so wait it out
                    Err(err) if lost_connection(&err) => {
                        error!("could not paginate, retrying: {}", err);
                        tokio::time::sleep(RETRY_DELAY).await;
                    }
                    Err(err) => {
                        matrix.send(Error(err.to_string()));
                        return;
                    }
                }
            }
        });

        task.abort_handle()
    }

    /// Add our reaction to the message, or take it back if it's there.
    pub fn toggle_reaction(&self, timeline: Arc<Timeline>, id: TimelineEventItemId, key: String) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Reacting.", 500);

            if let Err(err) = timeline.toggle_reaction(&id, &key).await {
                matrix.send(Error(err.to_string()));
            }
        });
    }

    /// Keep each room's send queue going, since any error turns it off: try
    /// again a little later after an error that may pass, and right away
    /// after a request gets stuck, which holds back the ones after it. A
    /// stuck edit or delete has nothing on screen to remove it by, so cancel
    /// it and show its error.
    pub fn watch_send_queue(&self) {
        const RETRY_DELAY: Duration = Duration::from_secs(5);

        let client = self.client();
        let mut updates = client.send_queue().subscribe();
        let matrix = self.clone();

        self.spawn(async move {
            loop {
                let update = match updates.recv().await {
                    Ok(update) => update,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return,
                };

                let RoomSendQueueUpdate::SendError {
                    transaction_id,
                    error,
                    is_recoverable,
                } = update.update
                else {
                    continue;
                };

                let Some(room) = client.get_room(&update.room_id) else {
                    continue;
                };

                if is_recoverable {
                    matrix.spawn(async move {
                        tokio::time::sleep(RETRY_DELAY).await;
                        room.send_queue().set_enabled(true);
                    });

                    continue;
                }

                cancel_stuck_change(&matrix, &room, &transaction_id, &error).await;
                room.send_queue().set_enabled(true);
            }
        });
    }

    /// Every earlier body of an edited message, oldest first. Normally a
    /// read from the event cache.
    pub fn edit_history(&self, timeline: &Timeline, id: &OwnedEventId) -> Vec<MessageType> {
        let revisions = match self.handle.block_on(timeline.edit_revisions(id)) {
            Ok(revisions) => revisions,
            Err(err) => {
                error!("could not fetch edit history: {}", err);
                return vec![];
            }
        };

        // the last revision is the body showing
        let earlier = revisions.len().saturating_sub(1);

        revisions
            .into_iter()
            .take(earlier)
            .filter_map(|r| r.content.as_message().map(|m| m.msgtype().clone()))
            .collect()
    }
}

async fn send_items(matrix: &Matrix, timeline: Arc<Timeline>) {
    let (mut items, stream) = timeline.subscribe().await;
    let mut stream = pin!(stream);
    matrix.send(MatuiEvent::TimelineItems(timeline.clone(), items.clone()));

    while let Some(diffs) = stream.next().await {
        diffs.into_iter().for_each(|d| d.apply(&mut items));
        matrix.send(MatuiEvent::TimelineItems(timeline.clone(), items.clone()));
    }
}

/// Our own receipt moving, such as from reading the room in another session,
/// counts as visiting the room.
async fn visit_on_own_receipts(matrix: &Matrix, timeline: &Timeline) {
    let changes = timeline.subscribe_own_user_read_receipts_changed().await;
    let mut changes = pin!(changes);

    while changes.next().await.is_some() {
        matrix.room_visit_event(timeline.room().clone());
    }
}

/// Cancel a request that got stuck if it's an edit or a delete, and show why.
async fn cancel_stuck_change(
    matrix: &Matrix,
    room: &Room,
    transaction_id: &TransactionId,
    error: &matrix_sdk::Error,
) {
    let echoes = match room.send_queue().subscribe().await {
        Ok((echoes, _)) => echoes,
        Err(err) => {
            error!("could not read the send queue: {}", err);
            return;
        }
    };

    let Some(echo) = echoes
        .into_iter()
        .find(|e| e.transaction_id == transaction_id)
    else {
        return;
    };

    let aborted = match echo.content {
        LocalEchoContent::Event {
            serialized_event,
            send_handle,
            ..
        } if is_edit(&serialized_event) => send_handle
            .abort()
            .await
            .map(|_| format!("Could not edit the message: {}", error)),
        LocalEchoContent::Redaction { send_handle, .. } => send_handle
            .abort()
            .await
            .map(|_| format!("Could not delete the message: {}", error)),
        _ => return,
    };

    match aborted {
        Ok(message) => matrix.send(Error(message)),
        Err(err) => error!("could not cancel a stuck request: {}", err),
    }
}

fn is_edit(content: &SerializableEventContent) -> bool {
    content.deserialize().is_ok_and(|content| {
        matches!(
            content.relation(),
            Some(encrypted::Relation::Replacement(_))
        )
    })
}

fn lost_connection(err: &timeline::Error) -> bool {
    let timeline::Error::EventCacheError(EventCacheError::PaginationError(err)) = err else {
        return false;
    };

    matches!(&**err, matrix_sdk::Error::Http(err) if matches!(**err, HttpError::Reqwest(_)))
}
