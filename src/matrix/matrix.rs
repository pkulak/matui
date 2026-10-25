use matrix_sdk::ruma::events::room::message::MessageType::File;

use crate::media::get_attachment_info;
use std::{fs, thread};

use std::future::IntoFuture;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Sender, TryRecvError, channel};
use std::time::Duration;

use anyhow::{Context, bail};
use futures::stream::StreamExt;
use log::{error, info};
use matrix_sdk::RoomState;
use matrix_sdk::attachment::AttachmentConfig;
use matrix_sdk::authentication::AuthSession;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::authentication::oauth::qrcode::{GeneratedQrProgress, GrantLoginProgress};
use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::authentication::oauth::{ClientId, OAuthSession, UserSession};
use matrix_sdk::deserialized_responses::{TimelineEvent, TimelineEventKind};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::encryption::verification::{
    Emoji, SasState, SasVerification, Verification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::media::{MediaFormat, MediaRequestParameters};
use matrix_sdk::reqwest::Url;
use matrix_sdk::room::{MessagesOptions, Receipts, Room};
use matrix_sdk::ruma::api::Direction;
use matrix_sdk::ruma::api::client::directory::get_public_rooms_filtered;
use matrix_sdk::ruma::api::client::room::create_room::v3::RoomPreset;
use matrix_sdk::ruma::api::client::room::{Visibility, create_room};
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::reaction::ReactionEventContent;
use matrix_sdk::ruma::events::room::encryption::RoomEncryptionEventContent;
use matrix_sdk::ruma::events::room::member::{MembershipState, StrippedRoomMemberEvent};
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};
use matrix_sdk::ruma::exports::serde_json;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::utils::local_server::{LocalServerBuilder, LocalServerResponse};
use matrix_sdk::{Client, ServerName, SessionChange};
use matrix_sdk_ui::room_list_service::State as RoomListState;
use matrix_sdk_ui::sync_service::{State as SyncState, SyncService};
use once_cell::sync::OnceCell;
use rand::rng;
use rand::{RngExt, distr::Alphanumeric};

use matrix_sdk::ruma::events::relation::Annotation;
use matrix_sdk::ruma::events::room::message::MessageType::Audio;
use matrix_sdk::ruma::events::room::message::MessageType::Image;
use matrix_sdk::ruma::events::room::message::MessageType::Video;
use matrix_sdk::ruma::events::room::message::{
    AddMentions, ForwardThread, ReplyWithinThread, RoomMessageEventContent, TextMessageEventContent,
};
use matrix_sdk::ruma::events::{
    AnyMessageLikeEvent, AnySyncEphemeralRoomEvent, AnySyncTimelineEvent, AnyTimelineEvent,
    EmptyStateKey, InitialStateEvent, MessageLikeEvent, SyncEphemeralRoomEvent,
};
use matrix_sdk::ruma::{
    MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId, OwnedRoomOrAliasId, OwnedUserId,
    RoomOrAliasId, UInt,
};
use serde::{Deserialize, Serialize};

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
    client: Arc<OnceCell<Client>>,
    sync_service: Arc<OnceCell<SyncService>>,
    subscription_lock: Arc<tokio::sync::Mutex<()>>,
    room_cache: Arc<RoomCache>,
    notify: Arc<Notify>,
    sync_started: Arc<OnceCell<MilliSecondsSinceUnixEpoch>>,
}

/// What should we do with the file after we download it?
pub enum AfterDownload {
    View,
    Save,
}

#[derive(Clone, Copy)]
pub enum Moderation {
    Ban,
    Unban,
    Kick,
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

    pub fn init(&self) {
        info!("initializing matrix");

        let (_, session_file) = Matrix::dirs();

        if !session_file.exists() {
            App::send(MatuiEvent::LoginRequired);
            return;
        }

        let matrix = self.clone();

        App::spawn(async move {
            App::send(MatuiEvent::SyncStarted(SyncType::Latest));

            let client = match restore_session(session_file.as_path()).await {
                Ok(client) => client,
                Err(err) => {
                    App::send(Error(err.to_string()));
                    return;
                }
            };

            info!("session restored");

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            matrix.watch_session(client.clone());
            matrix.start_sync(client).await;
        });
    }

    pub fn login(&self, homeserver: &str, username: &str, password: &str) {
        let (data_dir, session_file) = Matrix::dirs();
        let homeserver = homeserver.to_string();
        let user = username.to_string();
        let pass = password.to_string();
        let matrix = self.clone();

        App::spawn(async move {
            App::send(MatuiEvent::LoginStarted);

            match login(&data_dir, &session_file, &homeserver, &user, &pass).await {
                Ok(client) => matrix.finish_signin(client).await,
                Err(err) => App::send(Error(err.to_string())),
            }
        });
    }

    /// Start the login flow by deciding whether this homeserver supports
    /// OIDC. OIDC homeservers go straight to browser login; everything else
    /// asks for username/password.
    pub fn start_login(&self, homeserver: String) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Checking homeserver.".to_string(), 0));

            match supports_oidc(&homeserver).await {
                Ok(true) => matrix.login_oauth(homeserver),
                Ok(false) => App::send(MatuiEvent::PasswordLoginRequired(homeserver)),
                Err(err) => App::send(Error(err.to_string())),
            }
        });
    }

    pub fn grant_login_with_qr(&self) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Preparing QR login.".to_string(), 0));

            let client = matrix.client();
            let oauth = client.oauth();
            let grant = oauth.grant_login_with_qr_code().generate();
            let mut progress = grant.subscribe_to_progress();

            let progress_task = tokio::spawn(async move {
                while let Some(state) = progress.next().await {
                    match state {
                        GrantLoginProgress::Starting => {}
                        GrantLoginProgress::EstablishingSecureChannel(
                            GeneratedQrProgress::QrReady(data),
                        ) => match render_qr_code(data.to_bytes()) {
                            Ok(code) => App::send(MatuiEvent::QrLoginCode(code)),
                            Err(err) => App::send(Error(err.to_string())),
                        },
                        GrantLoginProgress::EstablishingSecureChannel(
                            GeneratedQrProgress::QrScanned(sender),
                        ) => App::send(MatuiEvent::QrLoginScanned(sender)),
                        GrantLoginProgress::WaitingForAuth {
                            verification_uri,
                            continuation_sender,
                        } => {
                            let url = verification_uri.to_string();
                            let _ = open::that_detached(&url);
                            App::send(MatuiEvent::QrLoginAuth(url));

                            if let Err(err) = continuation_sender.confirm().await {
                                App::send(Error(err.to_string()));
                            }
                        }
                        GrantLoginProgress::SyncingSecrets => {
                            App::send(ProgressStarted("Syncing secrets.".to_string(), 0));
                        }
                        GrantLoginProgress::Done => {}
                    }
                }
            });

            match grant.await {
                Ok(()) => App::send(MatuiEvent::QrLoginDone),
                Err(err) => App::send(Error(err.to_string())),
            }

            progress_task.abort();
        });
    }

    pub fn login_oauth(&self, homeserver: String) {
        let (data_dir, session_file) = Matrix::dirs();
        let matrix = self.clone();

        App::spawn(async move {
            App::send(MatuiEvent::LoginStarted);

            match login_oauth_flow(&data_dir, &session_file, &homeserver).await {
                Ok(Some(client)) => matrix.finish_signin(client).await,
                // cancelled from the waiting popup; back to the form
                Ok(None) => App::send(MatuiEvent::LoginRequired),
                Err(err) => App::send(Error(err.to_string())),
            }
        });
    }

    /// Everything that happens after a fresh login, no matter the flow.
    async fn finish_signin(&self, client: Client) {
        self.client
            .set(client.clone())
            .expect("could not set client");

        self.watch_session(client.clone());

        App::send(MatuiEvent::LoginComplete);
        App::send(MatuiEvent::SyncStarted(SyncType::Initial));

        if self.start_sync(client).await {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            self.verify();
        }
    }

    /// Start syncing forever, and wait for the room list to load for the
    /// first time. Returns false if it never does.
    async fn start_sync(&self, client: Client) -> bool {
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

        let mut room_list_state = sync_service.room_list_service().state();

        // anything older than this doesn't notify
        let _ = self.sync_started.set(MilliSecondsSinceUnixEpoch::now());
        sync_service.start().await;

        while room_list_state.get() != RoomListState::Running {
            if room_list_state.next().await.is_none() {
                return false;
            }
        }

        self.room_cache.populate(client.clone()).await;
        App::send(MatuiEvent::SyncComplete);

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

    /// Keep the session file up to date as tokens refresh (OAuth access
    /// tokens are short-lived).
    fn watch_session(&self, client: Client) {
        App::spawn(async move {
            let mut changes = client.subscribe_to_session_changes();

            while let Ok(change) = changes.recv().await {
                match change {
                    SessionChange::TokensRefreshed => {
                        let (_, session_file) = Matrix::dirs();

                        if let Err(err) = persist_auth(&session_file, &client) {
                            error!("could not persist refreshed tokens: {}", err);
                        }
                    }
                    // future improvement: wipe local data and return to
                    // the signin form
                    SessionChange::UnknownToken(_) => App::send(Error(
                        "Your session has expired. Please sign in again.".to_string(),
                    )),
                }
            }
        });
    }

    pub fn verify(&self) {
        let matrix = self.clone();

        App::spawn(async move {
            let client = matrix.client();

            if let Some(user_id) = client.user_id() {
                match client.encryption().get_user_identity(user_id).await {
                    Ok(Some(identity)) => {
                        match identity
                            .request_verification_with_methods(vec![VerificationMethod::SasV1])
                            .await
                        {
                            Err(err) => error!("could not request verification: {}", err),
                            Ok(request) => matrix.wait_on_verify_request(request),
                        }
                    }
                    Ok(None) => error!("no user identity"),
                    Err(err) => error!("could not get user identity: {}", err),
                };
            };
        });
    }

    fn wait_on_verify_request(&self, request: VerificationRequest) {
        info!("verification requested");

        App::spawn(async move {
            let mut stream = request.changes();

            while let Some(state) = stream.next().await {
                if let VerificationRequestState::Transitioned {
                    verification: Verification::SasV1(s),
                } = state
                {
                    App::spawn(sas_verification_handler(s, App::get_sender()));
                    break;
                }
            }
        });
    }

    pub fn recover(&self, key: &str) {
        let matrix = self.clone();
        let key = key.to_string();

        App::spawn(async move {
            match matrix.client().encryption().recovery().recover(&key).await {
                Ok(()) => App::send(MatuiEvent::ProgressComplete),
                Err(_) => App::send(MatuiEvent::Error(
                    "Could not recover encryption keys.".to_string(),
                )),
            }
        });
    }

    pub fn confirm_verification(&self, sas: SasVerification) {
        App::spawn(async move {
            if let Err(err) = sas.confirm().await {
                error!("could not verify: {}", err);
                App::send(Error(format!("Could not verify: {}", err)));
            }
        });
    }

    pub fn mismatched_verification(&self, sas: SasVerification) {
        App::spawn(async move {
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

    pub fn fetch_messages(&self, room: Room, cursor: Option<String>, limit: usize) {
        App::spawn(async move {
            // fetch the actual messages
            let mut options = MessagesOptions::new(Direction::Backward);
            options.limit =
                UInt::new(limit.try_into().unwrap_or(25)).expect("invalid paging limit");
            options.from = cursor;

            let messages = match room.messages(options).await {
                Ok(msg) => msg,
                Err(err) => {
                    App::send(Error(err.to_string()));
                    return;
                }
            };

            let unpacked: Vec<AnyTimelineEvent> = messages
                .chunk
                .iter()
                .filter_map(|te| {
                    Matrix::deserialize_event(te, room.room_id().into())
                        .map_err(|err| error!("could not deserialize event: {}", err))
                        .ok()
                })
                .collect();

            let batch = Batch {
                room: room.clone(),
                events: unpacked,
                cursor: messages.end,
            };

            App::send(MatuiEvent::TimelineBatch(batch));
        });
    }

    pub fn fetch_room_member(&self, room: Room, id: OwnedUserId) {
        App::spawn(async move {
            match room.get_member(&id).await {
                Ok(Some(member)) => App::send(MatuiEvent::RoomMember(room, member)),
                Ok(None) => info!("no such room member: {}", id),
                Err(err) => error!("could not fetch room member {}: {}", id, err),
            }
        });
    }

    pub fn download_content(&self, message: MessageType, after: AfterDownload) {
        let matrix = self.clone();
        let octets = "application/octet-stream".to_string();

        App::spawn(async move {
            App::send(ProgressStarted("Downloading file.".to_string(), 250));

            let (content_type, request, file_name) = match message {
                Image(content) => {
                    let file_name = content.filename().to_string();
                    (
                        content
                            .info
                            .and_then(|info| info.mimetype)
                            .unwrap_or(octets),
                        MediaRequestParameters {
                            source: content.source,
                            format: MediaFormat::File,
                        },
                        file_name,
                    )
                }
                Video(content) => {
                    let file_name = content.filename().to_string();
                    (
                        content
                            .info
                            .and_then(|info| info.mimetype)
                            .unwrap_or(octets),
                        MediaRequestParameters {
                            source: content.source,
                            format: MediaFormat::File,
                        },
                        file_name,
                    )
                }
                Audio(content) => {
                    let file_name = content.filename().to_string();
                    (
                        content
                            .info
                            .and_then(|info| info.mimetype)
                            .unwrap_or(octets),
                        MediaRequestParameters {
                            source: content.source,
                            format: MediaFormat::File,
                        },
                        file_name,
                    )
                }
                File(content) => {
                    let file_name = content.filename().to_string();
                    (
                        content
                            .info
                            .and_then(|info| info.mimetype)
                            .unwrap_or(octets),
                        MediaRequestParameters {
                            source: content.source,
                            format: MediaFormat::File,
                        },
                        file_name,
                    )
                }
                _ => {
                    App::send(Error("Unknown file type.".to_string()));
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
                    App::send(Error(err.to_string()));
                    return;
                }
                Ok(mfh) => mfh,
            };

            App::send(ProgressComplete);

            match after {
                AfterDownload::View => {
                    tokio::task::spawn_blocking(move || view_file(handle));
                }
                AfterDownload::Save => match save_file(handle, &file_name).await {
                    Err(err) => App::send(Error(err.to_string())),
                    Ok(None) => {}
                    Ok(Some(path)) => App::send(MatuiEvent::Confirm(
                        "Download Complete".to_string(),
                        format!("Saved to {}", path.to_str().unwrap()),
                    )),
                },
            };
        });
    }

    pub fn send_text_message(&self, room: Room, message: String) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            if let Err(err) = room
                .send(RoomMessageEventContent::text_markdown(message))
                .await
            {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_reply(&self, room: Room, message: String, in_reply_to: OwnedEventId) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            let in_reply_to = match Matrix::get_room_event(&room, &in_reply_to).await {
                Some(e) => e,
                None => {
                    App::send(Error("Could not find reply event.".to_string()));
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
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_thread_message(
        &self,
        room: Room,
        message: String,
        target: OwnedEventId,
        is_reply: ReplyWithinThread,
    ) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending message.".to_string(), 500));

            let target = match Matrix::get_room_event(&room, &target).await {
                Some(e) => e,
                None => {
                    App::send(Error("Could not find thread event.".to_string()));
                    return;
                }
            };

            let Some(og_target) = target.as_original() else {
                return;
            };

            let reply = RoomMessageEventContent::text_markdown(message).make_for_thread(
                og_target,
                is_reply,
                AddMentions::No,
            );

            if let Err(err) = room.send(reply).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn send_attachments(&self, room: Room, attachments: Vec<(PathBuf, Option<String>)>) {
        let total = attachments.len();

        App::spawn(async move {
            for (i, (path, caption)) in attachments.into_iter().enumerate() {
                App::send(ProgressStarted(
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
                        App::send(Error(err.to_string()));
                        return;
                    }
                };

                // try to grab a thumbnail
                let config = match get_attachment_info(&path, &content_type) {
                    Ok((thumbnail, info)) => {
                        AttachmentConfig::new().thumbnail(thumbnail).info(info)
                    }
                    _ => AttachmentConfig::new(),
                }
                .caption(caption.map(TextMessageEventContent::plain));

                if let Err(err) = room
                    .send_attachment(&name, &content_type, data, config)
                    .await
                {
                    App::send(Error(err.to_string()));
                }

                App::send(ProgressComplete);
            }
        });
    }

    pub fn send_reaction(&self, room: Room, event_id: OwnedEventId, key: String) {
        App::spawn(async move {
            App::send(ProgressStarted("Sending reaction.".to_string(), 500));

            if let Err(err) = room
                .send(ReactionEventContent::new(Annotation::new(event_id, key)))
                .await
            {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn redact_event(&self, room: Room, event_id: OwnedEventId) {
        App::spawn(async move {
            App::send(ProgressStarted("Removing.".to_string(), 500));

            if let Err(err) = room.redact(&event_id, None, None).await {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn join_room(&self, room: Room) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            match room.join().await {
                Ok(_) => {
                    matrix.room_cache.add_room(room.clone()).await;
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::RoomSelected(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn join_room_by_target(&self, target: OwnedRoomOrAliasId) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            match matrix.client().join_room_by_id_or_alias(&target, &[]).await {
                Ok(room) => {
                    matrix.room_cache.add_room(room.clone()).await;
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::RoomSelected(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn join_room_by_name(&self, name: String) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Joining.".to_string(), 500));

            // a single token might be an alias on our own server
            if !name.contains(char::is_whitespace)
                && let Ok(alias) =
                    RoomOrAliasId::parse(format!("#{}:{}", name, matrix.me().server_name()))
                && let Ok(room) = matrix.client().join_room_by_id_or_alias(&alias, &[]).await
            {
                matrix.room_cache.add_room(room.clone()).await;
                App::send(ProgressComplete);
                App::send(MatuiEvent::RoomSelected(room));
                return;
            }

            // otherwise, let the server search its public room directory
            let mut request = get_public_rooms_filtered::v3::Request::new();
            request.limit = Some(1u32.into());
            request.filter.generic_search_term = Some(name.clone());

            match matrix.client().public_rooms_filtered(request).await {
                Ok(response) => match response.chunk.into_iter().next() {
                    Some(chunk) => {
                        App::send(ProgressComplete);
                        App::send(MatuiEvent::RoomFound(chunk));
                    }
                    None => {
                        App::send(ProgressComplete);
                        App::send(Error(format!(
                            "No public room found matching \"{}\".",
                            name
                        )));
                    }
                },
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn leave_room(&self, room: Room, forget: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Leaving.".to_string(), 500));

            match room.leave().await {
                Ok(_) => {
                    matrix.room_cache.remove_room(&room);
                    App::send(ProgressComplete);

                    if forget && let Err(err) = room.forget().await {
                        App::send(Error(err.to_string()));
                    }

                    App::send(MatuiEvent::RoomLeft(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    /// Log out on the server (invalidating this device, like Element's
    /// "remove session") and wipe all local state, then quit.
    pub fn logout(&self) {
        let client = self.client();

        App::spawn(async move {
            App::send(ProgressStarted("Logging out.".to_string(), 500));

            if let Err(err) = client.logout().await {
                App::send(ProgressComplete);
                App::send(Error(err.to_string()));
                return;
            }

            let (data_dir, _) = Matrix::dirs();

            App::send(ProgressComplete);

            match fs::remove_dir_all(&data_dir) {
                Ok(_) => App::send(MatuiEvent::LoggedOut),
                Err(err) => App::send(Error(err.to_string())),
            }
        });
    }

    pub fn create_room(
        &self,
        name: Option<String>,
        topic: Option<String>,
        alias: Option<String>,
        encrypted: bool,
        private: bool,
    ) {
        let matrix = self.clone();

        App::spawn(async move {
            App::send(ProgressStarted("Creating.".to_string(), 500));

            let mut request = create_room::v3::Request::new();
            request.name = name;
            request.topic = topic;
            request.room_alias_name = alias;

            if private {
                request.preset = Some(RoomPreset::PrivateChat);
            } else {
                request.preset = Some(RoomPreset::PublicChat);
                request.visibility = Visibility::Public;
            }

            if encrypted {
                request.initial_state = vec![
                    InitialStateEvent::new(
                        EmptyStateKey,
                        RoomEncryptionEventContent::with_recommended_defaults(),
                    )
                    .to_raw_any(),
                ];
            }

            match matrix.client().create_room(request).await {
                Ok(room) => {
                    matrix.room_cache.add_room(room.clone()).await;
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::RoomSelected(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn create_dm(&self, user_id: OwnedUserId, encrypted: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            // if we already have a DM with this user, just open it
            if let Some(room) = matrix.client().get_dm_room(&user_id) {
                App::send(MatuiEvent::RoomSelected(room));
                return;
            }

            App::send(ProgressStarted("Creating.".to_string(), 500));

            let result = if encrypted {
                matrix.client().create_dm(&user_id).await
            } else {
                // create_dm, minus the encryption state event
                let mut request = create_room::v3::Request::new();
                request.invite = vec![user_id.clone()];
                request.is_direct = true;
                request.preset = Some(RoomPreset::TrustedPrivateChat);

                matrix.client().create_room(request).await
            };

            match result {
                Ok(room) => {
                    matrix.room_cache.add_room(room.clone()).await;
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::RoomSelected(room));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn invite_user(&self, room: Room, user_id: OwnedUserId) {
        App::spawn(async move {
            App::send(ProgressStarted("Inviting.".to_string(), 500));

            match room.invite_user_by_id(&user_id).await {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        "Invited".to_string(),
                        format!("{} has been invited.", user_id),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn moderate(
        &self,
        room: Room,
        action: Moderation,
        user_id: OwnedUserId,
        reason: Option<String>,
    ) {
        App::spawn(async move {
            let (verb, past) = match action {
                Moderation::Ban => ("Banning", "Banned"),
                Moderation::Unban => ("Unbanning", "Unbanned"),
                Moderation::Kick => ("Kicking", "Kicked"),
            };

            App::send(ProgressStarted(format!("{}.", verb), 500));

            let result = match action {
                Moderation::Ban => room.ban_user(&user_id, reason.as_deref()).await,
                Moderation::Unban => room.unban_user(&user_id, reason.as_deref()).await,
                Moderation::Kick => room.kick_user(&user_id, reason.as_deref()).await,
            };

            match result {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
        });
    }

    pub fn ignore_user(&self, user_id: OwnedUserId, ignore: bool) {
        let matrix = self.clone();

        App::spawn(async move {
            let (verb, past) = if ignore {
                ("Ignoring", "Ignored")
            } else {
                ("Unignoring", "Unignored")
            };

            App::send(ProgressStarted(format!("{}.", verb), 500));

            let account = matrix.client().account();

            let result = if ignore {
                account.ignore_user(&user_id).await
            } else {
                account.unignore_user(&user_id).await
            };

            match result {
                Ok(_) => {
                    App::send(ProgressComplete);
                    App::send(MatuiEvent::Confirm(
                        past.to_string(),
                        format!("{} has been {}.", user_id, past.to_lowercase()),
                    ));
                }
                Err(err) => {
                    App::send(ProgressComplete);
                    App::send(Error(err.to_string()));
                }
            }
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
            TimelineEventKind::Decrypted(decrypted) => Ok(decrypted.event.deserialize()?),
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
        App::spawn(async move {
            App::send(ProgressStarted("Editing message.".to_string(), 500));

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
                .send(RoomMessageEventContent::text_markdown(message).make_replacement(event))
                .await
            {
                App::send(Error(err.to_string()));
            }

            App::send(ProgressComplete);
        });
    }

    pub fn me(&self) -> OwnedUserId {
        self.client().user_id().unwrap().to_owned()
    }

    pub fn timeline_event(&self, event: AnyTimelineEvent) {
        let matrix = self.clone();

        App::spawn(async move {
            matrix
                .room_cache
                .timeline_event(matrix.client(), &event)
                .await;

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

    pub fn read_to(&self, room: Room, to: OwnedEventId) {
        let receipts = Receipts::new()
            .fully_read_marker(Some(to.clone()))
            .public_read_receipt(Some(to));

        App::spawn(async move {
            if let Err(e) = room.send_multiple_receipts(receipts).await {
                error!("could not send read receipt: {}", e);
            }
        });
    }

    pub fn typing_notification(&self, room: Room, typing: bool) {
        App::spawn(async move {
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

/// The user's auth info, for either kind of login.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredAuth {
    // OAuth must come first: untagged deserializing tries variants in
    // order, and only OAuth sessions have a client_id
    OAuth {
        client_id: ClientId,
        user: UserSession,
    },
    Matrix(MatrixSession),
}

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct FullSession {
    client_session: ClientSession,
    user_session: StoredAuth,
}

fn stored_auth(client: &Client) -> anyhow::Result<StoredAuth> {
    match client.session().context("Your user has no session.")? {
        AuthSession::Matrix(session) => Ok(StoredAuth::Matrix(session)),
        AuthSession::OAuth(session) => Ok(StoredAuth::OAuth {
            client_id: session.client_id,
            user: session.user,
        }),
        _ => bail!("Unknown session type."),
    }
}

fn persist_session(
    session_file: &Path,
    client_session: ClientSession,
    client: &Client,
) -> anyhow::Result<()> {
    let serialized_session = serde_json::to_string(&FullSession {
        client_session,
        user_session: stored_auth(client)?,
    })?;

    fs::write(session_file, serialized_session)?;

    Ok(())
}

/// Update just the auth portion of an existing session file.
fn persist_auth(session_file: &Path, client: &Client) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file)?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.user_session = stored_auth(client)?;
    fs::write(session_file, serde_json::to_string(&full_session)?)?;

    Ok(())
}

async fn restore_session(session_file: &Path) -> anyhow::Result<Client> {
    let serialized_session = fs::read_to_string(session_file)?;
    let mut raw_session: serde_json::Value = serde_json::from_str(&serialized_session)?;

    let FullSession {
        client_session,
        user_session,
    } = serde_json::from_value(raw_session.clone())?;

    // only sessions that synced with /sync have a sync_token
    if let Some(session) = raw_session.as_object_mut()
        && session.remove("sync_token").is_some()
    {
        forget_legacy_to_device_token(&client_session.db_path)?;
        fs::write(session_file, raw_session.to_string())?;
    }

    let homeserver = <&ServerName>::try_from(client_session.homeserver.as_str())?;

    // Build the client with the previous settings from the session.
    let client = Client::builder()
        .server_name(homeserver)
        .sqlite_store(client_session.db_path, Some(&client_session.passphrase))
        .with_encryption_settings(build_encryption_settings())
        .handle_refresh_tokens()
        .build()
        .await?;

    // Restore the user session.
    let session: AuthSession = match user_session {
        StoredAuth::OAuth { client_id, user } => OAuthSession { client_id, user }.into(),
        StoredAuth::Matrix(session) => session.into(),
    };

    client.restore_session(session).await?;

    Ok(client)
}

/// Synapse rejects a /sync to-device token in sliding sync, so drop the one
/// /sync left in the crypto store.
///
/// TODO: drop this once matrix-sdk releases matrix-org/matrix-rust-sdk#7135,
/// which clears the token itself.
fn forget_legacy_to_device_token(db_path: &Path) -> anyhow::Result<()> {
    let db = rusqlite::Connection::open_with_flags(
        db_path.join("matrix-sdk-crypto.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    db.execute("DELETE FROM kv WHERE key = 'next_batch_token'", ())?;

    info!("forgot the /sync to-device token");

    Ok(())
}

async fn login(
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
    username: &str,
    password: &str,
) -> anyhow::Result<Client> {
    let server = ServerName::parse(homeserver)?;

    // the username can be bare, or a full matrix ID
    let username = username
        .strip_prefix('@')
        .unwrap_or(username)
        .split(':')
        .next()
        .unwrap_or_default();

    let (client, client_session) = build_client(data_dir, &server).await?;

    client
        .matrix_auth()
        .login_username(username, password)
        .initial_device_display_name("Matui")
        .send()
        .await?;

    persist_session(session_file, client_session, &client)?;

    Ok(client)
}

fn render_qr_code(data: Vec<u8>) -> anyhow::Result<String> {
    let code = qrcode::QrCode::new(data)?;

    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .quiet_zone(true)
        .build())
}

async fn supports_oidc(homeserver: &str) -> anyhow::Result<bool> {
    let server = ServerName::parse(homeserver)?;
    let client = Client::builder()
        .server_name(&server)
        .build()
        .await
        .with_context(|| format!("could not connect to {homeserver}"))?;

    match client.oauth().server_metadata().await {
        Ok(_) => Ok(true),
        Err(err) if err.is_not_supported() => Ok(false),
        Err(err) => bail!("could not check {homeserver} for OIDC: {err}"),
    }
}

/// Run the browser-based OAuth login flow. Returns None if the user
/// cancelled from the waiting popup.
async fn login_oauth_flow(
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
) -> anyhow::Result<Option<Client>> {
    let server = ServerName::parse(homeserver)?;
    let (client, client_session) = build_client(data_dir, &server).await?;

    let (redirect_url, redirect) = LocalServerBuilder::new()
        .response(LocalServerResponse::Html(
            "<h3>You're signed in! You can close this tab and return to your terminal.</h3>"
                .to_string(),
        ))
        .spawn()
        .await?;

    let mut metadata = ClientMetadata::new(
        ApplicationType::Native,
        vec![OAuthGrantType::AuthorizationCode {
            redirect_uris: vec![redirect_url.clone()],
        }],
        Localized::new(Url::parse("https://github.com/pkulak/matui")?, []),
    );

    metadata.client_name = Some(Localized::new("Matui".to_string(), []));

    let oauth = client.oauth();

    let auth_data = oauth
        .login(redirect_url, None, Some(Raw::new(&metadata)?.into()), None)
        .build()
        .await?;

    // best effort: the waiting popup has fallbacks if this doesn't work
    let _ = open::that_detached(auth_data.url.as_str());

    let cancel = Arc::new(tokio::sync::Notify::new());

    App::send(MatuiEvent::OauthStarted(
        auth_data.url.to_string(),
        cancel.clone(),
    ));

    let query = tokio::select! {
        query = redirect.into_future() => query,
        _ = cancel.notified() => {
            oauth.abort_login(&auth_data.state).await;
            return Ok(None);
        }
    };

    let query = query.context("The local redirect server was shut down.")?;

    oauth.finish_login(query.into()).await?;

    persist_session(session_file, client_session, &client)?;

    Ok(Some(client))
}

async fn build_client(
    data_dir: &Path,
    server: &ServerName,
) -> anyhow::Result<(Client, ClientSession)> {
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
        .server_name(server)
        .sqlite_store(&db_path, Some(passphrase.as_str()))
        .with_encryption_settings(build_encryption_settings())
        .handle_refresh_tokens()
        .build()
        .await?;

    Ok((
        client,
        ClientSession {
            homeserver: server.as_str().to_string(),
            db_path,
            passphrase,
        },
    ))
}

fn build_encryption_settings() -> EncryptionSettings {
    EncryptionSettings {
        auto_enable_cross_signing: true,
        auto_enable_backups: true,
        backup_download_strategy: matrix_sdk::encryption::BackupDownloadStrategy::OneShot,
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

            App::spawn(request_verification_handler(request));
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

                App::spawn(request_verification_handler(request));
            }
        },
    );
}

async fn request_verification_handler(request: VerificationRequest) {
    info!(
        "Accepting verification request from {}",
        request.other_user_id()
    );

    request
        .accept()
        .await
        .expect("Can't accept verification request");

    let mut stream = request.changes();

    while let Some(state) = stream.next().await {
        match state {
            VerificationRequestState::Created { .. }
            | VerificationRequestState::Requested { .. }
            | VerificationRequestState::Ready { .. } => (),
            VerificationRequestState::Transitioned { verification } => {
                // We only support SAS verification.
                if let Verification::SasV1(s) = verification {
                    App::spawn(sas_verification_handler(s, App::get_sender()));
                    break;
                }
            }
            VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => break,
        }
    }
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
