//! Delivered ticks (issue #598): how far the messages of a DM or group DM reached the members'
//! devices. A device reports the newest message it stored (gateway `delivered`); the server keeps
//! one position per membership and tells each member one aggregate, "every other member has it up
//! to here" (gateway `delivery_update`), never who has what (e2ee.md §4.4).

use teamcraft_db::DeliveryRow;
use teamcraft_proto::{ConversationId, GatewayEvent, MessageId, UserId};

use super::access::load_conversation;
use super::notifications::has_receipts;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Records that a device of the caller stored `conversation` up to `message`, and tells the other
/// members whose aggregate moved. Quietly does nothing where there are no ticks (channels), for a
/// caller who doesn't share delivery, and when the position was already there.
pub async fn note_delivered(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  message: MessageId,
) -> Result<(), ApiError> {
  let access = load_conversation(state, caller, conversation).await?;
  if !access.is_member() || !has_receipts(&access.view.conversation) {
    return Ok(());
  }
  let mut members = state
    .database
    .list_delivery_members(conversation, default_is_sharing(state))
    .await?;
  let Some(own) = members.iter().position(|member| member.user == caller.user.id) else {
    return Ok(());
  };
  if !members[own].is_sharing || members[own].maybe_delivered.is_some_and(|known| known >= message) {
    return Ok(());
  }
  // A position only ever names a message of this conversation, so a client can't push it past the
  // conversation's end.
  if state.database.get_message(conversation, message).await?.is_none() {
    return Err(ApiError::InvalidRequest("unknown message".into()));
  }
  if !state
    .database
    .set_delivered_position(conversation, caller.user.id, message)
    .await?
  {
    return Ok(());
  }
  let before = members.clone();
  members[own].maybe_delivered = Some(message);
  for viewer in members.iter().filter(|member| member.user != caller.user.id) {
    let maybe_now = delivered_to_others(&members, viewer.user);
    if let Some(message_id) = maybe_now
      && maybe_now != delivered_to_others(&before, viewer.user)
    {
      state.hub.publish(
        [viewer.user],
        &GatewayEvent::DeliveryUpdate {
          conversation_id: conversation,
          message_id,
        },
      );
    }
  }
  Ok(())
}

/// The caller's aggregate for `GET .../read-receipts`.
pub async fn delivered_up_to(
  state: &AppState,
  conversation: ConversationId,
  viewer: UserId,
) -> Result<Option<MessageId>, ApiError> {
  let members = state
    .database
    .list_delivery_members(conversation, default_is_sharing(state))
    .await?;
  Ok(delivered_to_others(&members, viewer))
}

/// Users who never chose follow the instance's receipts default.
fn default_is_sharing(state: &AppState) -> bool {
  state.settings.get().notifications.read_receipts_default
}

/// The newest message every member but `viewer` has on a device. `None` when the viewer doesn't
/// share delivery (reciprocal), when another member doesn't (their state is unknown, so the ticks
/// never claim more than is true), when some member has no device that reported yet, or when the
/// viewer is alone.
pub(crate) fn delivered_to_others(members: &[DeliveryRow], viewer: UserId) -> Option<MessageId> {
  if !members
    .iter()
    .any(|member| member.user == viewer && member.is_sharing)
  {
    return None;
  }
  let mut others = members.iter().filter(|member| member.user != viewer).peekable();
  others.peek()?;
  others.try_fold(None, |lowest: Option<MessageId>, member| {
    let delivered = member.maybe_delivered.filter(|_| member.is_sharing)?;
    Some(Some(lowest.map_or(delivered, |lowest| lowest.min(delivered))))
  })?
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_dm_shows_how_far_the_other_member_got() {
    let members = [row(1, true, Some(10)), row(2, true, Some(7))];
    assert_eq!(delivered_to_others(&members, user(1)), Some(message(7)));
    assert_eq!(delivered_to_others(&members, user(2)), Some(message(10)));
  }

  #[test]
  fn a_group_shows_the_lowest_of_the_others() {
    let members = [row(1, true, None), row(2, true, Some(9)), row(3, true, Some(5))];
    assert_eq!(delivered_to_others(&members, user(1)), Some(message(5)));
    // Member 1 has no device that reported, so the others see nothing yet.
    assert_eq!(delivered_to_others(&members, user(2)), None);
  }

  #[test]
  fn delivery_is_reciprocal() {
    let members = [row(1, false, Some(10)), row(2, true, Some(7))];
    assert_eq!(delivered_to_others(&members, user(1)), None);
    assert_eq!(delivered_to_others(&members, user(2)), None);
  }

  #[test]
  fn alone_or_unknown_shows_nothing() {
    assert_eq!(delivered_to_others(&[row(1, true, Some(3))], user(1)), None);
    assert_eq!(delivered_to_others(&[row(1, true, Some(3))], user(2)), None);
  }

  fn row(id: i64, is_sharing: bool, maybe_delivered: Option<i64>) -> DeliveryRow {
    DeliveryRow {
      user: user(id),
      is_sharing,
      maybe_delivered: maybe_delivered.map(message),
    }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn message(id: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
