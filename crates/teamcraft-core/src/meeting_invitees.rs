//! Who a meeting's host adds to the meeting chat's group (#1726, e2ee.md §4.6.5, "Meetings
//! (D10)"): the organizer signs the invitee list into the admitters statement, and a Join that
//! starts from the server's attendees adds only users that list names. Anybody else knocks at the
//! lobby like anybody with the link.
//!
//! The pure decisions live here; `client::guest` reads and keeps the lists.

use std::collections::BTreeSet;

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
/// yet, unless a newer statement replaced it, or else the newest statement the organizer's key
/// signed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignedInvitees {
  /// No list of the organizer's is known here (a meeting from before signed invitees, a statement
  /// of version 1, or none at all).
  Unknown,
  Known(Vec<UserId>),
}

/// The organizer's own choice on this device, pending until it is signed. It was made on the
/// newest statement with invitees this device had verified (`maybe_base_sequence`); a statement
/// newer than that (another device's later decision, or this device's own once signed) replaces
/// it, so a choice whose signing failed never overrides what was decided since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenInvitees {
  pub invitees: Vec<UserId>,
  pub maybe_base_sequence: Option<u64>,
}

/// What a host's device knows when it decides who of a list to add.
#[derive(Clone, Copy, Debug)]
pub struct InviteeFacts<'a> {
  pub signed: &'a SignedInvitees,
  /// [`signs_invitees`] on this device.
  pub signs_invitees: bool,
  /// Users with a device in the chat's group on this device, guests left out: they came in
  /// through signed adds, so the server's list may carry them over.
  pub in_group: &'a [UserId],
  /// Users a verified commit took out of the group since a verified commit or this device's own
  /// choice last let them in.
  pub removed: &'a BTreeSet<UserId>,
}

/// The pending choice unless a statement newer than its base was verified since, else the newest
/// verified statement's invitees (`sequence`, `invitees`), without the users a verified commit took
/// out of the group (`removed`). No statement lifts a removal, whatever its sequence: statements
/// and commits share no order, and the server picks which statement a device sees. Only a verified
/// commit that adds the user again, or this device's own choice, does.
pub fn newest_signed_invitees(
  maybe_chosen: Option<&ChosenInvitees>,
  maybe_statement: Option<(u64, &[UserId])>,
  removed: &BTreeSet<UserId>,
) -> SignedInvitees {
  match newest_list(maybe_chosen, maybe_statement) {
    SignedInvitees::Known(invitees) => SignedInvitees::Known(
      invitees
        .into_iter()
        .filter(|user| !removed.contains(user))
        .collect(),
    ),
    SignedInvitees::Unknown => SignedInvitees::Unknown,
  }
}

fn newest_list(
  maybe_chosen: Option<&ChosenInvitees>,
  maybe_statement: Option<(u64, &[UserId])>,
) -> SignedInvitees {
  let maybe_pending = maybe_chosen.filter(|chosen| {
    maybe_statement
      .is_none_or(|(sequence, _)| chosen.maybe_base_sequence.is_some_and(|base| sequence <= base))
  });
  match (maybe_pending, maybe_statement) {
    (Some(chosen), _) => SignedInvitees::Known(chosen.invitees.clone()),
    (None, Some((_, invitees))) => SignedInvitees::Known(invitees.to_vec()),
    (None, None) => SignedInvitees::Unknown,
  }
}

/// Which users of `list` a host's device adds to the meeting chat's group; for a decision of the
/// organizer's, also the new invitee list it keeps and signs. Users the organizer added count as
/// they are. Users from the server's list count where they are in the group already, or where
/// the signed list names them and no verified commit took them out since one let them in (the
/// server's word can only lower). Without a signed list: before [`SIGNED_INVITEES_RELEASE`] as before it
/// (the server's list, removals aside), from it on only users in the group. Anybody else knocks at
/// the lobby.
pub fn invitees_to_add(list: &InviteeList, facts: &InviteeFacts<'_>) -> Vec<UserId> {
  let (added, from_server) = match list {
    InviteeList::Chosen { added, kept } => (added.as_slice(), kept.as_slice()),
    InviteeList::Listed(listed) => (&[][..], listed.as_slice()),
  };
  let mut result: Vec<UserId> = Vec::new();
  let vouched = from_server.iter().filter(|user| is_vouched(**user, facts));
  for user in added.iter().chain(vouched) {
    if !result.contains(user) {
      result.push(*user);
    }
  }
  result
}

fn is_vouched(user: UserId, facts: &InviteeFacts<'_>) -> bool {
  if facts.in_group.contains(&user) {
    return true;
  }
  let is_named = match facts.signed {
    SignedInvitees::Known(invitees) => invitees.contains(&user),
    SignedInvitees::Unknown => !facts.signs_invitees,
  };
  is_named && !facts.removed.contains(&user)
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

  const BOB: u64 = 2;
  const CAROL: u64 = 3;
  const MALLORY: u64 = 66;

  #[test]
  fn the_users_the_organizer_added_count_as_they_are() {
    let chosen = InviteeList::Chosen {
      added: vec![user(CAROL)],
      kept: Vec::new(),
    };
    let signed = known(&[BOB]);
    assert_eq!(
      invitees_to_add(&chosen, &facts(&signed, &[], &[])),
      vec![user(CAROL)]
    );
  }

  #[test]
  fn the_servers_list_counts_only_where_the_signed_list_names_the_user() {
    let signed = known(&[BOB, CAROL]);
    let listed = InviteeList::Listed(vec![user(BOB), user(MALLORY)]);
    assert_eq!(
      invitees_to_add(&listed, &facts(&signed, &[], &[])),
      vec![user(BOB)],
      "Mallory knocks; Carol, no longer listed, isn't added"
    );
    // An edit carries the server's list over: Mallory, slipped into it, isn't kept either.
    let edit = InviteeList::Chosen {
      added: vec![user(4)],
      kept: vec![user(BOB), user(MALLORY)],
    };
    assert_eq!(
      invitees_to_add(&edit, &facts(&signed, &[], &[])),
      vec![user(4), user(BOB)]
    );
  }

  #[test]
  fn without_a_signed_list_only_users_in_the_group_count_once_signing_is_on() {
    let listed = InviteeList::Listed(vec![user(BOB), user(MALLORY)]);
    let unknown = SignedInvitees::Unknown;
    assert!(invitees_to_add(&listed, &facts(&unknown, &[], &[])).is_empty());
    assert_eq!(
      invitees_to_add(&listed, &facts(&unknown, &[BOB], &[])),
      vec![user(BOB)],
      "a legacy meeting's invitee already in the group is carried over"
    );
    let before = InviteeFacts {
      signs_invitees: false,
      ..facts(&unknown, &[], &[])
    };
    assert_eq!(
      invitees_to_add(&listed, &before),
      vec![user(BOB), user(MALLORY)],
      "before the release, as before"
    );
  }

  #[test]
  fn no_statement_lifts_a_removal() {
    let listed = InviteeList::Listed(vec![user(CAROL)]);
    // Carol was taken out; whatever statement the server serves names her.
    assert!(invitees_to_add(&listed, &facts(&known(&[CAROL]), &[], &[CAROL])).is_empty());
    let invited_again = InviteeList::Chosen {
      added: vec![user(CAROL)],
      kept: Vec::new(),
    };
    assert_eq!(
      invitees_to_add(&invited_again, &facts(&known(&[]), &[], &[CAROL])),
      vec![user(CAROL)],
      "the organizer's own choice does"
    );
  }

  #[test]
  fn a_pending_choice_counts_until_a_newer_statement_than_its_base() {
    let chosen = ChosenInvitees {
      invitees: vec![user(BOB), user(CAROL)],
      maybe_base_sequence: Some(5),
    };
    let signed = [user(BOB)];
    assert_eq!(
      newest_signed_invitees(Some(&chosen), Some((5, &signed)), &BTreeSet::new()),
      known(&[BOB, CAROL])
    );
    assert_eq!(
      newest_signed_invitees(Some(&chosen), Some((6, &signed)), &BTreeSet::new()),
      known(&[BOB]),
      "another device decided since: its list counts"
    );
    let on_nothing = ChosenInvitees {
      maybe_base_sequence: None,
      ..chosen
    };
    assert_eq!(
      newest_signed_invitees(Some(&on_nothing), None, &BTreeSet::new()),
      known(&[BOB, CAROL])
    );
    assert_eq!(
      newest_signed_invitees(Some(&on_nothing), Some((1, &signed)), &BTreeSet::new()),
      known(&[BOB])
    );
    assert_eq!(
      newest_signed_invitees(None, None, &BTreeSet::new()),
      SignedInvitees::Unknown
    );
    // Carol was taken out after the choice (no newer statement): never signed back in.
    let removed: BTreeSet<UserId> = [user(CAROL)].into_iter().collect();
    assert_eq!(
      newest_signed_invitees(Some(&on_nothing.clone()), None, &removed),
      known(&[BOB])
    );
  }

  #[test]
  fn invitees_are_signed_from_the_release_on() {
    assert!(!signs_invitees(None));
    assert!(!signs_invitees(ReleaseVersion::parse("1.0.31")));
    assert!(signs_invitees(Some(SIGNED_INVITEES_RELEASE)));
    assert!(signs_invitees(ReleaseVersion::parse("1.1.0")));
  }

  fn known(users: &[u64]) -> SignedInvitees {
    SignedInvitees::Known(users.iter().map(|raw| user(*raw)).collect())
  }

  /// Signing on, `in_group` in the group, `removed` taken out; leaked for the borrowed fields
  /// (tests only).
  fn facts<'a>(signed: &'a SignedInvitees, in_group: &[u64], removed: &[u64]) -> InviteeFacts<'a> {
    let in_group: &'a [UserId] = Vec::leak(in_group.iter().map(|raw| user(*raw)).collect());
    let removed: &'a BTreeSet<UserId> = Box::leak(Box::new(removed.iter().map(|raw| user(*raw)).collect()));
    InviteeFacts {
      signed,
      signs_invitees: true,
      in_group,
      removed,
    }
  }

  fn user(raw: u64) -> UserId {
    raw.to_string().parse().unwrap()
  }
}
