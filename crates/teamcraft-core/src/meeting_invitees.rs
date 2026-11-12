//! Who a meeting's host adds to the meeting chat's group (#1726, e2ee.md §4.6.5, "Meetings
//! (D10)"): the organizer signs the invitee list into the admitters statement, and a Join that
//! starts from the server's attendees adds only users that list names. Anybody else knocks at the
//! lobby like anybody with the link.
//!
//! The pure decisions live here; `client::guest` reads and keeps the lists.

use serde::{Deserialize, Serialize};
use teamcraft_proto::UserId;

use crate::anchoring::ReleaseVersion;

/// The first release whose apps all read a version 2 admitters statement (the invitees): once the
/// server's `minimum_client_version` reaches it, the organizer's devices sign the invitees, and a
/// Join without a signed list adds nobody from the server's word.
pub const SIGNED_INVITEES_RELEASE: ReleaseVersion = ReleaseVersion::new(1, 0, 32);

/// A meeting's invitees as handed to the core, by where each user comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InviteeList {
  /// A decision of the organizer's on this device, kept and signed into the next admitters
  /// statement: `added`, the users the organizer chose just now (a create request, the users an
  /// edit adds), count as they are; `kept`, users carried over from the server's list (an edit
  /// sends the whole list), only where the signed list names them.
  Chosen { added: Vec<UserId>, kept: Vec<UserId> },
  /// The meeting's attendees as the server lists them (a Join, an edit finished after a restart):
  /// only users the newest signed list names count, and nothing is kept.
  Listed(Vec<UserId>),
}

/// The UI's invitee list, as the core takes it.
#[cfg(feature = "ui")]
impl From<teamcraft_client_api::meeting_invitees::ChatInvitees> for InviteeList {
  fn from(invitees: teamcraft_client_api::meeting_invitees::ChatInvitees) -> Self {
    use teamcraft_client_api::meeting_invitees::ChatInvitees;
    match invitees {
      ChatInvitees::Chosen { added, kept } => Self::Chosen { added, kept },
      ChatInvitees::Listed { listed } => Self::Listed(listed),
    }
  }
}

/// The newest invitee list this device knows the organizer decided on: its own choice not signed
/// yet, or else the newest statement the organizer's key signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedInvitees {
  /// No list of the organizer's is known here (a meeting from before signed invitees, a statement
  /// of version 1, or none at all).
  Unknown,
  Known(Vec<UserId>),
}

/// The organizer's own choice on this device, pending until this device signed it into a
/// statement (then the statement speaks for it, and a later one of another device replaces it).
/// Not ordered by time against statements: a sequence can run ahead of the clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenInvitees {
  pub invitees: Vec<UserId>,
}

/// This device's pending choice if there is one, else the newest verified statement's invitees.
pub fn newest_signed_invitees(
  maybe_chosen: Option<&ChosenInvitees>,
  maybe_statement: Option<&[UserId]>,
) -> SignedInvitees {
  match (maybe_chosen, maybe_statement) {
    (Some(chosen), _) => SignedInvitees::Known(chosen.invitees.clone()),
    (None, Some(invitees)) => SignedInvitees::Known(invitees.to_vec()),
    (None, None) => SignedInvitees::Unknown,
  }
}

/// Which users of `list` a host's device adds to the meeting chat's group; for a decision of the
/// organizer's, also the new invitee list it keeps and signs. Users the organizer added count as
/// they are. Users from the server's list count only where the signed list names them (its word
/// can only lower). Without a signed list: before [`SIGNED_INVITEES_RELEASE`] as before it (the
/// server's list), from it on nobody, who then knocks at the lobby.
pub fn invitees_to_add(list: &InviteeList, signed: &SignedInvitees, signs_invitees: bool) -> Vec<UserId> {
  let (added, from_server) = match list {
    InviteeList::Chosen { added, kept } => (added.as_slice(), kept.as_slice()),
    InviteeList::Listed(listed) => (&[][..], listed.as_slice()),
  };
  let mut result: Vec<UserId> = Vec::new();
  let vouched = from_server.iter().filter(|user| match signed {
    SignedInvitees::Known(named) => named.contains(user),
    SignedInvitees::Unknown => !signs_invitees,
  });
  for user in added.iter().chain(vouched) {
    if !result.contains(user) {
      result.push(*user);
    }
  }
  result
}

/// Whether the organizer's devices sign the invitees (a version 2 statement): once the highest
/// `minimum_client_version` this device has seen reaches [`SIGNED_INVITEES_RELEASE`]. Before, an
/// app too old to read version 2 would drop the whole statement.
pub fn signs_invitees(maybe_highest_minimum: Option<ReleaseVersion>) -> bool {
  maybe_highest_minimum.is_some_and(|minimum| minimum >= SIGNED_INVITEES_RELEASE)
}

#[cfg(test)]
mod tests {
  use super::*;

  const BOB: UserId = UserId::new(2);
  const CAROL: UserId = UserId::new(3);
  const MALLORY: UserId = UserId::new(66);

  #[test]
  fn the_users_the_organizer_added_count_as_they_are() {
    let chosen = InviteeList::Chosen {
      added: vec![CAROL],
      kept: Vec::new(),
    };
    let to_add = invitees_to_add(&chosen, &SignedInvitees::Known(vec![BOB]), true);
    assert_eq!(to_add, vec![CAROL]);
  }

  #[test]
  fn the_servers_list_counts_only_where_the_signed_list_names_the_user() {
    let signed = SignedInvitees::Known(vec![BOB, CAROL]);
    let listed = InviteeList::Listed(vec![BOB, MALLORY]);
    assert_eq!(
      invitees_to_add(&listed, &signed, true),
      vec![BOB],
      "Mallory knocks; Carol, no longer listed, isn't added"
    );
    // An edit carries the server's list over: Mallory, slipped into it, isn't kept either.
    let edit = InviteeList::Chosen {
      added: vec![UserId::new(4)],
      kept: vec![BOB, MALLORY],
    };
    assert_eq!(invitees_to_add(&edit, &signed, true), vec![UserId::new(4), BOB]);
  }

  #[test]
  fn without_a_signed_list_the_servers_list_adds_nobody_once_signing_is_on() {
    let listed = InviteeList::Listed(vec![BOB, MALLORY]);
    assert!(invitees_to_add(&listed, &SignedInvitees::Unknown, true).is_empty());
    assert_eq!(
      invitees_to_add(&listed, &SignedInvitees::Unknown, false),
      vec![BOB, MALLORY],
      "before the release, as before"
    );
  }

  #[test]
  fn a_pending_own_choice_counts_before_the_statement() {
    let chosen = ChosenInvitees { invitees: vec![BOB] };
    let signed = [CAROL];
    assert_eq!(
      newest_signed_invitees(Some(&chosen), Some(&signed)),
      SignedInvitees::Known(vec![BOB])
    );
    assert_eq!(
      newest_signed_invitees(None, Some(&signed)),
      SignedInvitees::Known(vec![CAROL])
    );
    assert_eq!(newest_signed_invitees(None, None), SignedInvitees::Unknown);
  }

  #[test]
  fn invitees_are_signed_from_the_release_on() {
    assert!(!signs_invitees(None));
    assert!(!signs_invitees(ReleaseVersion::parse("1.0.31")));
    assert!(signs_invitees(Some(SIGNED_INVITEES_RELEASE)));
    assert!(signs_invitees(ReleaseVersion::parse("1.1.0")));
  }
}
