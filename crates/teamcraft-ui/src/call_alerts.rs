//! Incoming calls outside the window (notifications.md, "Calls"): the system notification with
//! Accept and Decline, the ringtone and the window's attention request while a call rings, their
//! withdrawal the moment it stops ringing here, and a missed-call notification when nobody answered.
//!
//! [`CallAlerts`] is the pure state machine: it is fed the ringing calls each time the backend
//! changes and says what to start and stop. Why a ring ended comes from the call roster: if the user
//! is in the call (answered on another device) or someone else is (a group call went on without
//! them), it was not missed; a call that ended or still holds only the caller was. A call accepted or
//! declined on this device is never missed. [`decide_ring`] is the policy: mutes, the
//! notification level and do-not-disturb (with its allow list) apply as they do to a direct message
//! from the caller.

use std::collections::{BTreeMap, BTreeSet};

use teamcraft_proto::PushContentMode;

use crate::calls::IncomingCall;
use crate::model::{Conversation, ConversationId, TimestampMs, User, UserId};
use crate::notifier::{CandidateKind, Decision, PolicyInput, SkipReason, decide};
use crate::prefs::NotificationPrefs;

/// What should happen now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallAlert {
  /// A call started ringing on this device.
  Ring(IncomingCall),
  /// It stopped ringing (answered anywhere, declined, cancelled, timed out): withdraw the
  /// notification and stop the ringtone.
  StopRinging(ConversationId),
  /// Nobody answered: show a missed call (after [`CallAlert::StopRinging`]).
  Missed(IncomingCall),
}

/// Why a call stopped ringing, from the roster at that moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingEnd {
  /// The user is in the call: answered on another device.
  AnsweredByMe,
  /// Someone else answered (a group call); it goes on without the user.
  AnsweredByOthers,
  /// The caller gave up, or the ringing timed out.
  Unanswered,
}

/// How a ringing call reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingDecision {
  /// A system notification with Accept and Decline (the window is not in front).
  pub shows_notification: bool,
  pub plays_ringtone: bool,
  /// Flash the taskbar entry or bounce the dock icon (the window is not focused).
  pub requests_attention: bool,
}

/// Everything [`decide_ring`] looks at.
pub struct RingInput<'a> {
  pub prefs: &'a NotificationPrefs,
  pub me: &'a User,
  pub conversation: &'a Conversation,
  pub caller: UserId,
  pub now: TimestampMs,
  /// The window is focused and visible: the in-app card is enough.
  pub is_window_in_front: bool,
  pub utc_offset_minutes: i16,
  pub maybe_dnd_until: Option<TimestampMs>,
}

/// Tracks which calls ring here.
#[derive(Debug, Default)]
pub struct CallAlerts {
  ringing: BTreeMap<ConversationId, IncomingCall>,
  /// Calls the user accepted or declined on this device since they started ringing.
  handled_here: BTreeSet<ConversationId>,
}

impl CallAlerts {
  /// Compares the calls ringing now with the last state. `ring_end` tells, for a call that stopped
  /// ringing, why.
  pub fn update(
    &mut self,
    incoming: &[IncomingCall],
    ring_end: impl Fn(&IncomingCall) -> RingEnd,
  ) -> Vec<CallAlert> {
    let now: BTreeMap<ConversationId, IncomingCall> =
      incoming.iter().map(|call| (call.conversation, *call)).collect();
    let mut alerts = Vec::new();
    for (conversation, call) in &self.ringing {
      if now.contains_key(conversation) {
        continue;
      }
      alerts.push(CallAlert::StopRinging(*conversation));
      let was_handled = self.handled_here.remove(conversation);
      if !was_handled && ring_end(call) == RingEnd::Unanswered {
        alerts.push(CallAlert::Missed(*call));
      }
    }
    for (conversation, call) in &now {
      if !self.ringing.contains_key(conversation) {
        self.handled_here.remove(conversation);
        alerts.push(CallAlert::Ring(*call));
      }
    }
    self.ringing = now;
    alerts
  }

  /// The user accepted or declined on this device: no missed call when it stops ringing.
  pub fn handled_here(&mut self, conversation: ConversationId) {
    if self.ringing.contains_key(&conversation) {
      self.handled_here.insert(conversation);
    }
  }

  pub fn is_ringing(&self) -> bool {
    !self.ringing.is_empty()
  }
}

/// Why a ring ended, from who is in the call now (`None`: the call is over).
pub fn ring_end(call: &IncomingCall, me: UserId, maybe_participants: Option<&[UserId]>) -> RingEnd {
  let Some(participants) = maybe_participants else {
    return RingEnd::Unanswered;
  };
  if participants.contains(&me) {
    RingEnd::AnsweredByMe
  } else if participants.iter().any(|user| *user != call.caller) {
    RingEnd::AnsweredByOthers
  } else {
    RingEnd::Unanswered
  }
}

/// The policy for a ringing call: `None` keeps it to the in-app card (muted, level "nothing", do
/// not disturb). Otherwise the ringtone plays (if the user wants one), and with the window not in
/// front a notification and an attention request come too.
pub fn decide_ring(input: &RingInput, wants_ringtone: bool) -> Option<RingDecision> {
  if decide(&as_message(input)) != Decision::Notify {
    return None;
  }
  Some(RingDecision {
    shows_notification: !input.is_window_in_front,
    plays_ringtone: wants_ringtone,
    requests_attention: !input.is_window_in_front,
  })
}

/// Why a missed call stays quiet, if it does: the same rules as a direct message from the caller.
pub fn decide_missed(input: &RingInput) -> Decision {
  decide(&as_message(input))
}

/// A call counts as a direct message from the caller with no text: no mention rules, no viewing
/// check (the card shows in every view).
fn as_message<'a>(input: &'a RingInput) -> PolicyInput<'a> {
  PolicyInput {
    prefs: input.prefs,
    me: input.me,
    conversation: input.conversation,
    sender: input.caller,
    body: "",
    kind: CandidateKind::Message,
    now: input.now,
    is_viewing: false,
    is_urgent: false,
    utc_offset_minutes: input.utc_offset_minutes,
    maybe_dnd_until: input.maybe_dnd_until,
  }
}

/// Whether the workspace policy lets a call or meeting notification name people and meetings on
/// the lock screen: like a message's sender, never under the generic mode (or an unknown one).
pub fn may_name_names(mode: &PushContentMode) -> bool {
  matches!(mode, PushContentMode::Full | PushContentMode::Sender)
}

/// Whether a skip was do-not-disturb (the missed call is still listed in the call history).
pub fn is_dnd(decision: Decision) -> bool {
  decision == Decision::Skip(SkipReason::DoNotDisturb)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{ConversationKind, PresenceStatus, SidebarCategory, id_from_raw};
  use crate::prefs::{ConversationNotify, DndSchedule, MuteFor, NotifyLevel};

  const NOW: TimestampMs = 1_792_540_800_000 + 12 * 3_600_000;

  mod lifecycle {
    use super::*;

    #[test]
    fn a_new_call_rings_once() {
      let mut alerts = CallAlerts::default();
      assert_eq!(
        alerts.update(&[call(7)], unanswered),
        vec![CallAlert::Ring(call(7))]
      );
      assert_eq!(alerts.update(&[call(7)], unanswered), Vec::new());
      assert!(alerts.is_ringing());
    }

    #[test]
    fn answering_on_another_device_withdraws_without_a_missed_call() {
      let mut alerts = CallAlerts::default();
      alerts.update(&[call(7)], unanswered);
      let stopped = alerts.update(&[], |call| ring_end(call, me().id, Some(&[call.caller, me().id])));
      assert_eq!(stopped, vec![CallAlert::StopRinging(id_from_raw(7))]);
      assert!(!alerts.is_ringing());
    }

    #[test]
    fn a_group_call_someone_else_answered_is_not_missed() {
      let mut alerts = CallAlerts::default();
      alerts.update(&[call(7)], unanswered);
      let stopped = alerts.update(&[], |call| {
        ring_end(call, me().id, Some(&[call.caller, id_from_raw(5)]))
      });
      assert_eq!(stopped, vec![CallAlert::StopRinging(id_from_raw(7))]);
    }

    #[test]
    fn a_cancelled_or_timed_out_call_becomes_a_missed_call() {
      for participants in [None, Some(vec![id_from_raw::<UserId>(2)])] {
        let mut alerts = CallAlerts::default();
        alerts.update(&[call(7)], unanswered);
        let stopped = alerts.update(&[], |call| ring_end(call, me().id, participants.as_deref()));
        assert_eq!(
          stopped,
          vec![CallAlert::StopRinging(id_from_raw(7)), CallAlert::Missed(call(7))]
        );
      }
    }

    #[test]
    fn a_call_declined_or_accepted_here_is_never_missed() {
      let mut alerts = CallAlerts::default();
      alerts.update(&[call(7)], unanswered);
      alerts.handled_here(id_from_raw(7));
      assert_eq!(
        alerts.update(&[], unanswered),
        vec![CallAlert::StopRinging(id_from_raw(7))]
      );
    }

    #[test]
    fn handling_is_forgotten_when_the_conversation_rings_again() {
      let mut alerts = CallAlerts::default();
      alerts.update(&[call(7)], unanswered);
      alerts.handled_here(id_from_raw(7));
      alerts.update(&[], unanswered);
      alerts.update(&[call(7)], unanswered);
      assert_eq!(
        alerts.update(&[], unanswered),
        vec![CallAlert::StopRinging(id_from_raw(7)), CallAlert::Missed(call(7))]
      );
    }

    #[test]
    fn handling_a_call_that_does_not_ring_changes_nothing() {
      let mut alerts = CallAlerts::default();
      alerts.handled_here(id_from_raw(7));
      alerts.update(&[call(7)], unanswered);
      assert_eq!(
        alerts.update(&[], unanswered),
        vec![CallAlert::StopRinging(id_from_raw(7)), CallAlert::Missed(call(7))]
      );
    }

    #[test]
    fn two_calls_are_tracked_apart() {
      let mut alerts = CallAlerts::default();
      alerts.update(&[call(7)], unanswered);
      assert_eq!(
        alerts.update(&[call(7), call(8)], unanswered),
        vec![CallAlert::Ring(call(8))]
      );
      assert_eq!(
        alerts.update(&[call(8)], |_| RingEnd::AnsweredByMe),
        vec![CallAlert::StopRinging(id_from_raw(7))]
      );
    }
  }

  mod policy {
    use super::*;
    use teamcraft_proto::{DndWindow, Weekday};

    #[test]
    fn a_call_in_the_background_notifies_rings_and_asks_for_attention() {
      let prefs = NotificationPrefs::default();
      assert_eq!(
        decide_ring(&input(&prefs, false), true),
        Some(RingDecision {
          shows_notification: true,
          plays_ringtone: true,
          requests_attention: true,
        })
      );
    }

    #[test]
    fn with_the_window_in_front_only_the_ringtone_plays() {
      let prefs = NotificationPrefs::default();
      assert_eq!(
        decide_ring(&input(&prefs, true), true),
        Some(RingDecision {
          shows_notification: false,
          plays_ringtone: true,
          requests_attention: false,
        })
      );
    }

    #[test]
    fn the_ringtone_can_be_turned_off() {
      let prefs = NotificationPrefs::default();
      let decision = decide_ring(&input(&prefs, false), false).unwrap();
      assert!(!decision.plays_ringtone);
      assert!(decision.shows_notification);
    }

    #[test]
    fn muted_conversations_and_level_nothing_do_not_ring() {
      let mut muted = NotificationPrefs::default();
      muted.set_conversation(
        &id_from_raw::<ConversationId>(7).to_string(),
        ConversationNotify {
          maybe_level: None,
          mute: MuteFor::Forever,
        },
      );
      assert_eq!(decide_ring(&input(&muted, false), true), None);
      let nothing = NotificationPrefs {
        desktop: NotifyLevel::Nothing,
        ..NotificationPrefs::default()
      };
      assert_eq!(decide_ring(&input(&nothing, false), true), None);
    }

    #[test]
    fn do_not_disturb_silences_calls_except_from_allowed_people() {
      let mut prefs = NotificationPrefs {
        dnd: DndSchedule {
          is_enabled: true,
          windows: vec![DndWindow {
            weekdays: vec![Weekday::Wednesday],
            start_minute: 10 * 60,
            end_minute: 14 * 60,
          }],
          ..DndSchedule::default()
        },
        ..NotificationPrefs::default()
      };
      assert_eq!(decide_ring(&input(&prefs, false), true), None);
      assert!(is_dnd(decide_missed(&input(&prefs, false))));
      prefs.dnd.allow_user_ids = vec![id_from_raw(2)];
      assert!(decide_ring(&input(&prefs, false), true).is_some());
    }

    #[test]
    fn manual_do_not_disturb_silences_calls_until_it_ends() {
      let prefs = NotificationPrefs::default();
      let me = User {
        presence: PresenceStatus::DoNotDisturb,
        ..me()
      };
      let mut candidate = input(&prefs, false);
      candidate.me = &me;
      candidate.maybe_dnd_until = Some(NOW + 1);
      assert_eq!(decide_ring(&candidate, true), None);
      candidate.maybe_dnd_until = Some(NOW);
      assert!(decide_ring(&candidate, true).is_some());
    }

    #[test]
    fn the_lock_screen_policy_decides_whether_names_show() {
      assert!(may_name_names(&PushContentMode::Full));
      assert!(may_name_names(&PushContentMode::Sender));
      assert!(!may_name_names(&PushContentMode::Generic));
      assert!(!may_name_names(&PushContentMode::Unknown("later".into())));
    }

    #[test]
    fn missed_calls_follow_the_direct_message_rules() {
      let prefs = NotificationPrefs::default();
      assert_eq!(decide_missed(&input(&prefs, true)), Decision::Notify);
    }
  }

  fn unanswered(_: &IncomingCall) -> RingEnd {
    RingEnd::Unanswered
  }

  fn call(conversation: u64) -> IncomingCall {
    IncomingCall {
      caller: id_from_raw(2),
      conversation: id_from_raw(conversation),
      with_video: false,
    }
  }

  fn input(prefs: &NotificationPrefs, is_window_in_front: bool) -> RingInput<'_> {
    // Leaked: tests need `&'a` values for a whole call and the sizes are tiny.
    let me: &User = Box::leak(Box::new(me()));
    let conversation: &Conversation = Box::leak(Box::new(direct()));
    RingInput {
      prefs,
      me,
      conversation,
      caller: id_from_raw(2),
      now: NOW,
      is_window_in_front,
      utc_offset_minutes: 0,
      maybe_dnd_until: None,
    }
  }

  fn me() -> User {
    User {
      id: id_from_raw(1),
      username: "alice".into(),
      display_name: "Alice Owner".into(),
      presence: PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 10,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: crate::model::DeviceSetup::Unknown,
      maybe_email: None,
    }
  }

  fn direct() -> Conversation {
    Conversation {
      id: id_from_raw(7),
      maybe_space: None,
      kind: ConversationKind::Direct,
      category: SidebarCategory::DirectMessages,
      name: "Bob".into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_image_version: None,
    }
  }
}
