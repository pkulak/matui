use std::sync::mpsc::Sender;

use futures::stream::StreamExt;
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
use crate::app::App;
use crate::event::Event;
use crate::event::Event::Matui;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::{
    Error, VerificationAnsweredElsewhere, VerificationCancelled, VerificationCompleted,
    VerificationOffered, VerificationRequested, VerificationStarted,
};

impl Matrix {
    pub fn offer_verification(&self) {
        App::spawn(offer_verification(self.client(), App::get_sender()));
    }

    pub fn verify(&self) {
        App::spawn(request_verification(self.client(), App::get_sender()));
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

    /// Accept a request from another device. Matui has followed it since
    /// prompting, and goes on into the emoji comparison.
    pub fn accept_verification(&self, request: VerificationRequest) {
        App::spawn(accept_request(request, App::get_sender()));
    }

    pub fn decline_verification(&self, request: VerificationRequest) {
        App::spawn(decline_request(request));
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
}

pub(super) fn add_verification_handlers(client: Client, sender: Sender<Event>) {
    let to_device_sender = sender.clone();

    client.add_event_handler(
        move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| async move {
            match client
                .encryption()
                .get_verification_request(&ev.sender, &ev.content.transaction_id)
                .await
            {
                Some(request) => ask_to_accept(request, &to_device_sender),
                None => error!("could not create request"),
            }
        },
    );

    client.add_event_handler(
        move |ev: OriginalSyncRoomMessageEvent, client: Client| async move {
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype {
                match client
                    .encryption()
                    .get_verification_request(&ev.sender, &ev.event_id)
                    .await
                {
                    Some(request) => ask_to_accept(request, &sender),
                    None => error!("could not create request"),
                }
            }
        },
    );
}

/// Ask whether to accept a request from another device. Only an incoming
/// request still waiting for an answer is `Requested`: one we sent ourselves
/// (which reaches us when it goes to all our devices) starts out `Created`.
fn ask_to_accept(request: VerificationRequest, sender: &Sender<Event>) {
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

    sender
        .send(Matui(VerificationRequested(request.clone(), message)))
        .expect("could not send verification request event");

    // follow it while the prompt is up too, so a cancellation closes it
    tokio::spawn(follow_request(request, sender.clone()));
}

async fn accept_request(request: VerificationRequest, sender: Sender<Event>) {
    if let Err(err) = request.accept().await {
        error!("could not accept verification request: {}", err);
        let _ = sender.send(Matui(Error(format!(
            "Could not accept verification: {}",
            err
        ))));
    }
}

/// Offer to verify this session, unless it's verified already: it has the
/// cross-signing keys, such as the first session on an account. Asking
/// another session only works while it's open, so the user picks when, and
/// the offer only includes it when there's a cross-signed session to ask.
async fn offer_verification(client: Client, sender: Sender<Event>) {
    let encryption = client.encryption();

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

    let _ = sender.send(Matui(VerificationOffered { other_session }));
}

/// Ask the user's other cross-signed sessions to verify this one. With none,
/// the SDK sends the request to every device, this one included, where it
/// stays pending for ten minutes. Until then, the SDK cancels it along with
/// any request the user starts from another device. So it's only offered
/// when another session can answer.
async fn request_verification(client: Client, sender: Sender<Event>) {
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
                Ok(request) => follow_request(request, sender).await,
            }
        }
        Ok(None) => error!("no user identity"),
        Err(err) => error!("could not get user identity: {}", err),
    };
}

/// Follow a request into the emoji comparison. The side that sent the
/// request starts it once the other side is ready.
async fn follow_request(request: VerificationRequest, sender: Sender<Event>) {
    let mut stream = request.changes();

    while let Some(state) = stream.next().await {
        match state {
            VerificationRequestState::Created { .. }
            | VerificationRequestState::Requested { .. } => (),
            VerificationRequestState::Ready { .. } => {
                if request.we_started()
                    && let Err(err) = request.start_sas().await
                {
                    error!("could not start SAS verification: {}", err);
                    let _ = sender.send(Matui(Error(format!(
                        "Could not start verification: {}",
                        err
                    ))));
                    break;
                }
            }
            VerificationRequestState::Transitioned { verification } => {
                // We only support SAS verification.
                if let Verification::SasV1(s) = verification {
                    sas_verification_handler(s, sender).await;
                    break;
                }
            }
            VerificationRequestState::Cancelled(info) => {
                info!("verification request cancelled: {}", info.reason());

                let event = match info.cancel_code() {
                    CancelCode::Accepted => Some(VerificationAnsweredElsewhere),
                    // nobody answered ours. Had another device asked since,
                    // the SDK would have cancelled both already, so the user
                    // verified some other way, such as recovery, or ignored it.
                    CancelCode::Timeout if request.we_started() => None,
                    _ => Some(VerificationCancelled(info)),
                };

                if let Some(event) = event {
                    let _ = sender.send(Matui(event));
                }

                break;
            }
            VerificationRequestState::Done => break,
        }
    }
}

async fn decline_request(request: VerificationRequest) {
    if let Err(err) = request.cancel().await {
        error!("could not decline verification request: {}", err);
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
            SasState::Cancelled(info) => {
                info!("verification cancelled: {}", info.reason());

                let _ = sender.send(Matui(VerificationCancelled(info)));
                break;
            }
            SasState::Created { .. } => info!("verification created"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};

    use matrix_sdk::encryption::verification::{Emoji, VerificationRequest};
    use matrix_sdk::ruma::exports::serde_json::json;
    use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, TransactionId, device_id, user_id};
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk::test_utils::mocks::encryption::PendingToDeviceMessages;
    use wiremock::ResponseTemplate;

    use super::*;

    type Queue = Arc<Mutex<PendingToDeviceMessages>>;

    /// Two sessions for one user that know each other's devices: matui,
    /// with its handlers sending to the returned receiver, and another
    /// device. Every to-device message either sends waits in the queue.
    async fn two_sessions() -> (MatrixMockServer, Client, Client, Queue, Receiver<Event>) {
        let server = MatrixMockServer::new().await;
        server.mock_crypto_endpoints_preset().await;

        let user = user_id!("@alice:example.org");

        let matui = server
            .client_builder_for_crypto_end_to_end(user, device_id!("MATUI"))
            .build()
            .await;

        server.mock_sync().ok_and_run(&matui, |_| {}).await;

        let other = server
            .set_up_new_device_for_encryption(&matui, device_id!("OTHER"), vec![])
            .await;

        let (sender, events) = channel();
        add_verification_handlers(matui.clone(), sender);

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
    /// along with the one matui prompted for.
    async fn request_from_other(
        server: &MatrixMockServer,
        matui: &Client,
        other: &Client,
        queue: &Queue,
        events: &Receiver<Event>,
    ) -> (VerificationRequest, VerificationRequest) {
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

        let event = events.try_recv().expect("a prompt");
        let Matui(VerificationRequested(received, message)) = event else {
            panic!("expected a verification prompt, got {:?}", event);
        };

        assert_eq!(message, "OTHER asks to verify this session. Accept?");
        assert!(events.try_recv().is_err());

        (sent, received)
    }

    /// Wait for the next event, while tasks such as `follow_request` run.
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

    /// The cancellation in an event, as who cancelled and why.
    fn cancellation(event: Event) -> (bool, &'static str) {
        let Matui(VerificationCancelled(info)) = event else {
            panic!("expected a cancellation, got {:?}", event);
        };

        (info.cancelled_by_us(), info.reason())
    }

    /// Have matui ask the other device to verify, and both go on into the
    /// emoji comparison. Returns the rest of what matui sends, and matui's
    /// comparison with its emoji, then the other device's comparison.
    async fn compare_emoji(
        server: &MatrixMockServer,
        matui: &Client,
        other: &Client,
        queue: &Queue,
    ) -> (
        Receiver<Event>,
        (SasVerification, [Emoji; 7]),
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
        let (sender, events) = channel();
        tokio::spawn(follow_request(request, sender));

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

        let Matui(VerificationStarted(ours, emoji)) = event else {
            panic!("expected the emoji, got {:?}", event);
        };

        let Some(Verification::SasV1(theirs)) =
            other.encryption().get_verification(user, &flow_id).await
        else {
            panic!("the other device is comparing emoji");
        };

        (events, (ours, emoji), theirs)
    }

    #[tokio::test]
    async fn prompts_and_waits_to_accept() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, received) = request_from_other(&server, &matui, &other, &queue, &events).await;

        // the prompt alone answers nothing
        relay(&server, &queue, &other).await;
        assert!(!received.is_ready());
        assert!(!sent.is_ready());

        let (sender, _events) = channel();
        accept_request(received, sender).await;
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

        let (sent, received) = request_from_other(&server, &matui, &other, &queue, &events).await;

        decline_request(received).await;
        relay(&server, &queue, &other).await;

        assert!(sent.is_cancelled());
        assert_eq!(
            cancellation(next_event(&events).await),
            (true, "The user cancelled the verification.")
        );
    }

    #[tokio::test]
    async fn shows_a_request_cancelled_by_the_other_device() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, _received) = request_from_other(&server, &matui, &other, &queue, &events).await;

        sent.cancel().await.unwrap();
        relay(&server, &queue, &matui).await;

        assert_eq!(
            cancellation(next_event(&events).await),
            (false, "The user cancelled the verification.")
        );
    }

    #[tokio::test]
    async fn closes_a_request_answered_by_another_device() {
        let (server, matui, other, queue, events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (sent, _received) = request_from_other(&server, &matui, &other, &queue, &events).await;

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

        let event = next_event(&events).await;
        assert!(
            matches!(event, Matui(VerificationAnsweredElsewhere)),
            "got {:?}",
            event
        );
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

        let (sender, mine) = channel();
        tokio::spawn(follow_request(pending, sender));

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
        assert_eq!(
            cancellation(next_event(&mine).await),
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

        let (events, _, theirs) = compare_emoji(&server, &matui, &other, &queue).await;

        theirs.mismatch().await.unwrap();
        relay(&server, &queue, &matui).await;

        assert_eq!(
            cancellation(next_event(&events).await),
            (false, "The SAS did not match.")
        );
    }

    #[tokio::test]
    async fn shows_the_emoji_not_matching_here() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (events, (ours, _), _) = compare_emoji(&server, &matui, &other, &queue).await;

        ours.mismatch().await.unwrap();

        assert_eq!(
            cancellation(next_event(&events).await),
            (true, "The SAS did not match.")
        );
    }

    #[tokio::test]
    async fn starts_the_emoji_comparison_once_accepted() {
        let (server, matui, other, queue, _events) = two_sessions().await;
        let user = matui.user_id().unwrap();
        let _guard = server
            .capture_put_to_device_traffic(user, queue.clone())
            .await;

        let (_, (_, emoji), theirs) = compare_emoji(&server, &matui, &other, &queue).await;

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

        let (sender, events) = channel();
        offer_verification(matui, sender).await;

        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn offers_only_recovery_without_another_verified_session() {
        let (_server, matui, _other, _queue, _events) = two_sessions().await;

        let (sender, events) = channel();
        offer_verification(matui, sender).await;

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

        let (sender, events) = channel();
        offer_verification(matui, sender).await;

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

        let (sender, _events) = channel();
        tokio::spawn(request_verification(matui.clone(), sender));

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
