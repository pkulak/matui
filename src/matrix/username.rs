use core::fmt;

use matrix_sdk::room::RoomMember;
use ruma::OwnedUserId;

/// A way to store a user ID, with a display name that can be updated later.
#[derive(Clone)]
pub struct Username {
    pub id: OwnedUserId,
    pub display_name: Option<String>,
}

impl PartialEq for Username {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Username {
    pub fn new(id: OwnedUserId) -> Self {
        Username {
            id,
            display_name: None,
        }
    }

    pub fn update(&mut self, member: &RoomMember) {
        if self.id == member.user_id() {
            self.display_name = member.display_name().map(String::from);
        }
    }

    pub fn as_str(&self) -> &str {
        if self.display_name.is_some() {
            self.display_name.as_ref().unwrap().as_str()
        } else {
            self.id.as_str()
        }
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(dn) = &self.display_name {
            fmt::Display::fmt(&dn, f)
        } else {
            fmt::Display::fmt(&self.id, f)
        }
    }
}
