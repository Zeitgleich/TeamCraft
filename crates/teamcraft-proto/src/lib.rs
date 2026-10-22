//! API, gateway and SFU signaling types shared by server and clients.
//!
//! This crate is wasm-compatible and contains no I/O. Wire enums carry an `Unknown` catch-all so
//! older clients survive new server variants (api-conventions.md).

mod api;
mod bytes;
mod calls;
mod conversation;
mod error;
mod gateway;
mod id;
mod permission;
mod rendezvous;
mod role;
mod wire_enum;

pub use api::*;
pub use bytes::{Base64Bytes, ClientMessageId, ClientMessageIdError, UnixMillis};
pub use calls::{
  CALL_JOIN_TOKEN_AUDIENCE, CallClientMessage, CallEvent, CallJoinClaims, CallParticipant, CallServerMessage,
  MediaStreamId,
};
pub use conversation::{ConversationKind, ConversationVisibility, HistoryPolicy};
pub use error::{ErrorBody, ErrorCode};
pub use gateway::{GatewayClientMessage, GatewayEvent, GatewayServerMessage};
pub use id::{
  AttachmentId, AuditEntryId, CallId, CategoryId, ConversationId, DeviceId, Id, IdError, InviteId,
  MeetingAttendeeId, MeetingId, MeetingInviteId, MessageId, MlsMessageId, SNOWFLAKE_EPOCH_MS,
  SNOWFLAKE_SEQUENCE_BITS, SNOWFLAKE_WORKER_BITS, Snowflake, SpaceId, ThreadId, UserId,
};
pub use permission::{
  ALL_KNOWN as ALL_PERMISSIONS, ChannelContext, ChannelModeration, ChannelRole, Permission, PermissionScheme,
  PermissionScope, SchemeRole, SpaceContext, SpaceRole, Target, is_allowed,
};
pub use rendezvous::{RendezvousClientMessage, RendezvousServerMessage};
pub use role::Role;
