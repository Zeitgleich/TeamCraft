//! "Send later" (docs/scheduled-messages.md): the sender's device seals a message now, and the server
//! holds the ciphertext and releases it at its time. Only the sender's own devices can list or change
//! their entries, and they read the text like any message of the conversation.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{
  Base64Bytes, ClientMessageId, ConversationId, DeviceId, MessageResponse, ScheduledMessageId, ThreadId,
  UnixMillis, UserId,
};

wire_enum! {
  /// Why a message didn't go out at its time. It stays with its sender until this is solved.
  pub enum ScheduledHoldReason {
    /// The group changed since it was sealed (or the device that sealed it left): never released
    /// under that key. It goes out once a device of the sender seals it again.
    StaleEpoch => "stale_epoch",
    /// The workspace turned scheduling off.
    PolicyOff => "policy_off",
    /// The sender is no longer in the conversation.
    NotAMember => "not_a_member",
    /// The sender may no longer post there.
    NoPermission => "no_permission",
    /// The sender's account is deactivated.
    SenderInactive => "sender_inactive",
    /// The conversation was deleted.
    ConversationGone => "conversation_gone",
    /// The thread it answers is gone, or threads were turned off.
    ThreadGone => "thread_gone",
  }
}

/// What a device sealed: the ciphertext and the metadata that any send shows the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SealedScheduledMessage {
  /// The device that sealed and signed it, the caller's.
  pub device_id: DeviceId,
  pub content_key_id: Base64Bytes,
  pub ciphertext: Base64Bytes,
  #[serde(default)]
  pub mentioned_user_ids: Vec<UserId>,
  #[serde(default)]
  pub is_urgent: bool,
  /// As in [`crate::SendMessageRequest::is_channel_mention`].
  #[serde(default, skip_serializing_if = "crate::api::messages::is_false")]
  pub is_channel_mention: bool,
  /// The MLS epoch it was sealed in, which must be the group's current one.
  pub mls_epoch: u64,
}

/// `POST /api/v1/conversations/{id}/scheduled-messages`. Sending the same client message ID again
/// answers with what is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScheduleMessageRequest {
  pub client_message_id: ClientMessageId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub sealed: SealedScheduledMessage,
  pub due_at: UnixMillis,
}

/// `PUT /api/v1/me/scheduled-messages/{id}`: a new time, and with `sealed` the text sealed again (an
/// edit, or the current epoch's key after the group changed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateScheduledMessageRequest {
  /// The version this change is based on; two devices' changes can't overwrite each other unseen.
  pub expected_version: u64,
  pub due_at: UnixMillis,
  #[serde(rename = "sealed", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sealed: Option<SealedScheduledMessage>,
}

/// A held message as its sender's devices see it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScheduledMessageResponse {
  pub id: ScheduledMessageId,
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
  pub sender_device_id: DeviceId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub content_key_id: Base64Bytes,
  pub ciphertext: Base64Bytes,
  #[serde(default)]
  pub mentioned_user_ids: Vec<UserId>,
  #[serde(default)]
  pub is_urgent: bool,
  /// As in [`crate::SendMessageRequest::is_channel_mention`].
  #[serde(default, skip_serializing_if = "crate::api::messages::is_false")]
  pub is_channel_mention: bool,
  pub mls_epoch: u64,
  pub due_at: UnixMillis,
  pub version: u64,
  #[serde(rename = "hold", default, skip_serializing_if = "Option::is_none")]
  pub maybe_hold: Option<ScheduledHoldReason>,
  /// For a hold nothing can solve any more (the sender left, their account is off, the conversation
  /// is gone): when the server removes it, after `limits.scheduled_hold_retention_days`.
  #[serde(rename = "removes_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_removes_at: Option<UnixMillis>,
  /// False once the group left `mls_epoch`: a device should seal it again, ahead of its time.
  pub is_epoch_current: bool,
}

/// `GET /api/v1/me/scheduled-messages`: the caller's held messages, soonest first. Bounded by the
/// workspace's limit per user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListScheduledMessagesResponse {
  pub messages: Vec<ScheduledMessageResponse>,
  /// The server's clock, which decides when a message is due.
  pub server_now: UnixMillis,
}

/// `POST /api/v1/me/scheduled-messages/{id}/send`: released now, like a send.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SendScheduledMessageResponse {
  pub message: MessageResponse,
}
