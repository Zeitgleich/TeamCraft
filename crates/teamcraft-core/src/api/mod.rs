//! The server as the client core sees it: REST calls and the realtime gateway (docs/gateway.md).
//!
//! [`ServerApi`] has two implementations: [`HttpServerApi`] against the real `teamcraft-server` and
//! [`fake::FakeServer`] in process for tests. Futures are not required to be `Send`, so the same
//! code runs on a single-threaded browser executor.

#[cfg(not(target_arch = "wasm32"))]
mod connect;
pub mod fake;
mod http;
#[cfg(not(target_arch = "wasm32"))]
pub mod proxy;
#[cfg(not(target_arch = "wasm32"))]
mod tls;
#[cfg(target_arch = "wasm32")]
mod web_socket;

use std::future::Future;

use serde::{Deserialize, Serialize};

pub use http::{
  AccessTokenSource, HttpGateway, HttpRendezvous, HttpServerApi, StaticToken, change_web_push_subscription,
};
use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse,
  ConversationId, ConversationMemberResponse, ConversationResponse, CreateChannelRequest, CreateDmRequest,
  CreateGroupDmRequest, DeviceId, DeviceListResponse, DeviceResponse, ErrorBody, ErrorCode,
  GatewayClientMessage, GatewayServerMessage, GetMeResponse, IdentityKeyResponse, KeyBagResponse,
  KeyPackageStatusResponse, ListConversationMembersResponse, ListConversationsResponse, ListDevicesResponse,
  ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse,
  ListSpaceMembersResponse, ListSpacesResponse, MessageResponse, MlsMessageResponse, MoveChannelRequest,
  PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PutDeviceListRequest, PutKeyBagRequest,
  RegisterDeviceRequest, SearchUsersResponse, SendMessageRequest, SpaceId, SpaceMemberResponse,
  UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserSummary,
};
use teamcraft_proto::{
  AppendWorkspaceLogRequest, ListWorkspaceLogQuery, ListWorkspaceLogResponse, WorkspaceLogEntryResponse,
  WorkspaceLogKind,
};
use teamcraft_proto::{
  AttachmentDownloadResponse, AttachmentId, AttachmentResponse, CreateAttachmentRequest,
  CreateAttachmentResponse, CreateShareLinkRequest, RegisterPushRequest, ShareLinkResponse,
};
use teamcraft_proto::{
  BackgroundDownloadResponse, BackgroundId, CreateBackgroundUploadRequest, CreateBackgroundUploadResponse,
  CreateOrgBackgroundUploadRequest, CreateOrgBackgroundUploadResponse, OrgBackgroundDownloadResponse,
  OrgBackgroundId,
};
use teamcraft_proto::{CallJoinTokenResponse, CreateCallJoinTokenRequest};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest, InviteId,
  InviteResponse, ListCategoriesResponse, ListInvitesResponse, SpaceResponse, UpdateCategoryRequest,
  UpdateConversationRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest,
};
use teamcraft_proto::{GatewayInfoResponse, PushGatewayResponse, WebPushKeyResponse};
use teamcraft_proto::{
  ListCallHistoryQuery, ListCallHistoryResponse, ListPresenceResponse, PrivacyResponse, UpdatePrivacyRequest,
};
use teamcraft_proto::{ListThreadsQuery, ListThreadsResponse, MessageId, UnreadResponse};
use teamcraft_proto::{NotificationSettings, TestPushResponse};
use teamcraft_proto::{RendezvousClientMessage, RendezvousServerMessage};
use teamcraft_proto::{SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest};
#[cfg(not(target_arch = "wasm32"))]
pub use tls::set_platform_roots;

/// Everything the sync engine needs from the server.
pub trait ServerApi {
  type Gateway: GatewayConnection;
  type Rendezvous: RendezvousConnection;

  fn me(&self) -> impl Future<Output = Result<GetMeResponse, ApiError>>;
  fn user(&self, user: UserId) -> impl Future<Output = Result<UserSummary, ApiError>>;
  /// The directory (`GET /users?query=`): people whose name or username contains `query`, at most
  /// one page of the server's size. Guests only find people they share a conversation with.
  fn search_users(&self, query: &str) -> impl Future<Output = Result<SearchUsersResponse, ApiError>>;

  fn register_device(
    &self,
    request: &RegisterDeviceRequest,
  ) -> impl Future<Output = Result<DeviceResponse, ApiError>>;
  fn list_user_devices(&self, user: UserId) -> impl Future<Output = Result<ListDevicesResponse, ApiError>>;
  fn put_identity_key(
    &self,
    request: &UploadIdentityKeyRequest,
  ) -> impl Future<Output = Result<IdentityKeyResponse, ApiError>>;
  /// The user's identity key; `None` before they enrolled (or when the server doesn't know them).
  fn identity_key(&self, user: UserId)
  -> impl Future<Output = Result<Option<IdentityKeyResponse>, ApiError>>;
  /// The user's signed device list (security review A4); `None` if they never published one.
  fn device_list(&self, user: UserId) -> impl Future<Output = Result<Option<DeviceListResponse>, ApiError>>;
  fn put_device_list(
    &self,
    request: &PutDeviceListRequest,
  ) -> impl Future<Output = Result<DeviceListResponse, ApiError>>;
  /// Revokes one of the caller's own devices (it can't log in any more).
  fn revoke_device(&self, device: DeviceId) -> impl Future<Output = Result<(), ApiError>>;

  fn upload_key_packages(
    &self,
    device: DeviceId,
    request: &UploadKeyPackagesRequest,
  ) -> impl Future<Output = Result<KeyPackageStatusResponse, ApiError>>;
  fn key_package_status(
    &self,
    device: DeviceId,
  ) -> impl Future<Output = Result<KeyPackageStatusResponse, ApiError>>;
  fn claim_key_packages(
    &self,
    user: UserId,
    request: &ClaimKeyPackagesRequest,
  ) -> impl Future<Output = Result<ClaimKeyPackagesResponse, ApiError>>;

  fn list_spaces(&self) -> impl Future<Output = Result<ListSpacesResponse, ApiError>>;
  fn list_space_members(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<ListSpaceMembersResponse, ApiError>>;
  fn list_conversations(&self) -> impl Future<Output = Result<ListConversationsResponse, ApiError>>;
  fn conversation(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  fn list_members(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<ListConversationMembersResponse, ApiError>>;
  fn create_channel(
    &self,
    space: SpaceId,
    request: &CreateChannelRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  fn create_dm(
    &self,
    request: &CreateDmRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  fn create_group_dm(
    &self,
    request: &CreateGroupDmRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  fn add_member(
    &self,
    conversation: ConversationId,
    request: &AddConversationMemberRequest,
  ) -> impl Future<Output = Result<ConversationMemberResponse, ApiError>>;
  /// Asks whether the caller may remove `user` (or leave), changing nothing
  /// (`GET /conversations/{id}/members/{user}/removal`). Asked before the MLS removal commit.
  fn check_member_removal(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Removes `user` (or leaves); idempotent. Sent after the MLS removal commit (e2ee.md §4.6).
  fn remove_member(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Joins an open space (`POST /spaces/{id}/join`).
  fn join_space(&self, space: SpaceId) -> impl Future<Output = Result<SpaceResponse, ApiError>>;
  /// Adds a person to a space as member or guest (`POST /spaces/{id}/members`). On someone who is
  /// already a member it changes their role, so callers skip existing members.
  fn add_space_member(
    &self,
    space: SpaceId,
    request: &AddSpaceMemberRequest,
  ) -> impl Future<Output = Result<SpaceMemberResponse, ApiError>>;
  /// Joins a public channel (`POST /conversations/{id}/join`).
  fn join_conversation(&self, conversation: ConversationId) -> impl Future<Output = Result<(), ApiError>>;
  /// Renames, converts, archives or recategorizes a channel (`PATCH /conversations/{id}`).
  fn update_conversation(
    &self,
    conversation: ConversationId,
    request: &UpdateConversationRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  /// Moves a channel to another space (`PUT /conversations/{id}/space`).
  fn move_conversation(
    &self,
    conversation: ConversationId,
    request: &MoveChannelRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>>;
  /// Public channels of a space the caller hasn't joined (the channel browser).
  fn list_joinable(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<ListConversationsResponse, ApiError>>;
  fn create_space(
    &self,
    request: &CreateSpaceRequest,
  ) -> impl Future<Output = Result<SpaceResponse, ApiError>>;
  fn update_space(
    &self,
    space: SpaceId,
    request: &UpdateSpaceRequest,
  ) -> impl Future<Output = Result<SpaceResponse, ApiError>>;
  fn list_categories(&self, space: SpaceId)
  -> impl Future<Output = Result<ListCategoriesResponse, ApiError>>;
  fn create_category(
    &self,
    space: SpaceId,
    request: &CreateCategoryRequest,
  ) -> impl Future<Output = Result<CategoryResponse, ApiError>>;
  fn update_category(
    &self,
    space: SpaceId,
    category: CategoryId,
    request: &UpdateCategoryRequest,
  ) -> impl Future<Output = Result<CategoryResponse, ApiError>>;
  fn delete_category(
    &self,
    space: SpaceId,
    category: CategoryId,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Invites someone by email to a space, as member or guest.
  fn create_invite(
    &self,
    space: SpaceId,
    request: &CreateInviteRequest,
  ) -> impl Future<Output = Result<InviteResponse, ApiError>>;
  /// Pending invitations of a space (`GET /spaces/{id}/invites`, member managers only).
  fn list_invites(&self, space: SpaceId) -> impl Future<Output = Result<ListInvitesResponse, ApiError>>;
  /// Withdraws a pending invitation (`DELETE /spaces/{id}/invites/{invite}`).
  fn delete_invite(&self, space: SpaceId, invite: InviteId) -> impl Future<Output = Result<(), ApiError>>;
  /// Changes a space member's role (`PATCH /spaces/{id}/members/{user}`).
  fn update_space_member(
    &self,
    space: SpaceId,
    user: UserId,
    request: &UpdateSpaceMemberRequest,
  ) -> impl Future<Output = Result<SpaceMemberResponse, ApiError>>;
  /// Removes someone from a space and its channels (`DELETE /spaces/{id}/members/{user}`).
  fn remove_space_member(&self, space: SpaceId, user: UserId) -> impl Future<Output = Result<(), ApiError>>;
  /// A member's role, or the caller's own preferences (favorite, notification level).
  fn update_member(
    &self,
    conversation: ConversationId,
    user: UserId,
    request: &UpdateConversationMemberRequest,
  ) -> impl Future<Output = Result<(), ApiError>>;
  fn set_read_position(
    &self,
    conversation: ConversationId,
    request: &SetReadPositionRequest,
  ) -> impl Future<Output = Result<(), ApiError>>;
  fn set_presence(&self, request: &SetPresenceRequest) -> impl Future<Output = Result<(), ApiError>>;
  /// Sets the caller's custom status (`PUT /me/status`); replaces the previous one.
  fn set_custom_status(
    &self,
    request: &teamcraft_proto::SetCustomStatusRequest,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Clears the caller's custom status (`DELETE /me/status`).
  fn clear_custom_status(&self) -> impl Future<Output = Result<(), ApiError>>;
  /// Presence and last seen of some people (`GET /presence`, at most 200).
  fn list_presence(&self, users: &[UserId]) -> impl Future<Output = Result<ListPresenceResponse, ApiError>>;
  /// The caller's privacy choices (`GET /me/privacy`).
  fn get_privacy(&self) -> impl Future<Output = Result<PrivacyResponse, ApiError>>;
  fn put_privacy(
    &self,
    request: &UpdatePrivacyRequest,
  ) -> impl Future<Output = Result<PrivacyResponse, ApiError>>;
  /// The caller's calls, newest first (`GET /calls/history`).
  fn list_call_history(
    &self,
    query: &ListCallHistoryQuery,
  ) -> impl Future<Output = Result<ListCallHistoryResponse, ApiError>>;
  /// A token to join the conversation's call on the SFU (starts the call if none runs).
  fn create_call_join_token(
    &self,
    conversation: ConversationId,
    request: &CreateCallJoinTokenRequest,
  ) -> impl Future<Output = Result<CallJoinTokenResponse, ApiError>>;

  fn send_message(
    &self,
    conversation: ConversationId,
    request: &SendMessageRequest,
  ) -> impl Future<Output = Result<MessageResponse, ApiError>>;
  fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> impl Future<Output = Result<ListMessagesResponse, ApiError>>;

  /// Sets the server's pin flag (pinned messages survive retention). Members learn about pins from
  /// the encrypted pin message.
  fn set_message_pinned(
    &self,
    conversation: ConversationId,
    message: MessageId,
    is_pinned: bool,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Confirms having read a message that asked for acknowledgement.
  fn acknowledge_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> impl Future<Output = Result<(), ApiError>>;

  /// Followed threads, most recently active first (`GET /threads`).
  fn list_threads(
    &self,
    query: &ListThreadsQuery,
  ) -> impl Future<Output = Result<ListThreadsResponse, ApiError>>;
  /// Unread and mention counts (`GET /unread`).
  fn unread(&self) -> impl Future<Output = Result<UnreadResponse, ApiError>>;

  fn post_mls_commit(
    &self,
    conversation: ConversationId,
    request: &PostMlsCommitRequest,
  ) -> impl Future<Output = Result<PostMlsCommitResponse, ApiError>>;
  fn post_mls_message(
    &self,
    conversation: ConversationId,
    request: &PostMlsMessageRequest,
  ) -> impl Future<Output = Result<MlsMessageResponse, ApiError>>;
  fn list_mls_messages(
    &self,
    conversation: ConversationId,
    query: &ListMlsMessagesQuery,
  ) -> impl Future<Output = Result<ListMlsMessagesResponse, ApiError>>;
  fn list_welcomes(
    &self,
    device: DeviceId,
    query: &ListMlsMessagesQuery,
  ) -> impl Future<Output = Result<ListMlsMessagesResponse, ApiError>>;
  /// The group information for the group's current epoch (a new device joins with it).
  fn mls_group_info(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<teamcraft_proto::MlsGroupInfoResponse, ApiError>>;
  fn put_mls_group_info(
    &self,
    conversation: ConversationId,
    request: &teamcraft_proto::PutMlsGroupInfoRequest,
  ) -> impl Future<Output = Result<(), ApiError>>;

  fn put_key_bag(&self, request: &PutKeyBagRequest)
  -> impl Future<Output = Result<KeyBagResponse, ApiError>>;
  fn key_bag(&self) -> impl Future<Output = Result<KeyBagResponse, ApiError>>;

  /// Entries of the roster or policy log after `query.maybe_after`, oldest first.
  fn list_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    query: &ListWorkspaceLogQuery,
  ) -> impl Future<Output = Result<ListWorkspaceLogResponse, ApiError>>;
  /// Appends an owner-signed entry (owners only).
  fn append_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    request: &AppendWorkspaceLogRequest,
  ) -> impl Future<Output = Result<WorkspaceLogEntryResponse, ApiError>>;

  /// Announces an attachment upload; returns the presigned upload URL.
  fn create_attachment(
    &self,
    request: &CreateAttachmentRequest,
  ) -> impl Future<Output = Result<CreateAttachmentResponse, ApiError>>;
  fn complete_attachment(
    &self,
    attachment: AttachmentId,
  ) -> impl Future<Output = Result<AttachmentResponse, ApiError>>;
  fn download_attachment(
    &self,
    attachment: AttachmentId,
  ) -> impl Future<Output = Result<AttachmentDownloadResponse, ApiError>>;
  /// Announces the upload of one of the user's own call backgrounds; returns the presigned URL.
  fn create_background_upload(
    &self,
    request: &CreateBackgroundUploadRequest,
  ) -> impl Future<Output = Result<CreateBackgroundUploadResponse, ApiError>>;
  fn complete_background_upload(
    &self,
    background: BackgroundId,
  ) -> impl Future<Output = Result<(), ApiError>>;
  fn download_background(
    &self,
    background: BackgroundId,
  ) -> impl Future<Output = Result<BackgroundDownloadResponse, ApiError>>;
  fn delete_background(&self, background: BackgroundId) -> impl Future<Output = Result<(), ApiError>>;
  /// Admins: announces an organization background (stored unencrypted, hash in the policy).
  fn create_org_background_upload(
    &self,
    request: &CreateOrgBackgroundUploadRequest,
  ) -> impl Future<Output = Result<CreateOrgBackgroundUploadResponse, ApiError>>;
  fn complete_org_background_upload(
    &self,
    background: OrgBackgroundId,
  ) -> impl Future<Output = Result<(), ApiError>>;
  fn download_org_background(
    &self,
    background: OrgBackgroundId,
  ) -> impl Future<Output = Result<OrgBackgroundDownloadResponse, ApiError>>;
  fn delete_org_background(&self, background: OrgBackgroundId) -> impl Future<Output = Result<(), ApiError>>;
  /// Uploads an encrypted blob to a presigned URL (object storage, not the API).
  fn put_blob(&self, url: &str, bytes: Vec<u8>) -> impl Future<Output = Result<(), ApiError>>;
  /// Downloads an encrypted blob from a presigned URL.
  fn get_blob(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, ApiError>>;
  /// Like [`Self::get_blob`], but stops reading (and fails) once the blob grows past `max_bytes`,
  /// so a hostile server or storage can't make the client buffer an unbounded download.
  fn get_blob_capped(&self, url: &str, max_bytes: u64) -> impl Future<Output = Result<Vec<u8>, ApiError>>;
  /// Makes an attachment downloadable without an account (`POST /shares`; the request names the attachment).
  fn create_share_link(
    &self,
    attachment: AttachmentId,
    request: &CreateShareLinkRequest,
  ) -> impl Future<Output = Result<ShareLinkResponse, ApiError>>;
  fn register_push(
    &self,
    device: DeviceId,
    request: &RegisterPushRequest,
  ) -> impl Future<Output = Result<(), ApiError>>;
  /// Removes the device's push registration (`DELETE /devices/{id}/push`).
  fn unregister_push(&self, device: DeviceId) -> impl Future<Output = Result<(), ApiError>>;
  /// Asks the server to send a test wake-up to the device (`POST /devices/{id}/push/test`).
  fn send_test_push(&self, device: DeviceId) -> impl Future<Output = Result<TestPushResponse, ApiError>>;
  /// The user's notification settings (`GET /me/notification-settings`).
  fn notification_settings(&self) -> impl Future<Output = Result<NotificationSettings, ApiError>>;
  /// How the server reaches its push gateway (`GET /push/gateway`), for sealing device tokens.
  fn push_gateway(&self) -> impl Future<Output = Result<PushGatewayResponse, ApiError>>;
  /// The key browsers subscribe to Web Push with (`GET /push/web-key`).
  fn web_push_key(&self) -> impl Future<Output = Result<WebPushKeyResponse, ApiError>>;
  /// The public information of a push gateway (`GET /v1/info`, no account): its token key. Asked
  /// of the gateway itself, never through the server, so a server can't substitute a key.
  fn gateway_info(&self, gateway_url: &str) -> impl Future<Output = Result<GatewayInfoResponse, ApiError>>;
  /// Ends a sealed device token at its gateway (`POST /v1/tokens/revoke`, no account).
  fn revoke_gateway_token(
    &self,
    gateway_url: &str,
    sealed_token: &[u8],
  ) -> impl Future<Output = Result<(), ApiError>>;

  /// Opens the gateway WebSocket. The caller runs the hello/identify handshake.
  fn connect_gateway(&self) -> impl Future<Output = Result<Self::Gateway, ApiError>>;
  /// Opens the device-linking relay (`/api/v1/rendezvous`). The caller sends `join` first.
  fn connect_rendezvous(&self) -> impl Future<Output = Result<Self::Rendezvous, ApiError>>;
  /// The token for the gateway's `identify`/`resume` frame.
  fn gateway_token(&self) -> Result<String, ApiError>;
}

/// One open gateway connection.
pub trait GatewayConnection {
  fn send(&mut self, message: &GatewayClientMessage) -> impl Future<Output = Result<(), ApiError>>;
  /// The next frame; `None` once the connection is closed.
  fn receive(&mut self) -> impl Future<Output = Option<Result<GatewayServerMessage, ApiError>>>;
}

/// One open device-linking relay connection (docs/gateway.md, "Device-linking relay").
pub trait RendezvousConnection {
  /// A text control frame (`join`).
  fn send_control(&mut self, message: &RendezvousClientMessage)
  -> impl Future<Output = Result<(), ApiError>>;
  /// A binary frame, relayed verbatim to the other device.
  fn send_binary(&mut self, frame: Vec<u8>) -> impl Future<Output = Result<(), ApiError>>;
  /// The next frame; `None` once the connection is closed.
  fn receive(&mut self) -> impl Future<Output = Option<Result<RendezvousFrame, ApiError>>>;
}

/// What arrives on the relay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendezvousFrame {
  Control(RendezvousServerMessage),
  Binary(Vec<u8>),
}

/// What answered instead of the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interception {
  /// A login or terms page of the network (hotel, train, guest Wi-Fi): an HTML page where the API
  /// answers JSON, a redirect to another site, or HTTP 511.
  CaptivePortal,
  /// The TLS certificate isn't trusted: usually a proxy that inspects encrypted traffic with a
  /// company CA this device doesn't trust.
  UntrustedCertificate,
}

impl std::fmt::Display for Interception {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self {
      Self::CaptivePortal => "captive portal",
      Self::UntrustedCertificate => "untrusted certificate",
    })
  }
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
  /// The server could not be reached; the request may be retried later.
  #[error("network error: {0}")]
  Network(String),
  /// Something on the way answered instead of the server: a captive portal or a TLS-intercepting
  /// proxy (issue #157). Retried like [`Self::Network`]; the UI says what the user can do.
  #[error("the network intercepted the connection ({kind}): {detail}")]
  Intercepted { kind: Interception, detail: String },
  #[error("server answered {status}: {}", .maybe_body.as_ref().map_or("", |body| body.message.as_str()))]
  Status {
    status: u16,
    maybe_body: Option<ErrorBody>,
  },
  #[error("unreadable server response: {0}")]
  Decode(String),
  #[error("gateway protocol error: {0}")]
  Gateway(String),
  #[error("no access token: {0}")]
  Token(String),
  #[error("not supported on this platform yet: {0}")]
  Unsupported(&'static str),
  /// A downloaded blob was larger than it may be.
  #[error("the download is larger than {max_bytes} bytes")]
  BlobTooLarge { max_bytes: u64 },
  /// The server URL is not `https://` (plain `http://` is only allowed to a loopback address, for
  /// development): no request or token is sent.
  #[error("the server address must use https: {0}")]
  InsecureServerUrl(String),
}

impl ApiError {
  pub fn error_code(&self) -> Option<&ErrorCode> {
    match self {
      Self::Status {
        maybe_body: Some(body),
        ..
      } => Some(&body.error_code),
      _ => None,
    }
  }

  /// Another member's commit won this epoch (first commit wins); fetch the backlog and rebase.
  pub fn is_epoch_conflict(&self) -> bool {
    self.error_code() == Some(&ErrorCode::EpochConflict)
  }

  pub fn is_version_conflict(&self) -> bool {
    self.error_code() == Some(&ErrorCode::VersionConflict)
  }

  pub fn is_not_found(&self) -> bool {
    matches!(self, Self::Status { status: 404, .. })
  }

  pub fn is_forbidden(&self) -> bool {
    matches!(self, Self::Status { status: 403, .. })
  }

  /// The request never reached the server: offline, or intercepted on the way.
  pub fn is_unreachable(&self) -> bool {
    matches!(self, Self::Network(_) | Self::Intercepted { .. })
  }

  /// What intercepted the connection, if something did.
  pub fn interception(&self) -> Option<Interception> {
    match self {
      Self::Intercepted { kind, .. } => Some(*kind),
      _ => None,
    }
  }

  /// Worth retrying later without changing the request (offline, server overloaded, a captive
  /// portal the user has yet to sign in to).
  pub fn is_transient(&self) -> bool {
    match self {
      Self::Network(_) | Self::Intercepted { .. } => true,
      Self::Status { status, .. } => *status == 429 || *status >= 500,
      _ => false,
    }
  }

  /// Worth retrying once the app has a fresh access token, too: after sleep the token has often
  /// expired before the app refreshed it. Queued work (outbox, uploads) waits instead of failing
  /// for good; the app handles the signed-out state itself.
  pub fn is_retry_later(&self) -> bool {
    self.is_transient() || self.is_sign_in_required()
  }

  /// No access token, or the server refused it: retrying with the same token won't help, the user
  /// (or the app's token refresh) has to sign in again.
  pub fn is_sign_in_required(&self) -> bool {
    matches!(self, Self::Token(_) | Self::Status { status: 401, .. })
  }

  pub(crate) fn status(status: u16, code: ErrorCode, message: impl Into<String>) -> Self {
    Self::Status {
      status,
      maybe_body: Some(ErrorBody::new(code, message)),
    }
  }
}
