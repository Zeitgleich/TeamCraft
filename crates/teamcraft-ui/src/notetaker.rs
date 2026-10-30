//! The notetaker as the call screen sees it (e2ee.md §8.2): who it is comes from the verified
//! policy, whether it is in the call from the participant list; never from what the server or the
//! notetaker say about themselves. What the user may do is only what the screen offers: the
//! notetaker decides on every request from its own copy of the signed data.

use teamcraft_proto::{NotetakerInviters, NotetakerPolicy};

use crate::model::UserId;

/// What the call screen shows and offers about the notetaker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotetakerInCall {
  pub bot: UserId,
  /// Where the audio goes, for the notices.
  pub transcription_model: String,
  /// The notetaker is a participant of the call now.
  pub is_present: bool,
  /// The notetaker is a member of the conversation. "Add notetaker" adds it first otherwise, and
  /// asks it in once its membership commit is in (e2ee.md §8.2).
  pub is_member: bool,
  /// "Add notetaker": on, allowed, not in the call yet, and a member of the conversation or one
  /// the user can add there.
  pub can_invite: bool,
  /// "Remove notetaker".
  pub can_stop: bool,
}

/// The notetaker of `policy` in a call with `participants` (the others), for the user in a
/// conversation with `member_ids`. `is_person`: the user is neither a guest nor a bot.
/// `can_add_members`: the user can add the notetaker to the conversation (a channel where the
/// user manages members, a meeting the user organizes; never a DM, which takes nobody more).
/// `None` without a notetaker account in the policy.
pub fn notetaker_in_call(
  policy: &NotetakerPolicy,
  is_person: bool,
  is_roster_admin: bool,
  member_ids: &[UserId],
  participants: &[UserId],
  can_add_members: bool,
) -> Option<NotetakerInCall> {
  // Its account counts as the notetaker even while the policy turns it off: it is still in the
  // call until it has left, and the notices stay until then.
  let bot = policy.maybe_bot?;
  let is_present = participants.contains(&bot);
  let is_member = member_ids.contains(&bot);
  let may_invite = may_ask_in(policy, is_person, is_roster_admin);
  let can_stop = is_present && (policy.can_anyone_stop || may_invite || (is_roster_admin && is_person));
  Some(NotetakerInCall {
    bot,
    transcription_model: policy.transcription_model.clone(),
    is_present,
    is_member,
    can_invite: may_invite && !is_present && (is_member || can_add_members),
    can_stop,
  })
}

/// Whether the user may ask the notetaker in, as the policy says (the notetaker checks again).
pub fn may_ask_in(policy: &NotetakerPolicy, is_person: bool, is_roster_admin: bool) -> bool {
  policy.active_bot().is_some()
    && is_person
    && match policy.inviters {
      NotetakerInviters::Members => true,
      NotetakerInviters::Admins => is_roster_admin,
      NotetakerInviters::Unknown(_) => false,
    }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_member_asks_it_in_where_it_is_a_member_and_not_yet_in_the_call() {
    let policy = policy(NotetakerInviters::Members);
    let alice = true;
    let in_call = notetaker_in_call(&policy, alice, false, &[id(1), id(9)], &[id(2)], false).unwrap();
    assert!(in_call.can_invite && !in_call.is_present && !in_call.can_stop);
    let elsewhere = notetaker_in_call(&policy, alice, false, &[id(1)], &[], false).unwrap();
    assert!(!elsewhere.can_invite, "not a member, and alice can't add it");
    assert!(!elsewhere.is_member);
    let present = notetaker_in_call(&policy, alice, false, &[id(1), id(9)], &[id(9)], false).unwrap();
    assert!(present.is_present && !present.can_invite && present.can_stop);
  }

  #[test]
  fn who_can_add_members_asks_it_in_where_it_is_no_member_yet() {
    let policy = policy(NotetakerInviters::Members);
    let in_call = notetaker_in_call(&policy, true, false, &[id(1)], &[id(1)], true).unwrap();
    assert!(in_call.can_invite && !in_call.is_member && !in_call.is_present);
    let present = notetaker_in_call(&policy, true, false, &[id(1), id(9)], &[id(9)], true).unwrap();
    assert!(!present.can_invite, "in the call already");
    let guest = notetaker_in_call(&policy, false, false, &[id(1)], &[], true).unwrap();
    assert!(
      !guest.can_invite,
      "the policy decides, not the right to add members"
    );
    let mut off = policy.clone();
    off.is_enabled = false;
    let disabled = notetaker_in_call(&off, true, false, &[id(1)], &[], true).unwrap();
    assert!(!disabled.can_invite);
  }

  #[test]
  fn with_admins_only_a_member_who_can_add_people_still_does_not_ask_it_in() {
    let policy = policy(NotetakerInviters::Admins);
    let member = notetaker_in_call(&policy, true, false, &[id(1)], &[], true).unwrap();
    assert!(!member.can_invite);
    let admin = notetaker_in_call(&policy, true, true, &[id(1)], &[], true).unwrap();
    assert!(admin.can_invite && !admin.is_member);
  }

  #[test]
  fn guests_never_ask_it_in_but_may_send_it_away_when_anyone_may() {
    let mut policy = policy(NotetakerInviters::Members);
    let guest = false;
    let present = notetaker_in_call(&policy, guest, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!present.can_invite && present.can_stop);
    policy.can_anyone_stop = false;
    let strict = notetaker_in_call(&policy, guest, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!strict.can_stop);
  }

  #[test]
  fn with_admins_only_members_neither_ask_nor_stop_unless_anyone_may() {
    let mut policy = policy(NotetakerInviters::Admins);
    policy.can_anyone_stop = false;
    let member = true;
    let absent = notetaker_in_call(&policy, member, false, &[id(9)], &[], false).unwrap();
    assert!(!absent.can_invite);
    let admin = notetaker_in_call(&policy, member, true, &[id(9)], &[], false).unwrap();
    assert!(admin.can_invite);
    let present = notetaker_in_call(&policy, member, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!present.can_stop);
  }

  #[test]
  fn a_disabled_notetaker_still_shows_while_it_is_in_the_call() {
    let mut policy = policy(NotetakerInviters::Members);
    policy.is_enabled = false;
    let in_call = notetaker_in_call(&policy, true, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(in_call.is_present && !in_call.can_invite);
    policy.maybe_bot = None;
    assert_eq!(notetaker_in_call(&policy, true, false, &[], &[], true), None);
  }

  fn policy(inviters: NotetakerInviters) -> NotetakerPolicy {
    NotetakerPolicy {
      is_enabled: true,
      maybe_bot: Some(id(9)),
      inviters,
      ..NotetakerPolicy::default()
    }
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
