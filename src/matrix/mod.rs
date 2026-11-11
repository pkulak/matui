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
mod timeline;
pub mod username;
mod verification;

pub use messages::AfterDownload;
pub use rooms::Moderation;

use std::future::Future;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use log::error;
use matrix_sdk::Client;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk::ruma::{
    EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId, OwnedUserId, RoomId,
};
use matrix_sdk_ui::sync_service::SyncService;
use once_cell::sync::OnceCell;
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::app::NoticeId;
use crate::event::Event;
use crate::handler::MatuiEvent;
use notify::Notify;
use roomcache::{DecoratedRoom, RoomCache};

/// Notices withdrawn when this drops, however their work ends.
pub struct Notice {
    id: NoticeId,
    sender: Sender<Event>,
}

impl Notice {
    pub fn id(&self) -> NoticeId {
        self.id
    }
}

impl Drop for Notice {
    fn drop(&mut self) {
        // the app may already be gone
        let _ = self
            .sender
            .send(Event::Matui(MatuiEvent::Withdraw(self.id)));
    }
}

/// What the user is looking at: the open room, and the thread open in it,
/// while the terminal has focus. Terminals that don't report focus always
/// count as focused.
struct Attention {
    focused: bool,
    room: Option<OwnedRoomId>,
    thread: Option<OwnedEventId>,
}

impl Attention {
    fn looking_at(&self) -> Option<&RoomId> {
        self.room.as_deref().filter(|_| self.focused)
    }

    /// Whether the user is looking at this room, or at this thread in it.
    /// The room counts as seen under an open thread, as it counts as read.
    fn sees(&self, room: &RoomId, thread: Option<&EventId>) -> bool {
        self.looking_at() == Some(room) && (thread.is_none() || self.thread.as_deref() == thread)
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
    attention: Arc<Mutex<Attention>>,
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
            attention: Arc::new(Mutex::new(Attention {
                focused: true,
                room: None,
                thread: None,
            })),
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
    pub fn progress(&self, text: &str, delay: u64) -> Notice {
        let notice = self.notice();
        self.send(MatuiEvent::ProgressStarted(
            notice.id(),
            text.to_string(),
            delay,
        ));

        notice
    }

    /// A new ID for notices, which are withdrawn when the returned handle
    /// drops.
    #[must_use]
    pub fn notice(&self) -> Notice {
        Notice {
            id: NoticeId::next(),
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
        // the room being looked at is read, whatever the server's count says
        if let Some(room) = self.looking_at() {
            self.room_cache.visit(&room);
        }

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

    /// The room the user is looking at, if any.
    pub fn looking_at(&self) -> Option<OwnedRoomId> {
        let attention = self.attention.lock().unwrap();
        attention.looking_at().map(ToOwned::to_owned)
    }

    /// Whether the user is looking at this room, or at this thread in it.
    pub fn sees(&self, room: &RoomId, thread: Option<&EventId>) -> bool {
        self.attention.lock().unwrap().sees(room, thread)
    }

    pub fn focus_event(&self) {
        let mut attention = self.attention.lock().unwrap();
        attention.focused = true;
        let room = attention.room.clone();
        let thread = attention.thread.clone();
        drop(attention);

        if let Some(room) = room {
            self.notify.close_notification(self, room.as_str());
        }

        if let Some(root) = thread {
            self.notify.close_notification(self, root.as_str());
        }
    }

    pub fn blur_event(&self) {
        self.attention.lock().unwrap().focused = false;
    }

    /// The app opened this room, which the user is looking at if the
    /// terminal has focus.
    pub fn room_opened_event(&self, room: &Room) {
        let mut attention = self.attention.lock().unwrap();
        attention.room = Some(room.room_id().to_owned());
        attention.thread = None;
        drop(attention);

        self.notify
            .close_notification(self, room.room_id().as_str());
    }

    /// The app opened this thread over the open room.
    pub fn thread_opened_event(&self, root: &EventId) {
        self.attention.lock().unwrap().thread = Some(root.to_owned());
        self.notify.close_notification(self, root.as_str());
    }

    /// The app went back from a thread to its room.
    pub fn thread_closed_event(&self) {
        self.attention.lock().unwrap().thread = None;
    }

    /// The user read this room, here or in another session.
    pub fn room_visit_event(&self, room: Room) {
        self.notify
            .close_notification(self, room.room_id().as_str());
        self.room_cache.visit(room.room_id());
    }
}
