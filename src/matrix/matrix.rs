use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::app::App;
use crate::event::Event;
use crate::event::Event::{Matui, Tick};
use crate::handler::MatuiEvent::{
    Error, ProgressComplete, ProgressStarted, VerificationCompleted, VerificationStarted,
};
use crate::handler::{Batch, MatuiEvent, SyncType};
use crate::matrix::roomcache::{DecoratedRoom, RoomCache};
use anyhow::{bail, Context};
use futures::stream::StreamExt;
use log::{error, info, warn};
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::verification::{Emoji, SasState, SasVerification, Verification};
use matrix_sdk::room::{Joined, MessagesOptions, Room};
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
use matrix_sdk::{Client, LoopCtrl, ServerName, Session};
use once_cell::sync::OnceCell;
use rand::rngs::OsRng;
use rand::{distributions::Alphanumeric, Rng};
use ruma::events::reaction::ReactionEventContent;
use ruma::events::relation::Annotation;
use ruma::events::room::message::RoomMessageEventContent;
use ruma::events::{AnySyncTimelineEvent, AnyTimelineEvent};
use ruma::{OwnedEventId, UInt};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

/// A Matrix client that maintains it's own Tokio runtime
#[derive(Clone)]
pub struct Matrix {
    rt: Arc<Runtime>,
    client: Arc<OnceCell<Client>>,
    room_cache: Arc<RoomCache>,
    send: Sender<Event>,
}

impl Matrix {
    pub fn new(send: Sender<Event>) -> Matrix {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        Matrix {
            rt: Arc::new(rt),
            client: Arc::new(OnceCell::default()),
            room_cache: Arc::new(RoomCache::default()),
            send,
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

    fn send(&self, event: MatuiEvent) {
        self.send
            .send(Matui(event))
            .expect("could not send Matrix event");
    }

    pub fn init(&self) {
        info!("initializing matrix");

        let (_, session_file) = Matrix::dirs();

        if !session_file.exists() {
            self.send(MatuiEvent::LoginRequired);
            return;
        }

        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix.send(MatuiEvent::SyncStarted(SyncType::Latest));

            let (client, token) = match restore_session(session_file.as_path()).await {
                Ok(tuple) => tuple,
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                    return;
                }
            };

            info!("session restored");

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            if let Err(err) = sync_once(client.clone(), token, &session_file).await {
                matrix.send(Error(err.to_string()));
                return;
            };

            matrix.room_cache.populate(client).await;

            matrix.send(MatuiEvent::SyncComplete);
        });
    }

    pub fn login(&self, username: &str, password: &str) {
        let (data_dir, session_file) = Matrix::dirs();
        let user = username.to_string();
        let pass = password.to_string();
        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix.send(MatuiEvent::LoginStarted);

            let client = match login(&data_dir, &session_file, &user, &pass).await {
                Ok(client) => client,
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                    return;
                }
            };

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            matrix.send(MatuiEvent::LoginComplete);
            matrix.send(MatuiEvent::SyncStarted(SyncType::Initial));

            if let Err(err) = sync_once(client.clone(), None, &session_file).await {
                matrix.send(Error(err.to_string()));
                return;
            };

            matrix.room_cache.populate(client).await;

            matrix.send(MatuiEvent::SyncComplete);
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
                            error!("no sync result: {}", err.to_string());
                            return Ok(LoopCtrl::Continue);
                        }
                    };

                    let (_, session_file) = Matrix::dirs();

                    // We persist the token each time to keep the disk up-to-date
                    if let Err(err) = persist_sync_token(&session_file, response.next_batch) {
                        error!("could not persist sync token {}", err.to_string())
                    }

                    Ok(LoopCtrl::Continue)
                })
                .await
                .expect("could not sync");
        });
    }

    pub fn confirm_verification(&self, sas: SasVerification) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            if let Err(err) = sas.confirm().await {
                error!("could not verify: {}", err.to_string());
                matrix.send(Error(format!("Could not verify: {}", err.to_string())));
            }
        });
    }

    pub fn mismatched_verification(&self, sas: SasVerification) {
        self.rt.spawn(async move {
            if let Err(err) = sas.mismatch().await {
                error!("could not cancel SAS verification: {}", err.to_string())
            } else {
                info!("verification has been cancelled")
            }
        });
    }

    pub fn fetch_rooms(&self) -> Vec<DecoratedRoom> {
        self.room_cache.get_rooms()
    }

    pub fn fetch_messages(&self, room: Joined, cursor: Option<String>) {
        let matrix = self.clone();
        let sender = self.send.clone();

        self.rt.spawn(async move {
            // fetch the actual messages
            let mut options = MessagesOptions::new(Direction::Backward);
            options.limit = UInt::from(25 as u16);
            options.from = cursor;

            let messages = match room.messages(options).await {
                Ok(msg) => msg,
                Err(err) => {
                    matrix.send(Error(err.to_string()));
                    return;
                }
            };

            let unpacked: Vec<AnyTimelineEvent> = messages
                .chunk
                .iter()
                .map(|te| te.event.deserialize().expect("could not deserialize"))
                .collect();

            let batch = Batch {
                room: room.clone(),
                events: unpacked.clone(),
                cursor: messages.end,
            };

            sender
                .send(Matui(MatuiEvent::TimelineBatch(batch)))
                .expect("could not send messages event");

            // and look up the detail about every user
            async fn send_member_event(
                msg: &AnyTimelineEvent,
                room: Joined,
                sender: Sender<Event>,
            ) -> anyhow::Result<()> {
                let member = room
                    .get_member(msg.sender())
                    .await?
                    .context("not a member")?;

                sender.send(Matui(MatuiEvent::Member(member)))?;

                Ok(())
            }

            for msg in unpacked {
                let sender = sender.clone();

                if let Err(e) = send_member_event(&msg, room.clone(), sender).await {
                    warn!("Could not send room member event: {}", e.to_string());
                }
            }

            // finally, send a tick event to force a render
            sender.send(Tick).expect("could not send click event")
        });
    }

    pub fn send_text_message(&self, room: Joined, message: String) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix.send(ProgressStarted("Sending message.".to_string()));

            if let Err(err) = room
                .send(RoomMessageEventContent::text_plain(message), None)
                .await
            {
                matrix.send(Error(err.to_string()));
            }

            matrix.send(ProgressComplete);
        });
    }

    pub fn send_reaction(&self, room: Joined, event_id: OwnedEventId, key: String) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix.send(ProgressStarted("Sending reaction.".to_string()));

            if let Err(err) = room
                .send(
                    ReactionEventContent::new(Annotation::new(event_id, key)),
                    None,
                )
                .await
            {
                matrix.send(Error(err.to_string()));
            }

            matrix.send(ProgressComplete);
        });
    }

    pub fn redact_event(&self, room: Joined, event_id: OwnedEventId) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            matrix.send(ProgressStarted("Removing.".to_string()));

            if let Err(err) = room.redact(&event_id, None, None).await {
                matrix.send(Error(err.to_string()));
            }

            matrix.send(ProgressComplete);
        });
    }

    pub fn me(&self) -> String {
        self.client().user_id().unwrap().as_str().to_string()
    }

    pub fn timeline_event(&self, event: AnyTimelineEvent) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            let client = matrix.client();
            matrix.room_cache.timeline_event(client, &event).await;
        });
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
    user_session: Session,
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
        .sled_store(client_session.db_path, Some(&client_session.passphrase))
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

    client
        .login_username(username, password)
        .initial_device_display_name("Matui")
        .send()
        .await?;

    let user_session = client
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
    let mut rng = OsRng;

    let db_subfolder: String = (&mut rng)
        .sample_iter(Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();

    let db_path = data_dir.join(db_subfolder.as_str());

    // Generate a random passphrase.
    let passphrase: String = (&mut rng)
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let client = Client::builder()
        .server_name(id.server_name())
        .sled_store(&db_path, Some(passphrase.as_str()))
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
        App::get_sender().send(Matui(MatuiEvent::Timeline(
            event.into_full_event(room.room_id().into()),
        )))
    });
}

fn add_verification_handlers(client: Client) {
    client.add_event_handler(
        |ev: ToDeviceKeyVerificationRequestEvent, client: Client| async move {
            let request = client
                .encryption()
                .get_verification_request(&ev.sender, &ev.content.transaction_id)
                .await
                .expect("Request object wasn't created");

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
                let request = client
                    .encryption()
                    .get_verification_request(&ev.sender, &ev.event_id)
                    .await
                    .expect("Request object wasn't created");

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
