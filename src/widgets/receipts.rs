use ruma::{MilliSecondsSinceUnixEpoch, OwnedUserId};
use std::collections::{btree_map::Entry, BTreeMap, BinaryHeap};

use ruma::events::receipt::{ReceiptEventContent, ReceiptType};

/// A place to put and update read receipts.
pub struct Receipts {
    markers: BTreeMap<OwnedUserId, MilliSecondsSinceUnixEpoch>,
    ignore: OwnedUserId,
}

impl Receipts {
    pub fn new(ignore: OwnedUserId) -> Self {
        Receipts {
            markers: BTreeMap::default(),
            ignore,
        }
    }

    pub fn apply_event(&mut self, event: &ReceiptEventContent) {
        for types in event.values() {
            if let Some(user_ids) = types.get(&ReceiptType::Read) {
                for (user_id, receipt) in user_ids.iter() {
                    if let Some(ts) = &receipt.ts {
                        self.apply_timestamp_and_user(ts, user_id);
                    }
                }
            }
        }
    }

    pub fn get_all(&self) -> BinaryHeap<Receipt<'_>> {
        let mut heap = BinaryHeap::with_capacity(self.markers.len());

        heap.extend(self.markers.iter().map(|(k, v)| Receipt {
            timestamp: v,
            user_id: k,
        }));

        heap
    }

    fn apply_timestamp_and_user(
        &mut self,
        timestamp: &MilliSecondsSinceUnixEpoch,
        user_id: &OwnedUserId,
    ) {
        if user_id == &self.ignore {
            return;
        }

        match self.markers.entry(user_id.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(*timestamp);
            }
            Entry::Occupied(mut entry) => {
                if timestamp > entry.get() {
                    *entry.get_mut() = *timestamp
                }
            }
        };
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

#[derive(Eq, PartialEq, Ord, PartialOrd, Debug, Clone)]
pub struct Receipt<'a> {
    pub timestamp: &'a MilliSecondsSinceUnixEpoch,
    pub user_id: &'a OwnedUserId,
}
