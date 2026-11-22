//! Legal hold (issue #211, data-export.md §4.2, admin.md §3.14): conversations the organization must
//! keep, as part of the `compliance_archive` group of the owner-signed workspace policy. While a
//! conversation is held, retention, the inactivity policy, deletions and members' deletions of their
//! own messages don't remove it on the server, and every member sees that it is held. The list is in
//! the signed policy, which every signed-in user reads, so the server can't place a hold nobody
//! signed (it can delay or withhold a newer policy, like any policy update).
//!
//! A hold names a conversation ([`LegalHold`]: a channel, a direct message or a group) or a person
//! ([`PersonLegalHold`], issue #2135): then every conversation they are in is held, or only their
//! direct messages and groups.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationId, ConversationKind, UnixMillis, UserId};

/// The most holds one policy may place, on conversations and people together: the whole policy is
/// read by every client.
pub const MAX_LEGAL_HOLDS: usize = 500;
/// The longest case reference, in characters.
pub const MAX_LEGAL_HOLD_REFERENCE_CHARS: usize = 64;

/// One held conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LegalHold {
  pub conversation_id: ConversationId,
  /// The organization's case or matter reference ("Case 2026-14"). Every member reads it: the
  /// console asks for a reference, not a person's name or the reason.
  pub reference: String,
  /// When the owner placed it, by the owner's clock: shown, never used to decide anything.
  pub placed_at: UnixMillis,
}

wire_enum! {
  /// Which of a person's conversations a [`PersonLegalHold`] keeps.
  #[derive(Default)]
  pub enum PersonHoldScope {
    #[default]
    /// Every conversation they are in: channels, direct messages and groups. Meeting chats go with
    /// their meeting, which a hold can't stop, so they are not covered.
    All => "all",
    /// Only their direct messages and groups.
    Direct => "direct",
  }
}

impl PersonHoldScope {
  fn is_all(&self) -> bool {
    *self == Self::All
  }
}

/// A held person (issue #2135): every conversation they are in, as [`PersonHoldScope`] says, is held
/// like a conversation with its own [`LegalHold`], also the ones they leave while the hold lasts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonLegalHold {
  pub user_id: UserId,
  #[serde(default, skip_serializing_if = "PersonHoldScope::is_all")]
  pub scope: PersonHoldScope,
  /// The case reference, as for a conversation's hold: every member of a covered conversation reads
  /// it.
  pub reference: String,
  /// When the owner placed it, by the owner's clock: shown, never used to decide anything.
  pub placed_at: UnixMillis,
}

impl PersonLegalHold {
  /// Whether this hold keeps a conversation of `kind` whose participants or members are `people`.
  /// An unknown scope counts as all: a newer owner app's narrower scope is read as the wider one.
  pub fn covers(&self, kind: &ConversationKind, people: &[UserId]) -> bool {
    let is_kind_covered = match kind {
      ConversationKind::Dm | ConversationKind::Group => true,
      ConversationKind::Channel => self.scope != PersonHoldScope::Direct,
      ConversationKind::Meeting | ConversationKind::Unknown(_) => false,
    };
    is_kind_covered && people.contains(&self.user_id)
  }
}

/// Why a list of holds can't be signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LegalHoldError {
  #[error("more than {MAX_LEGAL_HOLDS} legal holds")]
  TooMany,
  #[error("a conversation is held twice")]
  Duplicate,
  #[error("a person is held twice")]
  DuplicatePerson,
  #[error("a legal hold reference is empty, too long or has control characters")]
  InvalidReference,
  #[error("a person's legal hold has an unknown scope")]
  UnknownScope,
}

impl LegalHold {
  /// Whether `reference` may be signed: 1 to [`MAX_LEGAL_HOLD_REFERENCE_CHARS`] characters after
  /// trimming, without control characters (it is shown in a banner and written to the audit log).
  pub fn is_valid_reference(reference: &str) -> bool {
    let trimmed = reference.trim();
    trimmed == reference
      && !trimmed.is_empty()
      && trimmed.chars().count() <= MAX_LEGAL_HOLD_REFERENCE_CHARS
      && !trimmed.chars().any(char::is_control)
  }
}

/// Checks the holds on conversations and on people before an owner's app signs them and before the
/// server stores them.
pub fn check_legal_holds(holds: &[LegalHold], people: &[PersonLegalHold]) -> Result<(), LegalHoldError> {
  if holds.len().saturating_add(people.len()) > MAX_LEGAL_HOLDS {
    return Err(LegalHoldError::TooMany);
  }
  if holds
    .iter()
    .map(|hold| hold.reference.as_str())
    .chain(people.iter().map(|hold| hold.reference.as_str()))
    .any(|reference| !LegalHold::is_valid_reference(reference))
  {
    return Err(LegalHoldError::InvalidReference);
  }
  if people
    .iter()
    .any(|hold| matches!(hold.scope, PersonHoldScope::Unknown(_)))
  {
    return Err(LegalHoldError::UnknownScope);
  }
  let mut seen = std::collections::HashSet::with_capacity(holds.len());
  if holds.iter().any(|hold| !seen.insert(hold.conversation_id)) {
    return Err(LegalHoldError::Duplicate);
  }
  let mut seen_people = std::collections::HashSet::with_capacity(people.len());
  if people.iter().any(|hold| !seen_people.insert(hold.user_id)) {
    return Err(LegalHoldError::DuplicatePerson);
  }
  Ok(())
}

/// The hold on `conversation` in `holds`, if any.
pub fn legal_hold_of(holds: &[LegalHold], conversation: ConversationId) -> Option<&LegalHold> {
  holds.iter().find(|hold| hold.conversation_id == conversation)
}

/// The hold on `person` in `people`, if any.
pub fn person_hold_of(people: &[PersonLegalHold], person: UserId) -> Option<&PersonLegalHold> {
  people.iter().find(|hold| hold.user_id == person)
}

/// The hold that keeps `conversation` (of `kind`, with these participants or members): its own,
/// else the first person hold that covers it, shown with that hold's reference. The one function
/// every app asks whether to show the banner, so a person's hold shows wherever a conversation's
/// would (issue #2135). Who is in the conversation is what the app knows of it.
pub fn hold_covering(
  holds: &[LegalHold],
  people: &[PersonLegalHold],
  conversation: ConversationId,
  kind: &ConversationKind,
  participants: &[UserId],
) -> Option<LegalHold> {
  if let Some(hold) = legal_hold_of(holds, conversation) {
    return Some(hold.clone());
  }
  people
    .iter()
    .find(|hold| hold.covers(kind, participants))
    .map(|hold| LegalHold {
      conversation_id: conversation,
      reference: hold.reference.clone(),
      placed_at: hold.placed_at,
    })
}

/// What changed between two lists of person holds, for the audit log: holds placed and released. A
/// changed scope counts as a release and a new hold.
pub fn person_hold_changes<'a>(
  before: &'a [PersonLegalHold],
  after: &'a [PersonLegalHold],
) -> (Vec<&'a PersonLegalHold>, Vec<&'a PersonLegalHold>) {
  let is_in = |list: &[PersonLegalHold], hold: &PersonLegalHold| {
    person_hold_of(list, hold.user_id).is_some_and(|other| other.scope == hold.scope)
  };
  let placed = after.iter().filter(|hold| !is_in(before, hold)).collect();
  let released = before.iter().filter(|hold| !is_in(after, hold)).collect();
  (placed, released)
}

/// What changed between two lists of holds, for the audit log: holds placed and holds released.
pub fn legal_hold_changes<'a>(
  before: &'a [LegalHold],
  after: &'a [LegalHold],
) -> (Vec<&'a LegalHold>, Vec<&'a LegalHold>) {
  let placed = after
    .iter()
    .filter(|hold| legal_hold_of(before, hold.conversation_id).is_none())
    .collect();
  let released = before
    .iter()
    .filter(|hold| legal_hold_of(after, hold.conversation_id).is_none())
    .collect();
  (placed, released)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::Id;

  #[test]
  fn a_hold_has_a_stable_wire_form() {
    let hold = hold(7, "Case 2026-14");
    assert_eq!(
      serde_json::to_string(&hold).unwrap(),
      r#"{"conversation_id":"7","reference":"Case 2026-14","placed_at":1700000000000}"#
    );
    assert_eq!(
      serde_json::from_str::<LegalHold>(&serde_json::to_string(&hold).unwrap()).unwrap(),
      hold
    );
  }

  #[test]
  fn a_person_hold_has_a_stable_wire_form() {
    let all = person(7, "Case 2026-14");
    assert_eq!(
      serde_json::to_string(&all).unwrap(),
      r#"{"user_id":"7","reference":"Case 2026-14","placed_at":1700000000000}"#
    );
    let direct = PersonLegalHold {
      scope: PersonHoldScope::Direct,
      ..all.clone()
    };
    let json = serde_json::to_string(&direct).unwrap();
    assert_eq!(
      json,
      r#"{"user_id":"7","scope":"direct","reference":"Case 2026-14","placed_at":1700000000000}"#
    );
    assert_eq!(serde_json::from_str::<PersonLegalHold>(&json).unwrap(), direct);
    let future: PersonLegalHold =
      serde_json::from_str(r#"{"user_id":"7","scope":"later","reference":"A","placed_at":1}"#).unwrap();
    assert_eq!(future.scope, PersonHoldScope::Unknown("later".into()));
  }

  mod covering {
    use super::*;

    #[test]
    fn a_conversations_own_hold_comes_first() {
      let holds = [hold(1, "Own")];
      let people = [person(9, "Person")];
      let found = hold_covering(
        &holds,
        &people,
        conversation(1),
        &ConversationKind::Dm,
        &[user(9)],
      );
      assert_eq!(found.map(|hold| hold.reference), Some("Own".to_owned()));
    }

    #[test]
    fn a_person_hold_covers_their_conversations_with_its_reference() {
      let people = [person(9, "Person")];
      for kind in [
        ConversationKind::Channel,
        ConversationKind::Dm,
        ConversationKind::Group,
      ] {
        let found = hold_covering(&[], &people, conversation(4), &kind, &[user(3), user(9)]);
        assert_eq!(
          found,
          Some(LegalHold {
            conversation_id: conversation(4),
            reference: "Person".to_owned(),
            placed_at: UnixMillis(1_700_000_000_000),
          }),
          "{kind:?}"
        );
      }
      assert_eq!(
        hold_covering(
          &[],
          &people,
          conversation(4),
          &ConversationKind::Channel,
          &[user(3)]
        ),
        None
      );
      assert_eq!(
        hold_covering(
          &[],
          &people,
          conversation(4),
          &ConversationKind::Meeting,
          &[user(9)]
        ),
        None
      );
    }

    #[test]
    fn a_direct_scope_leaves_channels_out() {
      let hold = PersonLegalHold {
        scope: PersonHoldScope::Direct,
        ..person(9, "A")
      };
      assert!(hold.covers(&ConversationKind::Dm, &[user(9)]));
      assert!(hold.covers(&ConversationKind::Group, &[user(9)]));
      assert!(!hold.covers(&ConversationKind::Channel, &[user(9)]));
      let unknown = PersonLegalHold {
        scope: PersonHoldScope::Unknown("later".into()),
        ..person(9, "A")
      };
      assert!(unknown.covers(&ConversationKind::Channel, &[user(9)]));
    }
  }

  mod checks {
    use super::*;

    #[test]
    fn valid_lists_pass() {
      assert_eq!(check_legal_holds(&[], &[]), Ok(()));
      assert_eq!(
        check_legal_holds(&[hold(1, "A"), hold(2, "B")], &[person(1, "C")]),
        Ok(())
      );
    }

    #[test]
    fn people_are_held_once_with_a_known_scope_and_a_valid_reference() {
      assert_eq!(
        check_legal_holds(&[], &[person(1, "A"), person(1, "B")]),
        Err(LegalHoldError::DuplicatePerson)
      );
      assert_eq!(
        check_legal_holds(&[], &[person(1, " A")]),
        Err(LegalHoldError::InvalidReference)
      );
      let unknown = PersonLegalHold {
        scope: PersonHoldScope::Unknown("later".into()),
        ..person(1, "A")
      };
      assert_eq!(
        check_legal_holds(&[], &[unknown]),
        Err(LegalHoldError::UnknownScope)
      );
    }

    #[test]
    fn references_must_be_short_plain_text() {
      for reference in ["", " ", " padded", "line\nbreak", &"x".repeat(65)] {
        assert_eq!(
          check_legal_holds(&[hold(1, reference)], &[]),
          Err(LegalHoldError::InvalidReference),
          "{reference:?}"
        );
      }
      assert_eq!(check_legal_holds(&[hold(1, &"ä".repeat(64))], &[]), Ok(()));
    }

    #[test]
    fn a_conversation_is_held_once() {
      assert_eq!(
        check_legal_holds(&[hold(1, "A"), hold(1, "B")], &[]),
        Err(LegalHoldError::Duplicate)
      );
    }

    #[test]
    fn the_list_is_bounded() {
      let holds: Vec<_> = (0..=MAX_LEGAL_HOLDS as i64).map(|id| hold(id, "A")).collect();
      assert_eq!(check_legal_holds(&holds, &[]), Err(LegalHoldError::TooMany));
      assert_eq!(check_legal_holds(&holds[..MAX_LEGAL_HOLDS], &[]), Ok(()));
      // Conversations and people count together.
      assert_eq!(
        check_legal_holds(&holds[..MAX_LEGAL_HOLDS], &[person(1, "A")]),
        Err(LegalHoldError::TooMany)
      );
    }
  }

  #[test]
  fn changes_name_placed_and_released_holds() {
    let before = [hold(1, "A"), hold(2, "B")];
    let after = [hold(2, "B"), hold(3, "C")];
    let (placed, released) = legal_hold_changes(&before, &after);
    assert_eq!(placed, vec![&after[1]]);
    assert_eq!(released, vec![&before[0]]);
    assert_eq!(legal_hold_of(&after, conversation(3)), Some(&after[1]));
    assert_eq!(legal_hold_of(&after, conversation(1)), None);
  }

  #[test]
  fn person_changes_name_placed_and_released_holds() {
    let before = [person(1, "A"), person(2, "B")];
    let narrowed = PersonLegalHold {
      scope: PersonHoldScope::Direct,
      ..person(2, "B")
    };
    let after = [narrowed.clone(), person(3, "C")];
    let (placed, released) = person_hold_changes(&before, &after);
    assert_eq!(placed, vec![&after[0], &after[1]]);
    assert_eq!(released, vec![&before[0], &before[1]]);
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(crate::Snowflake::new(id).unwrap())
  }

  fn person(id: i64, reference: &str) -> PersonLegalHold {
    PersonLegalHold {
      user_id: user(id),
      scope: PersonHoldScope::All,
      reference: reference.to_owned(),
      placed_at: UnixMillis(1_700_000_000_000),
    }
  }

  fn conversation(id: i64) -> ConversationId {
    ConversationId::from_snowflake(crate::Snowflake::new(id).unwrap())
  }

  fn hold(id: i64, reference: &str) -> LegalHold {
    LegalHold {
      conversation_id: conversation(id),
      reference: reference.to_owned(),
      placed_at: UnixMillis(1_700_000_000_000),
    }
  }
}
