use std::fs;
use std::future::IntoFuture;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use futures::stream::StreamExt;
use log::info;
use matrix_sdk::Client;
use matrix_sdk::authentication::oauth::qrcode::{GeneratedQrProgress, GrantLoginProgress};
use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::reqwest::Url;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::utils::local_server::{LocalServerBuilder, LocalServerResponse};

use super::Matrix;
use super::session::{build_client, persist_session, restore_session};
use crate::handler::MatuiEvent::Error;
use crate::handler::{MatuiEvent, SyncType};

impl Matrix {
    pub fn init(&self) {
        info!("initializing matrix");

        let (_, session_file) = Matrix::dirs();

        if !session_file.exists() {
            self.send(MatuiEvent::LoginRequired);
            return;
        }

        let matrix = self.clone();

        self.spawn(async move {
            let progress = matrix.progress("Syncing", 0);

            let client = match restore_session(session_file.as_path()).await {
                Ok(client) => client,
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

            matrix.watch_session(client.clone());
            matrix.start_sync(client, SyncType::Latest, progress).await;
        });
    }

    pub fn login(&self, homeserver: &str, username: &str, password: &str) {
        let (data_dir, session_file) = Matrix::dirs();
        let homeserver = homeserver.to_string();
        let user = username.to_string();
        let pass = password.to_string();
        let matrix = self.clone();

        self.spawn(async move {
            matrix.send(MatuiEvent::LoginStarted);

            match login(&data_dir, &session_file, &homeserver, &user, &pass).await {
                Ok(client) => matrix.finish_signin(client).await,
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }

    /// Start the login flow by deciding whether this homeserver supports
    /// OIDC. OIDC homeservers go straight to browser login; everything else
    /// asks for username/password.
    pub fn start_login(&self, homeserver: String) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Checking homeserver.", 0);

            match supports_oidc(&homeserver).await {
                Ok(true) => matrix.login_oauth(homeserver),
                Ok(false) => matrix.send(MatuiEvent::PasswordLoginRequired(homeserver)),
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }

    pub fn grant_login_with_qr(&self) {
        let matrix = self.clone();

        self.spawn(async move {
            let _progress = matrix.progress("Preparing QR login.", 0);

            let client = matrix.client();
            let oauth = client.oauth();
            let grant = oauth.grant_login_with_qr_code().generate();
            let mut progress = grant.subscribe_to_progress();
            let progress_matrix = matrix.clone();

            let progress_task = matrix.spawn(async move {
                let matrix = progress_matrix;

                while let Some(state) = progress.next().await {
                    match state {
                        GrantLoginProgress::Starting => {}
                        GrantLoginProgress::EstablishingSecureChannel(
                            GeneratedQrProgress::QrReady(data),
                        ) => match render_qr_code(data.to_bytes()) {
                            Ok(code) => matrix.send(MatuiEvent::QrLoginCode(code)),
                            Err(err) => matrix.send(Error(err.to_string())),
                        },
                        GrantLoginProgress::EstablishingSecureChannel(
                            GeneratedQrProgress::QrScanned(sender),
                        ) => matrix.send(MatuiEvent::QrLoginScanned(sender)),
                        GrantLoginProgress::WaitingForAuth {
                            verification_uri,
                            continuation_sender,
                        } => {
                            let url = verification_uri.to_string();
                            let _ = open::that_detached(&url);
                            matrix.send(MatuiEvent::QrLoginAuth(url));

                            if let Err(err) = continuation_sender.confirm().await {
                                matrix.send(Error(err.to_string()));
                            }
                        }
                        GrantLoginProgress::SyncingSecrets => {
                            matrix.send(MatuiEvent::QrLoginSyncing);
                        }
                        GrantLoginProgress::Done => {}
                    }
                }
            });

            match grant.await {
                Ok(()) => matrix.send(MatuiEvent::QrLoginDone),
                Err(err) => matrix.send(Error(err.to_string())),
            }

            progress_task.abort();
        });
    }

    pub fn login_oauth(&self, homeserver: String) {
        let (data_dir, session_file) = Matrix::dirs();
        let matrix = self.clone();

        self.spawn(async move {
            matrix.send(MatuiEvent::LoginStarted);

            match login_oauth_flow(&matrix, &data_dir, &session_file, &homeserver).await {
                Ok(Some(client)) => matrix.finish_signin(client).await,
                // cancelled from the waiting popup; back to the form
                Ok(None) => matrix.send(MatuiEvent::LoginRequired),
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }

    /// Everything that happens after a fresh login, no matter the flow.
    async fn finish_signin(&self, client: Client) {
        self.client
            .set(client.clone())
            .expect("could not set client");

        self.watch_session(client.clone());

        self.send(MatuiEvent::LoginComplete);
        let progress = self.progress("Performing initial sync.", 0);

        if self.start_sync(client, SyncType::Initial, progress).await {
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            self.offer_verification();
        }
    }

    /// Log out on the server (invalidating this device, like Element's
    /// "remove session") and wipe all local state, then quit.
    pub fn logout(&self) {
        let client = self.client();
        let matrix = self.clone();

        self.spawn(async move {
            let progress = matrix.progress("Logging out.", 500);

            if let Err(err) = client.logout().await {
                matrix.send(Error(err.to_string()));
                return;
            }

            let (data_dir, _) = Matrix::dirs();

            drop(progress);

            match fs::remove_dir_all(&data_dir) {
                Ok(_) => matrix.send(MatuiEvent::LoggedOut),
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }
}

async fn login(
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
    username: &str,
    password: &str,
) -> anyhow::Result<Client> {
    // the username can be bare, or a full matrix ID
    let username = username
        .strip_prefix('@')
        .unwrap_or(username)
        .split(':')
        .next()
        .unwrap_or_default();

    let (client, client_session) = build_client(data_dir, homeserver).await?;

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
    let client = Client::builder()
        .server_name_or_homeserver_url(homeserver)
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
    matrix: &Matrix,
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
) -> anyhow::Result<Option<Client>> {
    let (client, client_session) = build_client(data_dir, homeserver).await?;

    let (redirect_url, redirect) = LocalServerBuilder::new()
        .response(LocalServerResponse::Html(
            "<h3>You're signed in! You can close this tab and return to your terminal.</h3>"
                .to_string(),
        ))
        .spawn()
        .await?;

    let mut redirect_url_no_port = redirect_url.clone();
    redirect_url_no_port
        .set_port(None)
        .expect("loopback URL has a host");

    let mut metadata = ClientMetadata::new(
        ApplicationType::Native,
        vec![OAuthGrantType::AuthorizationCode {
            redirect_uris: vec![redirect_url_no_port],
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

    matrix.send(MatuiEvent::OauthStarted(
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
