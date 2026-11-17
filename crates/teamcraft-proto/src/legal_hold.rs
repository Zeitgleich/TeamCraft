//! Legal hold (issue #211, data-export.md §4.2, admin.md §3.14): conversations the organization must
//! keep, as part of the `compliance_archive` group of the owner-signed workspace policy. While a
//! conversation is held, retention, the inactivity policy and channel deletion don't remove it on
//! the server, and every member sees that it is held. The list is in the signed policy, which every
//! signed-in user reads, so the server can't place a hold nobody signed (it can delay or withhold a
//! newer policy, like any policy update).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ConversationId, UnixMillis};

/// The most conversations one policy may hold: the whole policy is read by every client.
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

/// Why a list of holds can't be signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LegalHoldError {
  #[error("more than {MAX_LEGAL_HOLDS} legal holds")]
  TooMany,
  #[error("a conversation is held twice")]
  Duplicate,
  #[error("a legal hold reference is empty, too long or has control characters")]
  InvalidReference,
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

/// Checks a list of holds before an owner's app signs it and before the server stores it.
pub fn check_legal_holds(holds: &[LegalHold]) -> Result<(), LegalHoldError> {
  if holds.len() > MAX_LEGAL_HOLDS {
    return Err(LegalHoldError::TooMany);
  }
  if holds
    .iter()
    .any(|hold| !LegalHold::is_valid_reference(&hold.reference))
  {
    return Err(LegalHoldError::InvalidReference);
  }
  let mut seen = std::collections::HashSet::with_capacity(holds.len());
  if holds.iter().any(|hold| !seen.insert(hold.conversation_id)) {
    return Err(LegalHoldError::Duplicate);
  }
  Ok(())
}

/// The hold on `conversation` in `holds`, if any.
pub fn legal_hold_of(holds: &[LegalHold], conversation: ConversationId) -> Option<&LegalHold> {
  holds.iter().find(|hold| hold.conversation_id == conversation)
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

  mod checks {
    use super::*;

    #[test]
    fn valid_lists_pass() {
      assert_eq!(check_legal_holds(&[]), Ok(()));
      assert_eq!(check_legal_holds(&[hold(1, "A"), hold(2, "B")]), Ok(()));
    }

    #[test]
    fn references_must_be_short_plain_text() {
      for reference in ["", " ", " padded", "line\nbreak", &"x".repeat(65)] {
        assert_eq!(
          check_legal_holds(&[hold(1, reference)]),
          Err(LegalHoldError::InvalidReference),
          "{reference:?}"
        );
      }
      assert_eq!(check_legal_holds(&[hold(1, &"ä".repeat(64))]), Ok(()));
    }

    #[test]
    fn a_conversation_is_held_once() {
      assert_eq!(
        check_legal_holds(&[hold(1, "A"), hold(1, "B")]),
        Err(LegalHoldError::Duplicate)
      );
    }

    #[test]
    fn the_list_is_bounded() {
      let holds: Vec<_> = (0..=MAX_LEGAL_HOLDS as i64).map(|id| hold(id, "A")).collect();
      assert_eq!(check_legal_holds(&holds), Err(LegalHoldError::TooMany));
      assert_eq!(check_legal_holds(&holds[..MAX_LEGAL_HOLDS]), Ok(()));
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
