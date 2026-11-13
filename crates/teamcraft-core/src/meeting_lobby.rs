//! A meeting's lobby as a host's device decides on it (docs/meetings.md, "Lobby"; e2ee.md §4.6,
//! "Meeting guests"): which waiting people a host can admit now, and how an admitted person's
//! devices get into the meeting chat's encryption. Desktop and web (`teamcraft-ui`'s lobby) and the
//! phone apps (through teamcraft-ffi) decide alike; `Client::let_in_from_lobby` acts on it.
//!
//! The server's lobby entry is only a hint of who knocked: the host's device adds a guest only
//! with the identity key it pins and whose fingerprint the guest's page reported, and only when it
//! is the organizer's, a co-host's by the signed host list or a signed admitter's.

use teamcraft_proto::{LobbyEntry, UserKind};

/// How a host's device lets a waiting person in, by their lobby entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LobbyAdmission {
  /// A member of the instance: their devices join the chat's group as any member's.
  Member,
  /// A guest whose page set up its device and reported the fingerprint of its identity key: only
  /// a key with this fingerprint is added.
  Guest { knock_fingerprint: String },
  /// A guest whose page has not set up its device yet: nothing to add, so no "Admit" yet.
  GuestDevicePending,
}

impl LobbyAdmission {
  /// How `entry` is let in.
  pub fn of(entry: &LobbyEntry) -> Self {
    if entry.user.kind != UserKind::Guest {
      return Self::Member;
    }
    match (&entry.maybe_knock_fingerprint, entry.has_device) {
      (Some(fingerprint), true) => Self::Guest {
        knock_fingerprint: fingerprint.clone(),
      },
      _ => Self::GuestDevicePending,
    }
  }

  /// Whether there is anything to add: everybody but a guest whose device isn't set up. Enough for
  /// the organizer's own "no lobby" admission of link arrivals, which asks nobody.
  pub fn has_device_to_add(&self) -> bool {
    !matches!(self, Self::GuestDevicePending)
  }

  /// Whether a host's "Admit" can be offered: a member at once, a guest only once the key code of
  /// the key this device pinned for them is shown (`has_shown_key_code`), so the host can compare
  /// it with the guest's screen first. That comparison is the only defence against a guest the
  /// server made up with its own key (e2ee.md §4.6). Every client's lobby reads this one rule.
  pub fn can_admit(&self, has_shown_key_code: bool) -> bool {
    match self {
      Self::Member => true,
      Self::Guest { .. } => has_shown_key_code,
      Self::GuestDevicePending => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_guest_is_admitted_only_with_a_device_and_its_reported_key() {
    assert_eq!(
      LobbyAdmission::of(&entry(UserKind::Guest, true, Some("ab"))),
      LobbyAdmission::Guest {
        knock_fingerprint: "ab".into()
      }
    );
    let without_key = LobbyAdmission::of(&entry(UserKind::Guest, true, None));
    assert_eq!(without_key, LobbyAdmission::GuestDevicePending);
    assert!(!without_key.has_device_to_add());
    assert_eq!(
      LobbyAdmission::of(&entry(UserKind::Guest, false, Some("ab"))),
      LobbyAdmission::GuestDevicePending
    );
  }

  #[test]
  fn a_member_is_admitted_as_a_member_whatever_the_entry_says_of_keys() {
    let member = LobbyAdmission::of(&entry(UserKind::Member, false, Some("ab")));
    assert_eq!(member, LobbyAdmission::Member);
    assert!(member.has_device_to_add());
    assert!(member.can_admit(false), "members show no key code");
  }

  #[test]
  fn a_guest_is_admitted_only_once_the_key_code_is_shown() {
    let guest = LobbyAdmission::of(&entry(UserKind::Guest, true, Some("ab")));
    assert!(guest.has_device_to_add());
    assert!(!guest.can_admit(false), "the host hasn't seen the code yet");
    assert!(guest.can_admit(true));
    let pending = LobbyAdmission::of(&entry(UserKind::Guest, false, None));
    assert!(!pending.can_admit(true), "nothing to add without a device");
  }

  fn entry(kind: UserKind, has_device: bool, maybe_fingerprint: Option<&str>) -> LobbyEntry {
    let kind = if kind == UserKind::Guest {
      "guest"
    } else {
      "member"
    };
    serde_json::from_value(serde_json::json!({
      "user": {"id": "77", "display_name": "Gus", "kind": kind},
      "requested_at": 1,
      "is_by_link": true,
      "has_device": has_device,
      "knock_fingerprint": maybe_fingerprint,
    }))
    .unwrap()
  }
}
