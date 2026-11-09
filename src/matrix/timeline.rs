use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use log::error;
use matrix_sdk::HttpError;
use matrix_sdk::event_cache::EventCacheError;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::OwnedEventId;
use matrix_sdk::ruma::events::room::message::MessageType;
use matrix_sdk_ui::Timeline;
use matrix_sdk_ui::timeline::{self, RoomExt, TimelineEventItemId};
use tokio::task::AbortHandle;

use super::Matrix;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::Error;

impl Matrix {
    /// Follow the room's timeline, sending all its items after every change.
    /// Abort the returned handle to stop.
    pub fn follow_timeline(&self, room: Room) -> AbortHandle {
        let matrix = self.clone();

        let task = self.spawn(async move {
            let timeline = match room.timeline_builder().build().await {
                Ok(timeline) => Arc::new(timeline),
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                    return;
                }
            };

            let (mut items, stream) = timeline.subscribe().await;
            let mut stream = pin!(stream);
            matrix.send(MatuiEvent::TimelineItems(timeline.clone(), items.clone()));

            while let Some(diffs) = stream.next().await {
                diffs.into_iter().for_each(|d| d.apply(&mut items));
                matrix.send(MatuiEvent::TimelineItems(timeline.clone(), items.clone()));
            }
        });

        task.abort_handle()
    }

    /// Page the timeline back by about this many events. Its items arrive
    /// separately, before or after it's done. Abort the returned handle to
    /// stop, including any retries.
    pub fn paginate(&self, timeline: Arc<Timeline>, events: u16) -> AbortHandle {
        const RETRY_DELAY: Duration = Duration::from_secs(3);

        let matrix = self.clone();

        let task = self.spawn(async move {
            let room_id = timeline.room().room_id().to_owned();

            loop {
                match timeline.paginate_backwards(events).await {
                    Ok(hit_start) => {
                        matrix.send(MatuiEvent::Paginated(room_id, hit_start));
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

    /// Add our reaction to the event, or take it back if it's there.
    pub fn toggle_reaction(&self, timeline: Arc<Timeline>, id: OwnedEventId, key: String) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Reacting.", 500);
            let id = TimelineEventItemId::EventId(id);

            if let Err(err) = timeline.toggle_reaction(&id, &key).await {
                matrix.send(Error(err.to_string()));
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

fn lost_connection(err: &timeline::Error) -> bool {
    let timeline::Error::EventCacheError(EventCacheError::PaginationError(err)) = err else {
        return false;
    };

    matches!(&**err, matrix_sdk::Error::Http(err) if matches!(**err, HttpError::Reqwest(_)))
}
