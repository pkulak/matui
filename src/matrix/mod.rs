mod decryption;
pub mod emoji;
mod login;
pub mod mentions;
mod messages;
pub mod mime;
pub mod notify;
pub mod roomcache;
mod rooms;
mod session;
mod sync;
pub mod username;
mod verification;

pub use decryption::RoomKey;
pub use messages::AfterDownload;
pub use rooms::Moderation;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Sender;

use log::error;
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedUserId};
use matrix_sdk_ui::sync_service::SyncService;
use once_cell::sync::OnceCell;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::event::Event;
use crate::handler::MatuiEvent;
use crate::widgets::progress::ProgressId;
use notify::Notify;
use roomcache::{DecoratedRoom, RoomCache};

/// A progress popup that closes when this drops, however its work ends.
pub struct InProgress {
    id: ProgressId,
    sender: Sender<Event>,
}

impl Drop for InProgress {
    fn drop(&mut self) {
        // the app may already be gone
        let _ = self
            .sender
            .send(Event::Matui(MatuiEvent::ProgressComplete(self.id)));
    }
}

/// A Matrix client that maintains it's own Tokio runtime
#[derive(Clone)]
pub struct Matrix {
    client: Arc<OnceCell<Client>>,
    sync_service: Arc<OnceCell<SyncService>>,
    subscription_lock: Arc<tokio::sync::Mutex<()>>,
    room_cache: Arc<RoomCache>,
    notify: Arc<Notify>,
    sync_started: Arc<OnceCell<MilliSecondsSinceUnixEpoch>>,
    sender: Sender<Event>,
    handle: Handle,
}

impl Matrix {
    pub fn new(sender: Sender<Event>, handle: Handle) -> Self {
        Matrix {
            client: Arc::new(OnceCell::default()),
            sync_service: Arc::new(OnceCell::default()),
            subscription_lock: Arc::new(tokio::sync::Mutex::default()),
            room_cache: Arc::new(RoomCache::default()),
            notify: Arc::new(Notify::default()),
            sync_started: Arc::new(OnceCell::default()),
            sender,
            handle,
        }
    }

    pub fn send(&self, event: MatuiEvent) {
        self.sender
            .send(Event::Matui(event))
            .expect("could not send event");
    }

    /// Show a progress popup until the returned handle drops.
    #[must_use]
    pub fn progress(&self, text: &str, delay: u64) -> InProgress {
        let id = ProgressId::next();
        self.send(MatuiEvent::ProgressStarted(id, text.to_string(), delay));

        InProgress {
            id,
            sender: self.sender.clone(),
        }
    }

    pub fn spawn<F>(&self, future: F) -> JoinHandle<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(future)
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }

    fn dirs() -> (PathBuf, PathBuf) {
        let data_dir = dirs::data_dir()
            .expect("no data directory found")
            .join("matui");

        let session_file = data_dir.join("session");
        (data_dir, session_file)
    }

    fn client(&self) -> Client {
        self.client
            .get()
            .expect("client expected but not set")
            .to_owned()
    }

    pub fn wrap_room(&self, room: &Room) -> Option<DecoratedRoom> {
        self.room_cache.wrap(room)
    }

    pub fn fetch_rooms(&self) -> Vec<DecoratedRoom> {
        self.room_cache.get_rooms()
    }

    #[cfg(test)]
    pub fn set_client(&self, client: Client) {
        self.client.set(client).expect("client already set");
    }

    /// Put a room in the room list, as the room list sync would.
    #[cfg(test)]
    pub async fn add_room(&self, room: Room) {
        self.room_cache.add_room(room).await;
    }

    pub fn me(&self) -> OwnedUserId {
        self.client().user_id().unwrap().to_owned()
    }

    pub fn timeline_event(&self, event: AnyTimelineEvent) {
        let matrix = self.clone();

        self.spawn(async move {
            let Some(&sync_started) = matrix.sync_started.get() else {
                return;
            };

            if let Err(e) = matrix
                .notify
                .timeline_event(&matrix, event, sync_started)
                .await
            {
                error!("could not send notification: {}", e);
            }
        });
    }

    pub fn focus_event(&self) {
        self.notify.focus_event();
    }

    pub fn blur_event(&self) {
        self.notify.blur_event();
    }

    pub fn room_visit_event(&self, room: Room) {
        self.notify.room_visit_event(room.clone());
        self.room_cache.room_visit_event(room);
    }
}
