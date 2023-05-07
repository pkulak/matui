use ruma::OwnedUserId;
use std::collections::HashMap;

use ruma::{
    events::receipt::{ReceiptEventContent, ReceiptType},
    OwnedEventId,
};

use crate::matrix::username::Username;

/// A place to put and update read receipts.
pub struct Receipts {
    events: HashMap<OwnedEventId, Vec<Username>>,
    ignore: OwnedUserId,
}

impl Receipts {
    pub fn new(ignore: OwnedUserId) -> Self {
        Receipts {
            events: HashMap::default(),
            ignore,
        }
    }

    pub fn get(&self, event_id: &OwnedEventId) -> Option<&Vec<Username>> {
        self.events.get(event_id)
    }

    pub fn apply_event(&mut self, event: &ReceiptEventContent) {
        for (event_id, types) in event.iter() {
            if let Some(user_ids) = types.get(&ReceiptType::Read) {
                for user_id in user_ids.keys() {
                    self.apply_event_and_user(event_id, user_id);
                }
            }
        }
    }

    fn apply_event_and_user(&mut self, event_id: &OwnedEventId, user_id: &OwnedUserId) {
        if user_id == &self.ignore {
            return;
        }

        // wipe out any previous receipts for this user
        for (_, usernames) in self.events.iter_mut() {
            usernames.retain(|u| &u.id != user_id)
        }

        // add our receipt
        self.events
            .entry(event_id.clone())
            .or_insert_with(|| Vec::with_capacity(1))
            .push(Username::new(user_id.clone()));

        // and clean up any now-empty vectors
        self.events.retain(|_, value| !value.is_empty());
    }

    pub fn get_senders(event: &ReceiptEventContent) -> Vec<&OwnedUserId> {
        let mut ids = vec![];

        for types in event.values() {
            if let Some(user_ids) = types.get(&ReceiptType::Read) {
                for user_id in user_ids.keys() {
                    ids.push(user_id);
                }
            }
        }

        ids
    }
}
