use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::app::App;
use crate::handler::MatuiEvent::VerificationStarted;
use crate::handler::{MatuiEvent, SyncType};
use anyhow::{bail, Context};
use futures::stream::StreamExt;
use log::info;
use matrix_sdk::config::SyncSettings;
use matrix_sdk::encryption::verification::{SasState, SasVerification, Verification};
use matrix_sdk::room::{Joined, Messages, MessagesOptions};
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
use matrix_sdk::{Client, ServerName, Session};
use once_cell::sync::OnceCell;
use rand::rngs::OsRng;
use rand::{distributions::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;

/// A Matrix client that maintains it's own Tokio runtime
#[derive(Clone)]
pub struct Matrix {
    rt: Arc<Runtime>,
    client: Arc<OnceCell<Client>>,
    sync_token: Arc<OnceCell<String>>,
    send: Sender<MatuiEvent>,
}

pub enum RoomEvent {
    FetchCompleted(Messages),
}

impl Matrix {
    pub fn new(send: Sender<MatuiEvent>) -> Matrix {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();

        Matrix {
            rt: Arc::new(rt),
            client: Arc::new(OnceCell::default()),
            sync_token: Arc::new(OnceCell::default()),
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
        self.send.send(event).expect("could not send Matrix event");
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
                    matrix.send(MatuiEvent::Error(err.to_string()));
                    return;
                }
            };

            info!("session restored");

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            let token = match sync(client, token, &session_file).await {
                Ok(sync) => sync,
                Err(err) => {
                    matrix.send(MatuiEvent::Error(err.to_string()));
                    return;
                }
            };

            info!("sync complete");

            matrix
                .sync_token
                .set(token)
                .expect("could not set sync token");

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
                    matrix.send(MatuiEvent::Error(err.to_string()));
                    return;
                }
            };

            matrix
                .client
                .set(client.clone())
                .expect("could not set client");

            matrix.send(MatuiEvent::LoginComplete);
            matrix.send(MatuiEvent::SyncStarted(SyncType::Initial));

            let sync_token = match sync(client, None, &session_file).await {
                Ok(sync) => sync,
                Err(err) => {
                    matrix.send(MatuiEvent::Error(err.to_string()));
                    return;
                }
            };

            matrix
                .sync_token
                .set(sync_token)
                .expect("could not set sync token");

            matrix.send(MatuiEvent::SyncComplete);
        });
    }

    pub fn joined_rooms(&self) -> Vec<Joined> {
        self.client().joined_rooms()
    }

    pub fn fetch_messages(&self, room: Joined, sender: Sender<RoomEvent>) {
        let matrix = self.clone();

        self.rt.spawn(async move {
            let messages = match room
                .messages(MessagesOptions::new(Direction::Backward))
                .await
            {
                Ok(msg) => msg,
                Err(err) => {
                    matrix.send(MatuiEvent::Error(err.to_string()));
                    return;
                }
            };

            sender
                .send(RoomEvent::FetchCompleted(messages))
                .expect("count not send room event");
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

async fn sync(
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

async fn add_verification_handlers(client: Client) -> matrix_sdk::Result<()> {
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

    client.sync(SyncSettings::new()).await?;

    Ok(())
}

async fn sas_verification_handler(sas: SasVerification, sender: Sender<MatuiEvent>) {
    sas.accept().await.unwrap();

    let mut stream = sas.changes();

    while let Some(state) = stream.next().await {
        match state {
            SasState::KeysExchanged {
                emojis,
                decimals: _,
            } => {
                let emoji_slice = emojis.expect("only emoji verification is supported").emojis;

                sender
                    .send(VerificationStarted(sas.clone(), emoji_slice))
                    .expect("could not send sas event")
            }
            SasState::Done { .. } => {}
            SasState::Cancelled(cancel_info) => {}
            SasState::Started { .. } | SasState::Accepted { .. } | SasState::Confirmed => (),
        }
    }
}
