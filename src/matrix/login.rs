use std::fs;
use std::future::IntoFuture;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use futures::stream::StreamExt;
use log::info;
use matrix_sdk::Client;
use matrix_sdk::authentication::oauth::ClientRegistrationData;
use matrix_sdk::authentication::oauth::qrcode::{
    CheckCodeSender, GeneratedQrProgress, GrantLoginProgress, LoginProgress, QRCodeLoginError,
};
use matrix_sdk::authentication::oauth::registration::{
    ApplicationType, ClientMetadata, Localized, OAuthGrantType,
};
use matrix_sdk::reqwest::Url;
use matrix_sdk::ruma::api::FeatureFlag;
use matrix_sdk::ruma::api::client::discovery::get_authorization_server_metadata::v1::GrantType;
use matrix_sdk::ruma::serde::Raw;
use matrix_sdk::utils::local_server::{LocalServerBuilder, LocalServerResponse};
use tokio::sync::Notify;

use super::session::{build_client, persist_session, restore_session};
use super::sync::NO_SLIDING_SYNC;
use super::{Matrix, Notice};
use crate::handler::MatuiEvent::{Error, QrLoginFailed};
use crate::handler::{Login, MatuiEvent, SyncType};

impl Matrix {
    pub fn init(&self) {
        info!("initializing matrix");

        let (_, session_file) = Matrix::dirs();

        if !session_file.exists() {
            self.send(MatuiEvent::Login(Login::Homeserver));
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
            let progress = matrix.progress("Logging in", 0);
            let result = login(&data_dir, &session_file, &homeserver, &user, &pass).await;
            drop(progress);

            match result {
                Ok(client) => matrix.finish_signin(client).await,
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }

    /// Start the best login flow this homeserver supports: a QR code to
    /// scan with a signed-in device, then browser login, then
    /// username/password. The progress stays up until the flow's first
    /// screen does, so the form can't be submitted twice.
    pub fn start_login(&self, homeserver: String) {
        let matrix = self.clone();

        self.spawn(async move {
            // delayed, since it's usually over before it would show
            let progress = matrix.progress("Checking homeserver.", 500);

            match login_kind(&homeserver).await {
                Ok(LoginKind::Qr) => matrix.login_qr(homeserver, progress),
                Ok(LoginKind::Oauth) => matrix.login_oauth(homeserver, progress),
                Ok(LoginKind::Password) => {
                    matrix.send(MatuiEvent::Login(Login::Password { homeserver }))
                }
                Err(err) => matrix.send(Error(err.to_string())),
            }
        });
    }

    /// Log this device in by showing a QR code for a signed-in device to
    /// scan. Cancelling falls back to browser login. The progress closes
    /// once the code shows.
    fn login_qr(&self, homeserver: String, progress: Notice) {
        let (data_dir, session_file) = Matrix::dirs();
        let matrix = self.clone();

        self.spawn(async move {
            match login_qr_flow(&matrix, &data_dir, &session_file, &homeserver, progress).await {
                Ok(Some(client)) => matrix.finish_signin(client).await,
                Ok(None) => {
                    let progress = matrix.progress("Logging in", 500);
                    matrix.login_oauth(homeserver, progress);
                }
                Err(err) => matrix.signin_failed(err),
            }
        });
    }

    /// Answer the other device with the check code it shows, whichever
    /// side of a QR login this is.
    pub fn send_check_code(&self, sender: CheckCodeSender, code: u8) {
        let matrix = self.clone();

        self.spawn(async move {
            if let Err(err) = sender.send(code).await {
                matrix.send(Error(err.to_string()));
            }
        });
    }

    pub fn grant_login_with_qr(&self) {
        let matrix = self.clone();

        self.spawn(async move {
            // every progress popup of the flow shares this ID, so they all
            // go when it ends, even ones the user covered
            let flow = matrix.progress("Preparing QR login.", 0);
            let id = flow.id();

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
                            Err(err) => matrix.send(QrLoginFailed(err.to_string())),
                        },
                        GrantLoginProgress::EstablishingSecureChannel(
                            GeneratedQrProgress::QrScanned(sender),
                        ) => matrix.send(MatuiEvent::QrLoginScanned(sender, id)),
                        GrantLoginProgress::WaitingForAuth {
                            verification_uri,
                            continuation_sender,
                        } => {
                            let url = verification_uri.to_string();
                            let _ = open::that_detached(&url);
                            matrix.send(MatuiEvent::QrLoginAuth(url));

                            if let Err(err) = continuation_sender.confirm().await {
                                matrix.send(QrLoginFailed(err.to_string()));
                            }
                        }
                        GrantLoginProgress::SyncingSecrets => {
                            matrix.send(MatuiEvent::QrLoginSyncing(id));
                        }
                        GrantLoginProgress::Done => {}
                    }
                }
            });

            match grant.await {
                Ok(()) => matrix.send(MatuiEvent::QrLoginDone),
                Err(err) => matrix.send(QrLoginFailed(err.to_string())),
            }

            progress_task.abort();
        });
    }

    /// Log in with the browser. The progress closes once the login is
    /// waiting on the browser.
    fn login_oauth(&self, homeserver: String, progress: Notice) {
        let (data_dir, session_file) = Matrix::dirs();
        let matrix = self.clone();

        self.spawn(async move {
            match login_oauth_flow(&matrix, &data_dir, &session_file, &homeserver, progress).await {
                Ok(Some(client)) => matrix.finish_signin(client).await,
                // cancelled while waiting on the browser; back to the form
                Ok(None) => matrix.send(MatuiEvent::Login(Login::Homeserver)),
                Err(err) => matrix.signin_failed(err),
            }
        });
    }

    /// Go back to the start of signing in, and say why.
    fn signin_failed(&self, err: anyhow::Error) {
        log::warn!("sign-in failed: {err}");
        self.send(MatuiEvent::Login(Login::Homeserver));
        self.send(Error(err.to_string()));
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

#[derive(Debug, PartialEq)]
enum LoginKind {
    Qr,
    Oauth,
    Password,
}

async fn login_kind(homeserver: &str) -> anyhow::Result<LoginKind> {
    let client = Client::builder()
        .server_name_or_homeserver_url(homeserver)
        .build()
        .await
        .with_context(|| format!("could not connect to {homeserver}"))?;

    if client.available_sliding_sync_versions().await.is_empty() {
        bail!(NO_SLIDING_SYNC);
    }

    let metadata = match client.oauth().server_metadata().await {
        Ok(metadata) => metadata,
        Err(err) if err.is_not_supported() => return Ok(LoginKind::Password),
        Err(err) => bail!("could not check {homeserver} for OIDC: {err}"),
    };

    // the same check Element Web makes before it offers QR login
    let qr = metadata
        .grant_types_supported
        .contains(&GrantType::DeviceCode)
        && client
            .unstable_features()
            .await?
            .contains(&FeatureFlag::from("org.matrix.msc4108"));

    Ok(if qr { LoginKind::Qr } else { LoginKind::Oauth })
}

fn client_metadata(grant_types: Vec<OAuthGrantType>) -> anyhow::Result<Raw<ClientMetadata>> {
    let mut metadata = ClientMetadata::new(
        ApplicationType::Native,
        grant_types,
        Localized::new(Url::parse("https://github.com/pkulak/matui")?, []),
    );

    metadata.client_name = Some(Localized::new("Matui".to_string(), []));

    Ok(Raw::new(&metadata)?)
}

/// Run the QR login flow, where a signed-in device scans our code and
/// hands over its keys. Returns None if the user cancelled.
async fn login_qr_flow(
    matrix: &Matrix,
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
    progress: Notice,
) -> anyhow::Result<Option<Client>> {
    let (mut client, mut client_session) = build_client(data_dir, homeserver).await?;
    let registration: ClientRegistrationData =
        client_metadata(vec![OAuthGrantType::DeviceCode])?.into();
    let cancel = Arc::new(Notify::new());
    let mut progress = Some(progress);

    loop {
        let oauth = client.oauth();
        let login = oauth.login_with_qr_code(Some(&registration)).generate();
        let scanned = Arc::new(AtomicBool::new(false));

        let progress_task = matrix.spawn(show_qr_login_progress(
            matrix.clone(),
            login.subscribe_to_progress(),
            cancel.clone(),
            scanned.clone(),
            progress.take(),
        ));

        let result = tokio::select! {
            result = login.into_future() => Some(result),
            _ = cancel.notified() => None,
        };

        progress_task.abort();

        match result {
            Some(Ok(())) => break,
            // the server ends the login a while after showing the code, so
            // show a new one
            Some(Err(QRCodeLoginError::NotFound)) if !scanned.load(Ordering::Relaxed) => continue,
            Some(Err(QRCodeLoginError::NotFound)) => {
                matrix.send(Error(
                    "The QR login took too long. Scan the new code to try again.".to_string(),
                ));

                // this client may be partway signed in, so start over
                (client, client_session) = build_client(data_dir, homeserver).await?;
            }
            Some(Err(err)) => return Err(err.into()),
            None => return Ok(None),
        }
    }

    persist_session(session_file, client_session, &client)?;

    Ok(Some(client))
}

async fn show_qr_login_progress(
    matrix: Matrix,
    mut progress: impl futures::Stream<Item = LoginProgress<GeneratedQrProgress>> + Unpin,
    cancel: Arc<Notify>,
    scanned: Arc<AtomicBool>,
    mut checking: Option<Notice>,
) {
    while let Some(state) = progress.next().await {
        match state {
            LoginProgress::Starting => {}
            LoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrReady(data)) => {
                match render_qr_code(data.to_bytes()) {
                    Ok(code) => matrix.send(MatuiEvent::Login(Login::QrCode {
                        code,
                        cancel: cancel.clone(),
                    })),
                    Err(err) => matrix.send(Error(err.to_string())),
                }

                checking.take();
            }
            LoginProgress::EstablishingSecureChannel(GeneratedQrProgress::QrScanned(sender)) => {
                scanned.store(true, Ordering::Relaxed);
                matrix.send(MatuiEvent::Login(Login::QrCheck {
                    sender,
                    cancel: cancel.clone(),
                }))
            }
            LoginProgress::WaitingForToken { user_code } => {
                matrix.send(MatuiEvent::Login(Login::QrApprove { code: user_code }))
            }
            LoginProgress::SyncingSecrets => matrix.send(MatuiEvent::Login(Login::SyncingSecrets)),
            LoginProgress::Done => {}
        }
    }
}

/// Run the browser-based OAuth login flow. Returns None if the user
/// cancelled while waiting on the browser.
async fn login_oauth_flow(
    matrix: &Matrix,
    data_dir: &Path,
    session_file: &Path,
    homeserver: &str,
    progress: Notice,
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

    let metadata = client_metadata(vec![OAuthGrantType::AuthorizationCode {
        redirect_uris: vec![redirect_url_no_port],
    }])?;

    let oauth = client.oauth();

    let auth_data = oauth
        .login(redirect_url, None, Some(metadata.into()), None)
        .build()
        .await?;

    // best effort: the waiting screen has fallbacks if this doesn't work
    let _ = open::that_detached(auth_data.url.as_str());

    let cancel = Arc::new(tokio::sync::Notify::new());

    drop(progress);
    matrix.send(MatuiEvent::Login(Login::Browser {
        url: auth_data.url.to_string(),
        cancel: cancel.clone(),
    }));

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

#[cfg(test)]
mod tests {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use matrix_sdk::test_utils::mocks::MatrixMockServer;

    use super::*;
    use crate::app::tests::test_app;
    use crate::app::{App, Popup, Session};
    use crate::event::Event;
    use crate::handler::handle_app_event;
    use crate::widgets::signin::Signin;

    /// Handle the app's events until an error shows.
    async fn until_error(app: &mut App, events: &Receiver<Event>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);

        while !matches!(app.popup, Some(Popup::Error(_))) {
            assert!(tokio::time::Instant::now() < deadline, "no error showed");

            match events.try_recv() {
                Ok(Event::Matui(event)) => handle_app_event(event, app),
                _ => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    }

    #[tokio::test]
    async fn an_unreachable_homeserver_leaves_its_form_showing() {
        let (mut app, events) = test_app();
        handle_app_event(MatuiEvent::Login(Login::Homeserver), &mut app);

        app.matrix.start_login("http://127.0.0.1:1".to_string());
        until_error(&mut app, &events).await;
        app.close_popup();

        assert!(app.popup.is_none());
        assert!(matches!(
            app.session,
            Session::SignedOut(Signin::Homeserver(_))
        ));
    }

    #[tokio::test]
    async fn a_failed_signin_shows_its_error_over_the_homeserver_form() {
        let (mut app, events) = test_app();
        handle_app_event(
            MatuiEvent::Login(Login::QrApprove {
                code: "42".to_string(),
            }),
            &mut app,
        );

        app.matrix.signin_failed(anyhow::anyhow!("denied"));
        until_error(&mut app, &events).await;

        assert!(matches!(
            app.session,
            Session::SignedOut(Signin::Homeserver(_))
        ));
    }

    async fn kind(oauth: bool, msc4108: bool) -> LoginKind {
        let server = MatrixMockServer::new().await;
        let oauth_server = server.oauth();
        let metadata = oauth_server.mock_server_metadata();

        if oauth {
            metadata.ok().mount().await;
        } else {
            metadata.error_unrecognized().mount().await;
        }

        server
            .mock_versions()
            .with_simplified_sliding_sync()
            .with_feature("org.matrix.msc4108", msc4108)
            .ok()
            .mount()
            .await;

        login_kind(&server.uri()).await.unwrap()
    }

    #[tokio::test]
    async fn login_kind_rejects_a_homeserver_without_sliding_sync() {
        let server = MatrixMockServer::new().await;
        server.mock_versions().ok().mount().await;

        let err = login_kind(&server.uri()).await.unwrap_err();

        assert_eq!(err.to_string(), NO_SLIDING_SYNC);
    }

    #[tokio::test]
    async fn login_kind_prefers_qr_then_oauth_then_password() {
        assert_eq!(kind(true, true).await, LoginKind::Qr);
        assert_eq!(kind(true, false).await, LoginKind::Oauth);
        assert_eq!(kind(false, false).await, LoginKind::Password);
    }
}
