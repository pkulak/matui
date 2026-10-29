use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use log::{error, info};
use matrix_sdk::authentication::AuthSession;
use matrix_sdk::authentication::matrix::MatrixSession;
use matrix_sdk::authentication::oauth::{ClientId, OAuthSession, UserSession};
use matrix_sdk::encryption::EncryptionSettings;
use matrix_sdk::encryption::vodozemac::base64_decode;
use matrix_sdk::ruma::exports::serde_json;
use matrix_sdk::{Client, ServerName, SessionChange, SqliteStoreConfig};
use matrix_sdk_sqlite::Base64Variant;
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

pub(super) async fn restore_session(session_file: &Path) -> anyhow::Result<Client> {
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
        .sqlite_store_with_config_and_cache_path(
            store_config(&client_session.db_path, &client_session.passphrase)?,
            None::<&Path>,
        )
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
        .sqlite_store_with_config_and_cache_path(
            store_config(&db_path, &passphrase)?,
            None::<&Path>,
        )
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

/// Our passphrases are 32 random base64 characters, so the store can use the
/// bytes they encode as its key, rather than run PBKDF2 for each of its four
/// databases at every launch. Opening a store set up with the passphrase
/// switches it to the key, so older versions of Matui can't open it after.
fn store_config(db_path: &Path, passphrase: &str) -> anyhow::Result<SqliteStoreConfig> {
    let key = base64_decode(passphrase)?;

    Ok(
        SqliteStoreConfig::new(db_path)
            .high_entropy_passphrase(Some(&key), Base64Variant::Unpadded),
    )
}

fn build_encryption_settings() -> EncryptionSettings {
    EncryptionSettings {
        auto_enable_cross_signing: true,
        auto_enable_backups: true,
        backup_download_strategy: matrix_sdk::encryption::BackupDownloadStrategy::OneShot,
    }
}

#[cfg(test)]
mod tests {
    use matrix_sdk::{SqliteStateStore, StateStore};

    use super::*;

    #[tokio::test]
    async fn store_set_up_with_the_passphrase_switches_to_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let passphrase = "aZ09bY18cX27dW36eV45fU54gT63hS72";

        let store = SqliteStateStore::open_with_config(
            &SqliteStoreConfig::new(dir.path()).passphrase(Some(passphrase)),
        )
        .await
        .unwrap();
        store
            .set_custom_value(b"kept", b"yes".to_vec())
            .await
            .unwrap();
        drop(store);

        // the first open migrates, and the second uses the key alone
        for _ in 0..2 {
            let config = store_config(dir.path(), passphrase).unwrap();
            let store = SqliteStateStore::open_with_config(&config).await.unwrap();
            let kept = store.get_custom_value(b"kept").await.unwrap();
            assert_eq!(kept.as_deref(), Some(b"yes".as_slice()));
        }

        let old = SqliteStateStore::open_with_config(
            &SqliteStoreConfig::new(dir.path()).passphrase(Some(passphrase)),
        )
        .await;
        assert!(old.is_err(), "the store should need the key now");
    }
}
