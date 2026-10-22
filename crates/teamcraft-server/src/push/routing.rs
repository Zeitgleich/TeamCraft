//! Who gets a push for a new message (e2ee.md §7): the server decides from metadata it has anyway.

use std::time::Duration;

use teamcraft_db::{DueReminder, PushDeviceRow, PushRecipientRow};
use teamcraft_proto::{
  ConversationId, ConversationKind, DndSchedule, MessageId, NotificationLevel, PushKind, PushMode,
  PushProvider, PushWakeUp, ThreadId, UserId,
};
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use super::sender::{PushError, PushTarget, Urgency};
use crate::gateway::{ActiveDevices, DeviceAggregate};
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
  /// A device of the user is active: they see the message there.
  Active,
  /// The channel only notifies on mentions and this one doesn't mention the user.
  NotMentioned,
  /// Integration channels push only for mentions.
  IntegrationChannel,
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
  pub is_dnd: bool,
  /// The user follows the thread the message replies in.
  pub is_followed_thread: bool,
  /// Push even while a device of the user is active.
  pub always_pushes: bool,
  pub devices: DeviceAggregate,
}

/// The routing rules:
///
/// 1. Muted (level `none`, or muted until a later time) never notifies. Urgent messages don't
///    override a mute.
/// 2. Do-not-disturb (manual, or the schedule unless the sender is allowed) suppresses all pushes
///    (mentions are collected and shown afterwards).
/// 3. No push while a device of the user is active: they see the message live (unless the user
///    chose to be pushed always).
/// 4. Mentions, direct messages and replies in followed threads push; otherwise only `all` channels do.
pub fn decide(state: RecipientState) -> Result<PushKind, Skip> {
  if state.level_is_none {
    return Err(Skip::Muted);
  }
  if state.is_dnd {
    return Err(Skip::DoNotDisturb);
  }
  if state.devices == DeviceAggregate::Active && !state.always_pushes {
    return Err(Skip::Active);
  }
  if state.is_mentioned || state.is_direct {
    return Ok(PushKind::Mention);
  }
  if state.is_integration {
    return Err(Skip::IntegrationChannel);
  }
  if state.level_is_all || state.is_followed_thread {
    Ok(PushKind::Message)
  } else {
    Err(Skip::NotMentioned)
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
    push_to_recipient(state, event, &recipient, now);
  }
  Ok(())
}

/// Pushes an urgent direct message again to one recipient who has not acknowledged it (the
/// scheduler of `services::urgent`). The routing rules apply as for the first push.
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
    .iter()
    .filter(|recipient| recipient.user == reminder.user)
  {
    push_to_recipient(state, &event, recipient, now);
  }
  Ok(())
}

fn push_to_recipient(
  state: &AppState,
  event: &NewMessageEvent,
  recipient: &PushRecipientRow,
  now: OffsetDateTime,
) {
  let unix_seconds = now.unix_timestamp();
  let verdict = decide(RecipientState {
    level_is_all: recipient.notification_level == NotificationLevel::All,
    level_is_none: recipient.notification_level == NotificationLevel::None
      || notifications::is_muted(recipient.maybe_muted_until, now),
    is_mentioned: event.mentioned.contains(&recipient.user),
    is_direct: matches!(event.kind, ConversationKind::Dm | ConversationKind::Group),
    is_integration: event.is_integration,
    is_dnd: is_dnd(recipient, now, unix_seconds, event.sender),
    is_followed_thread: recipient.is_following_thread,
    always_pushes: PushMode::from(recipient.push_mode.clone()) == PushMode::Always,
    devices: blocking_activity(state, recipient),
  });
  let kind = match verdict {
    Ok(kind) => kind,
    Err(skip) => {
      debug!(user_id = %recipient.user, ?skip, "no push");
      return;
    },
  };
  let delay = mobile_delay(state, recipient);
  for device in &recipient.devices {
    // A device with a live connection already got the event.
    if state.hub.is_device_connected(recipient.user, device.device) {
      continue;
    }
    let wake_up = PushWakeUp {
      kind: kind.clone(),
      conversation_id: event.conversation,
      maybe_message_id: Some(event.message),
      maybe_sender_id: Some(event.sender),
      is_urgent: event.is_urgent,
    };
    match delay {
      Some(delay) if is_mobile(&device.provider) => {
        spawn_delayed_delivery(state.clone(), recipient.user, device.clone(), wake_up, delay);
      },
      _ => spawn_delivery(state.clone(), recipient.user, device.clone(), wake_up),
    }
  }
}

/// A push to a phone is held back while the user works at a desktop (see
/// `notifications.mobile_push_delay_secs`).
fn mobile_delay(state: &AppState, recipient: &PushRecipientRow) -> Option<Duration> {
  let seconds = state.settings.notifications.mobile_push_delay_secs;
  if seconds == 0 || PushMode::from(recipient.push_mode.clone()) == PushMode::Always {
    return None;
  }
  let active = active_devices(state, recipient);
  (active.is_desktop_active && !active.is_mobile_active).then(|| Duration::from_secs(seconds))
}

/// The user is active on a phone: they see messages there, whatever the desktop does.
fn blocking_activity(state: &AppState, recipient: &PushRecipientRow) -> DeviceAggregate {
  let active = active_devices(state, recipient);
  if active.is_mobile_active {
    return DeviceAggregate::Active;
  }
  // Without a delay, a desktop in use keeps pushes away as before.
  if active.is_desktop_active && state.settings.notifications.mobile_push_delay_secs == 0 {
    return DeviceAggregate::Active;
  }
  if active.is_desktop_active {
    DeviceAggregate::Idle
  } else {
    state.hub.device_aggregate(recipient.user)
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

fn is_dnd(recipient: &PushRecipientRow, now: OffsetDateTime, unix_seconds: i64, sender: UserId) -> bool {
  let is_manual =
    recipient.presence_pref == "dnd" && recipient.maybe_dnd_until.is_none_or(|until| until > now);
  is_manual || is_scheduled_dnd(recipient, unix_seconds, sender)
}

fn is_scheduled_dnd(recipient: &PushRecipientRow, unix_seconds: i64, sender: UserId) -> bool {
  let Some(raw) = &recipient.maybe_dnd_schedule else {
    return false;
  };
  match serde_json::from_str::<DndSchedule>(raw) {
    Ok(schedule) => schedule.silences(unix_seconds, sender),
    Err(error) => {
      // Validated on write, so this means a stored value from an incompatible version: notify.
      warn!(%error, user_id = %recipient.user, "ignoring an unreadable DND schedule");
      false
    },
  }
}

/// Waits, then pushes if the message is still unread and no phone of the user shows it by now.
fn spawn_delayed_delivery(
  state: AppState,
  user: UserId,
  device: PushDeviceRow,
  wake_up: PushWakeUp,
  delay: Duration,
) {
  tokio::spawn(async move {
    tokio::time::sleep(delay).await;
    if let (Some(conversation), Some(message)) = (Some(wake_up.conversation_id), wake_up.maybe_message_id) {
      match state.database.has_read_message(user, conversation, message).await {
        Ok(true) => {
          debug!(user_id = %user, "the message was read meanwhile; no push to the phone");
          return;
        },
        Ok(false) => {},
        Err(error) => warn!(%error, "checking the read position failed; pushing anyway"),
      }
    }
    if state.hub.is_device_connected(user, device.device) {
      return;
    }
    spawn_delivery(state, user, device, wake_up);
  });
}

fn spawn_delivery(state: AppState, user: UserId, device: PushDeviceRow, wake_up: PushWakeUp) {
  tokio::spawn(async move {
    let target = PushTarget {
      user,
      device: device.device,
      provider: device.provider,
      token: device.token,
      push_key: device.push_key,
    };
    let urgency = if wake_up.is_urgent {
      Urgency::High
    } else {
      Urgency::Normal
    };
    match state
      .push
      .deliver(&target, &wake_up, urgency, state.settings.push.ttl)
      .await
    {
      Ok(()) => debug!(device_id = %target.device, "push sent"),
      Err(PushError::Gone) => {
        info!(device_id = %target.device, "push registration is gone, forgetting it");
        if let Err(error) = state.database.clear_push_registration(user, target.device).await {
          warn!(%error, "clearing the push registration failed");
        }
      },
      Err(PushError::NotConfigured) => {
        debug!(provider = %target.provider, "push provider unavailable");
      },
      Err(error) => warn!(%error, device_id = %target.device, "push failed"),
    }
  });
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Id;

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
      devices: DeviceAggregate::NoDevice,
    }
  }

  #[test]
  fn channels_that_notify_on_mentions_only_push_for_mentions() {
    assert_eq!(decide(base()), Err(Skip::NotMentioned));
    assert_eq!(
      decide(RecipientState {
        is_mentioned: true,
        ..base()
      }),
      Ok(PushKind::Mention)
    );
  }

  #[test]
  fn channels_that_notify_on_everything_push_every_message() {
    assert_eq!(
      decide(RecipientState {
        level_is_all: true,
        ..base()
      }),
      Ok(PushKind::Message)
    );
    assert_eq!(
      decide(RecipientState {
        level_is_all: true,
        is_mentioned: true,
        ..base()
      }),
      Ok(PushKind::Mention)
    );
  }

  #[test]
  fn direct_messages_notify_like_mentions() {
    assert_eq!(
      decide(RecipientState {
        is_direct: true,
        ..base()
      }),
      Ok(PushKind::Mention)
    );
  }

  #[test]
  fn replies_in_followed_threads_push_even_in_mention_only_channels() {
    assert_eq!(
      decide(RecipientState {
        is_followed_thread: true,
        ..base()
      }),
      Ok(PushKind::Message)
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
  fn users_who_chose_always_are_pushed_while_a_device_is_active() {
    assert_eq!(
      decide(RecipientState {
        is_mentioned: true,
        always_pushes: true,
        devices: DeviceAggregate::Active,
        ..base()
      }),
      Ok(PushKind::Mention)
    );
  }

  #[test]
  fn a_mute_beats_everything_else() {
    let muted = RecipientState {
      level_is_none: true,
      is_mentioned: true,
      is_direct: true,
      level_is_all: true,
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
        ..base()
      }),
      Err(Skip::DoNotDisturb)
    );
  }

  #[test]
  fn an_active_device_suppresses_the_push_but_an_idle_one_does_not() {
    let mentioned = RecipientState {
      is_mentioned: true,
      ..base()
    };
    assert_eq!(
      decide(RecipientState {
        devices: DeviceAggregate::Active,
        ..mentioned
      }),
      Err(Skip::Active)
    );
    assert_eq!(
      decide(RecipientState {
        devices: DeviceAggregate::Idle,
        ..mentioned
      }),
      Ok(PushKind::Mention)
    );
  }

  mod do_not_disturb {
    use teamcraft_proto::{DndWindow, Snowflake, Weekday};

    use super::*;

    // 2026-01-05 (Monday) 23:00 UTC.
    const MONDAY_NIGHT: i64 = 1_767_571_200 + 23 * 3600;

    #[test]
    fn timed_do_not_disturb_ends() {
      let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
      let at = |until: Option<i64>| PushRecipientRow {
        maybe_dnd_until: until.map(|s| now + time::Duration::seconds(s)),
        ..recipient("dnd", None)
      };
      let sender = user(2);
      assert!(is_dnd(&at(None), now, 0, sender));
      assert!(is_dnd(&at(Some(60)), now, 0, sender));
      assert!(!is_dnd(&at(Some(-60)), now, 0, sender));
      assert!(!is_dnd(&recipient("auto", None), now, 0, sender));
    }

    #[test]
    fn the_schedule_silences_everybody_but_allowed_senders() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT).unwrap();
      let schedule = DndSchedule {
        utc_offset_minutes: 0,
        windows: vec![DndWindow {
          weekdays: vec![Weekday::Monday],
          start_minute: 22 * 60,
          end_minute: 7 * 60,
        }],
        allow_user_ids: vec![user(3)],
      };
      let scheduled = recipient("auto", Some(serde_json::to_string(&schedule).unwrap()));
      assert!(is_dnd(&scheduled, now, MONDAY_NIGHT, user(2)));
      assert!(!is_dnd(&scheduled, now, MONDAY_NIGHT, user(3)));
      assert!(!is_dnd(&scheduled, now, MONDAY_NIGHT - 5 * 3600, user(2)));
    }

    #[test]
    fn an_unreadable_schedule_does_not_silence() {
      let now = OffsetDateTime::from_unix_timestamp(MONDAY_NIGHT).unwrap();
      let broken = recipient("auto", Some("{not json".into()));
      assert!(!is_dnd(&broken, now, MONDAY_NIGHT, user(2)));
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
