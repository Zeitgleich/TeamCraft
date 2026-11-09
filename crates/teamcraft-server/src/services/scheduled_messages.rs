//! "Send later" (docs/scheduled-messages.md, e2ee.md §4.4): the server holds a ciphertext the sender's
//! device sealed and releases it at its time by this server's clock, through the normal path of a
//! message. It compares MLS epochs, the one thing it can know: while the group's epoch is the one the
//! message was sealed in, no commit was accepted since, so nobody was removed and the key is as good
//! as when it was sealed. Once it moved the message waits for a device of the sender to seal it again.

use std::collections::HashSet;
use std::time::Duration;

use teamcraft_db::{
  NewMessage, NewScheduledMessage, ReleaseOutcome, ScheduleOutcome, ScheduledUpdateOutcome, SealedParts,
  StaleWakes, StoredScheduledMessage,
};
use teamcraft_proto::{
  Base64Bytes, ConversationId, DeviceId, GatewayEvent, ListScheduledMessagesResponse, MessageId, Permission,
  Role, ScheduleMessageRequest, ScheduledHoldReason, ScheduledMessageId, ScheduledMessageResponse,
  SealedScheduledMessage, SendScheduledMessageResponse, UnixMillis, UpdateScheduledMessageRequest, UserId,
  UserKind,
};
use time::OffsetDateTime;
use tracing::warn;

use super::access::{ConversationAccess, allows, load_conversation, require};
use super::convert::{message_response, unix_millis};
use super::messages::{self, MAX_CONTENT_KEY_ID_BYTES};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Messages released per scheduler run; a busy instance catches up on the next run.
const BATCH_SIZE: i64 = 200;
/// How long a scheduler instance owns the messages it claimed. Longer than a batch takes; after a
/// crash the next instance takes them over, and a release only ever stores a message once.
const CLAIM_LEASE: Duration = Duration::from_secs(60);
/// The wakes without a commit (docs/scheduled-messages.md, "The wake"): in the last 15 minutes
/// before its time every 5 minutes (as often as `scheduled_wake_min_interval` allows by default), and
/// for a day after it was held at its time every hour. A wake that got lost is tried again; a phone
/// that stays off costs at most these.
const STALE_WAKES: StaleWakes = StaleWakes {
  last_stretch: Duration::from_secs(15 * 60),
  last_stretch_every: Duration::from_secs(5 * 60),
  held_every: Duration::from_secs(60 * 60),
  held_for: Duration::from_secs(24 * 60 * 60),
};

/// Holds a message for later. Refuses what the caller couldn't send now, and a ciphertext from
/// another epoch than the group's current one.
pub async fn schedule_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: ScheduleMessageRequest,
) -> Result<ScheduledMessageResponse, ApiError> {
  let policy = state.policy.get().scheduled_messages.clone();
  if !policy.is_enabled {
    return Err(ApiError::ScheduledMessagesDisabled);
  }
  let access = check_member_may_post(state, caller, conversation).await?;
  let due_at = check_due_at(state, request.due_at)?;
  check_sealed(state, caller, conversation, &access, &request.sealed).await?;
  check_mentions_are_members(state, conversation, &request.sealed.mentioned_user_ids).await?;
  if let Some(thread) = request.maybe_thread_id {
    messages::thread_root(state, conversation, thread).await?;
  }
  let mentions = sorted_mentions(&request.sealed.mentioned_user_ids);
  let sealed = sealed_parts(&request.sealed, &mentions);
  let outcome = state
    .database
    .create_scheduled_message(
      &NewScheduledMessage {
        id: state.next_id()?,
        user_id: caller.user.id,
        conversation_id: conversation,
        client_message_id: request.client_message_id,
        maybe_thread_id: request.maybe_thread_id,
        sealed,
        due_at,
      },
      policy.max_per_user,
    )
    .await?;
  match outcome {
    ScheduleOutcome::Created(stored) => {
      tell_devices(state, caller.user.id);
      Ok(response(state, &stored))
    },
    ScheduleOutcome::Existing(stored) => Ok(response(state, &stored)),
    ScheduleOutcome::LimitReached => Err(ApiError::ScheduledMessageLimit {
      max: policy.max_per_user,
    }),
  }
}

pub async fn list_scheduled_messages(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListScheduledMessagesResponse, ApiError> {
  let stored = state.database.list_scheduled_messages(caller.user.id).await?;
  Ok(ListScheduledMessagesResponse {
    messages: stored.iter().map(|stored| response(state, stored)).collect(),
    server_now: unix_millis(OffsetDateTime::now_utc()),
  })
}

/// Changes a held message of the caller. With the policy off only a new seal at the same time goes
/// through: the time can't move, but a message still waiting shouldn't stay under an old key.
pub async fn update_scheduled_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ScheduledMessageId,
  request: UpdateScheduledMessageRequest,
) -> Result<ScheduledMessageResponse, ApiError> {
  // Versions are positive 64-bit integers in the database; a larger one matches nothing.
  if i64::try_from(request.expected_version).is_err() {
    return Err(ApiError::InvalidRequest(
      "expected_version is out of range".into(),
    ));
  }
  let current = own_entry(state, caller, id).await?;
  let policy = state.policy.get().scheduled_messages.clone();
  let is_same_time = unix_millis(current.due_at) == request.due_at;
  if !policy.is_enabled && !is_same_time {
    return Err(ApiError::ScheduledMessagesDisabled);
  }
  let access = check_member_may_post(state, caller, current.conversation_id).await?;
  // Sealing again keeps a time that may have passed: that is what a stale message waits for.
  let due_at = if is_same_time {
    current.due_at
  } else {
    check_due_at(state, request.due_at)?
  };
  if let Some(sealed) = &request.maybe_sealed {
    check_sealed(state, caller, current.conversation_id, &access, sealed).await?;
  }
  // Somebody mentioned may have left since the first seal, and it is a member's device sealing the
  // same text again: they drop out, as at the release, rather than the message getting stuck.
  let mentions = match &request.maybe_sealed {
    Some(sealed) => mentions_still_in(
      &sorted_mentions(&sealed.mentioned_user_ids),
      &member_set(state, current.conversation_id).await?,
    ),
    None => Vec::new(),
  };
  let maybe_parts = request
    .maybe_sealed
    .as_ref()
    .map(|sealed| sealed_parts(sealed, &mentions));
  let outcome = state
    .database
    .update_scheduled_message(
      id,
      caller.user.id,
      request.expected_version,
      due_at,
      maybe_parts.as_ref(),
    )
    .await?;
  match outcome {
    ScheduledUpdateOutcome::Updated(stored) => {
      tell_devices(state, caller.user.id);
      Ok(response(state, &stored))
    },
    ScheduledUpdateOutcome::VersionConflict { current_version } => {
      Err(ApiError::VersionConflict { current_version })
    },
    ScheduledUpdateOutcome::NotFound => Err(ApiError::scheduled_message_not_found()),
  }
}

/// Cancels a held message of the caller. If the release came first this says it is gone.
pub async fn cancel_scheduled_message(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ScheduledMessageId,
) -> Result<(), ApiError> {
  if !state
    .database
    .delete_scheduled_message(id, caller.user.id)
    .await?
  {
    return Err(ApiError::scheduled_message_not_found());
  }
  tell_devices(state, caller.user.id);
  Ok(())
}

/// Releases a held message now, as a send of the caller: the policy doesn't matter here, the rest
/// does. Under an old epoch this refuses with `epoch_conflict`, and the device seals it again first.
pub async fn send_scheduled_now(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ScheduledMessageId,
) -> Result<SendScheduledMessageResponse, ApiError> {
  let entry = own_entry(state, caller, id).await?;
  let access = check_member_may_post(state, caller, entry.conversation_id).await?;
  let maybe_thread_root = match entry.maybe_thread_id {
    Some(thread) => Some(messages::thread_root(state, entry.conversation_id, thread).await?),
    None => None,
  };
  if !state
    .database
    .is_active_device_of(caller.user.id, entry.sender_device_id)
    .await?
  {
    let current_epoch = state.database.current_epoch(entry.conversation_id).await?;
    return Err(ApiError::EpochConflict { current_epoch });
  }
  let members = member_set(state, entry.conversation_id).await?;
  let mentions = mentions_still_in(&entry.mentioned_user_ids, &members);
  let message_id = state.next_id()?;
  let new = new_message(&entry, message_id, &mentions);
  match state
    .database
    .release_scheduled_message(entry.id, entry.version, entry.mls_epoch, &new)
    .await?
  {
    ReleaseOutcome::Released(outcome) => {
      if outcome.is_new {
        messages::spend_large_mention(state, entry.user_id, mentions.len());
        let audience = messages::Audience {
          members,
          is_channel_mention: entry.is_channel_mention
            && allows(state, caller, &Permission::UseChannelMentions, &access.target()),
        };
        messages::deliver_stored(
          state,
          &access.view.conversation,
          &outcome.message,
          audience,
          maybe_thread_root.as_ref(),
          OffsetDateTime::now_utc(),
        )
        .await?;
      }
      tell_devices(state, caller.user.id);
      Ok(SendScheduledMessageResponse {
        message: message_response(&outcome.message),
      })
    },
    ReleaseOutcome::StaleEpoch { current_epoch } => Err(ApiError::EpochConflict { current_epoch }),
    ReleaseOutcome::Changed => Err(ApiError::scheduled_message_not_found()),
  }
}

/// A commit moved the group's epoch: whoever holds a message there is told to seal it again. Devices
/// online hear of the commit anyway; the others get a push that shows nothing. Both the event and the
/// push go at most once per user and conversation for a burst of commits and per
/// `scheduled_wake_min_interval`, so a member committing in a loop costs nobody a list per commit.
pub fn note_epoch_moved(state: &AppState, conversation: ConversationId) {
  let state = state.clone();
  tokio::spawn(async move {
    let users = match state.database.users_waiting_in_conversation(conversation).await {
      Ok(users) => users,
      Err(error) => {
        warn!(%error, "reading who holds scheduled messages failed");
        return;
      },
    };
    for user in users {
      crate::push::routing::wake_to_seal(state.clone(), user, conversation);
    }
  });
}

/// Releases what is due at `now` and returns how many messages it handled. Safe with several
/// instances and across restarts: instances claim different rows, a release stores a message once,
/// and what a crash left behind comes up again after the claim's lease.
pub async fn run_due(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let limits = &state.settings.get().limits;
  let hold_max_age =
    Duration::from_secs(u64::from(limits.scheduled_hold_retention_days).saturating_mul(86_400));
  // Holds whose sender left since can't be solved any more; they count from now for the purge.
  let departed = state
    .database
    .hold_departed_scheduled_messages(now, BATCH_SIZE)
    .await?;
  for user in departed.into_iter().collect::<HashSet<_>>() {
    tell_devices(state, user);
  }
  // The text's only copy outside the user's devices goes: they keep it to copy until dismissed.
  let purged = state
    .database
    .purge_dead_scheduled_messages(now, hold_max_age, BATCH_SIZE)
    .await?;
  // One event per person, however many of theirs went.
  for user in purged.into_iter().collect::<HashSet<_>>() {
    tell_devices(state, user);
  }
  // Messages still under an old epoch, for a wake that got lost (a restart, a push service that
  // dropped it, a phone that was off). Not a poll: paced per message by `STALE_WAKES`.
  for (user, conversation) in state
    .database
    .claim_stale_wakes(now, STALE_WAKES, BATCH_SIZE)
    .await?
  {
    crate::push::routing::wake_to_seal(state.clone(), user, conversation);
  }
  let due = state
    .database
    .claim_due_scheduled_messages(now, CLAIM_LEASE, BATCH_SIZE)
    .await?;
  for entry in &due {
    // One message in trouble doesn't hold back the others; its claim runs out and it comes up again.
    if let Err(error) = release_at_time(state, entry, now).await {
      warn!(%error, scheduled_message_id = %entry.id, "a scheduled message could not be released");
    }
  }
  Ok(due.len())
}

/// Sends one due message, or notes why it can't go.
async fn release_at_time(
  state: &AppState,
  entry: &StoredScheduledMessage,
  now: OffsetDateTime,
) -> Result<(), ApiError> {
  let check = match check_release(state, entry).await? {
    Ok(check) => check,
    Err(reason) => return hold(state, entry, &reason, now).await,
  };
  let members = member_set(state, entry.conversation_id).await?;
  let mentions = mentions_still_in(&entry.mentioned_user_ids, &members);
  let message_id = state.next_id()?;
  let new = new_message(entry, message_id, &mentions);
  match state
    .database
    .release_scheduled_message(entry.id, entry.version, entry.mls_epoch, &new)
    .await?
  {
    ReleaseOutcome::Released(outcome) => {
      if outcome.is_new {
        messages::spend_large_mention(state, entry.user_id, mentions.len());
        let audience = messages::Audience {
          members,
          is_channel_mention: entry.is_channel_mention && check.may_mention_channel,
        };
        messages::deliver_stored(
          state,
          &check.access.view.conversation,
          &outcome.message,
          audience,
          check.maybe_thread_root.as_ref(),
          now,
        )
        .await?;
      }
      tell_devices(state, entry.user_id);
      Ok(())
    },
    ReleaseOutcome::StaleEpoch { .. } => hold(state, entry, &ScheduledHoldReason::StaleEpoch, now).await,
    // Cancelled or changed since the claim: the change decides.
    ReleaseOutcome::Changed => Ok(()),
  }
}

/// What a release needs and found to be fine.
struct ReleaseCheck {
  access: ConversationAccess,
  maybe_thread_root: Option<teamcraft_db::StoredMessage>,
  /// The sender may still address the whole conversation.
  may_mention_channel: bool,
}

/// Everything a send would check, now that nobody is signed in: the sender's account as it is now,
/// and their last known role.
async fn check_release(
  state: &AppState,
  entry: &StoredScheduledMessage,
) -> Result<Result<ReleaseCheck, ScheduledHoldReason>, ApiError> {
  let Some(user) = state.database.get_user(entry.user_id).await? else {
    return Ok(Err(ScheduledHoldReason::SenderInactive));
  };
  if user.is_deactivated || user.is_deleted {
    return Ok(Err(ScheduledHoldReason::SenderInactive));
  }
  let role = sender_role(state, entry.user_id, &user.kind).await?;
  let sender = AuthenticatedUser {
    user,
    role,
    maybe_oidc_session: None,
    has_verified_token_binding: false,
  };
  let access = match load_conversation(state, &sender, entry.conversation_id).await {
    Ok(access) => access,
    Err(ApiError::NotFound { .. }) => {
      return Ok(Err(
        if state.database.conversation_exists(entry.conversation_id).await? {
          ScheduledHoldReason::NotAMember
        } else {
          ScheduledHoldReason::ConversationGone
        },
      ));
    },
    Err(error) => return Err(error),
  };
  if !access.is_member() {
    return Ok(Err(ScheduledHoldReason::NotAMember));
  }
  if !state.policy.get().scheduled_messages.is_enabled {
    return Ok(Err(ScheduledHoldReason::PolicyOff));
  }
  if require(state, &sender, Permission::CreatePost, &access.target()).is_err() {
    return Ok(Err(ScheduledHoldReason::NoPermission));
  }
  let maybe_thread_root = match entry.maybe_thread_id {
    Some(thread) => match messages::thread_root(state, entry.conversation_id, thread).await {
      Ok(root) => Some(root),
      Err(ApiError::InvalidRequest(_) | ApiError::Forbidden(_)) => {
        return Ok(Err(ScheduledHoldReason::ThreadGone));
      },
      Err(error) => return Err(error),
    },
    None => None,
  };
  // The device that signed it left: members would refuse the message (e2ee.md §4.4), another device
  // of the sender can seal it again.
  if !entry.is_epoch_current
    || !state
      .database
      .is_active_device_of(entry.user_id, entry.sender_device_id)
      .await?
  {
    return Ok(Err(ScheduledHoldReason::StaleEpoch));
  }
  let may_mention_channel = allows(state, &sender, &Permission::UseChannelMentions, &access.target());
  Ok(Ok(ReleaseCheck {
    access,
    maybe_thread_root,
    may_mention_channel,
  }))
}

/// The sender's role, as their last sign-in brought it: a permission scheme may decide by it.
async fn sender_role(state: &AppState, user: UserId, kind: &UserKind) -> Result<Role, ApiError> {
  let maybe_role = state.database.last_oidc_role(user).await?;
  Ok(match (maybe_role.map(Role::from), kind) {
    (_, UserKind::Guest) => Role::Guest,
    (Some(role), _) if !matches!(role, Role::Unknown(_)) => role,
    _ => Role::Member,
  })
}

async fn hold(
  state: &AppState,
  entry: &StoredScheduledMessage,
  reason: &ScheduledHoldReason,
  now: OffsetDateTime,
) -> Result<(), ApiError> {
  if state
    .database
    .hold_scheduled_message(entry.id, entry.version, reason, now)
    .await?
  {
    tell_devices(state, entry.user_id);
    if *reason == ScheduledHoldReason::StaleEpoch {
      crate::push::routing::wake_to_seal(state.clone(), entry.user_id, entry.conversation_id);
    }
  }
  Ok(())
}

async fn own_entry(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ScheduledMessageId,
) -> Result<StoredScheduledMessage, ApiError> {
  state
    .database
    .get_scheduled_message(id, caller.user.id)
    .await?
    .ok_or_else(ApiError::scheduled_message_not_found)
}

/// Being able to see a public channel isn't enough: a held message is a member's.
async fn check_member_may_post(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ConversationAccess, ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can schedule messages".into()));
  }
  require(state, caller, Permission::CreatePost, &access.target())?;
  Ok(access)
}

/// The time must lie ahead, within the workspace's horizon, by this server's clock.
fn check_due_at(state: &AppState, due_at: UnixMillis) -> Result<OffsetDateTime, ApiError> {
  let now = OffsetDateTime::now_utc();
  let horizon_days = i64::from(state.policy.get().scheduled_messages.max_horizon_days);
  let time = OffsetDateTime::from_unix_timestamp_nanos(i128::from(due_at.0) * 1_000_000)
    .map_err(|_| ApiError::ScheduledTimeInvalid("the time is out of range".into()))?;
  if time <= now {
    return Err(ApiError::ScheduledTimeInvalid("the time has passed".into()));
  }
  if time > now + time::Duration::days(horizon_days) {
    return Err(ApiError::ScheduledTimeInvalid(format!(
      "the time is more than {horizon_days} days ahead"
    )));
  }
  Ok(time)
}

/// The limits of a send, the caller's own device, people who are members, and the current epoch.
async fn check_sealed(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  access: &ConversationAccess,
  sealed: &SealedScheduledMessage,
) -> Result<(), ApiError> {
  let limits = &state.settings.get().limits;
  if sealed.ciphertext.is_empty() {
    return Err(ApiError::InvalidRequest("ciphertext must not be empty".into()));
  }
  // Smaller than a message's limit: every list of the sender carries it, on every device.
  if sealed.ciphertext.len() > limits.max_scheduled_ciphertext_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "ciphertext exceeds {} bytes",
      limits.max_scheduled_ciphertext_bytes
    )));
  }
  if sealed.content_key_id.is_empty() || sealed.content_key_id.len() > MAX_CONTENT_KEY_ID_BYTES {
    return Err(ApiError::InvalidRequest(
      "content_key_id has an invalid size".into(),
    ));
  }
  messages::check_mention_count(state, caller, &access.target(), sealed.mentioned_user_ids.len())?;
  // Checked here, where a device can wait and try again; spent when the server sends it.
  messages::check_large_mention(state, caller.user.id, sealed.mentioned_user_ids.len())?;
  ensure_own_device(state, caller, sealed.device_id).await?;
  let current_epoch = state.database.current_epoch(conversation).await?;
  if sealed.mls_epoch != current_epoch {
    return Err(ApiError::EpochConflict { current_epoch });
  }
  Ok(())
}

/// A first seal names only members.
async fn check_mentions_are_members(
  state: &AppState,
  conversation: ConversationId,
  mentioned: &[UserId],
) -> Result<(), ApiError> {
  let members = member_set(state, conversation).await?;
  match mentioned.iter().find(|user| !members.contains(user)) {
    Some(stranger) => Err(ApiError::InvalidRequest(format!(
      "mentioned user {stranger} is not in the conversation"
    ))),
    None => Ok(()),
  }
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

async fn member_set(state: &AppState, conversation: ConversationId) -> Result<HashSet<UserId>, ApiError> {
  Ok(
    state
      .database
      .conversation_member_ids(conversation)
      .await?
      .into_iter()
      .collect(),
  )
}

/// The mention list a release stores: people who left since it was sealed aren't told, and a message
/// never names somebody outside the conversation.
fn mentions_still_in(mentioned: &[UserId], members: &HashSet<UserId>) -> Vec<UserId> {
  mentioned
    .iter()
    .copied()
    .filter(|user| members.contains(user))
    .collect()
}

fn sorted_mentions(mentioned: &[UserId]) -> Vec<UserId> {
  let mut sorted = mentioned.to_vec();
  sorted.sort_unstable();
  sorted.dedup();
  sorted
}

fn sealed_parts<'a>(sealed: &'a SealedScheduledMessage, mentions: &'a [UserId]) -> SealedParts<'a> {
  SealedParts {
    sender_device_id: sealed.device_id,
    mentioned_user_ids: mentions,
    is_urgent: sealed.is_urgent,
    is_channel_mention: sealed.is_channel_mention,
    content_key_id: sealed.content_key_id.as_slice(),
    ciphertext: sealed.ciphertext.as_slice(),
    mls_epoch: sealed.mls_epoch,
  }
}

fn new_message<'a>(
  entry: &'a StoredScheduledMessage,
  id: MessageId,
  mentions: &'a [UserId],
) -> NewMessage<'a> {
  NewMessage {
    id,
    client_message_id: entry.client_message_id,
    conversation_id: entry.conversation_id,
    sender_user_id: entry.user_id,
    sender_device_id: entry.sender_device_id,
    maybe_thread_id: entry.maybe_thread_id,
    mentioned_user_ids: mentions,
    is_urgent: entry.is_urgent,
    content_key_id: &entry.content_key_id,
    ciphertext: &entry.ciphertext,
  }
}

/// The user's other devices read the list again. Changes the user made and releases are bounded by
/// the write limit and the list's size; a burst of commits goes through `wake_to_seal` instead.
fn tell_devices(state: &AppState, user: UserId) {
  state.hub.publish([user], &GatewayEvent::ScheduledMessagesUpdate);
}

fn response(state: &AppState, stored: &StoredScheduledMessage) -> ScheduledMessageResponse {
  let retention_days = state.settings.get().limits.scheduled_hold_retention_days;
  let maybe_removes_at = stored
    .maybe_hold
    .as_ref()
    .filter(|reason| is_dead(reason))
    .and(stored.maybe_held_at)
    .map(|held_at| unix_millis(held_at + time::Duration::days(i64::from(retention_days))));
  ScheduledMessageResponse {
    id: stored.id,
    conversation_id: stored.conversation_id,
    client_message_id: stored.client_message_id,
    sender_device_id: stored.sender_device_id,
    maybe_thread_id: stored.maybe_thread_id,
    content_key_id: Base64Bytes::new(stored.content_key_id.clone()),
    ciphertext: Base64Bytes::new(stored.ciphertext.clone()),
    mentioned_user_ids: stored.mentioned_user_ids.clone(),
    is_urgent: stored.is_urgent,
    is_channel_mention: stored.is_channel_mention,
    mls_epoch: stored.mls_epoch,
    due_at: unix_millis(stored.due_at),
    version: stored.version,
    maybe_hold: stored.maybe_hold.clone(),
    maybe_removes_at,
    is_epoch_current: stored.is_epoch_current,
  }
}

/// Holds nothing can solve any more: the purge removes them after the retention.
fn is_dead(reason: &ScheduledHoldReason) -> bool {
  matches!(
    reason,
    ScheduledHoldReason::SenderInactive
      | ScheduledHoldReason::NotAMember
      | ScheduledHoldReason::ConversationGone
  )
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn people_who_left_drop_out_of_the_mentions_at_release() {
    let (stays, left) = (user(1), user(2));
    let members: HashSet<UserId> = [stays].into_iter().collect();
    assert_eq!(mentions_still_in(&[stays, left], &members), [stays]);
  }

  #[test]
  fn mentions_are_stored_once_each_in_order() {
    assert_eq!(sorted_mentions(&[user(2), user(1), user(2)]), [user(1), user(2)]);
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
