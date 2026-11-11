//! A meeting guest's session kept across a reload (meetings.md, "Guests in the browser").
//!
//! Once the server let the guest into the lobby, the page seals the session (the server's token,
//! the meeting and the guest's device) with a fresh key from `teamcraft-crypto`, keeps only the
//! ciphertext in this tab's session storage and puts the key into the address fragment
//! (`/meeting/<link>#r=<key>&v=<fingerprint>`). A reload of that address opens the session again and
//! asks the server whether the guest is still waiting or was admitted; the server decides, the
//! stored copy proves nothing on its own. Session storage belongs to the tab and goes when it
//! closes, so a closed tab, a copied address or another browser opens nothing. Leaving, a
//! turned-away guest and any request the server refuses as unauthorized (the meeting's access over,
//! the guest removed) forget the copy.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::guest_resume::GuestResumeKey;
use teamcraft_proto::{ConversationId, MeetingId, UserId};
use zeroize::{Zeroize, Zeroizing};

use crate::route::MeetingHandle;

/// Session storage key of the sealed session: one per tab, gone when the tab closes.
const STORAGE_KEY: &str = "teamcraft.guest_session";
/// The key's name in the address fragment.
pub const RESUME_FRAGMENT_KEY: &str = "r=";
/// The organizer's fingerprint in the fragment, kept so a failed resume knocks with it.
const VERIFIER_FRAGMENT_KEY: &str = "v=";

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
  pub fn keep(&self, handle: &MeetingHandle) -> Option<Zeroizing<String>> {
    let key = GuestResumeKey::generate(&mut OsRng);
    let plaintext = Zeroizing::new(
      serde_json::to_vec(self)
        .map_err(|error| tracing::debug!(%error, "could not encode the guest session"))
        .ok()?,
    );
    let sealed = key
      .seal(&plaintext, &mut OsRng)
      .map_err(|error| tracing::debug!(%error, "could not seal the guest session"))
      .ok()?;
    let storage = session_storage()?;
    if storage.set_item(STORAGE_KEY, &STANDARD.encode(sealed)).is_err() {
      tracing::debug!("could not keep the guest session for a reload");
      return None;
    }
    Some(resume_address(
      handle,
      &key.to_fragment(),
      self.maybe_organizer_fingerprint.as_deref(),
    ))
  }

  /// The session this tab keeps for the key from the address, if it opens.
  pub fn load(fragment_key: &str) -> Option<Self> {
    let key = GuestResumeKey::from_fragment(fragment_key).ok()?;
    let stored = session_storage()?.get_item(STORAGE_KEY).ok().flatten()?;
    let sealed = STANDARD.decode(stored).ok()?;
    let plaintext = key.open(&sealed).ok()?;
    serde_json::from_slice(plaintext.expose_secret()).ok()
  }

  /// Forgets the kept session (left, turned away, refused, or a new knock in this tab).
  pub fn forget() {
    if let Some(storage) = session_storage() {
      // Nothing to do when it can't be removed: it goes with the tab.
      let _ = storage.remove_item(STORAGE_KEY);
    }
  }
}

impl Drop for StoredGuestSession {
  fn drop(&mut self) {
    self.guest_token.zeroize();
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

/// The guest's own address of the meeting: the link's path with the key (and the organizer's
/// fingerprint, when known) in the fragment.
fn resume_address(
  handle: &MeetingHandle,
  fragment_key: &str,
  maybe_fingerprint: Option<&str>,
) -> Zeroizing<String> {
  let mut address = Zeroizing::new(format!(
    "/meeting/{}#{RESUME_FRAGMENT_KEY}{fragment_key}",
    handle.path_segment()
  ));
  if let Some(fingerprint) = maybe_fingerprint {
    address.push('&');
    address.push_str(VERIFIER_FRAGMENT_KEY);
    address.push_str(fingerprint);
  }
  address
}

fn session_storage() -> Option<web_sys::Storage> {
  // Native unit tests have no browser: web-sys would panic.
  if cfg!(not(target_arch = "wasm32")) {
    return None;
  }
  web_sys::window()?.session_storage().ok().flatten()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn resume_address_keeps_the_link_and_puts_the_key_and_fingerprint_in_the_fragment() {
    let handle = MeetingHandle::Id(id(123_456_789));
    assert_eq!(
      resume_address(&handle, "abc", None).as_str(),
      "/meeting/123456789#r=abc"
    );
    assert_eq!(
      resume_address(&handle, "abc", Some("ab12")).as_str(),
      "/meeting/123456789#r=abc&v=ab12"
    );
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
