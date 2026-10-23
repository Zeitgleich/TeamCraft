//! Who gets a push, and when (e2ee.md §7, notifications.md "Push routing" and "Mobile push
//! delivery"): the server decides from metadata it has anyway. Messages, rings, withdrawals after
//! reading elsewhere and test pushes all start here.

use std::collections::HashSet;
use std::time::Duration;

use teamcraft_db::{DueReminder, PushDeviceRow, PushRecipientRow};
use teamcraft_proto::{
  CallId, ConversationId, ConversationKind, DeviceId, DndSchedule, Id, MessageId, NotificationLevel,
  PushKind, PushMode, PushProvider, PushWakeUp, Snowflake, TestPushOutcome, TestPushResponse, ThreadId,
  UserId,
};
use time::OffsetDateTime;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;
use tracing::{debug, warn};

use super::delivery::{self, can_dismiss};
use super::sender::{PushError, now_millis};
use super::tracker::{DeliveryEvent, Ring};
use crate::gateway::ActiveDevices;
use crate::services::notifications;
use crate::state::AppState;

/// A message that was just stored.
#[derive(Debug, Clone)]
pub struct NewMessageEvent {
  pub conversation: ConversationId,
  pub kind: ConversationKind,
  pub message: MessageId,
  pub sender: UserId,
  pub mentioned: Vec<UserId>,
  /// The thread the message replies in.
  pub maybe_thread: Option<ThreadId>,
  pub is_urgent: bool,
  /// The conversation is an integration channel (logs, monitoring).
  pub is_integration: bool,
}

/// Why a user is not pushed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
  Muted,
  DoNotDisturb,
  /// A device of the user is in use: they see the message there.
  Active,
  /// The channel only notifies on mentions and this one doesn't mention the user.
  NotMentioned,
  /// Integration channels push only for mentions.
  IntegrationChannel,
}

/// What the user is doing on their connected devices right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
  /// Nothing in use (devices may be connected but idle).
  Away,
  /// A phone or tablet has the app in the foreground.
  MobileActive,
  /// The user is looking at a desktop or web session.
  DesktopAttentive,
}

/// When the push goes out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
  Now,
  /// After the grace delay, and only if the message is still unread then (the user may read it on
  /// the desktop they are looking at).
  AfterGrace,
}

/// The routing's answer for one recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
  pub kind: PushKind,
  pub timing: Timing,
}

/// What the routing knows about one recipient.
#[derive(Debug, Clone, Copy)]
pub struct RecipientState {
  pub level_is_all: bool,
  pub level_is_none: bool,
  pub is_mentioned: bool,
  /// DMs and group DMs notify like a mention.
  pub is_direct: bool,
  /// Integration channel: only mentions notify.
  pub is_integration: bool,
  /// DND is on and no override lets this message through.
  pub is_dnd: bool,
  /// The user follows the thread the message replies in.
  pub is_followed_thread: bool,
  /// Push even while a device of the user is in use.
  pub always_pushes: bool,
  pub attention: Attention,
  /// `notifications.mobile_push_delay_secs` is not 0.
  pub has_grace: bool,
}

/// The routing rules (notifications.md, "Push routing"):
///
/// 1. Muted (level `none`, or muted until a later time) never notifies.
/// 2. Do-not-disturb suppresses the push unless an override applies (decided by the caller).
/// 3. Mentions, direct messages and replies in followed threads push; otherwise only `all` channels
///    do, and integration channels only for mentions.
/// 4. Unless the user chose to be pushed always: no push while a phone is in use; while a desktop
///    is attentive, the push waits for the grace delay (or is dropped without one).
pub fn decide(state: RecipientState) -> Result<Decision, Skip> {
  if state.level_is_none {
    return Err(Skip::Muted);
  }
  if state.is_dnd {
    return Err(Skip::DoNotDisturb);
  }
  let kind = if state.is_mentioned || state.is_direct {
    PushKind::Mention
  } else if state.is_integration {
    return Err(Skip::IntegrationChannel);
  } else if state.level_is_all || state.is_followed_thread {
    PushKind::Message
  } else {
    return Err(Skip::NotMentioned);
  };
  let timing = match (state.always_pushes, state.attention, state.has_grace) {
    (true, _, _) | (false, Attention::Away, _) => Timing::Now,
    (false, Attention::MobileActive, _) | (false, Attention::DesktopAttentive, false) => {
      return Err(Skip::Active);
    },
    (false, Attention::DesktopAttentive, true) => Timing::AfterGrace,
  };
  Ok(Decision { kind, timing })
}

/// Rings ring every device, in use or not; only a mute or DND without an override stops them.
pub fn decide_ring(is_muted: bool, is_dnd: bool) -> Result<(), Skip> {
  if is_muted {
    Err(Skip::Muted)
  } else if is_dnd {
    Err(Skip::DoNotDisturb)
  } else {
    Ok(())
  }
}

/// Pushes for one stored message. Runs in the background; failures are logged and never reach the sender.
pub async fn notify_message(state: AppState, event: NewMessageEvent) {
  if let Err(error) = route(&state, &event, OffsetDateTime::now_utc()).await {
    warn!(%error, conversation_id = %event.conversation, "push routing failed");
  }
}

async fn route(
  state: &AppState,
  event: &NewMessageEvent,
  now: OffsetDateTime,
) -> Result<(), teamcraft_db::DbError> {
  let recipients = state
    .database
    .list_push_recipients(event.conversation, event.sender, event.maybe_thread)
    .await?;
  for recipient in recipients {
    push_to_recipient(state, event, recipient, now).await;
  }
  Ok(())
}

/// Pushes an urgent direct message again to one recipient who has not acknowledged it (the
/// scheduler of `services::urgent`). The routing rules apply as for the first push; the apps replace
/// the earlier notification and alert again.
pub async fn notify_reminder(
  state: &AppState,
  reminder: DueReminder,
  now: OffsetDateTime,
) -> Result<(), teamcraft_db::DbError> {
  let Some(message) = state
    .database
    .get_message(reminder.conversation, reminder.message)
    .await?
  else {
    return Ok(());
  };
  let event = NewMessageEvent {
    conversation: reminder.conversation,
    kind: ConversationKind::Dm,
    message: message.id,
    sender: message.sender_user_id,
    mentioned: vec![reminder.user],
    maybe_thread: message.maybe_thread_id,
    is_urgent: true,
    is_integration: false,
  };
  let recipients = state
    .database
    .list_push_recipients(
      reminder.conversation,
      message.sender_user_id,
      message.maybe_thread_id,
    )
    .await?;
  for recipient in recipients
    .into_iter()
    .filter(|recipient| recipient.user == reminder.user)
  {
    push_to_recipient(state, &event, recipient, now).await;
  }
  Ok(())
}

async fn push_to_recipient(
  state: &AppState,
  event: &NewMessageEvent,
  recipient: PushRecipientRow,
  now: OffsetDateTime,
) {
  let verdict = decide(RecipientState {
    level_is_all: recipient.notification_level == NotificationLevel::All,
    level_is_none: is_muted(&recipient, now),
    is_mentioned: event.mentioned.contains(&recipient.user),
    is_direct: matches!(event.kind, ConversationKind::Dm | ConversationKind::Group),
    is_integration: event.is_integration,
    is_dnd: is_dnd(&recipient, now, event.sender, event.is_urgent),
    is_followed_thread: recipient.is_following_thread,
    always_pushes: always_pushes(&recipient),
    attention: attention(state, &recipient),
    has_grace: state.settings.notifications.mobile_push_delay_secs > 0,
  });
  let decision = match verdict {
    Ok(decision) => decision,
    Err(skip) => {
      debug!(user_id = %recipient.user, ?skip, "no push");
      if skip == Skip::DoNotDisturb {
        record_all(state, &recipient, DeliveryEvent::SuppressedDnd);
      }
      return;
    },
  };
  let wake_up = PushWakeUp {
    maybe_thread_id: event.maybe_thread,
    maybe_badge: badge(state, recipient.user, &recipient.devices).await,
    maybe_sent_at: Some(now_millis()),
    ..PushWakeUp::for_message(
      decision.kind,
      event.conversation,
      event.message,
      event.sender,
      event.is_urgent,
    )
  };
  match decision.timing {
    Timing::Now => send_to_offline_devices(state, recipient.user, recipient.devices, &wake_up),
    Timing::AfterGrace => {
      let grace = Duration::from_secs(state.settings.notifications.mobile_push_delay_secs);
      tokio::spawn(hold_for_desktop(state.clone(), recipient, wake_up, grace));
    },
  }
}

/// Rings the members of a DM or group DM (except the caller) on every device without a live
/// connection; devices with one get the gateway's `call_ring`.
pub async fn notify_ring(state: AppState, conversation: ConversationId, call: CallId, caller: UserId) {
  let recipients = match state
    .database
    .list_push_recipients(conversation, caller, None)
    .await
  {
    Ok(recipients) => recipients,
    Err(error) => {
      warn!(%error, conversation_id = %conversation, "listing the people to ring failed");
      return;
    },
  };
  let now = OffsetDateTime::now_utc();
  let mut targets = Vec::new();
  for recipient in recipients {
    let verdict = decide_ring(is_muted(&recipient, now), is_dnd(&recipient, now, caller, false));
    if let Err(skip) = verdict {
      debug!(user_id = %recipient.user, ?skip, "no ring push");
      if skip == Skip::DoNotDisturb {
        record_all(&state, &recipient, DeliveryEvent::SuppressedDnd);
      }
      continue;
    }
    let wake_up = PushWakeUp {
      kind: PushKind::Call,
      conversation_id: conversation,
      maybe_message_id: None,
      maybe_sender_id: Some(caller),
      is_urgent: false,
      maybe_thread_id: None,
      maybe_call_id: Some(call),
      maybe_badge: None,
      maybe_sent_at: Some(now_millis()),
    };
    for device in recipient.devices {
      if state.hub.is_device_connected(recipient.user, device.device) {
        continue;
      }
      if can_dismiss(&device.provider) {
        targets.push((recipient.user, device.device));
      }
      delivery::spawn(state.clone(), recipient.user, device, wake_up.clone());
    }
  }
  if !targets.is_empty() {
    state.push_tracker.record_ring(
      call,
      Ring {
        conversation,
        targets,
      },
    );
  }
}

/// The ring ended (answered, timed out, cancelled): phones that rang stop.
pub async fn end_ring(state: AppState, call: CallId) {
  let Some(ring) = state.push_tracker.take_ring(call) else {
    return;
  };
  let mut users: Vec<UserId> = ring.targets.iter().map(|(user, _)| *user).collect();
  users.sort_unstable();
  users.dedup();
  for user in users {
    let devices: HashSet<DeviceId> = ring
      .targets
      .iter()
      .filter(|(owner, _)| *owner == user)
      .map(|(_, device)| *device)
      .collect();
    let wake_up = PushWakeUp {
      kind: PushKind::Dismiss,
      conversation_id: ring.conversation,
      maybe_message_id: None,
      maybe_sender_id: None,
      is_urgent: false,
      maybe_thread_id: None,
      maybe_call_id: Some(call),
      maybe_badge: None,
      maybe_sent_at: Some(now_millis()),
    };
    send_dismissal(&state, user, &devices, wake_up).await;
  }
}

/// The user read the conversation (or a thread of it) up to `up_to`: phones that show a
/// notification for it remove it. Devices with a live connection learn it from the gateway.
pub async fn dismiss_read(
  state: AppState,
  user: UserId,
  conversation: ConversationId,
  maybe_thread: Option<ThreadId>,
  up_to: MessageId,
) {
  let devices: HashSet<DeviceId> = state
    .push_tracker
    .take_read(user, conversation, maybe_thread, up_to)
    .into_iter()
    .collect();
  if devices.is_empty() {
    return;
  }
  let maybe_badge = match state.database.push_badge_count(user).await {
    Ok(count) => Some(count),
    Err(error) => {
      warn!(%error, "counting the badge failed");
      None
    },
  };
  let wake_up = PushWakeUp {
    kind: PushKind::Dismiss,
    conversation_id: conversation,
    maybe_message_id: Some(up_to),
    maybe_sender_id: None,
    is_urgent: false,
    maybe_thread_id: maybe_thread,
    maybe_call_id: None,
    maybe_badge,
    maybe_sent_at: Some(now_millis()),
  };
  send_dismissal(&state, user, &devices, wake_up).await;
}

/// Sends a test wake-up to one of the user's devices and says how the push service took it.
pub async fn send_test(
  state: &AppState,
  user: UserId,
  device: DeviceId,
) -> Result<TestPushResponse, teamcraft_db::DbError> {
  let devices = state.database.list_push_devices(user).await?;
  let sent_at = now_millis();
  let Some(row) = devices.into_iter().find(|row| row.device == device) else {
    return Ok(TestPushResponse {
      outcome: TestPushOutcome::NotRegistered,
      maybe_provider: None,
      sent_at,
    });
  };
  let wake_up = PushWakeUp {
    kind: PushKind::Test,
    conversation_id: no_conversation(),
    maybe_message_id: None,
    maybe_sender_id: None,
    is_urgent: false,
    maybe_thread_id: None,
    maybe_call_id: None,
    maybe_badge: None,
    maybe_sent_at: Some(sent_at),
  };
  let outcome = match delivery::deliver_once(state, user, &row, &wake_up).await {
    Ok(()) => TestPushOutcome::Sent,
    Err(PushError::Gone) => TestPushOutcome::Gone,
    Err(PushError::NotConfigured) => TestPushOutcome::NotConfigured,
    Err(PushError::Unavailable(_)) => TestPushOutcome::Unavailable,
    Err(PushError::Rejected(_) | PushError::Invalid(_) | PushError::Crypto(_)) => TestPushOutcome::Rejected,
  };
  Ok(TestPushResponse {
    outcome,
    maybe_provider: Some(row.provider),
    sent_at,
  })
}

/// Waits while the user looks at a desktop: until the grace delay is over or the desktop stops being
/// attentive. Then pushes the phones if the message is still unread.
async fn hold_for_desktop(
  state: AppState,
  recipient: PushRecipientRow,
  wake_up: PushWakeUp,
  grace: Duration,
) {
  let deadline = Instant::now() + grace;
  let mut lost = state.hub.subscribe_attention_lost();
  loop {
    tokio::select! {
      () = tokio::time::sleep_until(deadline) => break,
      result = lost.recv() => match result {
        Ok(user) if user == recipient.user && attention(&state, &recipient) != Attention::DesktopAttentive => break,
        Ok(_) | Err(RecvError::Lagged(_)) => {},
        // The hub is gone: the server shuts down.
        Err(RecvError::Closed) => return,
      },
    }
  }
  if let Some(message) = wake_up.maybe_message_id {
    match state
      .database
      .has_read_message(recipient.user, wake_up.conversation_id, message)
      .await
    {
      Ok(true) => {
        debug!(user_id = %recipient.user, "the message was read meanwhile; no push to the phone");
        record_all(&state, &recipient, DeliveryEvent::HeldForDesktop);
        return;
      },
      Ok(false) => {},
      Err(error) => warn!(%error, "checking the read position failed; pushing anyway"),
    }
  }
  // Devices as they are now: one revoked (or re-registered) during the wait gets nothing.
  let devices = match state.database.list_push_devices(recipient.user).await {
    Ok(current) => current
      .into_iter()
      .filter(|device| recipient.devices.iter().any(|held| held.device == device.device))
      .collect(),
    Err(error) => {
      warn!(%error, "reading the push devices failed; the held push is dropped");
      return;
    },
  };
  send_to_offline_devices(&state, recipient.user, devices, &wake_up);
}

/// A device with a live connection already got the event over the gateway.
fn send_to_offline_devices(
  state: &AppState,
  user: UserId,
  devices: Vec<PushDeviceRow>,
  wake_up: &PushWakeUp,
) {
  for device in devices {
    if state.hub.is_device_connected(user, device.device) {
      continue;
    }
    delivery::spawn(state.clone(), user, device, wake_up.clone());
  }
}

async fn send_dismissal(state: &AppState, user: UserId, devices: &HashSet<DeviceId>, wake_up: PushWakeUp) {
  let rows = match state.database.list_push_devices(user).await {
    Ok(rows) => rows,
    Err(error) => {
      warn!(%error, "listing push devices for a withdrawal failed");
      return;
    },
  };
  for row in rows {
    if devices.contains(&row.device)
      && can_dismiss(&row.provider)
      && !state.hub.is_device_connected(user, row.device)
    {
      delivery::spawn(state.clone(), user, row, wake_up.clone());
    }
  }
}

/// The badge for the user's phones; absent without a phone or when counting fails.
async fn badge(state: &AppState, user: UserId, devices: &[PushDeviceRow]) -> Option<u32> {
  if !devices.iter().any(|device| is_mobile(&device.provider)) {
    return None;
  }
  match state.database.push_badge_count(user).await {
    Ok(count) => Some(count),
    Err(error) => {
      warn!(%error, "counting the badge failed");
      None
    },
  }
}

fn record_all(state: &AppState, recipient: &PushRecipientRow, event: DeliveryEvent) {
  let now = now_millis();
  for device in &recipient.devices {
    state
      .push_tracker
      .record(recipient.user, device.device, event, now);
  }
}

fn attention(state: &AppState, recipient: &PushRecipientRow) -> Attention {
  let active = active_devices(state, recipient);
  if active.is_mobile_active {
    Attention::MobileActive
  } else if active.is_desktop_attentive {
    Attention::DesktopAttentive
  } else {
    Attention::Away
  }
}

fn active_devices(state: &AppState, recipient: &PushRecipientRow) -> ActiveDevices {
  state.hub.active_devices(recipient.user, |device| {
    recipient
      .devices
      .iter()
      .any(|registered| registered.device == device && is_mobile(&registered.provider))
  })
}

fn is_mobile(provider: &PushProvider) -> bool {
  !matches!(provider, PushProvider::WebPush | PushProvider::Unknown(_))
}

fn always_pushes(recipient: &PushRecipientRow) -> bool {
  PushMode::from(recipient.push_mode.clone()) == PushMode::Always
}

fn is_muted(recipient: &PushRecipientRow, now: OffsetDateTime) -> bool {
  recipient.notification_level == NotificationLevel::None
    || notifications::is_muted(recipient.maybe_muted_until, now)
}

/// DND, manual or scheduled, unless an override (allowed sender, urgent message) lets this through.
fn is_dnd(recipient: &PushRecipientRow, now: OffsetDateTime, sender: UserId, is_urgent: bool) -> bool {
  let maybe_schedule = schedule(recipient);
  if maybe_schedule
    .as_ref()
    .is_some_and(|schedule| schedule.lets_through(sender, is_urgent))
  {
    return false;
  }
  let is_manual =
    recipient.presence_pref == "dnd" && recipient.maybe_dnd_until.is_none_or(|until| until > now);
  is_manual
    || maybe_schedule.is_some_and(|schedule| {
      let unix_seconds = now.unix_timestamp();
      schedule.is_active_at(
        unix_seconds,
        notifications::utc_offset_at(&schedule, unix_seconds),
      )
    })
}

fn schedule(recipient: &PushRecipientRow) -> Option<DndSchedule> {
  let raw = recipient.maybe_dnd_schedule.as_ref()?;
  match serde_json::from_str::<DndSchedule>(raw) {
    Ok(schedule) => Some(schedule),
    Err(error) => {
      // Validated on write, so this means a stored value from an incompatible version: notify.
      warn!(%error, user_id = %recipient.user, "ignoring an unreadable DND schedule");
      None
    },
  }
}

/// Test pushes name no conversation; ID `0` would be the first instant of the Snowflake epoch on
/// worker 0, which no server hands out in practice.
fn no_conversation() -> ConversationId {
  ConversationId::from_snowflake(Snowflake::ZERO)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn base() -> RecipientState {
    RecipientState {
      level_is_all: false,
      level_is_none: false,
      is_mentioned: false,
      is_direct: false,
      is_integration: false,
      is_dnd: false,
      is_followed_thread: false,
      always_pushes: false,
      attention: Attention::Away,
      has_grace: true,
    }
  }

  fn now(kind: PushKind) -> Result<Decision, Skip> {
    Ok(Decision {
      kind,
      timing: Timing::Now,
    })
  }

  #[test]
  fn channels_that_notify_on_mentions_only_push_for_mentions() {
    assert_eq!(decide(base()), Err(Skip::NotMentioned));
    assert_eq!(
      decide(RecipientState {
        is_mentioned: true,
        ..base()
      }),
      now(PushKind::Mention)
    );
  }

  #[test]
  fn channels_that_notify_on_everything_push_every_message() {
    assert_eq!(
      decide(RecipientState {
        level_is_all: true,
        ..base()
      }),
      now(PushKind::Message)
    );
    assert_eq!(
      decide(RecipientState {
        level_is_all: true,
        is_mentioned: true,
        ..base()
      }),
      now(PushKind::Mention)
    );
  }

  #[test]
  fn direct_messages_notify_like_mentions() {
    assert_eq!(
      decide(RecipientState {
        is_direct: true,
        ..base()
      }),
      now(PushKind::Mention)
    );
  }

  #[test]
  fn integration_channels_push_only_for_mentions() {
    let integration = RecipientState {
      is_integration: true,
      level_is_all: true,
      ..base()
    };
    assert_eq!(decide(integration), Err(Skip::IntegrationChannel));
    assert_eq!(
      decide(RecipientState {
        is_mentioned: true,
        ..integration
      }),
      now(PushKind::Mention)
    );
  }

  #[test]
  fn replies_in_followed_threads_push_even_in_mention_only_channels() {
    assert_eq!(
      decide(RecipientState {
        is_followed_thread: true,
        ..base()
      }),
      now(PushKind::Message)
    );
    assert_eq!(
      decide(RecipientState {
        is_followed_thread: true,
        level_is_none: true,
        ..base()
      }),
      Err(Skip::Muted)
    );
  }

  #[test]
  fn a_mute_beats_everything_else() {
    let muted = RecipientState {
      level_is_none: true,
      is_mentioned: true,
      is_direct: true,
      level_is_all: true,
      always_pushes: true,
      ..base()
    };
    assert_eq!(decide(muted), Err(Skip::Muted));
  }

  #[test]
  fn do_not_disturb_suppresses_even_mentions() {
    assert_eq!(
      decide(RecipientState {
        is_dnd: true,
        is_mentioned: true,
        always_pushes: true,
        ..base()
      }),
      Err(Skip::DoNotDisturb)
    );
  }

  mod attention {
    use super::*;

    fn mention(attention: Attention) -> RecipientState {
      RecipientState {
        is_mentioned: true,
        attention,
        ..base()
      }
    }

    #[test]
    fn a_phone_in_use_gets_no_push() {
      assert_eq!(decide(mention(Attention::MobileActive)), Err(Skip::Active));
    }

    #[test]
    fn an_attentive_desktop_holds_the_push_for_the_grace_delay() {
      assert_eq!(
        decide(mention(Attention::DesktopAttentive)),
        Ok(Decision {
          kind: PushKind::Mention,
          timing: Timing::AfterGrace,
        })
      );
    }

    #[test]
    fn without_a_grace_delay_an_attentive_desktop_suppresses_the_push() {
      assert_eq!(
        decide(RecipientState {
          has_grace: false,
          ..mention(Attention::DesktopAttentive)
        }),
        Err(Skip::Active)
      );
    }

    #[test]
    fn a_desktop_that_is_open_but_not_looked_at_does_not_delay_the_phone() {
      assert_eq!(decide(mention(Attention::Away)), now(PushKind::Mention));
    }

    #[test]
    fn users_who_chose_always_are_pushed_at_once_whatever_they_use() {
      for attention in [Attention::MobileActive, Attention::DesktopAttentive] {
        assert_eq!(
          decide(RecipientState {
            always_pushes: true,
            ..mention(attention)
          }),
          now(PushKind::Mention)
        );
      }
    }
  }

  mod rings {
    use super::*;

    #[test]
    fn rings_stop_only_for_mutes_and_dnd() {
      assert_eq!(decide_ring(false, false), Ok(()));
      assert_eq!(decide_ring(true, false), Err(Skip::Muted));
      assert_eq!(decide_ring(false, true), Err(Skip::DoNotDisturb));
    }
  }

  mod do_not_disturb {
    use teamcraft_proto::{DndWindow, Weekday};

    use super::*;

    // 2026-01-05 (Monday) 23:00 UTC.
    const MONDAY_NIGHT: i64 = 1_767_571_200 + 23 * 3600;
    // 2026-07-06 (Monday) 00:00 UTC, in summer time.
    const SUMMER_MONDAY: i64 = 1_783_296_000;

    #[test]
    fn timed_do_not_disturb_ends() {
      let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
      let at = |until: Option<i64>| PushRecipientRow {
        maybe_dnd_until: until.map(|s| now + time::Duration::seconds(s)),
        ..recipient("dnd", None)
      };
      let sender = user(2);
      assert!(is_dnd(&at(None), now, sender, false));
      assert!(is_dnd(&at(Some(60)), now, sender, false));
      assert!(!is_dnd(&at(Some(-60)), now, sender, false));
      assert!(!is_dnd(&recipient("auto", None), now, sender, false));
    }

    #[test]
    fn the_schedule_silences_everybody_but_allowed_senders() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT).unwrap();
      let scheduled = recipient("auto", Some(night_shift(None)));
      assert!(is_dnd(&scheduled, now, user(2), false));
      assert!(!is_dnd(&scheduled, now, user(3), false));
      let evening = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT - 5 * 3600).unwrap();
      assert!(!is_dnd(&scheduled, evening, user(2), false));
    }

    #[test]
    fn overrides_apply_to_manual_dnd_too() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT - 12 * 3600).unwrap();
      let manual = recipient("dnd", Some(night_shift(None)));
      assert!(is_dnd(&manual, now, user(2), false));
      assert!(!is_dnd(&manual, now, user(3), false));
    }

    #[test]
    fn urgent_messages_break_through_when_the_user_allows_it() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT).unwrap();
      let strict = recipient("dnd", Some(night_shift(None)));
      assert!(is_dnd(&strict, now, user(2), true));
      let mut schedule: DndSchedule = serde_json::from_str(&night_shift(None)).unwrap();
      schedule.allows_urgent = true;
      let lenient = recipient("dnd", Some(serde_json::to_string(&schedule).unwrap()));
      assert!(!is_dnd(&lenient, now, user(2), true));
      assert!(is_dnd(&lenient, now, user(2), false));
    }

    #[test]
    fn the_time_zone_follows_daylight_saving_time() {
      // 22:00-07:00 in Berlin: 21:00 UTC in winter (CET), 20:00 UTC in summer (CEST). The stored
      // offset (+60) is the winter one, as a client saved it in January.
      let berlin = recipient("auto", Some(night_shift(Some("Europe/Berlin"))));
      let summer_evening = |hour: i64| {
        // Sunday evening, so the Sunday window applies.
        OffsetDateTime::from_unix_timestamp(SUMMER_MONDAY - 24 * 3600 + hour * 3600).unwrap()
      };
      assert!(is_dnd(
        &berlin,
        summer_evening(20) + time::Duration::minutes(30),
        user(2),
        false
      ));
      assert!(!is_dnd(
        &berlin,
        summer_evening(19) + time::Duration::minutes(30),
        user(2),
        false
      ));
      // Without the zone, the stale offset puts the start an hour late.
      let fixed = recipient("auto", Some(night_shift(None)));
      assert!(!is_dnd(
        &fixed,
        summer_evening(20) + time::Duration::minutes(30),
        user(2),
        false
      ));
    }

    #[test]
    fn an_unknown_time_zone_falls_back_to_the_offset() {
      let schedule: DndSchedule = serde_json::from_str(&night_shift(Some("Mars/Olympus"))).unwrap();
      assert_eq!(notifications::utc_offset_at(&schedule, MONDAY_NIGHT), 60);
      let berlin: DndSchedule = serde_json::from_str(&night_shift(Some("Europe/Berlin"))).unwrap();
      assert_eq!(notifications::utc_offset_at(&berlin, SUMMER_MONDAY), 120);
    }

    #[test]
    fn an_unreadable_schedule_does_not_silence() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT).unwrap();
      let broken = recipient("auto", Some("{not json".into()));
      assert!(!is_dnd(&broken, now, user(2), false));
    }

    /// 22:00-07:00 local every day, user 3 allowed, stored with a +60 offset.
    fn night_shift(maybe_time_zone: Option<&str>) -> String {
      let all_week = vec![
        Weekday::Monday,
        Weekday::Tuesday,
        Weekday::Wednesday,
        Weekday::Thursday,
        Weekday::Friday,
        Weekday::Saturday,
        Weekday::Sunday,
      ];
      serde_json::to_string(&DndSchedule {
        utc_offset_minutes: 60,
        maybe_time_zone: maybe_time_zone.map(str::to_owned),
        windows: vec![DndWindow {
          weekdays: all_week,
          start_minute: 22 * 60,
          end_minute: 7 * 60,
        }],
        allow_user_ids: vec![user(3)],
        allows_urgent: false,
      })
      .unwrap()
    }

    fn user(raw: i64) -> UserId {
      UserId::from_snowflake(Snowflake::new(raw).unwrap())
    }

    fn recipient(presence_pref: &str, maybe_dnd_schedule: Option<String>) -> PushRecipientRow {
      PushRecipientRow {
        user: user(1),
        notification_level: NotificationLevel::All,
        presence_pref: presence_pref.into(),
        maybe_dnd_until: None,
        maybe_muted_until: None,
        push_mode: "away_only".into(),
        maybe_dnd_schedule,
        is_following_thread: false,
        devices: Vec::new(),
      }
    }
  }
}
