//! API, gateway and SFU signaling types shared by server and clients.
//!
//! This crate is wasm-compatible and contains no I/O. Wire enums carry an `Unknown` catch-all so
//! older clients survive new server variants (api-conventions.md).

mod api;
mod bytes;
mod call_media;
mod calls;
mod conversation;
mod error;
mod gateway;
mod id;
mod integration;
pub mod live;
mod permission;
mod rendezvous;
mod role;
mod wire_enum;

pub use api::*;
pub use bytes::{Base64Bytes, ClientMessageId, ClientMessageIdError, IdempotencyKey, UnixMillis};
pub use call_media::{
  AudioCodec, MAX_SIMULCAST_LAYERS, MediaCapabilities, TransportFeedback, VideoCodec, VideoLayerInfo,
  VideoTrack, VideoTrackKind,
};
pub use calls::{
  CALL_JOIN_TOKEN_AUDIENCE, CallClientMessage, CallEvent, CallJoinClaims, CallParticipant, CallServerMessage,
  DaveKeyBinding, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE, ScreenAudioTrack, ScreenContentMode,
};
pub use conversation::{ConversationKind, ConversationVisibility, HistoryPolicy};
pub use error::{ErrorBody, ErrorCode, SignInReason};
pub use gateway::{ATTENTION_AWAY_DELAY_SECS, GatewayClientMessage, GatewayEvent, GatewayServerMessage};
pub use id::{
  AnnouncementId, AttachmentId, AuditEntryId, BackgroundId, CallId, CategoryId, ConversationId, DeviceId, Id,
  IdError, InviteId, MeetingAttendeeId, MeetingId, MeetingInviteId, MessageId, MlsMessageId, OrgBackgroundId,
  PushRetryId, RecoveryRequestId, SNOWFLAKE_EPOCH_MS, SNOWFLAKE_SEQUENCE_BITS, SNOWFLAKE_WORKER_BITS,
  Snowflake, SpaceId, SystemEventId, ThreadId, UserGroupId, UserId,
};
pub use integration::{
  DEFAULT_SOURCE_ALERT_LEVELS, DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE, MAX_SOURCE_ALERT_LEVEL_CHARS,
  MAX_SOURCE_ALERT_LEVELS, MAX_SOURCE_NAME_CHARS, MAX_SOURCE_RATE_LIMIT_PER_MINUTE, SourceChange, SourceId,
  SourceKind, SourceOptions, SourceTokenHash, SourceTokenHashError,
};
pub use permission::{
  ALL_KNOWN as ALL_PERMISSIONS, ChannelContext, ChannelModeration, ChannelRole, Permission, PermissionScheme,
  PermissionScope, SchemeChangeError, SchemeRole, SpaceContext, SpaceRole, Target, check_scheme_change,
  is_allowed,
};
pub use rendezvous::{RendezvousClientMessage, RendezvousServerMessage};
pub use role::Role;
