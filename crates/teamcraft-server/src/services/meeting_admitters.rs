//! Who may let guests into a meeting besides the organizer's devices (#1103, e2ee.md §4.6, "Meeting
//! guests"): a statement the organizer's identity key signs (`teamcraft_crypto::meeting_admitters`).
//!
//! The server keeps the newest statement per meeting and hands it to the members of the meeting
//! chat, the admitted guests among them. It is addressed by the chat, which every device of the
//! chat knows. The server checks the shape only: that the statement is for this meeting and its
//! chat, is signed by the key it names, fits the size limit, and that its sequence is higher than
//! the stored one. Whether that key is the organizer's is for guests (against `v` of the link) and
//! members (against the organizer's pinned key) to check; the server's word counts for nothing
//! there.

use teamcraft_crypto::meeting_admitters::SignedMeetingAdmitters;
use teamcraft_db::StoredMeeting;
use teamcraft_proto::{
  Base64Bytes, ConversationId, Id, MeetingAdmittersResponse, PutMeetingAdmittersRequest,
};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Keeps the organizer's signed admitters of the meeting whose chat is `conversation` (the
/// organizer only). `409` when a statement with the same or a higher sequence is stored already:
/// the organizer's other device was first.
pub async fn put_admitters(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: PutMeetingAdmittersRequest,
) -> Result<(), ApiError> {
  let meeting = member_meeting(state, caller, conversation).await?;
  if meeting.organizer != caller.user.id {
    return Err(ApiError::Forbidden(
      "only the organizer says who may let guests in".into(),
    ));
  }
  let bytes = request.statement.as_slice();
  let statement = SignedMeetingAdmitters::from_bytes(bytes)
    .map_err(|error| ApiError::InvalidRequest(format!("unreadable admitters: {error}")))?;
  statement
    .check_shape()
    .map_err(|error| ApiError::InvalidRequest(format!("malformed admitters: {error}")))?;
  if !is_same_id(statement.unverified_meeting_id().get(), meeting.id) {
    return Err(ApiError::InvalidRequest(
      "the admitters name another meeting".into(),
    ));
  }
  if !is_same_id(statement.unverified_conversation_id().get(), conversation) {
    return Err(ApiError::InvalidRequest(
      "the admitters name another conversation".into(),
    ));
  }
  let sequence = i64::try_from(statement.unverified_sequence())
    .map_err(|_| ApiError::InvalidRequest("the admitters' sequence is too large".into()))?;
  if !state
    .database
    .put_meeting_admitters(meeting.id, sequence, bytes)
    .await?
  {
    return Err(ApiError::Conflict(
      "admitters with the same or a higher sequence are stored".into(),
    ));
  }
  tracing::info!(meeting_id = %meeting.id, sequence, "meeting admitters published");
  Ok(())
}

/// The newest signed admitters of the meeting whose chat is `conversation` (members of the chat,
/// admitted guests included).
pub async fn admitters(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<MeetingAdmittersResponse, ApiError> {
  let meeting = member_meeting(state, caller, conversation).await?;
  let statement = state
    .database
    .meeting_admitters(meeting.id)
    .await?
    .ok_or_else(ApiError::meeting_admitters_not_found)?;
  Ok(MeetingAdmittersResponse {
    statement: Base64Bytes::new(statement),
  })
}

/// The meeting whose chat is `conversation`, if the caller is in the chat; "not found" for
/// everybody else and for chats of no meeting.
pub(super) async fn member_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<StoredMeeting, ApiError> {
  if state.maybe_meetings.is_none() {
    return Err(ApiError::Unavailable("meetings are not configured".into()));
  }
  let members = state.database.conversation_member_ids(conversation).await?;
  if !members.contains(&caller.user.id) {
    return Err(ApiError::meeting_not_found());
  }
  state
    .database
    .get_meeting_by_conversation(conversation)
    .await?
    .ok_or_else(ApiError::meeting_not_found)
}

/// Whether an ID inside a signed statement (unsigned 64 bit) is `id`.
fn is_same_id(raw: u64, id: impl Id) -> bool {
  i64::try_from(raw).is_ok_and(|raw| raw == id.snowflake().get())
}
