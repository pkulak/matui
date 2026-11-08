use std::sync::mpsc::Sender;

use futures::stream::StreamExt;
use log::{error, info};
use matrix_sdk::Client;
use matrix_sdk::encryption::verification::{
    SasState, SasVerification, Verification, VerificationRequest, VerificationRequestState,
};
use matrix_sdk::ruma::events::key::verification::VerificationMethod;
use matrix_sdk::ruma::events::key::verification::request::ToDeviceKeyVerificationRequestEvent;
use matrix_sdk::ruma::events::room::message::{MessageType, OriginalSyncRoomMessageEvent};

use super::Matrix;
use crate::app::App;
use crate::event::Event;
use crate::event::Event::Matui;
use crate::handler::MatuiEvent;
use crate::handler::MatuiEvent::{
    Error, VerificationCompleted, VerificationRequested, VerificationStarted,
};

impl Matrix {
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

    /// Accept a request from another device, and follow it into the emoji
    /// comparison.
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
        .send(Matui(VerificationRequested(request, message)))
        .expect("could not send verification request event");
}

async fn accept_request(request: VerificationRequest, sender: Sender<Event>) {
    if let Err(err) = request.accept().await {
        error!("could not accept verification request: {}", err);
        let _ = sender.send(Matui(Error(format!(
            "Could not accept verification: {}",
            err
        ))));
        return;
    }

    let mut stream = request.changes();

    while let Some(state) = stream.next().await {
        match state {
            VerificationRequestState::Created { .. }
            | VerificationRequestState::Requested { .. }
            | VerificationRequestState::Ready { .. } => (),
            VerificationRequestState::Transitioned { verification } => {
                // We only support SAS verification.
                if let Verification::SasV1(s) = verification {
                    sas_verification_handler(s, sender).await;
                    break;
                }
            }
            VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => break,
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
            SasState::Cancelled(_) => info!("verification cancelled"),
            SasState::Created { .. } => info!("verification created"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};

    use matrix_sdk::encryption::verification::VerificationRequest;
    use matrix_sdk::ruma::exports::serde_json::{Value, json};
    use matrix_sdk::ruma::{MilliSecondsSinceUnixEpoch, device_id, user_id};
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk::test_utils::mocks::encryption::PendingToDeviceMessages;

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

    /// Deliver the to-device messages waiting for this client.
    async fn relay(server: &MatrixMockServer, queue: &Queue, to: &Client) {
        let messages: Vec<_> = queue
            .lock()
            .unwrap()
            .get_mut(to.user_id().unwrap())
            .and_then(|devices| devices.remove(to.device_id().unwrap()))
            .unwrap_or_default();

        for message in messages {
            server
                .mock_sync()
                .ok_and_run(to, |builder| {
                    builder.add_to_device_event(message.deserialize_as::<Value>().unwrap());
                })
                .await;
        }
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
        tokio::spawn(accept_request(received, sender));

        // accepting goes on to follow the request, so wait for its answer
        let answered = async {
            while !sent.is_ready() {
                tokio::task::yield_now().await;
                relay(&server, &queue, &other).await;
            }
        };

        tokio::time::timeout(std::time::Duration::from_secs(5), answered)
            .await
            .expect("the other device sees the request accepted");
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
}
