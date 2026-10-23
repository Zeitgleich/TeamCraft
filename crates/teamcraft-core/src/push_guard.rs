//! What the device decides about a push on its own (notifications.md, "Mobile push delivery"),
//! from the local store only, so the iOS notification extension and the Android receiver decide
//! alike without a [`crate::Client`]:
//!
//! - **Duplicates**: a second wake-up for a message already shown is not shown again (urgent repeats
//!   are: they replace the earlier notification and alert again).
//! - **Second layer**: the server suppresses pushes for mutes and DND; the device checks the same
//!   settings again, so a change the server had not seen yet (or a push that was already on its way)
//!   stays silent.
//! - **Diagnostics**: when the last push arrived, what it was and how long it took.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ConversationId, ConversationResponse, DndCandidate, MessageId, NotificationLevel, NotificationSettings,
  PresenceState, PushKind, PushWakeUp, TestPushResponse,
};

use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The user's notification settings as last fetched (`GET /me/notification-settings`).
pub(crate) const NOTIFICATION_SETTINGS_RECORD: &str = "notification_settings";
const SHOWN_RECORD: &str = "push_shown";
const DIAGNOSTICS_RECORD: &str = "push_diagnostics";
/// Messages remembered as shown; older ones may be shown again, which only matters if the server
/// sends a wake-up for them a second time much later.
const MAX_REMEMBERED_SHOWN: usize = 500;

/// Why the device keeps a push silent although the server sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Suppression {
  /// The conversation is muted (level "nothing", or muted until later).
  Muted,
  /// Do not disturb is on and no override lets this message through.
  DoNotDisturb,
}

/// What the device saw of pushes, for the notification diagnostics screen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushDiagnostics {
  pub received_count: u64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_received_at_ms: Option<i64>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_kind: Option<PushKind>,
  /// From the server sending to the device opening it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_delay_ms: Option<i64>,
  /// When the last test push from the diagnostics arrived.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_test_at_ms: Option<i64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Shown {
  messages: VecDeque<(ConversationId, MessageId)>,
}

/// Notes that a wake-up arrived (for the diagnostics); never fails the push for it.
pub fn record_received(store: &impl LocalStore, wake_up: &PushWakeUp, now_ms: i64) {
  let result = update_diagnostics(store, |diagnostics| {
    diagnostics.received_count += 1;
    diagnostics.maybe_last_received_at_ms = Some(now_ms);
    diagnostics.maybe_last_kind = Some(wake_up.kind.clone());
    diagnostics.maybe_last_delay_ms = wake_up.maybe_sent_at.map(|sent| (now_ms - sent.0).max(0));
    if wake_up.kind == PushKind::Test {
      diagnostics.maybe_last_test_at_ms = Some(now_ms);
    }
  });
  if let Err(error) = result {
    tracing::debug!(%error, "could not record the push for the diagnostics");
  }
}

pub fn push_diagnostics(store: &impl LocalStore) -> Result<PushDiagnostics, CoreError> {
  Ok(records::get_json(store, Table::Account, DIAGNOSTICS_RECORD)?.unwrap_or_default())
}

/// Whether this message was shown before; if not, it is remembered as shown now. Urgent wake-ups
/// always count as new: a repeat is meant to alert again.
pub fn is_duplicate(store: &impl LocalStore, wake_up: &PushWakeUp) -> Result<bool, CoreError> {
  let Some(message) = wake_up.maybe_message_id else {
    return Ok(false);
  };
  if !matches!(wake_up.kind, PushKind::Message | PushKind::Mention) {
    return Ok(false);
  }
  let mut shown: Shown = records::get_json(store, Table::Account, SHOWN_RECORD)?.unwrap_or_default();
  let key = (wake_up.conversation_id, message);
  let was_shown = shown.messages.contains(&key);
  if was_shown && !wake_up.is_urgent {
    return Ok(true);
  }
  if !was_shown {
    shown.messages.push_back(key);
    while shown.messages.len() > MAX_REMEMBERED_SHOWN {
      shown.messages.pop_front();
    }
    records::put_json(store, Table::Account, SHOWN_RECORD, &shown)?;
  }
  Ok(false)
}

/// The device's own check of mutes and DND (the server checked first). `utc_offset_minutes` is the
/// device's current offset, which the schedule's windows are evaluated in.
pub fn local_suppression(
  store: &impl LocalStore,
  wake_up: &PushWakeUp,
  now_ms: i64,
  utc_offset_minutes: i16,
) -> Result<Option<Suppression>, CoreError> {
  if !matches!(
    wake_up.kind,
    PushKind::Message | PushKind::Mention | PushKind::Call
  ) {
    return Ok(None);
  }
  let maybe_conversation: Option<ConversationResponse> =
    records::get_json(store, Table::Conversations, &wake_up.conversation_id.to_string())?;
  if maybe_conversation
    .as_ref()
    .and_then(|conversation| conversation.maybe_my_membership.as_ref())
    .is_some_and(|membership| {
      membership.notification_level == NotificationLevel::None
        || membership.maybe_muted_until.is_some_and(|until| until.0 > now_ms)
    })
  {
    return Ok(Some(Suppression::Muted));
  }
  let maybe_settings: Option<NotificationSettings> =
    records::get_json(store, Table::Account, NOTIFICATION_SETTINGS_RECORD)?;
  let maybe_schedule = maybe_settings.and_then(|settings| settings.maybe_dnd_schedule);
  // DMs and group DMs have participants; channels don't.
  let is_direct = maybe_conversation
    .as_ref()
    .is_some_and(|conversation| !conversation.participants.is_empty());
  let lets_through = match (&maybe_schedule, wake_up.maybe_sender_id) {
    (Some(schedule), Some(sender)) => schedule.lets_through(DndCandidate {
      sender,
      is_urgent: wake_up.is_urgent,
      is_direct,
    }),
    _ => false,
  };
  if lets_through {
    return Ok(None);
  }
  let is_manual_dnd = match records::load_account(store)? {
    Some(account) => {
      records::get_json::<PresenceState>(store, Table::Presence, &account.user_id.to_string())?
        == Some(PresenceState::DoNotDisturb)
    },
    None => false,
  };
  let is_scheduled_dnd =
    maybe_schedule.is_some_and(|schedule| schedule.is_active_at(now_ms.div_euclid(1000), utc_offset_minutes));
  Ok((is_manual_dnd || is_scheduled_dnd).then_some(Suppression::DoNotDisturb))
}

/// Asks the server for a test wake-up to this device (notification diagnostics). Works without a
/// running client, like the rest of this module.
pub async fn send_test_push<A: ServerApi>(
  store: &impl LocalStore,
  api: &A,
) -> Result<TestPushResponse, CoreError> {
  let account = records::load_account(store)?.ok_or(CoreError::NotEnrolled)?;
  Ok(api.send_test_push(account.device_id).await?)
}

/// Remembers the settings the second layer checks.
pub fn save_notification_settings(
  store: &impl LocalStore,
  settings: &NotificationSettings,
) -> Result<(), CoreError> {
  records::put_json(store, Table::Account, NOTIFICATION_SETTINGS_RECORD, settings)
}

pub fn notification_settings(store: &impl LocalStore) -> Result<Option<NotificationSettings>, CoreError> {
  records::get_json(store, Table::Account, NOTIFICATION_SETTINGS_RECORD)
}

fn update_diagnostics(
  store: &impl LocalStore,
  change: impl FnOnce(&mut PushDiagnostics),
) -> Result<(), CoreError> {
  let mut diagnostics = push_diagnostics(store)?;
  change(&mut diagnostics);
  records::put_json(store, Table::Account, DIAGNOSTICS_RECORD, &diagnostics)
}
