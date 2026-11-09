use log::error;
use matrix_sdk::deserialized_responses::TimelineEventKind;
use matrix_sdk::room::Room;
use matrix_sdk::ruma::OwnedRoomId;
use matrix_sdk::ruma::events::AnyTimelineEvent;
use matrix_sdk::ruma::events::room::encrypted::{EncryptedEventScheme, OriginalRoomEncryptedEvent};
use matrix_sdk::ruma::exports::serde_json::json;
use matrix_sdk::ruma::serde::Raw;

use super::Matrix;
use crate::handler::MatuiEvent;

/// A room key that arrived: the room it's for, and its megolm session.
#[derive(Clone, Debug)]
pub struct RoomKey {
    pub room_id: OwnedRoomId,
    pub session_id: String,
}

impl Matrix {
    /// Decrypt again the events these keys are for, and send any that
    /// decrypt to the UI. With no keys, try them all.
    pub fn decrypt_again(
        &self,
        room: Room,
        events: Vec<OriginalRoomEncryptedEvent>,
        keys: Option<Vec<RoomKey>>,
    ) {
        let matrix = self.clone();

        self.spawn(async move {
            let decrypted = decrypt_again(&room, events, keys.as_deref()).await;

            if !decrypted.is_empty() {
                matrix.send(MatuiEvent::Decrypted(decrypted));
            }
        });
    }
}

/// The events these keys are for that now decrypt. With no keys, try them all.
async fn decrypt_again(
    room: &Room,
    events: Vec<OriginalRoomEncryptedEvent>,
    keys: Option<&[RoomKey]>,
) -> Vec<AnyTimelineEvent> {
    let mut decrypted = vec![];

    // each miss asks the backup for its key, so only try the ones that match
    for event in events.into_iter().filter(|e| matches(room, e, keys)) {
        // ruma events don't serialize, so rebuild what decryption reads
        let raw = Raw::new(&json!({
            "type": "m.room.encrypted",
            "content": event.content,
            "event_id": event.event_id,
            "sender": event.sender,
            "origin_server_ts": event.origin_server_ts,
        }));

        let result = match raw {
            Ok(raw) => room.decrypt_event(&raw.cast_unchecked(), None).await,
            Err(err) => {
                error!("could not rebuild encrypted event: {}", err);
                continue;
            }
        };

        match result.map(|e| e.kind) {
            Ok(TimelineEventKind::Decrypted(d)) => match d.event.deserialize() {
                Ok(event) => decrypted.push(event),
                Err(err) => error!("could not deserialize decrypted event: {}", err),
            },
            Ok(_) => {}
            Err(err) => error!("could not decrypt event: {}", err),
        }
    }

    decrypted
}

fn matches(room: &Room, event: &OriginalRoomEncryptedEvent, keys: Option<&[RoomKey]>) -> bool {
    let EncryptedEventScheme::MegolmV1AesSha2(content) = &event.content.scheme else {
        return false;
    };

    keys.is_none_or(|keys| {
        keys.iter()
            .any(|k| k.room_id == room.room_id() && k.session_id == content.session_id)
    })
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::room::member::RoomMemberEvent;
    use matrix_sdk::ruma::events::room::message::RoomMessageEventContent;
    use matrix_sdk::ruma::events::{AnyMessageLikeEvent, MessageLikeEvent};
    use matrix_sdk::ruma::{RoomVersionId, event_id, owned_room_id, room_id};
    use matrix_sdk::test_utils::mocks::MatrixMockServer;
    use matrix_sdk_test::JoinedRoomBuilder;
    use matrix_sdk_test::event_factory::EventFactory;

    use super::*;

    /// Bob's room, and an event in it that Alice encrypted and Bob can't
    /// decrypt yet. Returns once Bob has its room key, with that key.
    async fn late_key() -> (MatrixMockServer, Room, OriginalRoomEncryptedEvent, RoomKey) {
        let server = MatrixMockServer::new().await;
        server.mock_crypto_endpoints_preset().await;

        let (alice, bob) = server.set_up_alice_and_bob_for_encryption().await;
        let room_id = room_id!("!room:example.org");
        let alice_id = alice.user_id().unwrap();
        let bob_id = bob.user_id().unwrap();

        let factory = EventFactory::new().room(room_id).sender(alice_id);
        let members: Vec<Raw<RoomMemberEvent>> = vec![
            factory.member(alice_id).into_raw(),
            factory.member(bob_id).sender(bob_id).into_raw(),
        ];

        for client in [&alice, &bob] {
            let mut room = JoinedRoomBuilder::new(room_id)
                .add_state_event(factory.create(alice_id, RoomVersionId::V1))
                .add_state_event(factory.room_encryption());

            for member in &members {
                room = room.add_state_event(member.clone().cast_unchecked());
            }

            server
                .mock_sync()
                .ok_and_run(client, |builder| {
                    builder.add_joined_room(room);
                })
                .await;
        }

        server.mock_get_members().ok(members.clone()).mount().await;

        let (sent, mock) = server
            .mock_room_send()
            .ok_with_capture(event_id!("$secret"), alice_id);
        mock.mock_once().mount().await;

        // hold back the room key until Bob has the event
        let key_sent = server
            .mock_capture_put_to_device_then_sync_back(alice_id, &bob)
            .await;

        alice
            .get_room(room_id)
            .unwrap()
            .send(RoomMessageEventContent::text_plain("secret"))
            .await
            .unwrap();

        let event = sent
            .await
            .unwrap()
            .deserialize()
            .unwrap()
            .into_full_event(room_id.to_owned());

        let AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomEncrypted(
            MessageLikeEvent::Original(event),
        )) = event
        else {
            panic!("expected an encrypted event, got {:?}", event);
        };

        let EncryptedEventScheme::MegolmV1AesSha2(content) = &event.content.scheme else {
            panic!("expected a megolm event");
        };

        let key = RoomKey {
            room_id: room_id.to_owned(),
            session_id: content.session_id.clone(),
        };

        let bob_room = bob.get_room(room_id).unwrap();

        // nothing to decrypt with yet
        assert!(
            decrypt_again(
                &bob_room,
                vec![event.clone()],
                Some(std::slice::from_ref(&key))
            )
            .await
            .is_empty()
        );

        key_sent.await;

        (server, bob_room, event, key)
    }

    #[tokio::test]
    async fn decrypts_once_the_key_arrives() {
        let (_server, room, event, key) = late_key().await;

        let decrypted = decrypt_again(&room, vec![event], Some(&[key])).await;

        let [AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(message))] =
            decrypted.as_slice()
        else {
            panic!("expected one message, got {:?}", decrypted);
        };

        assert_eq!(message.event_id(), "$secret");
        assert_eq!(message.as_original().unwrap().content.body(), "secret");
    }

    #[tokio::test]
    async fn decrypts_everything_without_keys() {
        let (_server, room, event, _key) = late_key().await;

        assert_eq!(decrypt_again(&room, vec![event], None).await.len(), 1);
    }

    #[tokio::test]
    async fn ignores_keys_for_another_room_or_session() {
        let (_server, room, event, key) = late_key().await;

        let other_room = RoomKey {
            room_id: owned_room_id!("!other:example.org"),
            ..key.clone()
        };

        let other_session = RoomKey {
            session_id: "other".to_string(),
            ..key
        };

        let decrypted = decrypt_again(&room, vec![event], Some(&[other_room, other_session])).await;

        assert!(decrypted.is_empty());
    }
}
