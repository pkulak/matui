use futures::stream::{BoxStream, StreamExt};
use log::{error, info};
use matrix_sdk::Client;
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, Verification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::events::key::verification::cancel::CancelCode;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};

use super::Matrix;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::{
    Error, VerificationCancelled, VerificationOffered, VerificationRequested, VerificationStarted,
};

impl Matrix {
    pub fn offer_verification(&self) {
        self.spawn(offer_verification(self.clone()));
    }

    pub fn verify(&self) {
        self.spawn(request_verification(self.clone()));
    }

    pub fn recover(&self, key: &str) {
        let matrix = self.clone();
        let key = key.to_string();

        self.spawn(async move {
            let _progress = matrix.progress("Fetching encryption keys.", 0);

            if matrix
                .client()
                .encryption()
                .recovery()
                .recover(&key)
                .await
                .is_err()
            {
                matrix.send(MatuiEvent::Error(
                    "Could not recover encryption keys.".to_string(),
                ));
            }
        });
    }

    /// Accept a request from another device. Matui has followed it since
    /// prompting, and goes on into the emoji comparison.
    pub fn accept_verification(&self, request: VerificationRequest) {
        self.spawn(accept_request(request, self.clone()));
    }

    pub fn decline_verification(&self, request: VerificationRequest) {
        self.spawn(decline_request(request));
    }

    pub fn confirm_verification(&self, sas: SasVerification) {
        let matrix = self.clone();

        self.spawn(async move {
            if let Err(err) = sas.confirm().await {
                error!("could not verify: {}", err);
                matrix.send(Error(format!("Could not verify: {}", err)));
            }
        });
    }

    pub fn mismatched_verification(&self, sas: SasVerification) {
        self.spawn(async move {
            if let Err(err) = sas.mismatch().await {
                error!("could not cancel SAS verification: {}", err)
            } else {
                info!("verification has been cancelled")
            }
        });
    }
}

pub(super) fn add_verification_handlers(matrix: &Matrix) {
    let client = matrix.client();
    let to_device_matrix = matrix.clone();

    client.add_event_handler(
        move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| async move {
            match client
                .encryption()
                .get_verification_request(&ev.sender, &ev.content.transaction_id)
                .await
            {
                Some(request) => ask_to_accept(request, &to_device_matrix),
                None => error!("could not create request"),
            }
        },
    );

    let matrix = matrix.clone();

    client.add_event_handler(
        move |ev: OriginalSyncRoomMessageEvent, client: Client| async move {
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype {
                match client
                    .encryption()
                    .get_verification_request(&ev.sender, &ev.event_id)
                    .await
                {
                    Some(request) => ask_to_accept(request, &matrix),
                    None => error!("could not create request"),
                }
            }
        },
    );
}

/// Ask whether to accept a request from another device. Only an incoming
/// request still waiting for an answer is `Requested`: one we sent ourselves
/// (which reaches us when it goes to all our devices) starts out `Created`.
fn ask_to_accept(request: VerificationRequest, matrix: &Matrix) {
    let VerificationRequestState::Requested {
        other_device_data: device,
        ..
    } = request.state()
    else {
        return;
    };

    info!(
        "verification requested by {} on {}",
        request.other_user_id(),
        device.device_id()
    );

    let device = match device.display_name() {
        Some(name) => format!("{} ({})", name, device.device_id()),
        None => device.device_id().to_string(),
    };

    let message = if request.is_self_verification() {
        format!("{} asks to verify this session. Accept?", device)
    } else {
        format!(
            "{} asks to verify you from {}. Accept?",
            request.other_user_id(),
            device
        )
    };

    matrix.spawn(own_verification(request, matrix.clone(), Some(message)));
}

async fn accept_request(request: VerificationRequest, matrix: Matrix) {
    if let Err(err) = request.accept().await {
        error!("could not accept verification request: {}", err);
        matrix.send(Error(format!("Could not accept verification: {}", err)));
    }
}

/// Offer to verify this session, unless it's verified already: it has the
/// cross-signing keys, such as the first session on an account. Asking
/// another session only works while it's open, so the user picks when, and
/// the offer only includes it when there's a cross-signed session to ask.
async fn offer_verification(matrix: Matrix) {
    let encryption = matrix.client().encryption();

    if encryption
        .cross_signing_status()
        .await
        .is_some_and(|status| status.is_complete())
    {
        return;
    }

    // this session is unverified, so the device found can't be this one
    let other_session = match encryption.has_devices_to_verify_against().await {
        Ok(found) => found,
        Err(err) => {
            error!("could not get devices: {}", err);
            return;
        }
    };

    matrix.send(VerificationOffered { other_session });
}

/// Ask the user's other cross-signed sessions to verify this one. With none,
/// the SDK sends the request to every device, this one included, where it
/// stays pending for ten minutes. Until then, the SDK cancels it along with
/// any request the user starts from another device. So it's only offered
/// when another session can answer.
async fn request_verification(matrix: Matrix) {
    let client = matrix.client();

    let Some(user_id) = client.user_id() else {
        return;
    };

    match client.encryption().get_user_identity(user_id).await {
        Ok(Some(identity)) => {
            match identity
                .request_verification_with_methods(vec![VerificationMethod::SasV1])
                .await
            {
                Err(err) => error!("could not request verification: {}", err),
                Ok(request) => own_verification(request, matrix, None).await,
            }
        }
        Ok(None) => error!("no user identity"),
        Err(err) => error!("could not get user identity: {}", err),
    };
}

/// The emoji comparison a request moved into, and its changes.
type Comparison = (SasVerification, BoxStream<'static, SasState>);

enum Change {
    Request(VerificationRequestState),
    Comparison(SasState),
}

/// Own a verification until it ends: prompt for a request from another
/// device, follow the request into the emoji comparison, and withdraw its
/// popups at the end, however it ends. The side that sent the request
/// starts the comparison once the other side is ready. Some clients, such as
/// Element X, start one too, and if theirs wins, it replaces ours, so this
/// goes on to follow the replacement. The first cancellation it hears, from
/// the request or the comparison, is the only one it shows.
async fn own_verification(request: VerificationRequest, matrix: Matrix, prompt: Option<String>) {
    let notice = matrix.notice();

    if let Some(message) = prompt {
        matrix.send(VerificationRequested(notice.id(), request.clone(), message));
    }

    let mut requests = request.changes();
    let mut comparison: Option<Comparison> = None;

    loop {
        let change = tokio::select! {
            state = requests.next() => match state {
                Some(state) => Change::Request(state),
                None => return,
            },
            state = next_comparison_state(&mut comparison) => Change::Comparison(state),
        };

        match change {
            Change::Request(
                VerificationRequestState::Created { .. }
                | VerificationRequestState::Requested { .. },
            ) => (),
            Change::Request(VerificationRequestState::Ready { .. }) => {
                if request.we_started()
                    && let Err(err) = request.start_sas().await
                {
                    error!("could not start SAS verification: {}", err);
                    matrix.send(Error(format!("Could not start verification: {}", err)));
                    return;
                }
            }
            Change::Request(VerificationRequestState::Transitioned { verification }) => {
                // We only support SAS verification.
                if let Verification::SasV1(sas) = verification {
                    let changes = sas.changes().boxed();

                    if let Err(err) = sas.accept().await {
                        error!("could not accept SAS verification: {}", err);
                        matrix.send(Error(format!("Could not start verification: {}", err)));
                        return;
                    }

                    comparison = Some((sas, changes));
                }
            }
            Change::Request(VerificationRequestState::Cancelled(info)) => {
                info!("verification request cancelled: {}", info.reason());

                match info.cancel_code() {
                    // another session answered
                    CancelCode::Accepted => (),
                    // nobody answered ours. Had another device asked since,
                    // the SDK would have cancelled both already, so the user
                    // verified some other way, such as recovery, or ignored it.
                    CancelCode::Timeout if request.we_started() => (),
                    _ => matrix.send(VerificationCancelled(info)),
                }

                return;
            }
            Change::Request(VerificationRequestState::Done) => return,
            Change::Comparison(SasState::KeysExchanged { emojis, .. }) => {
                info!("verification keys exchanged");

                let emoji = emojis.expect("only emoji verification is supported").emojis;

                if let Some((sas, _)) = &comparison {
                    matrix.send(VerificationStarted(notice.id(), sas.clone(), emoji));
                }
            }
            Change::Comparison(SasState::Done { .. }) => {
                info!("verification done");
                return;
            }
            Change::Comparison(SasState::Cancelled(info)) => {
                info!("verification cancelled: {}", info.reason());

                matrix.send(VerificationCancelled(info));
                return;
            }
            Change::Comparison(state) => info!("verification state: {:?}", state),
        }
    }
}

/// The next change to the emoji comparison, once there is one.
async fn next_comparison_state(comparison: &mut Option<Comparison>) -> SasState {
    if let Some((_, changes)) = comparison
        && let Some(state) = changes.next().await
    {
        return state;
    }

    std::future::pending().await
}

async fn decline_request(request: VerificationRequest) {
    if let Err(err) = request.cancel().await {
        error!("could not decline verification request: {}", err);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};

    use matrix_sdk::RoomDisplayName;
    use matrix_sdk::encryption::verification::{Emoji, VerificationRequest};
    use matrix_sdk::ruma::exports::serde_json::json;
    use matrix_sdk::ruma::{
        DeviceId, MilliSecondsSinceUnixEpoch, TransactionId, device_id, room_id, user_id,
    };
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk::test_utils::mocks::encryption::PendingToDeviceMessages;
    use tokio::runtime::Handle;
    use wiremock::ResponseTemplate;

    use super::*;
    use crate::app::{App, NoticeId, Popup};
    use crate::event::Event;
    use crate::event::Event::Matui;
    use crate::handler::MatuiEvent::Withdraw;
    use crate::handler::handle_app_event;
    use crate::matrix::roomcache::DecoratedRoom;
    use crate::widgets::compose::Compose;

    type Queue = Arc<Mutex<PendingToDeviceMessages>>;

    /// A `Matrix` for this client, sending to the returned receiver.
    fn matrix_for(client: &Client) -> (Matrix, Receiver<Event>) {
        let (sender, events) = channel();
        let matrix = Matrix::new(sender, Handle::current());
        matrix.client.set(client.clone()).unwrap();

        (matrix, events)
    }

    /// Two sessions for one user that know each other's devices: matui,
    /// with its handlers sending to the returned receiver, and another
    /// device. Every to-device message either sends waits in the queue.
    async fn two_sessions() -> (MatrixMockServer, Client, Client, Queue, Receiver<Event>) {
        two_sessions_with(device_id!("OTHER")).await
    }

    /// The same, with the other device's ID, which decides whose emoji
    /// comparison wins when both sides start one.
    async fn two_sessions_with(
        other_device: &DeviceId,
    ) -> (MatrixMockServer, Client, Client, Queue, Receiver<Event>) {
        let server = MatrixMockServer::new().await;
        server.mock_crypto_endpoints_preset().await;

        let user = user_id!("@alice:example.org");

        let matui = server
            .client_builder_for_crypto_end_to_end(user, device_id!("MATUI"))
            .build()
            .await;

        server.mock_sync().ok_and_run(&matui, |_| {}).await;

        let other = server
            .set_up_new_device_for_encryption(&matui, other_device, vec![])
            .await;

        let (matrix, events) = matrix_for(&matui);
        add_verification_handlers(&matrix);

        (server, matui, other, Queue::default(), events)
    }

    /// Deliver the to-device messages waiting for this client. This builds
    /// its own sync response: the mock server's sync builder never clears
    /// to-device events, so it would send each one again on every sync, to
    /// either client.
    async fn relay(server: &MatrixMockServer, queue: &Queue, to: &Client) {
        let messages: Vec<_> = queue
            .lock()
            .unwrap()
            .get_mut(to.user_id().unwrap())
            .and_then(|devices| devices.remove(to.device_id().unwrap()))
            .unwrap_or_default();

        let _sync = server
            .mock_sync()
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "next_batch": TransactionId::new(),
                "to_device": { "events": messages },
            })))
            .mount_as_scoped()
            .await;

        to.sync_once(Default::default()).await.unwrap();
    }

    /// Have the other device ask matui to verify, and return its request
    /// along with the one matui prompted for, and the prompt's ID.
    async fn request_from_other(
        server: &MatrixMockServer,
        matui: &Client,
        other: &Client,
        queue: &Queue,
        events: &Receiver<Event>,
    ) -> (VerificationRequest, VerificationRequest, NoticeId) {
        let sent = other
            .encryption()
            .get_device(matui.user_id().unwrap(), matui.device_id().unwrap())
            .await
            .unwrap()
            .expect("the other device knows matui's")
            .request_verification()
            .await
            .unwrap();

        relay(server, queue, matui).await;

        let event = next_event(events).await;
        let Matui(VerificationRequested(id, received, message)) = event else {
            panic!("expected a verification prompt, got {:?}", event);
        };

        assert_eq!(message, "OTHER asks to verify this session. Accept?");
        assert!(events.try_recv().is_err());

        (sent, received, id)
    }

    /// Wait for the next event, while tasks such as `own_verification` run.
    async fn next_event(events: &Receiver<Event>) -> Event {
        let next = async {
            loop {
                if let Ok(event) = events.try_recv() {
                    return event;
                }

                tokio::task::yield_now().await;
            }
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), next)
            .await
            .expect("an event")
    }

    /// The withdrawal of a verification's popups, which is the last thing
    /// its owner sends.
    async fn withdrawn(events: &Receiver<Event>) -> NoticeId {
        let event = next_event(events).await;
        let Matui(Withdraw(id)) = event else {
            panic!("expected a withdrawal, got {:?}", event);
        };

        assert!(events.try_recv().is_err());
        id
    }

    /// How a verification ends when cancelled: one cancellation, as who
    /// cancelled and why, then the ID of the popups withdrawn.
    async fn cancelled(events: &Receiver<Event>) -> (bool, &'static str, NoticeId) {
        let event = next_event(events).await;
        let Matui(VerificationCancelled(info)) = event else {
            panic!("expected a cancellation, got {:?}", event);
        };

        (
            info.cancelled_by_us(),
            info.reason(),
            withdrawn(events).await,
        )
    }

    /// An App with nothing showing, and no client.
    fn test_app() -> App {
        App::new(channel().0, Handle::current())
    }

    /// Hand the app what the owner sends, until its popups are withdrawn.
    async fn deliver(app: &mut App, events: &Receiver<Event>) {
        loop {
            let Matui(event) = next_event(events).await else {
                panic!("expected a Matui event");
            };
            let done = matches!(event, Withdraw(_));

            handle_app_event(event, app);

            if done {
                return;
            }
        }
    }

    /// Have matui ask the other device to verify, and both go on into the
    /// emoji comparison. Returns the rest of what matui sends, and matui's
    /// popup ID and comparison with its emoji, then the other device's
    /// comparison.
    async fn compare_emoji(
        server: &MatrixMockServer,
        matui: &Client,
        other: &Client,
        queue: &Queue,
    ) -> (
        Receiver<Event>,
        (NoticeId, SasVerification, [Emoji; 7]),
        SasVerification,
    ) {
        let user = matui.user_id().unwrap();

        let request = matui
            .encryption()
            .get_device(user, other.device_id().unwrap())
            .await
            .unwrap()
            .expect("matui knows the other device")
            .request_verification_with_methods(vec![VerificationMethod::SasV1])
            .await
            .unwrap();

        let flow_id = request.flow_id().to_owned();
        let (matrix, events) = matrix_for(matui);
        tokio::spawn(own_verification(request, matrix, None));

        relay(server, queue, other).await;
        other
            .encryption()
            .get_verification_request(user, &flow_id)
            .await
            .expect("the other device got the request")
            .accept()
            .await
            .unwrap();

        // matui starts SAS; the other device goes along with it
        let compared = async {
            loop {
                tokio::task::yield_now().await;
                relay(server, queue, matui).await;
                relay(server, queue, other).await;

                if let Some(Verification::SasV1(sas)) =
                    other.encryption().get_verification(user, &flow_id).await
                {
                    sas.accept().await.unwrap();
                }

                if let Ok(event) = events.try_recv() {
                    return event;
                }
            }
        };

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), compared)
            .await
            .expect("matui shows the emoji");

        let Matui(VerificationStarted(id, ours, emoji)) = event else {
            panic!("expected the emoji, got {:?}", event);
        };

        let Some(Verification::SasV1(theirs)) =
            other.encryption().get_verification(user, &flow_id).await
        else {
            panic!("the other device is comparing emoji");
        };

        (events, (id, ours, emoji), theirs)
    }

    #[tokio::test]
    async fn prompts_and_waits_to_accept() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, received, _) =
            request_from_other(&server, &matui, &other, &queue, &events).await;

        // the prompt alone answers nothing
        relay(&server, &queue, &other).await;
        assert!(!received.is_ready());
        assert!(!sent.is_ready());

        let (matrix, _events) = matrix_for(&matui);
        accept_request(received, matrix).await;
        relay(&server, &queue, &other).await;

        assert!(sent.is_ready());
    }

    #[tokio::test]
    async fn declining_cancels_on_the_other_device() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, received, prompt) =
            request_from_other(&server, &matui, &other, &queue, &events).await;

        decline_request(received).await;
        relay(&server, &queue, &other).await;

        assert!(sent.is_cancelled());
        assert_eq!(
            cancelled(&events).await,
            (true, "The user cancelled the verification.", prompt)
        );
    }

    #[tokio::test]
    async fn shows_a_request_cancelled_by_the_other_device() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, _received, prompt) =
            request_from_other(&server, &matui, &other, &queue, &events).await;

        sent.cancel().await.unwrap();
        relay(&server, &queue, &matui).await;

        assert_eq!(
            cancelled(&events).await,
            (false, "The user cancelled the verification.", prompt)
        );
    }

    #[tokio::test]
    async fn a_cancelled_prompt_gives_way_to_its_reason() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let mut app = test_app();
        let (sent, received, prompt) =
            request_from_other(&server, &matui, &other, &queue, &events).await;
        handle_app_event(
            VerificationRequested(prompt, received, "Accept?".to_string()),
            &mut app,
        );
        assert!(matches!(app.popup, Some(Popup::Confirm(_))));

        sent.cancel().await.unwrap();
        relay(&server, &queue, &matui).await;
        deliver(&mut app, &events).await;

        assert!(matches!(app.popup, Some(Popup::Error(_))));
        app.close_popup();
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn a_cancelled_prompt_waiting_behind_compose_is_gone() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let mut app = test_app();
        let room = server
            .sync_joined_room(&matui, room_id!("!room:example.org"))
            .await;
        let room = DecoratedRoom {
            inner: room,
            name: RoomDisplayName::Empty,
            visited: true,
            last_message: None,
            last_sender: None,
            last_ts: None,
            recency_stamp: None,
        };
        app.set_popup(Popup::Compose(Compose::new(
            room,
            app.matrix.clone(),
            None,
            "half-written",
        )));

        let (sent, received, prompt) =
            request_from_other(&server, &matui, &other, &queue, &events).await;
        handle_app_event(
            VerificationRequested(prompt, received, "Accept?".to_string()),
            &mut app,
        );

        sent.cancel().await.unwrap();
        relay(&server, &queue, &matui).await;
        deliver(&mut app, &events).await;
        assert!(matches!(app.popup, Some(Popup::Compose(_))));

        // the reason shows, but the prompt it cancelled doesn't
        app.close_popup();
        assert!(matches!(app.popup, Some(Popup::Error(_))));
        app.close_popup();
        assert!(app.popup.is_none());
    }

    #[tokio::test]
    async fn closes_a_request_answered_by_another_device() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, _received, prompt) =
            request_from_other(&server, &matui, &other, &queue, &events).await;

        // what the asking device sends the rest once one of them accepts
        server
            .mock_sync()
            .ok_and_run(&matui, |builder| {
                builder.add_to_device_event(json!({
                    "sender": user,
                    "type": "m.key.verification.cancel",
                    "content": {
                        "transaction_id": sent.flow_id(),
                        "code": "m.accepted",
                        "reason": "A m.key.verification.request was accepted by a different device.",
                    },
                }));
            })
            .await;

        assert_eq!(withdrawn(&events).await, prompt);
    }

    #[tokio::test]
    async fn shows_a_collision() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let pending = matui
            .encryption()
            .get_device(user, other.device_id().unwrap())
            .await
            .unwrap()
            .expect("matui knows the other device")
            .request_verification_with_methods(vec![VerificationMethod::SasV1])
            .await
            .unwrap();

        let (matrix, mine) = matrix_for(&matui);
        tokio::spawn(own_verification(pending, matrix, None));

        // the user starts another from the other device, before answering
        other
            .encryption()
            .get_device(user, matui.device_id().unwrap())
            .await
            .unwrap()
            .expect("the other device knows matui's")
            .request_verification()
            .await
            .unwrap();

        relay(&server, &queue, &matui).await;

        // the SDK cancels both, as if the user had, and the new one is
        // cancelled before matui could prompt for it
        let (by_us, reason, _) = cancelled(&mine).await;
        assert_eq!(
            (by_us, reason),
            (true, "The user cancelled the verification.")
        );
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn shows_the_emoji_not_matching_on_the_other_device() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (events, (popup, _, _), theirs) = compare_emoji(&server, &matui, &other, &queue).await;

        theirs.mismatch().await.unwrap();
        relay(&server, &queue, &matui).await;

        assert_eq!(
            cancelled(&events).await,
            (false, "The SAS did not match.", popup)
        );
    }

    #[tokio::test]
    async fn shows_the_emoji_not_matching_here() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (events, (popup, ours, _), _) = compare_emoji(&server, &matui, &other, &queue).await;

        ours.mismatch().await.unwrap();

        assert_eq!(
            cancelled(&events).await,
            (true, "The SAS did not match.", popup)
        );
    }

    #[tokio::test]
    async fn withdraws_the_emoji_once_verified() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (events, (popup, ours, _), theirs) =
            compare_emoji(&server, &matui, &other, &queue).await;

        ours.confirm().await.unwrap();
        theirs.confirm().await.unwrap();

        let done = async {
            loop {
                tokio::task::yield_now().await;
                relay(&server, &queue, &matui).await;
                relay(&server, &queue, &other).await;

                if let Ok(event) = events.try_recv() {
                    return event;
                }
            }
        };

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), done)
            .await
            .expect("matui finishes verifying");

        assert!(
            matches!(event, Matui(Withdraw(id)) if id == popup),
            "got {:?}",
            event
        );
        assert!(ours.is_done());
    }

    #[tokio::test]
    async fn starts_the_emoji_comparison_once_accepted() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (_, (_, _, emoji), theirs) = compare_emoji(&server, &matui, &other, &queue).await;

        let symbols = |emoji: &[Emoji]| emoji.iter().map(|e| e.symbol).collect::<Vec<_>>();
        assert_eq!(symbols(&emoji), symbols(&theirs.emoji().unwrap()));
    }

    #[tokio::test]
    async fn follows_the_other_sides_emoji_comparison_when_both_start() {
        // sorting before MATUI, its comparison replaces matui's
        let (server, matui, other, queue, _events) = two_sessions_with(device_id!("ELEMENT")).await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let request = matui
            .encryption()
            .get_device(user, other.device_id().unwrap())
            .await
            .unwrap()
            .expect("matui knows the other device")
            .request_verification_with_methods(vec![VerificationMethod::SasV1])
            .await
            .unwrap();

        let flow_id = request.flow_id().to_owned();
        let (matrix, events) = matrix_for(&matui);
        tokio::spawn(own_verification(request, matrix, None));

        relay(&server, &queue, &other).await;
        let theirs = other
            .encryption()
            .get_verification_request(user, &flow_id)
            .await
            .expect("the other device got the request");
        theirs.accept().await.unwrap();

        // matui starts a comparison once it hears the other side is ready...
        relay(&server, &queue, &matui).await;
        let started = async {
            while matui
                .encryption()
                .get_verification(user, &flow_id)
                .await
                .is_none()
            {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), started)
            .await
            .expect("matui starts a comparison");

        // ...and so does the other side, as Element X does, before hearing
        // about matui's
        let theirs = theirs.start_sas().await.unwrap().expect("a comparison");

        let compared = async {
            loop {
                tokio::task::yield_now().await;
                relay(&server, &queue, &matui).await;
                relay(&server, &queue, &other).await;

                if let Ok(event) = events.try_recv() {
                    return event;
                }
            }
        };

        let event = tokio::time::timeout(std::time::Duration::from_secs(5), compared)
            .await
            .expect("matui shows the emoji");

        let Matui(VerificationStarted(_, _, emoji)) = event else {
            panic!("expected the emoji, got {:?}", event);
        };

        let symbols = |emoji: &[Emoji]| emoji.iter().map(|e| e.symbol).collect::<Vec<_>>();
        assert_eq!(symbols(&emoji), symbols(&theirs.emoji().unwrap()));
    }

    #[tokio::test]
    async fn ignores_its_own_request() {
        let (server, matui, _other, _queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();

        // with no other cross-signed device, the request goes to every
        // device, matui's own included
        server
            .mock_send_to_device()
            .expect_any_access_token()
            .ok()
            .mount()
            .await;

        matui
            .encryption()
            .bootstrap_cross_signing(None)
            .await
            .unwrap();

        let request = matui
            .encryption()
            .get_user_identity(user)
            .await
            .unwrap()
            .unwrap()
            .request_verification_with_methods(vec![VerificationMethod::SasV1])
            .await
            .unwrap();

        server
            .mock_sync()
            .ok_and_run(&matui, |builder| {
                builder.add_to_device_event(json!({
                    "sender": user,
                    "type": "m.key.verification.request",
                    "content": {
                        "from_device": "MATUI",
                        "transaction_id": request.flow_id(),
                        "methods": ["m.sas.v1"],
                        "timestamp": MilliSecondsSinceUnixEpoch::now(),
                    },
                }));
            })
            .await;

        assert!(events.try_recv().is_err());
    }

    /// Cross-sign the other session, and tell matui about it.
    async fn cross_sign_other(server: &MatrixMockServer, matui: &Client, other: &Client) {
        other
            .encryption()
            .bootstrap_cross_signing(None)
            .await
            .unwrap();

        server
            .mock_sync()
            .ok_and_run(matui, |builder| {
                builder.add_change_device(matui.user_id().unwrap());
            })
            .await;
        server.mock_sync().ok_and_run(matui, |_| {}).await;
    }

    #[tokio::test]
    async fn offers_nothing_once_verified() {
        let (_server, matui, _other, _queue, _events) = two_sessions().await;

        // the first session on an account cross-signs itself
        matui
            .encryption()
            .bootstrap_cross_signing(None)
            .await
            .unwrap();

        let (matrix, events) = matrix_for(&matui);
        offer_verification(matrix).await;

        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn offers_only_recovery_without_another_verified_session() {
        let (_server, matui, _other, _queue, _events) = two_sessions().await;

        let (matrix, events) = matrix_for(&matui);
        offer_verification(matrix).await;

        let event = events.try_recv().expect("an offer");
        assert!(
            matches!(
                event,
                Matui(VerificationOffered {
                    other_session: false
                })
            ),
            "got {:?}",
            event
        );
    }

    #[tokio::test]
    async fn offers_another_verified_session() {
        let (server, matui, other, _queue, _events) = two_sessions().await;
        cross_sign_other(&server, &matui, &other).await;

        let (matrix, events) = matrix_for(&matui);
        offer_verification(matrix).await;

        let event = events.try_recv().expect("an offer");
        assert!(
            matches!(
                event,
                Matui(VerificationOffered {
                    other_session: true
                })
            ),
            "got {:?}",
            event
        );
    }

    #[tokio::test]
    async fn asks_another_verified_session() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        cross_sign_other(&server, &matui, &other).await;

        let (matrix, _events) = matrix_for(&matui);
        tokio::spawn(request_verification(matrix));

        let asked = async {
            loop {
                tokio::task::yield_now().await;

                let queue = queue.lock().unwrap();
                if let Some(devices) = queue.get(user) {
                    return devices.keys().cloned().collect::<Vec<_>>();
                }
            }
        };

        let asked = tokio::time::timeout(std::time::Duration::from_secs(5), asked)
            .await
            .expect("matui sends a request");

        assert_eq!(asked, [other.device_id().unwrap()]);
    }
}
