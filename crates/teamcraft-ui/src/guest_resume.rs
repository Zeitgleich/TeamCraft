//! A meeting guest's session kept across a reload (meetings.md, "Guests in the browser").
//!
//! Once the server let the guest into the lobby, the page seals the session (the server's token,
//! the meeting and the guest's device) with a fresh key from `teamcraft-crypto`, keeps only the
//! ciphertext in this browser's storage and puts the key into the address fragment
//! (`/meeting/<link>#r=<key>`). A reload of that address opens the session again and asks the
//! server whether the guest is still waiting or was admitted; the server decides, the stored copy
//! only proves nothing. The fragment never reaches a server, and the address opens nothing in
//! another browser. Leaving, a refusal or an ended session forget the copy.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::guest_resume::GuestResumeKey;
use teamcraft_proto::{ConversationId, MeetingId, UserId};

use crate::route::MeetingHandle;
use crate::settings::{load_string, store_string};

/// Local storage key of the sealed session; one guest session per browser, like its database.
const STORAGE_KEY: &str = "teamcraft.guest_session";
/// The key's name in the address fragment.
pub const RESUME_FRAGMENT_KEY: &str = "r=";

/// What a reloaded guest page needs to carry on without knocking again.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredGuestSession {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  pub user_id: UserId,
  pub guest_token: String,
  pub maybe_organizer_fingerprint: Option<String>,
}

impl StoredGuestSession {
  /// Seals the session into this browser's storage; the address that brings the guest back, or
  /// `None` when it could not be kept (the page still works, a reload knocks anew).
  pub fn keep(&self, handle: &MeetingHandle) -> Option<String> {
    let key = GuestResumeKey::generate(&mut OsRng);
    let plaintext = serde_json::to_vec(self)
      .map_err(|error| tracing::debug!(%error, "could not encode the guest session"))
      .ok()?;
    let sealed = key
      .seal(&plaintext, &mut OsRng)
      .map_err(|error| tracing::debug!(%error, "could not seal the guest session"))
      .ok()?;
    store_string(STORAGE_KEY, &STANDARD.encode(sealed));
    Some(resume_address(handle, &key.to_fragment()))
  }

  /// The session this browser keeps for the key from the address, if it opens.
  pub fn load(fragment_key: &str) -> Option<Self> {
    let key = GuestResumeKey::from_fragment(fragment_key).ok()?;
    let sealed = STANDARD.decode(load_string(STORAGE_KEY)?).ok()?;
    let plaintext = key.open(&sealed).ok()?;
    serde_json::from_slice(plaintext.expose_secret()).ok()
  }

  /// Forgets the kept session (left, turned away, ended, or a page that can't resume it).
  pub fn forget() {
    store_string(STORAGE_KEY, "");
  }
}

impl fmt::Debug for StoredGuestSession {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("StoredGuestSession")
      .field("meeting", &self.meeting)
      .field("conversation", &self.conversation)
      .finish_non_exhaustive()
  }
}

/// The guest's own address of the meeting: the link's path with the key in the fragment.
fn resume_address(handle: &MeetingHandle, fragment_key: &str) -> String {
  format!(
    "/meeting/{}#{RESUME_FRAGMENT_KEY}{fragment_key}",
    handle.path_segment()
  )
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn resume_address_keeps_the_link_and_puts_the_key_in_the_fragment() {
    let handle = MeetingHandle::Id(id(123_456_789));
    assert_eq!(resume_address(&handle, "abc"), "/meeting/123456789#r=abc");
  }

  #[test]
  fn debug_hides_the_token() {
    let session = StoredGuestSession {
      meeting: id(1),
      conversation: id(2),
      user_id: id(3),
      guest_token: "secret-token".into(),
      maybe_organizer_fingerprint: None,
    };
    assert!(!format!("{session:?}").contains("secret-token"));
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
