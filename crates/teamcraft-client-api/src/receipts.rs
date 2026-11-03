//! How a conversation tells the user that others read their messages (#568, #635), the same on
//! desktop, web and the mobile apps (#769).

use crate::model::{ConversationKind, MessageId, UserId};

/// One model per conversation kind, never two saying the same thing. A DM has one reader, so the
/// ticks say it all; a group DM says *who* read it in "Seen by", which a double tick can't.
/// Channels have no receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadIndicator {
  /// Two ticks on the user's messages once the other member read them.
  Ticks,
  /// "Seen by <names>" under the user's newest message; their messages keep the single "sent" tick.
  SeenLine,
  /// No receipts: the single "sent" tick only.
  None,
}

impl ReadIndicator {
  pub fn of(kind: ConversationKind) -> Self {
    match kind {
      ConversationKind::Direct => Self::Ticks,
      ConversationKind::Group => Self::SeenLine,
      ConversationKind::Public | ConversationKind::Private | ConversationKind::Integration => Self::None,
    }
  }
}

/// Whether every other member of the conversation has read the message (#568). Without receipts
/// (channels, receipts off on either side) nobody is in `seen_by`, so it never reads as read.
pub fn is_read_by_all(seen_by: &[UserId], members: &[UserId], me: UserId) -> bool {
  let mut others = members.iter().filter(|member| **member != me).peekable();
  others.peek().is_some() && others.all(|member| seen_by.contains(member))
}

/// The members whose read receipt (the newest message they read) reaches `message`.
pub fn seen_by(receipts: &[(UserId, MessageId)], message: MessageId) -> Vec<UserId> {
  receipts
    .iter()
    .filter(|(_, read_up_to)| *read_up_to >= message)
    .map(|(user, _)| *user)
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  mod read_indicator {
    use super::*;

    #[test]
    fn dms_show_ticks_and_group_dms_the_seen_line() {
      assert_eq!(ReadIndicator::of(ConversationKind::Direct), ReadIndicator::Ticks);
      assert_eq!(
        ReadIndicator::of(ConversationKind::Group),
        ReadIndicator::SeenLine
      );
    }

    #[test]
    fn channels_show_neither() {
      for kind in [
        ConversationKind::Public,
        ConversationKind::Private,
        ConversationKind::Integration,
      ] {
        assert_eq!(ReadIndicator::of(kind), ReadIndicator::None);
      }
    }
  }

  mod read_by_all {
    use super::*;

    #[test]
    fn needs_every_other_member() {
      let (me, a, b) = (user(1), user(2), user(3));
      assert!(is_read_by_all(&[a, b], &[me, a, b], me));
      assert!(!is_read_by_all(&[a], &[me, a, b], me));
    }

    #[test]
    fn nobody_else_or_no_receipts_is_not_read() {
      let (me, a) = (user(1), user(2));
      assert!(!is_read_by_all(&[], &[me, a], me));
      assert!(!is_read_by_all(&[], &[me], me));
    }
  }

  #[test]
  fn a_receipt_covers_its_message_and_everything_before() {
    let (a, b) = (user(2), user(3));
    let receipts = [(a, message(20)), (b, message(10))];
    assert_eq!(seen_by(&receipts, message(10)), vec![a, b]);
    assert_eq!(seen_by(&receipts, message(15)), vec![a]);
    assert_eq!(seen_by(&receipts, message(21)), Vec::<UserId>::new());
  }

  fn user(raw: u64) -> UserId {
    id_from_raw(raw)
  }

  fn message(raw: u64) -> MessageId {
    id_from_raw(raw)
  }
}
