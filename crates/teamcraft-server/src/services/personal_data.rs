//! The caller's own records, for their data export (issue #211, docs/data-export.md): one answer
//! with everything the server holds about them that it can read. It reads through the same services
//! the single endpoints use, so it shows exactly what they show the caller and nothing of anybody
//! else. Message contents and files are ciphertext here; the caller's client exports those.

use teamcraft_proto::{
  ListConversationsQuery, PERSONAL_DATA_VERSION, PersonalAccount, PersonalDataResponse, PersonalKeyBag,
  PersonalScheduledMessage,
};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::convert::{presented_name, unix_millis};
use crate::services::{
  conversations, devices, email_digest, holiday_regions, key_bag, notifications, presence, profiles,
  scheduled_messages, spaces,
};
use crate::state::AppState;

/// Everything of the caller's own; a section their role may not read is left out, never refused
/// as a whole.
pub async fn personal_data(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<PersonalDataResponse, ApiError> {
  let user = &caller.user;
  let account = PersonalAccount {
    id: user.id,
    maybe_username: user.maybe_username.clone(),
    display_name: presented_name(&state.policy.get().directory, user),
    kind: user.kind.clone(),
    role: caller.role.clone(),
    maybe_organization: user.maybe_organization.clone(),
    created_at: unix_millis(user.created_at),
    is_deactivated: user.is_deactivated,
  };
  let maybe_profile = section(profiles::get_my_profile(state, caller).await)?;
  let maybe_privacy = section(presence::get_my_privacy(state, caller).await)?;
  let maybe_notification_settings = section(notifications::get_settings(state, caller).await)?;
  let maybe_email_digest = section(email_digest::get_settings(state, caller).await)?;
  let maybe_holiday_region = section(holiday_regions::get_holiday_region(state, caller).await)?;
  let devices = section(devices::list_devices(state, caller).await)?
    .map(|listed| listed.devices)
    .unwrap_or_default();
  let spaces = section(spaces::list_spaces(state, caller).await)?
    .map(|listed| listed.spaces)
    .unwrap_or_default();
  let conversations =
    section(conversations::list_conversations(state, caller, ListConversationsQuery::default()).await)?
      .map(|listed| listed.conversations)
      .unwrap_or_default();
  let scheduled_messages = section(scheduled_messages::list_scheduled_messages(state, caller).await)?
    .map(|listed| {
      listed
        .messages
        .into_iter()
        .map(|message| PersonalScheduledMessage {
          id: message.id,
          conversation_id: message.conversation_id,
          due_at: message.due_at,
          size_bytes: message.ciphertext.len() as u64,
          maybe_hold: message.maybe_hold,
        })
        .collect()
    })
    .unwrap_or_default();
  let maybe_key_bag = section(key_bag::get_key_bag(state, caller).await)?.map(|bag| PersonalKeyBag {
    version: bag.version,
    size_bytes: (bag.encrypted_key_bag.len() + bag.wrapped_backup_keys.len()) as u64,
    updated_at: bag.updated_at,
  });
  tracing::info!(user_id = %user.id, "personal data read for an export");
  Ok(PersonalDataResponse {
    version: PERSONAL_DATA_VERSION,
    generated_at: unix_millis(time::OffsetDateTime::now_utc()),
    account,
    maybe_profile,
    maybe_privacy,
    maybe_notification_settings,
    maybe_email_digest,
    maybe_holiday_region,
    devices,
    spaces,
    conversations,
    scheduled_messages,
    maybe_key_bag,
  })
}

/// One section: what the role may not read, or what doesn't exist (no key bag yet), is left out;
/// anything else fails the answer, so an export is never quietly short of what the server holds.
fn section<T>(result: Result<T, ApiError>) -> Result<Option<T>, ApiError> {
  match result {
    Ok(value) => Ok(Some(value)),
    Err(ApiError::Forbidden(_) | ApiError::NotFound { .. }) => Ok(None),
    Err(error) => Err(error),
  }
}
