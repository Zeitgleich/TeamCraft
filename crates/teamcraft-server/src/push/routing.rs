//! Who gets a push for a new message (e2ee.md §7): the server decides from metadata it has anyway.

use teamcraft_db::{PushDeviceRow, PushRecipientRow};
use teamcraft_proto::{
  ConversationId, ConversationKind, MessageId, NotificationLevel, PushKind, PushWakeUp, UserId,
};
use time::OffsetDateTime;
use tracing::{debug, info, warn};

use super::sender::{PushError, PushTarget, Urgency};
use crate::gateway::DeviceAggregate;
use crate::state::AppState;

/// A message that was just stored.
#[derive(Debug, Clone)]
pub struct NewMessageEvent {
  pub conversation: ConversationId,
  pub kind: ConversationKind,
  pub message: MessageId,
  pub sender: UserId,
  pub mentioned: Vec<UserId>,
  pub is_urgent: bool,
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
}

/// What the routing knows about one recipient.
#[derive(Debug, Clone, Copy)]
pub struct RecipientState {
  pub level_is_all: bool,
  pub level_is_none: bool,
  pub is_mentioned: bool,
  /// DMs and group DMs notify like a mention.
  pub is_direct: bool,
  pub is_dnd: bool,
  pub devices: DeviceAggregate,
}

/// The routing rules:
///
/// 1. Muted (`none`) never notifies. Urgent messages don't override a mute.
/// 2. Do-not-disturb suppresses all pushes (mentions are collected and shown afterwards).
/// 3. No push while a device of the user is active: they see the message live.
/// 4. In `mentions` channels only mentions and direct messages push; in `all` channels everything.
pub fn decide(state: RecipientState) -> Result<PushKind, Skip> {
  if state.level_is_none {
    return Err(Skip::Muted);
  }
  if state.is_dnd {
    return Err(Skip::DoNotDisturb);
  }
  if state.devices == DeviceAggregate::Active {
    return Err(Skip::Active);
  }
  if state.is_mentioned || state.is_direct {
    return Ok(PushKind::Mention);
  }
  if state.level_is_all {
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
    .list_push_recipients(event.conversation, event.sender)
    .await?;
  for recipient in recipients {
    let verdict = decide(RecipientState {
      level_is_all: recipient.notification_level == NotificationLevel::All,
      level_is_none: recipient.notification_level == NotificationLevel::None,
      is_mentioned: event.mentioned.contains(&recipient.user),
      is_direct: matches!(event.kind, ConversationKind::Dm | ConversationKind::Group),
      is_dnd: is_dnd(&recipient, now),
      devices: state.hub.device_aggregate(recipient.user),
    });
    let kind = match verdict {
      Ok(kind) => kind,
      Err(skip) => {
        debug!(user_id = %recipient.user, ?skip, "no push");
        continue;
      },
    };
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
      spawn_delivery(state.clone(), recipient.user, device.clone(), wake_up);
    }
  }
  Ok(())
}

fn is_dnd(recipient: &PushRecipientRow, now: OffsetDateTime) -> bool {
  recipient.presence_pref == "dnd" && recipient.maybe_dnd_until.is_none_or(|until| until > now)
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
      Err(PushError::NotConfigured | PushError::NotImplemented(_)) => {
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
      is_dnd: false,
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

  #[test]
  fn timed_do_not_disturb_ends() {
    let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
    let recipient = |until: Option<i64>| PushRecipientRow {
      user: UserId::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap()),
      notification_level: NotificationLevel::All,
      presence_pref: "dnd".into(),
      maybe_dnd_until: until.map(|s| now + time::Duration::seconds(s)),
      devices: Vec::new(),
    };
    assert!(is_dnd(&recipient(None), now));
    assert!(is_dnd(&recipient(Some(60)), now));
    assert!(!is_dnd(&recipient(Some(-60)), now));
    let auto = PushRecipientRow {
      presence_pref: "auto".into(),
      ..recipient(None)
    };
    assert!(!is_dnd(&auto, now));
  }
}
