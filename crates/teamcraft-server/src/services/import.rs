//! The import account's endpoints (admin.md §3.8, e2ee.md §4.10, issue #402).
//!
//! History from another chat system is encrypted on the importing client; the server only numbers
//! imported messages below a conversation's live ones and keeps them quiet. Every endpoint is open
//! to the one account the signed workspace policy names as the importer, while its import period
//! lasts; that account also passes the rate limits and the per-user upload quotas.

use std::collections::HashSet;

use serde_json::json;
use teamcraft_db::{ImportOutcome, MembershipChanges, NewConversation, NewImportedMessage, participants_key};
use teamcraft_proto::{
  ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, GatewayEvent,
  HistoryPolicy, ImportDmRequest, ImportMemberStateRequest, ImportMessagesRequest, ImportMessagesResponse,
  ImportedMessageId, MAX_IMPORT_BATCH, MAX_PROVISION_BATCH, NotificationLevel, ProvisionUsersRequest,
  ProvisionUsersResponse, UserId,
};
use time::OffsetDateTime;

use super::access::load_conversation;
use super::audit::{self, Target as AuditTarget};
use super::conversations::get_conversation;
use super::oidc;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_CONTENT_KEY_ID_BYTES: usize = 64;

/// Whether `user` is the policy's importer and its import period lasts.
pub fn is_importing(state: &AppState, user: UserId) -> bool {
  state.policy.get().import.allows(user, now_ms())
}

pub async fn provision_users(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: ProvisionUsersRequest,
) -> Result<ProvisionUsersResponse, ApiError> {
  require_importer(state, caller)?;
  if request.usernames.len() > MAX_PROVISION_BATCH {
    return Err(ApiError::InvalidRequest(format!(
      "at most {MAX_PROVISION_BATCH} user names at once"
    )));
  }
  let users = oidc::provision_users(state, &request.usernames).await?;
  let created = users
    .iter()
    .filter(|user| user.status == teamcraft_proto::ProvisionStatus::Created)
    .count();
  audit::record(
    state,
    Some(caller.user.id),
    "import_users_provisioned",
    None,
    &json!({"requested": request.usernames.len(), "created": created}),
  )
  .await?;
  Ok(ProvisionUsersResponse { users })
}

/// The participants' DM (or one user's notes), made if missing, with the importer as a member
/// that is no participant.
pub async fn import_dm(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: ImportDmRequest,
) -> Result<ConversationResponse, ApiError> {
  require_importer(state, caller)?;
  let participants: Vec<UserId> = request
    .participants
    .iter()
    .copied()
    .collect::<HashSet<_>>()
    .into_iter()
    .collect();
  if participants.is_empty() || participants.len() > 2 || participants.contains(&caller.user.id) {
    return Err(ApiError::InvalidRequest(
      "one or two participants, not the import account".into(),
    ));
  }
  let users = state.database.get_users(&participants).await?;
  if users.len() != participants.len() || users.iter().any(|user| user.is_deactivated) {
    return Err(ApiError::user_not_found());
  }
  let (id, is_new) = state
    .database
    .get_or_create_participants_conversation(
      &NewConversation {
        id: state.next_id()?,
        maybe_space: None,
        kind: ConversationKind::Dm,
        maybe_name: None,
        maybe_topic: None,
        maybe_description: None,
        visibility: ConversationVisibility::Private,
        history_policy: HistoryPolicy::FromJoin,
        maybe_category: None,
        maybe_participants_key: Some(participants_key(&participants)),
        is_integration: false,
      },
      &participants,
    )
    .await?;
  state.database.add_importer(id, caller.user.id).await?;
  let mut told = participants.clone();
  told.push(caller.user.id);
  state
    .hub
    .publish(told, &GatewayEvent::ConversationUpdate { conversation_id: id });
  audit::record(
    state,
    Some(caller.user.id),
    "import_dm_opened",
    Some(AuditTarget::new("conversation", id)),
    &json!({"is_new": is_new, "participants": participants.len()}),
  )
  .await?;
  get_conversation(state, caller, id).await
}

/// Stores a batch of imported messages: no push, no `message_create`, no unread.
pub async fn import_messages(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: ImportMessagesRequest,
) -> Result<ImportMessagesResponse, ApiError> {
  require_importer(state, caller)?;
  let access = load_conversation(state, caller, conversation).await?;
  if access.view.maybe_membership.is_none() {
    return Err(ApiError::Forbidden("the import account is not a member".into()));
  }
  if request.messages.is_empty() || request.messages.len() > MAX_IMPORT_BATCH {
    return Err(ApiError::InvalidRequest(format!(
      "1 to {MAX_IMPORT_BATCH} messages at once"
    )));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  let max_bytes = state.settings.get().limits.max_ciphertext_bytes;
  for message in &request.messages {
    if message.ciphertext.is_empty() {
      return Err(ApiError::InvalidRequest("ciphertext must not be empty".into()));
    }
    if message.ciphertext.len() > max_bytes {
      return Err(ApiError::PayloadTooLarge(format!(
        "ciphertext exceeds {max_bytes} bytes"
      )));
    }
    if message.content_key_id.is_empty() || message.content_key_id.len() > MAX_CONTENT_KEY_ID_BYTES {
      return Err(ApiError::InvalidRequest(
        "content_key_id has an invalid size".into(),
      ));
    }
  }
  let new: Vec<NewImportedMessage<'_>> = request
    .messages
    .iter()
    .map(|message| NewImportedMessage {
      client_message_id: message.client_message_id,
      maybe_thread_id: message.maybe_thread_id,
      sent_at_ms: message.sent_at_ms,
      content_key_id: message.content_key_id.as_slice(),
      ciphertext: message.ciphertext.as_slice(),
    })
    .collect();
  match state
    .database
    .store_imported_messages(conversation, caller.user.id, request.device_id, &new)
    .await?
  {
    ImportOutcome::Stored(ids) => Ok(ImportMessagesResponse {
      messages: ids
        .into_iter()
        .map(|(client_message_id, id)| ImportedMessageId {
          client_message_id,
          id,
        })
        .collect(),
    }),
    ImportOutcome::WindowFull => Err(ApiError::ImportWindowFull),
    ImportOutcome::UnknownThread(_) => Err(ApiError::InvalidRequest(
      "thread root is not a message of this conversation".into(),
    )),
    ImportOutcome::ConversationGone => Err(ApiError::conversation_not_found()),
  }
}

/// A member's read position, favourite and mute from the old system.
pub async fn set_member_state(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  user: UserId,
  request: ImportMemberStateRequest,
) -> Result<(), ApiError> {
  require_importer(state, caller)?;
  load_conversation(state, caller, conversation).await?;
  if !state
    .database
    .conversation_member_ids(conversation)
    .await?
    .contains(&user)
  {
    return Err(ApiError::user_not_found());
  }
  if let Some(message) = request.maybe_read_through {
    if state.database.get_message(conversation, message).await?.is_none() {
      return Err(ApiError::InvalidRequest("unknown message".into()));
    }
    state
      .database
      .set_read_position(conversation, user, message)
      .await?;
  }
  if request.is_favorite || request.is_muted {
    state
      .database
      .update_membership(
        conversation,
        user,
        &MembershipChanges {
          maybe_is_favorite: request.is_favorite.then_some(true),
          maybe_notification_level: request.is_muted.then_some(NotificationLevel::None),
          ..MembershipChanges::default()
        },
      )
      .await?;
  }
  state.hub.publish(
    [user],
    &GatewayEvent::ConversationUpdate {
      conversation_id: conversation,
    },
  );
  Ok(())
}

/// Marks the conversation's import as finished and tells its members, whose clients load the
/// history's start again.
pub async fn finish(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<(), ApiError> {
  require_importer(state, caller)?;
  load_conversation(state, caller, conversation).await?;
  let maybe_through = state.database.finish_import(conversation).await?;
  state.hub.publish(
    state.database.conversation_member_ids(conversation).await?,
    &GatewayEvent::ConversationUpdate {
      conversation_id: conversation,
    },
  );
  audit::record(
    state,
    Some(caller.user.id),
    "import_finished",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({"imported_through": maybe_through.map(|id| id.to_string())}),
  )
  .await?;
  Ok(())
}

fn require_importer(state: &AppState, caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if is_importing(state, caller.user.id) {
    Ok(())
  } else {
    Err(ApiError::ImportNotAllowed)
  }
}

fn now_ms() -> i64 {
  i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX)
}
