use crate::matrix::matrix::MessageType::File;
use crate::settings::blur_delay;
use crate::video::get_video_thumbnail;
use std::sync::atomic::{AtomicI64, Ordering};
use std::{fs, thread};

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use debounced::delayed;
use futures::stream::StreamExt;
use log::{error, info};
use matrix_sdk::attachment::AttachmentConfig;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::deserialized_responses::{TimelineEvent, TimelineEventKind};
use matrix_sdk::encryption::verification::{Emoji, SasState, SasVerification, Verification};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::room::{MessagesOptions, Receipts, Room};
use matrix_sdk::ruma::api::client::filter::{
    FilterDefinition, LazyLoadOptions, RoomEventFilter, RoomFilter,
};
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::key::verification::start::{
    OriginalSyncKeyVerificationStartEvent, ToDeviceKeyVerificationStartEvent,
};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::exports::serde_json;
use matrix_sdk::ruma::UserId;
use matrix_sdk::RoomState;
use matrix_sdk::{Client, LoopCtrl, ServerName};
use once_cell::sync::OnceCell;
use rand::rng;
use rand::{distr::Alphanumeric, Rng};
use ruma::events::key::verification::VerificationMethod;
use ruma::events::reaction::ReactionEventContent;

use ruma::events::relation::Annotation;
use ruma::events::room::message::MessageType::Image;
use ruma::events::room::message::MessageType::Video;
use ruma::events::room::message::{AddMentions, ForwardThread, RoomMessageEventContent};
use ruma::events::{
    AnyMessageLikeEvent, AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, AnyTimelineEvent,
    MessageLikeEvent, SyncEphemeralRoomEvent,
};
use ruma::{OwnedEventId, OwnedRoomId, OwnedUserId, UInt};
use serde::{Deserialize, Serialize};
use tokio::runtime::{Handle, Runtime};

use crate::app::App;
use crate::event::Event;
use crate::event::Event::Matui;
use crate::handler::MatuiEvent::{
    Error, ProgressComplete, ProgressStarted, VerificationCompleted, VerificationStarted,
};
use crate::handler::{Batch, MatuiEvent, SyncType};
use crate::matrix::roomcache::{DecoratedRoom, RoomCache};
use crate::spawn::{save_file, view_file};

use super::mime::mime_from_path;
use super::notify::Notify;

/// A Matrix client that maintains it's own Tokio runtime
#[derive(Clone)]
pub struct Matrix {
    rt: Handle,
    client: Arc<OnceCell<Client>>,
    room_cache: Arc<RoomCache>,
    notify: Arc<Notify>,
    focus_key: Arc<AtomicI64>,
}

/// What should we do with the file after we download it?
pub enum AfterDownload {
    View,
    Save,
}

impl Matrix {
    pub fn new(runtime: &Runtime) -> Self {
        Matrix {
            rt: runtime.handle().clone(),
            client: Arc::new(OnceCell::default()),
            room_cache: Arc::new(RoomCache::default()),
            notify: Arc::new(Notify::default()),
            focus_key: Arc::new(AtomicI64::new(0)),
        }
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

    pub fn send(event: MatuiEvent) {
        App::get_sender()
            .send(Matui(event))
            .expect("could not send Matrix event");
    }

    pub fn init(&self) {
        info!("initializing matrix");

        let (_, session_file) = Matrix::dirs();

        if !session_file.exists() {
            Matrix::send(MatuiEvent::LoginRequired);
            return;
        }

        let matrix = self.clone();

        self.rt.spawn(async move {
            Matrix::send(MatuiEvent::SyncStarted(SyncType::Latest));

            let (client, token) = match restore_session(session_file.as_path()).await {
                Ok(tuple) => tuple,
                Err(err) => {
                    Matrix::send(Error(err.to_string()));
                    return;
                }
            };

            info!("session restored");

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            info!("syncing with token {:?}", token);

            if let Err(err) = sync_once(client.clone(), token, &session_file).await {
                Matrix::send(Error(err.to_string()));
                return;
            };

            matrix.room_cache.populate(client).await;

            Matrix::send(MatuiEvent::SyncComplete);
        });
    }

    pub fn login(&self, username: &str, password: &str) {
        let (data_dir, session_file) = Matrix::dirs();
        let user = username.to_string();
        let pass = password.to_string();
        let matrix = self.clone();

        self.rt.spawn(async move {
            Matrix::send(MatuiEvent::LoginStarted);

            let client = match login(&data_dir, &session_file, &user, &pass).await {
                Ok(client) => client,
                Err(err) => {
                    Matrix::send(Error(err.to_string()));
                    return;
                }
            };

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            Matrix::send(MatuiEvent::LoginComplete);
            Matrix::send(MatuiEvent::SyncStarted(SyncType::Initial));

            if let Err(err) = sync_once(client.clone(), None, &session_file).await {
                Matrix::send(Error(err.to_string()));
                return;
            };

            matrix.room_cache.populate(client.clone()).await;

            Matrix::send(MatuiEvent::SyncComplete);

            if let Some(user_id) = client.user_id() {
                match client.encryption().get_user_identity(user_id).await {
                    Ok(Some(identity)) => {
                        if let Err(err) = identity
                            .request_verification_with_methods(vec![VerificationMethod::SasV1])
                            .await
                        {
                            error!("could not request verification: {}", err);
                        } else {
                            info!("verification requested");
                        }
                    }
                    Ok(None) => error!("no user identity"),
                    Err(err) => error!("could not get user identity: {}", err),
                }
            }
        });
    }

    pub fn sync(&self) {
        add_default_handlers(self.client());
        add_verification_handlers(self.client());

        let client = self.client();

        // apparently we only need the token for sync_once
        let sync_settings = build_sync_settings(None);

        self.rt.spawn(async move {
            client
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
                .await
                .expect("could not sync");
        });
    }

    pub fn confirm_verification(&self, sas: SasVerification) {
        self.rt.spawn(async move {
            if let Err(err) = sas.confirm().await {
                error!("could not verify: {}", err);
                Matrix::send(Error(format!("Could not verify: {}", err)));
            }
        });
    }

    pub fn mismatched_verification(&self, sas: SasVerification) {
        self.rt.spawn(async move {
            if let Err(err) = sas.mismatch().await {
                error!("could not cancel SAS verification: {}", err)
            } else {
                info!("verification has been cancelled")
            }
        });
    }

    pub fn fetch_rooms(&self) -> Vec<DecoratedRoom> {
        self.room_cache.get_rooms()
    }

    pub fn fetch_messages(&self, room: Room, cursor: Option<String>) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Fetching more messages.".to_string(), 1000));

            // fetch the actual messages
            let mut options = MessagesOptions::new(Direction::Backward);
            options.limit = UInt::from(25_u16);
            options.from = cursor;

            let messages = match room.messages(options).await {
                Ok(msg) => msg,
                Err(err) => {
                    Matrix::send(Error(err.to_string()));
                    return;
                }
            };

            let unpacked: Vec<AnyTimelineEvent> = messages
                .chunk
                .iter()
                .map(|te| {
                    Matrix::deserialize_event(te, room.room_id().into())
                        .expect("could not deserialize")
                })
                .collect();

            let batch = Batch {
                room: room.clone(),
                events: unpacked,
                cursor: messages.end,
            };

            Matrix::send(MatuiEvent::ProgressComplete);
            Matrix::send(MatuiEvent::TimelineBatch(batch));
        });
    }

    pub fn fetch_room_member(&self, room: Room, id: OwnedUserId) {
        self.rt.spawn(async move {
            match room.get_member(&id).await {
                Ok(Some(member)) => Matrix::send(MatuiEvent::RoomMember(room, member)),
                _ => todo!(),
            }
        });
    }

    pub fn download_content(&self, message: MessageType, after: AfterDownload) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Downloading file.".to_string(), 250));

            let (content_type, request, file_name) = match message {
                Image(content) => (
                    content.info.unwrap().mimetype.unwrap(),
                    MediaRequestParameters {
                        source: content.source,
                        format: MediaFormat::File,
                    },
                    content.body,
                ),
                Video(content) => (
                    content.info.unwrap().mimetype.unwrap(),
                    MediaRequestParameters {
                        source: content.source,
                        format: MediaFormat::File,
                    },
                    content.body,
                ),
                File(content) => (
                    match content.info {
                        Some(c) => match c.mimetype {
                            Some(m) => m,
                            None => "application/octet-stream".to_string(),
                        },
                        None => "application/octet-stream".to_string(),
                    },
                    MediaRequestParameters {
                        source: content.source,
                        format: MediaFormat::File,
                    },
                    content.body,
                ),
                _ => {
                    Matrix::send(Error("Unknown file type.".to_string()));
                    return;
                }
            };

            let handle = match matrix
                .client()
                .media()
                .get_media_file(&request, None, &content_type.parse().unwrap(), true, None)
                .await
            {
                Err(err) => {
                    Matrix::send(Error(err.to_string()));
                    return;
                }
                Ok(mfh) => mfh,
            };

            Matrix::send(ProgressComplete);

            match after {
                AfterDownload::View => {
                    tokio::task::spawn_blocking(move || view_file(handle));
                }
                AfterDownload::Save => match save_file(handle, &file_name) {
                    Err(err) => Matrix::send(Error(err.to_string())),
                    Ok(path) => Matrix::send(MatuiEvent::Confirm(
                        "Download Complete".to_string(),
                        format!("Saved to {}", path.to_str().unwrap()),
                    )),
                },
            };
        });
    }

    pub fn send_text_message(&self, room: Room, message: String) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Sending message.".to_string(), 500));

            if let Err(err) = room
                .send(RoomMessageEventContent::text_markdown(message))
                .await
            {
                Matrix::send(Error(err.to_string()));
            }

            Matrix::send(ProgressComplete);
        });
    }

    pub fn send_reply(&self, room: Room, message: String, in_reply_to: OwnedEventId) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Sending message.".to_string(), 500));

            let in_reply_to = match Matrix::get_room_event(&room, &in_reply_to).await {
                Some(e) => e,
                None => {
                    Matrix::send(Error("Could not find reply event.".to_string()));
                    return;
                }
            };

            let Some(og_in_reply_to) = in_reply_to.as_original() else {
                return;
            };

            let reply = RoomMessageEventContent::text_markdown(message).make_reply_to(
                og_in_reply_to,
                ForwardThread::Yes,
                AddMentions::No,
            );

            if let Err(err) = room.send(reply).await {
                Matrix::send(Error(err.to_string()));
            }

            Matrix::send(ProgressComplete);
        });
    }

    pub fn send_attachements(&self, room: Room, paths: Vec<PathBuf>) {
        let total = paths.len();

        self.rt.spawn(async move {
            for (i, path) in paths.into_iter().enumerate() {
                Matrix::send(ProgressStarted(
                    format!("Uploading {} of {}.", i + 1, total),
                    0,
                ));

                let content_type = mime_from_path(&path);

                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_str()
                    .unwrap_or_default()
                    .to_string();

                let data = match fs::read(path.to_str().unwrap()) {
                    Ok(d) => d,
                    Err(err) => {
                        Matrix::send(Error(err.to_string()));
                        return;
                    }
                };

                // try to grab a thumbnail if it's a video
                let config = if content_type.type_() == "video" {
                    match get_video_thumbnail(&path) {
                        Ok(thumbnail) => AttachmentConfig::new().thumbnail(Some(thumbnail)),
                        _ => AttachmentConfig::new(),
                    }
                } else {
                    AttachmentConfig::new()
                };

                if let Err(err) = room
                    .send_attachment(&name, &content_type, data, config)
                    .await
                {
                    Matrix::send(Error(err.to_string()));
                }

                Matrix::send(ProgressComplete);
            }
        });
    }

    pub fn send_reaction(&self, room: Room, event_id: OwnedEventId, key: String) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Sending reaction.".to_string(), 500));

            if let Err(err) = room
                .send(ReactionEventContent::new(Annotation::new(event_id, key)))
                .await
            {
                Matrix::send(Error(err.to_string()));
            }

            Matrix::send(ProgressComplete);
        });
    }

    pub fn redact_event(&self, room: Room, event_id: OwnedEventId) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Removing.".to_string(), 500));

            if let Err(err) = room.redact(&event_id, None, None).await {
                Matrix::send(Error(err.to_string()));
            }

            Matrix::send(ProgressComplete);
        });
    }

    async fn get_room_event(
        room: &Room,
        id: &OwnedEventId,
    ) -> Option<MessageLikeEvent<RoomMessageEventContent>> {
        match room.event(id, Option::None).await {
            Ok(event) => Matrix::get_room_message_event(room, &event),
            Err(err) => {
                error!("could not get room event: {}", err);
                None
            }
        }
    }

    pub fn deserialize_event(
        event: &TimelineEvent,
        room_id: OwnedRoomId,
    ) -> anyhow::Result<AnyTimelineEvent> {
        match &event.kind {
            TimelineEventKind::Decrypted(decrypted) => Ok(decrypted.event.deserialize()?.into()),
            TimelineEventKind::PlainText { event } => {
                Ok(event.deserialize()?.into_full_event(room_id))
            }
            TimelineEventKind::UnableToDecrypt { event, .. } => {
                Ok(event.deserialize()?.into_full_event(room_id))
            }
        }
    }

    pub fn get_room_message_event(
        room: &Room,
        event: &TimelineEvent,
    ) -> Option<MessageLikeEvent<RoomMessageEventContent>> {
        let Ok(event) = Matrix::deserialize_event(event, room.room_id().to_owned()) else {
            return None;
        };

        let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(event)) = event else {
            return None;
        };

        Some(event)
    }

    pub fn replace_event(
        &self,
        room: Room,
        id: OwnedEventId,
        message: String,
        in_reply_to: Option<OwnedEventId>,
    ) {
        self.rt.spawn(async move {
            Matrix::send(ProgressStarted("Editing message.".to_string(), 500));

            let Some(event) = Matrix::get_room_event(&room, &id).await else {
                return;
            };

            let Some(event) = event.as_original() else {
                return;
            };

            let reply_event = match in_reply_to {
                Some(id) => Matrix::get_room_event(&room, &id).await,
                None => None,
            };

            let reply_event = match reply_event {
                Some(e) => e.as_original().cloned(),
                None => None,
            };

            info!("reply event: {:?}", reply_event);

            if let Err(err) = room
                .send(
                    RoomMessageEventContent::text_markdown(message)
                        .make_replacement(event, reply_event.as_ref()),
                )
                .await
            {
                Matrix::send(Error(err.to_string()));
            }

            Matrix::send(ProgressComplete);
        });
    }

    pub fn me(&self) -> OwnedUserId {
        self.client().user_id().unwrap().to_owned()
    }

    pub fn timeline_event(&self, event: AnyTimelineEvent) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix
                .room_cache
                .timeline_event(matrix.client(), &event)
                .await;

            if let Err(e) = matrix.notify.timeline_event(matrix.client(), event).await {
                error!("could not send notification: {}", e);
            }
        });
    }

    pub fn focus_event(&self) {
        self.notify.focus_event();

        let delay = blur_delay();

        if delay <= 0 {
            return;
        }

        let focus_key = self.focus_key.fetch_add(1, Ordering::Relaxed) + 1;
        let old_focus_key = self.focus_key.clone();

        self.rt.spawn(async move {
            delayed((), Duration::from_secs(delay.try_into().unwrap())).await;

            if focus_key == old_focus_key.load(Ordering::Relaxed) {
                info!("sending synthetic blur event");

                App::get_sender()
                    .send(Event::Blur)
                    .expect("could not send blur event");
            }
        });
    }

    pub fn blur_event(&self) {
        self.focus_key.store(0, Ordering::Relaxed);
        self.notify.blur_event();
    }

    pub fn room_visit_event(&self, room: Room) {
        self.notify.room_visit_event(room.clone());
        self.room_cache.room_visit_event(room);
    }

    pub fn read_to(&self, room: Room, to: OwnedEventId) {
        let receipts = Receipts::new()
            .fully_read_marker(Some(to.clone()))
            .public_read_receipt(Some(to));

        self.rt.spawn(async move {
            if let Err(e) = room.send_multiple_receipts(receipts).await {
                error!("could not send read receipt: {}", e);
            }
        });
    }

    pub fn typing_notification(&self, room: Room, typing: bool) {
        self.rt.spawn(async move {
            if let Err(e) = room.typing_notice(typing).await {
                error!("could not send typing notice: {}", e);
            }
        });
    }

    pub fn begin_typing(&self, room: Room) -> Sender<()> {
        let (send, recv) = channel();
        let matrix = self.clone();

        thread::spawn(move || {
            while let Err(TryRecvError::Empty) = recv.try_recv() {
                matrix.typing_notification(room.clone(), true);
                thread::sleep(Duration::from_millis(1000));
            }
        });

        send
    }

    pub fn end_typing(&self, room: Room, send: Sender<()>) {
        send.send(()).expect("could not stop thread");
        self.typing_notification(room, false);
    }
}

/// The data needed to re-build a client.
#[derive(Debug, Serialize, Deserialize)]
struct ClientSession {
    homeserver: String,
    db_path: PathBuf,
    passphrase: String,
}

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct FullSession {
    client_session: ClientSession,
    user_session: MatrixSession,
    sync_token: Option<String>,
}

async fn restore_session(session_file: &Path) -> anyhow::Result<(Client, Option<String>)> {
    let serialized_session = fs::read_to_string(session_file)?;

    let FullSession {
        client_session,
        user_session,
        sync_token,
    } = serde_json::from_str(&serialized_session)?;

    let homeserver = <&ServerName>::try_from(client_session.homeserver.as_str())?;

    // Build the client with the previous settings from the session.
    let client = Client::builder()
        .server_name(homeserver)
        .sqlite_store(client_session.db_path, Some(&client_session.passphrase))
        .build()
        .await?;

    // Restore the Matrix user session.
    client.restore_session(user_session).await?;

    Ok((client, sync_token))
}

async fn login(
    data_dir: &Path,
    session_file: &Path,
    id: &str,
    password: &str,
) -> anyhow::Result<Client> {
    let id = <&UserId>::try_from(id)?;
    let username = id.localpart();

    let (client, client_session) = build_client(data_dir, id).await?;

    let matrix_auth = client.matrix_auth();

    matrix_auth
        .login_username(username, password)
        .initial_device_display_name("Matui")
        .send()
        .await?;

    let user_session = matrix_auth
        .session()
        .context("Your logged-in user has no session.")?;

    let serialized_session = serde_json::to_string(&FullSession {
        client_session,
        user_session,
        sync_token: None,
    })?;

    fs::write(session_file, serialized_session)?;

    Ok(client)
}

async fn build_client(data_dir: &Path, id: &UserId) -> anyhow::Result<(Client, ClientSession)> {
    let db_subfolder: String = (&mut rng())
        .sample_iter(Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();

    let db_path = data_dir.join(db_subfolder.as_str());

    // Generate a random passphrase.
    let passphrase: String = (&mut rng())
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let client = Client::builder()
        .server_name(id.server_name())
        .sqlite_store(&db_path, Some(passphrase.as_str()))
        .build()
        .await?;

    Ok((
        client,
        ClientSession {
            homeserver: id.server_name().host().to_string(),
            db_path,
            passphrase,
        },
    ))
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

async fn sync_once(
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

fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file)?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&full_session)?;
    fs::write(session_file, serialized_session)?;

    Ok(())
}

fn add_default_handlers(client: Client) {
    client.add_event_handler(|event: AnySyncTimelineEvent, room: Room| async move {
        App::get_sender()
            .send(Matui(MatuiEvent::Timeline(
                event.into_full_event(room.room_id().into()),
            )))
            .expect("could not send timeline event");
    });

    client.add_event_handler(|event: AnySyncEphemeralRoomEvent, room: Room| async move {
        if room.state() != RoomState::Joined {
            return;
        }

        match event {
            AnySyncEphemeralRoomEvent::Typing(SyncEphemeralRoomEvent { content: c }) => {
                App::get_sender()
                    .send(Matui(MatuiEvent::Typing(room, c.user_ids)))
                    .expect("could not send typing event");
            }
            AnySyncEphemeralRoomEvent::Receipt(SyncEphemeralRoomEvent { content: c }) => {
                App::get_sender()
                    .send(Matui(MatuiEvent::Receipt(room, c)))
                    .expect("could not send typing event");
            }
            _ => {}
        };
    });
}

fn add_verification_handlers(client: Client) {
    client.add_event_handler(
        |ev: ToDeviceKeyVerificationRequestEvent, client: Client| async move {
            let request = match client
                .encryption()
                .get_verification_request(&ev.sender, &ev.content.transaction_id)
                .await
            {
                Some(req) => req,
                None => {
                    error!("could not create request");
                    return;
                }
            };

            request
                .accept()
                .await
                .expect("Can't accept verification request");
        },
    );

    client.add_event_handler(
        |ev: ToDeviceKeyVerificationStartEvent, client: Client| async move {
            if let Some(Verification::SasV1(sas)) = client
                .encryption()
                .get_verification(&ev.sender, ev.content.transaction_id.as_str())
                .await
            {
                tokio::spawn(sas_verification_handler(sas, App::get_sender()));
            };
        },
    );

    client.add_event_handler(
        |ev: OriginalSyncRoomMessageEvent, client: Client| async move {
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype {
                let request = match client
                    .encryption()
                    .get_verification_request(&ev.sender, &ev.event_id)
                    .await
                {
                    Some(req) => req,
                    None => {
                        error!("could not create request");
                        return;
                    }
                };

                request
                    .accept()
                    .await
                    .expect("Can't accept verification request");
            }
        },
    );

    client.add_event_handler(
        |ev: OriginalSyncKeyVerificationStartEvent, client: Client| async move {
            if let Some(Verification::SasV1(sas)) = client
                .encryption()
                .get_verification(&ev.sender, ev.content.relates_to.event_id.as_str())
                .await
            {
                tokio::spawn(sas_verification_handler(sas, App::get_sender()));
            }
        },
    );
}

async fn sas_verification_handler(sas: SasVerification, sender: Sender<Event>) {
    sas.accept().await.unwrap();

    let mut stream = sas.changes();

    while let Some(state) = stream.next().await {
        match state {
            SasState::KeysExchanged {
                emojis,
                decimals: _,
            } => {
                info!("verification keys exchanged");

                let emoji_slice = emojis.expect("only emoji verification is supported").emojis;

                sender
                    .send(Matui(VerificationStarted(sas.clone(), emoji_slice)))
                    .expect("could not send sas started event");
            }
            SasState::Done { .. } => {
                info!("verification done");

                sender
                    .send(Matui(VerificationCompleted))
                    .expect("could not send sas completed event");
            }
            SasState::Started { .. } => info!("verification started"),
            SasState::Accepted { .. } => info!("verification accepted"),
            SasState::Confirmed => info!("verification confirmed"),
            SasState::Cancelled(_) => info!("verification cancelled"),
            SasState::Created { .. } => info!("verification created"),
        }
    }
}

pub fn pad_emoji(emoji: &str) -> String {
    // These are emojis that need VARIATION-SELECTOR-16 (U+FE0F) so that they are
    // rendered with coloured glyphs. For these, we need to add an extra
    // space after them so that they are rendered properly in terminals.
    const VARIATION_SELECTOR_EMOJIS: [&str; 8] = ["☁️", "❤️", "☂️", "✏️", "✂️", "☎️", "✈️", "‼️"];

    // Hack to make terminals behave properly when one of the above is printed.
    if VARIATION_SELECTOR_EMOJIS.contains(&emoji) {
        format!("{emoji} ")
    } else {
        emoji.to_owned()
    }
}

pub fn center_emoji(emoji: &str) -> String {
    let emoji = pad_emoji(emoji);

    // This is a trick to account for the fact that emojis are wider than other
    // monospace characters.
    let placeholder = ".".repeat(2);

    format!("{placeholder:^6}").replace(&placeholder, &emoji)
}

pub fn format_emojis(emojis: [Emoji; 7]) -> String {
    let emojis: Vec<_> = emojis.iter().map(|e| e.symbol).collect();

    emojis
        .iter()
        .map(|e| center_emoji(e))
        .collect::<Vec<_>>()
        .join("")
}
