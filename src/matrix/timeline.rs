use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use log::error;
use matrix_sdk::HttpError;
use matrix_sdk::QueueWedgeError;
use matrix_sdk::event_cache::{EventCacheError, PaginationStatus};
use matrix_sdk::room::Room;
use matrix_sdk::ruma::OwnedEventId;
use matrix_sdk::ruma::events::AnyMessageLikeEventContent;
use matrix_sdk::ruma::events::room::message::{MessageType, Relation};
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
                        visit_on_own_receipts(&matrix, &timeline),
                        send_not_at_start(&matrix, timeline.clone())
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
    /// after a request gets stuck, which holds back the ones after it. Only
    /// a stuck message stays, showing its error until it's deleted; any
    /// other stuck request is cancelled, with its error in a popup. This
    /// also sends what was waiting when matui last quit.
    pub fn watch_send_queue(&self) {
        const RETRY_DELAY: Duration = Duration::from_secs(5);

        let client = self.client();
        let mut updates = client.send_queue().subscribe();
        let matrix = self.clone();

        self.spawn(async move {
            // what got stuck while matui wasn't running
            matrix.unblock_send_queues().await;

            loop {
                let update = match updates.recv().await {
                    Ok(update) => update,
                    // a missed update may have been an error
                    Err(RecvError::Lagged(_)) => {
                        matrix.unblock_send_queues().await;
                        continue;
                    }
                    Err(RecvError::Closed) => return,
                };

                let RoomSendQueueUpdate::SendError { is_recoverable, .. } = update.update else {
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

                unblock(&matrix, &room).await;
            }
        });
    }

    /// Unblock every room with requests waiting to send.
    async fn unblock_send_queues(&self) {
        let client = self.client();

        let room_ids = match client.state_store().load_rooms_with_unsent_requests().await {
            Ok(room_ids) => room_ids,
            Err(err) => {
                error!("could not read the send queues: {}", err);
                return;
            }
        };

        for room in room_ids.iter().filter_map(|id| client.get_room(id)) {
            unblock(self, &room).await;
        }
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

/// The SDK can drop the earlier events of a live timeline, such as after a
/// sync with a gap in it, leaving it no longer at its start.
async fn send_not_at_start(matrix: &Matrix, timeline: Arc<Timeline>) {
    let Some((_, statuses)) = timeline.live_back_pagination_status().await else {
        return;
    };

    let mut statuses = pin!(statuses);

    while let Some(status) = statuses.next().await {
        if let PaginationStatus::Idle {
            hit_timeline_start: false,
        } = status
        {
            matrix.send(MatuiEvent::NotAtStart(timeline.clone()));
        }
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

/// Cancel every request stuck in the room's send queue but a message, and
/// show why, then turn the queue back on, since the error turned it off.
async fn unblock(matrix: &Matrix, room: &Room) {
    let queue = room.send_queue();

    match queue.subscribe().await {
        Ok((echoes, _)) => {
            for echo in echoes {
                cancel_if_stuck(matrix, echo.content).await;
            }
        }
        Err(err) => error!("could not read the send queue: {}", err),
    }

    queue.set_enabled(true);
}

async fn cancel_if_stuck(matrix: &Matrix, echo: LocalEchoContent) {
    let (cancelled, failed, error) = match echo {
        LocalEchoContent::Event {
            serialized_event,
            send_handle,
            send_error: Some(error),
        } => {
            let Some(failed) = what_failed(&serialized_event) else {
                return;
            };

            (send_handle.abort().await, failed, error)
        }
        LocalEchoContent::Redaction {
            send_handle,
            send_error: Some(error),
            ..
        } => (
            send_handle.abort().await,
            "Could not delete the message",
            error,
        ),
        _ => return,
    };

    match cancelled {
        Ok(true) => matrix.send(Error(format!("{}: {}", failed, reason(&error)))),
        Ok(false) => {}
        Err(err) => error!("could not cancel a stuck request: {}", err),
    }
}

/// What a stuck event failed to do, or nothing for a message, which the
/// room shows with its error until it's deleted.
fn what_failed(content: &SerializableEventContent) -> Option<&'static str> {
    match content.deserialize() {
        Ok(AnyMessageLikeEventContent::RoomMessage(message)) => match message.relates_to {
            Some(Relation::Replacement(_)) => Some("Could not edit the message"),
            _ => None,
        },
        Ok(AnyMessageLikeEventContent::Reaction(_)) => Some("Could not react to the message"),
        _ => Some("Could not send to the room"),
    }
}

fn reason(error: &QueueWedgeError) -> String {
    match error {
        QueueWedgeError::GenericApiError { msg } => msg.clone(),
        error => error.to_string(),
    }
}

fn lost_connection(err: &timeline::Error) -> bool {
    let timeline::Error::EventCacheError(EventCacheError::PaginationError(err)) = err else {
        return false;
    };

    matches!(&**err, matrix_sdk::Error::Http(err) if matches!(**err, HttpError::Reqwest(_)))
}
