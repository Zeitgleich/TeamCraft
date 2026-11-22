//! API, gateway and SFU signaling types shared by server and clients.
//!
//! This crate is wasm-compatible and contains no I/O. Wire enums carry an `Unknown` catch-all so
//! older clients survive new server variants (api-conventions.md).

mod api;
mod bytes;
mod call_direct;
mod call_fallback;
mod call_media;
mod call_recording;
mod calls;
mod canvas_page;
pub mod channel_name;
mod conversation;
pub mod custom_emoji_set;
mod error;
mod gateway;
mod id;
mod import;
mod integration;
mod legal_hold;
pub mod live;
mod log_level;
mod motion;
mod notetaker;
mod permission;
mod poll;
mod rendezvous;
mod role;
pub mod role_rules;
pub mod scim;
pub mod synced_settings;
mod trace_context;
pub mod web_links;
mod wire_enum;

pub use api::*;
pub use bytes::{Base64Bytes, ClientMessageId, ClientMessageIdError, IdempotencyKey, UnixMillis};
pub use call_direct::{
  CallPeerStanding, DirectCandidate, DirectCandidateKind, DirectMediaMode, DirectMediaPolicy,
  DirectOfferBody, MAX_DIRECT_CANDIDATES, MAX_DIRECT_OFFER_BYTES,
};
pub use call_fallback::{
  CALL_FALLBACK_PATH, CALL_FALLBACK_PROTOCOL, FallbackFrame, FallbackFrameError, JOIN_TOKEN_PROTOCOL_PREFIX,
  MAX_FALLBACK_CLOSE_REASON_BYTES, MAX_FALLBACK_DATAGRAM_BYTES, OfferedProtocols, OfferedProtocolsError,
  fallback_protocols, join_token_protocol, parse_offered_protocols,
};
pub use call_media::{
  AudioCodec, AudioFeature, KNOWN_VIDEO_CODECS, LEGACY_SIMULCAST_LAYERS, MAX_SIMULCAST_LAYERS,
  MAX_VIDEO_FRAME_FRAGMENTS, MediaCapabilities, SHORT_VIDEO_FRAME_FRAGMENTS, ScreenShareResolution,
  TransportFeedback, VideoCodec, VideoFeature, VideoLayerInfo, VideoPublicationId, VideoTrack,
  VideoTrackKind,
};
pub use call_recording::{
  CallRecorders, CallRecordingPolicy, DEFAULT_RECORDING_MINUTES, MAX_RECORDING_MINUTES,
  MAX_RECORDING_NOTICE_BYTES, RecorderRole, RecordingFiles,
};
pub use calls::{
  AUDIO_SOCKET_SECRET_BYTES, CALL_JOIN_TOKEN_AUDIENCE, CALL_PROBE_TICKET_AUDIENCE, CallClientMessage,
  CallEvent, CallJoinClaims, CallParticipant, CallProbeClaims, CallServerMessage, CallTokenKind,
  DaveKeyBinding, DaveTransitionId, DaveTransitionMode, MAX_NACK_SEQUENCE_NUMBERS, MediaStreamId,
  SFU_GOING_AWAY_CLOSE_CODE, SFU_SWITCHED_CLOSE_CODE, SWITCH_SECRET_BYTES, ScreenAudioTrack,
  ScreenContentMode, ScreenShareTakeover, UplinkFloor,
};
pub use canvas_page::{CanvasPageId, MAX_LISTED_CANVAS_PAGES, MAX_UNLISTED_CANVAS_PAGES};
pub use conversation::{ConversationKind, ConversationVisibility, HistoryPolicy};
pub use error::{ErrorBody, ErrorCode, SignInReason};
pub use gateway::{
  ATTENTION_AWAY_DELAY_SECS, GATEWAY_PATH, GatewayClientMessage, GatewayEvent, GatewayServerMessage,
};
pub use id::{
  AnnouncementId, AttachmentId, AuditEntryId, BackgroundId, CallId, CategoryId, ConversationId,
  CustomEmojiId, DeviceId, GroupMappingId, HealthNoticeId, Id, IdError, InviteId, MeetingAttendeeId,
  MeetingId, MeetingInviteId, MessageId, MlsMessageId, OrgBackgroundId, PushRetryId, RecoveryRequestId,
  SNOWFLAKE_EPOCH_MS, SNOWFLAKE_SEQUENCE_BITS, SNOWFLAKE_WORKER_BITS, ScheduledMessageId, ScimClientId,
  ScimGroupId, Snowflake, SpaceId, StickerPackId, StickerPictureId, SystemEventId, ThreadId, UserGroupId,
  UserId, WebhookEventId, WebhookId,
};
pub use import::{
  IMPORT_WORKER, ImportDmRequest, ImportMemberStateRequest, ImportMessagesRequest, ImportMessagesResponse,
  ImportPolicy, ImportedMessage, ImportedMessageId, MAX_IMPORT_BATCH, MAX_PROVISION_BATCH, ProvisionStatus,
  ProvisionUsersRequest, ProvisionUsersResponse, ProvisionedUser, next_import_id,
};
pub use integration::{
  DEFAULT_SOURCE_ALERT_LEVELS, DEFAULT_SOURCE_RATE_LIMIT_PER_MINUTE, MAX_SOURCE_ALERT_LEVEL_CHARS,
  MAX_SOURCE_ALERT_LEVELS, MAX_SOURCE_FILES_PER_MESSAGE, MAX_SOURCE_NAME_CHARS, MAX_SOURCE_NAME_WIRE_BYTES,
  MAX_SOURCE_PICTURE_BYTES, MAX_SOURCE_PICTURE_SIDE_PX, MAX_SOURCE_RATE_LIMIT_PER_MINUTE, SourceChange,
  SourceId, SourceIdentity, SourceKind, SourceOptions, SourcePictureError, SourcePictureHash,
  SourceTokenHash, SourceTokenHashError, check_source_picture, clean_source_name,
};
pub use legal_hold::{
  LegalHold, LegalHoldError, MAX_LEGAL_HOLD_REFERENCE_CHARS, MAX_LEGAL_HOLDS, check_legal_holds,
  legal_hold_changes, legal_hold_of,
};
pub use log_level::{LevelMatch, LogLevel, find_level, level_from_number, parse_level_field};
pub use motion::{DEFAULT_MAX_MOTION_BYTES, MAX_MOTION_BYTES_LIMIT, MIN_MOTION_BYTES_LIMIT, MotionLimits};
pub use notetaker::{
  DEFAULT_SUMMARY_MODEL, DEFAULT_SUMMARY_PROMPT, DEFAULT_TRANSCRIPTION_MODEL, LANGUAGE_AUTO, MAX_MODEL_CHARS,
  MAX_PROMPT_CHARS, NotetakerInviters, NotetakerPolicy, NotetakerPolicyError, NotetakerRequest,
  NotetakerScope,
};
pub use permission::{
  ALL_KNOWN as ALL_PERMISSIONS, ChannelContext, ChannelModeration, ChannelRole, Permission, PermissionScheme,
  PermissionScope, SchemeChangeError, SchemeRole, SpaceContext, SpaceRole, Target, check_scheme_change,
  is_allowed,
};
pub use poll::{
  DEFAULT_MAX_POLL_OPEN_DAYS, DEFAULT_MAX_POLL_OPTION_CHARS, DEFAULT_MAX_POLL_OPTIONS,
  DEFAULT_MAX_POLL_QUESTION_CHARS, MAX_POLL_OPEN_DAYS_LIMIT, MAX_RECEIVED_POLL_OPTIONS,
  MAX_RECEIVED_POLL_TEXT_CHARS, MIN_POLL_OPTIONS, PollChoice, PollError, PollLimits, PollResults, PollSpec,
  PollVoters, closing_problem,
};
pub use rendezvous::{RENDEZVOUS_PATH, RendezvousClientMessage, RendezvousServerMessage};
pub use role::Role;
pub use trace_context::{SENTRY_TRACE_HEADER, current_trace_header, route_template, set_trace_header_source};
