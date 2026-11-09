//! Where this device stands while it waits to be let into a channel's encryption group after a
//! join or an admin's self-join (admin.md §2.4, issue #908). The server can't add a device to the
//! group: a member's app commits the signed self-add. So what the user is told depends on whether
//! any member can do that now, read from presence the gateway keeps current, never polled.

use crate::backend::Backend;
use crate::model::{ConversationId, MembershipIssue, PresenceStatus};

/// How a pending admission into a channel's encryption group stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
  /// A member is online; their app adds this device, normally within seconds.
  Adding,
  /// No other member appears online: the request is kept, also across restarts, and a member's
  /// app adds this device once one is.
  NoMemberOnline,
  /// Members were online but nobody let this device in within the retry interval: their apps may
  /// have refused the request. The app asks again on its own; the user may ask at once.
  NotYetAdded,
}

impl Admission {
  /// The i18n key of the channel's banner.
  pub fn banner_text_key(self) -> &'static str {
    match self {
      Self::Adding => "waiting-for-admission",
      Self::NoMemberOnline => "waiting-for-admission-no-member-online",
      Self::NotYetAdded => "waiting-for-admission-not-yet",
    }
  }

  /// Whether asking again by hand makes sense: only once the automatic request had its chance.
  pub fn can_ask_again(self) -> bool {
    self == Self::NotYetAdded
  }
}

/// Where this device's admission into the conversation stands; `None` when it doesn't wait.
pub fn admission(backend: &dyn Backend, conversation: ConversationId) -> Option<Admission> {
  let attempts = backend
    .membership_issues(conversation)
    .into_iter()
    .find_map(|issue| match issue {
      MembershipIssue::WaitingForAdmission { attempts } => Some(attempts),
      _ => None,
    })?;
  Some(classify(
    attempts,
    is_another_member_online(backend, conversation),
  ))
}

/// The state for `attempts` requests posted so far and whether another member is online.
pub fn classify(attempts: u32, is_member_online: bool) -> Admission {
  if !is_member_online {
    Admission::NoMemberOnline
  } else if attempts > 1 {
    Admission::NotYetAdded
  } else {
    Admission::Adding
  }
}

/// Whether a member other than this user is connected (any presence but offline). Stops at the
/// first one, so a big channel costs little; only read while this device waits.
fn is_another_member_online(backend: &dyn Backend, conversation: ConversationId) -> bool {
  let me = backend.me().id;
  let Some(channel) = backend.conversation(conversation) else {
    return false;
  };
  channel
    .member_ids
    .iter()
    .filter(|&&user| user != me)
    .filter_map(|&user| backend.user(user))
    .any(|user| !user.is_deactivated && user.presence != PresenceStatus::Offline)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn nobody_online_says_so_whatever_the_attempts() {
    assert_eq!(classify(1, false), Admission::NoMemberOnline);
    assert_eq!(classify(4, false), Admission::NoMemberOnline);
  }

  #[test]
  fn a_member_online_adds_on_the_first_request() {
    assert_eq!(classify(1, true), Admission::Adding);
    assert!(!Admission::Adding.can_ask_again());
  }

  #[test]
  fn a_request_nobody_took_offers_asking_again() {
    assert_eq!(classify(2, true), Admission::NotYetAdded);
    assert!(Admission::NotYetAdded.can_ask_again());
  }
}
