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

pub use messages::AfterDownload;
pub use rooms::Moderation;

use std::path::PathBuf;
use std::sync::Arc;

use log::error;
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, OwnedUserId};
use matrix_sdk_ui::sync_service::SyncService;
use once_cell::sync::OnceCell;

use crate::app::App;
use notify::Notify;
use roomcache::{DecoratedRoom, RoomCache};

/// A Matrix client that maintains it's own Tokio runtime
#[derive(Clone)]
pub struct Matrix {
    client: Arc<OnceCell<Client>>,
    sync_service: Arc<OnceCell<SyncService>>,
    subscription_lock: Arc<tokio::sync::Mutex<()>>,
    room_cache: Arc<RoomCache>,
    notify: Arc<Notify>,
    sync_started: Arc<OnceCell<MilliSecondsSinceUnixEpoch>>,
}

impl Default for Matrix {
    fn default() -> Self {
        Matrix {
            client: Arc::new(OnceCell::default()),
            sync_service: Arc::new(OnceCell::default()),
            subscription_lock: Arc::new(tokio::sync::Mutex::default()),
            room_cache: Arc::new(RoomCache::default()),
            notify: Arc::new(Notify::default()),
            sync_started: Arc::new(OnceCell::default()),
        }
    }
}

impl Matrix {
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

    pub fn me(&self) -> OwnedUserId {
        self.client().user_id().unwrap().to_owned()
    }

    pub fn timeline_event(&self, event: AnyTimelineEvent) {
        let matrix = self.clone();

        App::spawn(async move {
            let Some(&sync_started) = matrix.sync_started.get() else {
                return;
            };

            if let Err(e) = matrix
                .notify
                .timeline_event(matrix.client(), event, sync_started)
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
