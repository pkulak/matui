use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use log::error;
use matrix_sdk::authentication::AuthSession;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::authentication::oauth::{ClientId, OAuthSession, UserSession};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::ruma::exports::serde_json;
use matrix_sdk::{Client, ServerName, SessionChange};
use rand::rng;
use rand::{RngExt, distr::Alphanumeric};
use serde::{Deserialize, Serialize};

use super::Matrix;
use crate::app::App;
use crate::handler::MatuiEvent::Error;

/// The data needed to re-build a client.
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ClientSession {
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
    sync_token: Option<String>,
}

impl Matrix {
    /// Keep the session file up to date as tokens refresh (OAuth access
    /// tokens are short-lived).
    pub(super) fn watch_session(&self, client: Client) {
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

pub(super) fn persist_session(
    session_file: &Path,
    client_session: ClientSession,
    client: &Client,
) -> anyhow::Result<()> {
    let serialized_session = serde_json::to_string(&FullSession {
        client_session,
        user_session: stored_auth(client)?,
        sync_token: None,
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

pub(super) fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file)?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&full_session)?;
    fs::write(session_file, serialized_session)?;

    Ok(())
}

pub(super) async fn restore_session(
    session_file: &Path,
) -> anyhow::Result<(Client, Option<String>)> {
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

    Ok((client, sync_token))
}

pub(super) async fn build_client(
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
