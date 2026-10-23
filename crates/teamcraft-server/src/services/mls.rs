//! MLS delivery service (e2ee.md §4): an ordered queue per conversation. The server never looks
//! inside the payloads; it orders entries, enforces "first commit per epoch wins" and fans them out.

use std::collections::HashSet;

use teamcraft_db::{CommitOutcome, MlsBacklogQuery, NewMlsMessage, PostOutcome, StoredMlsMessage};
use teamcraft_proto::{
  Base64Bytes, ConversationId, DeviceId, ErrorCode, GatewayEvent, ListMlsMessagesQuery,
  ListMlsMessagesResponse, MlsGroupInfoResponse, MlsMessageId, MlsMessageKind, MlsMessageResponse,
  Permission, PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PutMlsGroupInfoRequest,
  UserId,
};

use super::access::{load_conversation, require};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn post_commit(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: PostMlsCommitRequest,
) -> Result<PostMlsCommitResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can commit".into()));
  }
  ensure_own_device(state, caller, request.device_id).await?;
  check_payload(state, &request.commit)?;
  let limits = &state.settings.get().limits;
  if request.welcomes.len() > limits.max_welcomes_per_commit {
    return Err(ApiError::InvalidRequest("too many welcomes".into()));
  }
  for welcome in &request.welcomes {
    check_payload(state, &welcome.welcome)?;
  }
  if let Some(group_info) = &request.maybe_group_info {
    check_payload(state, group_info)?;
  }

  // Welcomes go to devices of members only, one per device.
  let members: HashSet<UserId> = state
    .database
    .conversation_member_ids(conversation)
    .await?
    .into_iter()
    .collect();
  let recipients: Vec<DeviceId> = request.welcomes.iter().map(|w| w.device_id).collect();
  if recipients.iter().collect::<HashSet<_>>().len() != recipients.len() {
    return Err(ApiError::InvalidRequest("duplicate welcome recipient".into()));
  }
  let owners = state.database.device_owners(&recipients).await?;
  if owners.len() != recipients.len() || owners.iter().any(|(_, owner)| !members.contains(owner)) {
    return Err(ApiError::InvalidRequest(
      "welcomes must address devices of members".into(),
    ));
  }

  let commit_id: MlsMessageId = state.next_id()?;
  let welcome_ids: Vec<MlsMessageId> = request
    .welcomes
    .iter()
    .map(|_| state.next_id())
    .collect::<Result<_, _>>()?;
  let welcomes: Vec<NewMlsMessage<'_>> = request
    .welcomes
    .iter()
    .zip(&welcome_ids)
    .map(|(welcome, id)| NewMlsMessage {
      id: *id,
      kind: MlsMessageKind::Welcome,
      maybe_recipient: Some(welcome.device_id),
      payload: welcome.welcome.as_slice(),
    })
    .collect();
  let commit = NewMlsMessage {
    id: commit_id,
    kind: MlsMessageKind::Commit,
    maybe_recipient: None,
    payload: request.commit.as_slice(),
  };

  let outcome = state
    .database
    .post_mls_commit(
      conversation,
      request.epoch,
      request.device_id,
      &commit,
      &welcomes,
      request.maybe_group_info.as_ref().map(Base64Bytes::as_slice),
    )
    .await?;
  let new_epoch = match outcome {
    CommitOutcome::Accepted { new_epoch } => new_epoch,
    CommitOutcome::EpochConflict { current_epoch } => return Err(ApiError::EpochConflict { current_epoch }),
  };

  let commit_response = response(conversation, request.epoch, request.device_id, &commit);
  state.hub.publish(
    members,
    &GatewayEvent::MlsMessage {
      message: commit_response.clone(),
    },
  );
  for (welcome, new) in request.welcomes.iter().zip(&welcomes) {
    let message = response(conversation, new_epoch, request.device_id, new);
    if let Some((_, owner)) = owners.iter().find(|(device, _)| *device == welcome.device_id) {
      state.hub.publish([*owner], &GatewayEvent::MlsMessage { message });
    }
  }
  Ok(PostMlsCommitResponse {
    new_epoch,
    commit: commit_response,
  })
}

/// Stores a proposal or application message for the current epoch.
///
/// Members may send both. Proposals can also come from non-members who may manage the channel
/// (external proposals of admins, admin.md §2.2); member clients verify the signed action inside.
pub async fn post_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: PostMlsMessageRequest,
) -> Result<MlsMessageResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  match request.kind {
    MlsMessageKind::Application if !access.is_member() => {
      return Err(ApiError::Forbidden("only members can send group messages".into()));
    },
    MlsMessageKind::Proposal if !access.is_member() => {
      require(state, caller, Permission::ManageMembers, &access.target())?;
    },
    MlsMessageKind::Application | MlsMessageKind::Proposal => {},
    _ => {
      return Err(ApiError::InvalidRequest(
        "kind must be proposal or application".into(),
      ));
    },
  }
  ensure_own_device(state, caller, request.device_id).await?;
  check_payload(state, &request.payload)?;

  let message = NewMlsMessage {
    id: state.next_id()?,
    kind: request.kind.clone(),
    maybe_recipient: None,
    payload: request.payload.as_slice(),
  };
  match state
    .database
    .post_mls_message(conversation, request.epoch, request.device_id, &message)
    .await?
  {
    PostOutcome::Accepted => {},
    PostOutcome::EpochConflict { current_epoch } => return Err(ApiError::EpochConflict { current_epoch }),
  }
  let stored = response(conversation, request.epoch, request.device_id, &message);
  let members = state.database.conversation_member_ids(conversation).await?;
  state.hub.publish(
    members,
    &GatewayEvent::MlsMessage {
      message: stored.clone(),
    },
  );
  Ok(stored)
}

/// The group information for the current epoch, for a member's new device to join by external
/// commit. Members only; the server never looks inside.
pub async fn group_info(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<MlsGroupInfoResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden(
      "only members can read the group information".into(),
    ));
  }
  match state.database.current_mls_group_info(conversation).await? {
    Some((epoch, group_info)) => Ok(MlsGroupInfoResponse {
      epoch,
      group_info: Base64Bytes::new(group_info),
    }),
    None => Err(ApiError::NotFound {
      code: ErrorCode::MlsGroupInfoNotFound,
      what: "MLS group information",
    }),
  }
}

/// A member publishes the group information for the current epoch.
pub async fn put_group_info(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: PutMlsGroupInfoRequest,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden(
      "only members can publish the group information".into(),
    ));
  }
  ensure_own_device(state, caller, request.device_id).await?;
  check_payload(state, &request.group_info)?;
  match state
    .database
    .put_mls_group_info(conversation, request.epoch, request.group_info.as_slice())
    .await?
  {
    PostOutcome::Accepted => Ok(()),
    PostOutcome::EpochConflict { current_epoch } => Err(ApiError::EpochConflict { current_epoch }),
  }
}

/// Backlog since the last entry the client processed, for catching up after a gap.
pub async fn list_messages(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  query: ListMlsMessagesQuery,
) -> Result<ListMlsMessagesResponse, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  let Some(membership) = &access.view.maybe_membership else {
    return Err(ApiError::Forbidden(
      "only members can read the group's messages".into(),
    ));
  };
  let limit = page_limit(state, query.maybe_limit);
  let rows = state
    .database
    .list_mls_messages(&MlsBacklogQuery {
      conversation,
      maybe_after: query.maybe_after,
      maybe_not_before: Some(membership.joined_floor),
      reader: caller.user.id,
      limit: i64::from(limit),
    })
    .await?;
  let current_epoch = state.database.current_epoch(conversation).await?;
  Ok(page(rows, limit, current_epoch))
}

/// Welcomes waiting for one of the caller's devices, across all conversations.
pub async fn list_welcomes(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
  query: ListMlsMessagesQuery,
) -> Result<ListMlsMessagesResponse, ApiError> {
  ensure_own_device(state, caller, device).await?;
  let limit = page_limit(state, query.maybe_limit);
  let rows = state
    .database
    .list_welcomes_for_device(device, query.maybe_after, i64::from(limit))
    .await?;
  Ok(page(rows, limit, 0))
}

fn page(mut rows: Vec<StoredMlsMessage>, limit: u32, current_epoch: u64) -> ListMlsMessagesResponse {
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  ListMlsMessagesResponse {
    messages: rows.iter().map(stored_response).collect(),
    has_more,
    current_epoch,
  }
}

fn page_limit(state: &AppState, maybe_limit: Option<u32>) -> u32 {
  let limits = &state.settings.get().limits;
  maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size)
}

async fn ensure_own_device(
  state: &AppState,
  caller: &AuthenticatedUser,
  device: DeviceId,
) -> Result<(), ApiError> {
  if state.database.is_active_device_of(caller.user.id, device).await? {
    Ok(())
  } else {
    Err(ApiError::device_not_found())
  }
}

fn check_payload(state: &AppState, payload: &Base64Bytes) -> Result<(), ApiError> {
  let max = state.settings.get().limits.max_mls_payload_bytes;
  if payload.is_empty() {
    return Err(ApiError::InvalidRequest("payload must not be empty".into()));
  }
  if payload.len() > max {
    return Err(ApiError::PayloadTooLarge(format!("payload exceeds {max} bytes")));
  }
  Ok(())
}

fn response(
  conversation: ConversationId,
  epoch: u64,
  sender: DeviceId,
  message: &NewMlsMessage<'_>,
) -> MlsMessageResponse {
  MlsMessageResponse {
    id: message.id,
    conversation_id: conversation,
    epoch,
    kind: message.kind.clone(),
    sender_device_id: sender,
    maybe_recipient_device_id: message.maybe_recipient,
    payload: Base64Bytes::new(message.payload.to_vec()),
  }
}

fn stored_response(message: &StoredMlsMessage) -> MlsMessageResponse {
  MlsMessageResponse {
    id: message.id,
    conversation_id: message.conversation_id,
    epoch: message.epoch,
    kind: message.kind.clone(),
    sender_device_id: message.sender_device_id,
    maybe_recipient_device_id: message.maybe_recipient_device_id,
    payload: Base64Bytes::new(message.payload.clone()),
  }
}
