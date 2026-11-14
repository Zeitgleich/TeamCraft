//! A meeting's lobby as a host's device decides on it (docs/meetings.md, "Lobby"; e2ee.md §4.6,
//! "Meeting guests"): which waiting people a host can admit now, and how an admitted person's
//! devices get into the meeting chat's encryption. Desktop and web (`teamcraft-ui`'s lobby) and the
//! phone apps (through teamcraft-ffi) decide alike; `Client::let_in_from_lobby` acts on it.
//!
//! The server's lobby entry is only a hint of who knocked: the host's device adds a guest only
//! with the identity key it pins and whose fingerprint the guest's page reported, and only when it
//! is the organizer's, a co-host's by the signed host list or a signed admitter's. Whether somebody
//! is a member is the owners' signed roster's word, never the server's user kind (#1873): a server
//! that calls a guest a member gets them nothing a guest wouldn't get.

use teamcraft_proto::{LobbyEntry, UserKind};

/// How long a guest's access lasts at most from their admission when this device holds no signed
/// end for the meeting (no organizer-signed admitters in force, #1873): the guests' grace after a
/// meeting (`teamcraft_client_api::meeting_invitees::GUEST_ACCESS_GRACE_MS`).
pub const UNSIGNED_GUEST_ACCESS_MS: i64 = 2 * 60 * 60_000;

/// How a host's device lets a waiting person in, by their lobby entry and the signed roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LobbyAdmission {
  /// A member of the workspace by the signed roster: their devices join the chat's group as any
  /// member's.
  Member,
  /// Somebody the signed roster doesn't vouch for, whose page set up its device and reported the
  /// fingerprint of its identity key: let in as a guest, only a key with this fingerprint, until
  /// the end of their access. Whatever the server's user kind says.
  Guest { knock_fingerprint: String },
  /// A guest whose page has not set up its device yet: nothing to add, so no "Admit" yet.
  GuestDevicePending,
  /// The server calls them a member, but the signed roster doesn't vouch for them (yet: the owners'
  /// apps haven't noted their account, e2ee.md §3.1.4) and they reported no key: no "Admit", as
  /// the member path would trust the server's word and the guest path has no key to check.
  UnconfirmedMember,
}

/// What the owners' signed roster says of a waiting person
/// ([`crate::workspace::WorkspaceState::vouches_for_member`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignedMembership {
  /// An owner, an admin, or somebody whose provider account the owners noted.
  Member,
  /// Not on the roster: a guest, or a member whose account isn't noted yet.
  NotSigned,
}

impl LobbyAdmission {
  /// How `entry` is let in, with `membership` from the signed roster. The server's user kind only
  /// tells an unconfirmed member from a guest still setting up, for the row's wording; it never
  /// opens the member path.
  pub fn of(entry: &LobbyEntry, membership: SignedMembership) -> Self {
    if membership == SignedMembership::Member {
      return Self::Member;
    }
    match (&entry.maybe_knock_fingerprint, entry.has_device) {
      (Some(fingerprint), true) => Self::Guest {
        knock_fingerprint: fingerprint.clone(),
      },
      _ if entry.user.kind != UserKind::Guest => Self::UnconfirmedMember,
      _ => Self::GuestDevicePending,
    }
  }

  /// Whether there is anything to add: a member, or a guest with a reported key. Enough for the
  /// organizer's own "no lobby" admission of link arrivals, which asks nobody.
  pub fn has_device_to_add(&self) -> bool {
    matches!(self, Self::Member | Self::Guest { .. })
  }

  /// Whether a host's "Admit" can be offered: a member at once, a guest only once the key code of
  /// the key this device pinned for them is shown (`has_shown_key_code`), so the host can compare
  /// it with the guest's screen first. That comparison is the only defence against a guest the
  /// server made up with its own key (e2ee.md §4.6). Every client's lobby reads this one rule.
  pub fn can_admit(&self, has_shown_key_code: bool) -> bool {
    match self {
      Self::Member => true,
      Self::Guest { .. } => has_shown_key_code,
      Self::GuestDevicePending | Self::UnconfirmedMember => false,
    }
  }
}

/// Until when a guest admitted now keeps access (e2ee.md §4.6, "Meeting guests"; #1873): the end
/// the host's app asked for (`asked_ms`, from the server's meeting), but never past the trusted end:
/// the validity of the organizer-signed admitters this device verified (`maybe_signed_ms`, the
/// meeting's end and the guests' grace as the organizer's device signed it) while it lies ahead, or
/// else [`UNSIGNED_GUEST_ACCESS_MS`] from `now_ms`. A server that lengthens the meeting lengthens
/// nobody's access.
pub fn guest_access_end(asked_ms: i64, maybe_signed_ms: Option<i64>, now_ms: i64) -> i64 {
  let trusted = maybe_signed_ms
    .filter(|signed| *signed > now_ms)
    .unwrap_or_else(|| now_ms.saturating_add(UNSIGNED_GUEST_ACCESS_MS));
  asked_ms.min(trusted)
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW_MS: i64 = 1_000_000;

  mod admission {
    use super::*;

    #[test]
    fn a_guest_is_admitted_only_with_a_device_and_its_reported_key() {
      assert_eq!(
        guest(true, Some("ab")),
        LobbyAdmission::Guest {
          knock_fingerprint: "ab".into()
        }
      );
      let without_key = guest(true, None);
      assert_eq!(without_key, LobbyAdmission::GuestDevicePending);
      assert!(!without_key.has_device_to_add());
      assert_eq!(guest(false, Some("ab")), LobbyAdmission::GuestDevicePending);
    }

    #[test]
    fn a_signed_member_is_admitted_as_a_member_whatever_the_entry_says() {
      for kind in [UserKind::Member, UserKind::Guest] {
        let member = LobbyAdmission::of(&entry(kind, false, Some("ab")), SignedMembership::Member);
        assert_eq!(member, LobbyAdmission::Member);
        assert!(member.has_device_to_add());
        assert!(member.can_admit(false), "members show no key code");
      }
    }

    #[test]
    fn a_guest_the_server_calls_a_member_is_still_a_guest() {
      let relabelled = LobbyAdmission::of(
        &entry(UserKind::Member, true, Some("ab")),
        SignedMembership::NotSigned,
      );
      assert_eq!(
        relabelled,
        LobbyAdmission::Guest {
          knock_fingerprint: "ab".into()
        }
      );
      assert!(
        !relabelled.can_admit(false),
        "the key code first, as for any guest"
      );
    }

    #[test]
    fn a_member_the_roster_doesnt_vouch_for_and_without_a_key_is_not_admitted() {
      let unconfirmed = LobbyAdmission::of(&entry(UserKind::Member, true, None), SignedMembership::NotSigned);
      assert_eq!(unconfirmed, LobbyAdmission::UnconfirmedMember);
      assert!(!unconfirmed.has_device_to_add());
      assert!(!unconfirmed.can_admit(true));
    }

    #[test]
    fn a_guest_is_admitted_only_once_the_key_code_is_shown() {
      let guest = guest(true, Some("ab"));
      assert!(guest.has_device_to_add());
      assert!(!guest.can_admit(false), "the host hasn't seen the code yet");
      assert!(guest.can_admit(true));
      assert!(
        !super::guest(false, None).can_admit(true),
        "nothing to add without a device"
      );
    }
  }

  mod access_end {
    use super::*;

    #[test]
    fn the_signed_end_caps_a_longer_end_from_the_server() {
      assert_eq!(
        guest_access_end(NOW_MS + 9_000, Some(NOW_MS + 5_000), NOW_MS),
        NOW_MS + 5_000
      );
    }

    #[test]
    fn an_earlier_end_from_the_server_is_kept() {
      assert_eq!(
        guest_access_end(NOW_MS + 1_000, Some(NOW_MS + 5_000), NOW_MS),
        NOW_MS + 1_000
      );
    }

    #[test]
    fn without_a_signed_end_ahead_access_lasts_the_grace_from_now() {
      let far = NOW_MS + 100 * UNSIGNED_GUEST_ACCESS_MS;
      let capped = NOW_MS + UNSIGNED_GUEST_ACCESS_MS;
      assert_eq!(guest_access_end(far, None, NOW_MS), capped);
      assert_eq!(
        guest_access_end(far, Some(NOW_MS - 1), NOW_MS),
        capped,
        "an ended statement"
      );
    }

    #[cfg(feature = "ui")]
    #[test]
    fn the_unsigned_access_is_the_guests_grace() {
      assert_eq!(
        UNSIGNED_GUEST_ACCESS_MS,
        teamcraft_client_api::meeting_invitees::GUEST_ACCESS_GRACE_MS
      );
    }
  }

  fn guest(has_device: bool, maybe_fingerprint: Option<&str>) -> LobbyAdmission {
    LobbyAdmission::of(
      &entry(UserKind::Guest, has_device, maybe_fingerprint),
      SignedMembership::NotSigned,
    )
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
