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
use crate::handler::MatuiEvent::{Error, VerificationCompleted, VerificationStarted};

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

pub(super) fn add_verification_handlers(client: Client) {
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
