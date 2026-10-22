//! The server as the client core sees it: REST calls and the realtime gateway (docs/gateway.md).
//!
//! [`ServerApi`] has two implementations: [`HttpServerApi`] against the real `teamcraft-server` and
//! [`fake::FakeServer`] in process for tests. Futures are not required to be `Send`, so the same
//! code runs on a single-threaded browser executor.

pub mod fake;
mod http;

use std::future::Future;

pub use http::{AccessTokenSource, HttpGateway, HttpServerApi, StaticToken};
use teamcraft_proto::{
  AddConversationMemberRequest, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ConversationId,
  ConversationMemberResponse, ConversationResponse, CreateChannelRequest, CreateDmRequest, DeviceId,
  DeviceResponse, ErrorBody, ErrorCode, GatewayClientMessage, GatewayServerMessage, GetMeResponse,
  IdentityKeyResponse, KeyBagResponse, KeyPackageStatusResponse, ListConversationMembersResponse,
  ListConversationsResponse, ListDevicesResponse, ListMessagesQuery, ListMessagesResponse,
  ListMlsMessagesQuery, ListMlsMessagesResponse, ListSpacesResponse, MessageResponse, MlsMessageResponse,
  PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PutKeyBagRequest,
  RegisterDeviceRequest, SendMessageRequest, SpaceId, UploadIdentityKeyRequest, UploadKeyPackagesRequest,
  UserId, UserSummary,
};
use teamcraft_proto::{SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest};

/// Everything the sync engine needs from the server.
pub trait ServerApi {
  type Gateway: GatewayConnection;

  fn me(&self) -> impl Future<Output = Result<GetMeResponse, ApiError>>;
  fn user(&self, user: UserId) -> impl Future<Output = Result<UserSummary, ApiError>>;

  fn register_device(
    &self,
    request: &RegisterDeviceRequest,
  ) -> impl Future<Output = Result<DeviceResponse, ApiError>>;
  fn list_user_devices(&self, user: UserId) -> impl Future<Output = Result<ListDevicesResponse, ApiError>>;
  fn put_identity_key(
    &self,
    request: &UploadIdentityKeyRequest,
  ) -> impl Future<Output = Result<IdentityKeyResponse, ApiError>>;
  fn identity_key(&self, user: UserId) -> impl Future<Output = Result<IdentityKeyResponse, ApiError>>;

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
  fn add_member(
    &self,
    conversation: ConversationId,
    request: &AddConversationMemberRequest,
  ) -> impl Future<Output = Result<ConversationMemberResponse, ApiError>>;
  fn remove_member(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>>;
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

  fn put_key_bag(&self, request: &PutKeyBagRequest)
  -> impl Future<Output = Result<KeyBagResponse, ApiError>>;
  fn key_bag(&self) -> impl Future<Output = Result<KeyBagResponse, ApiError>>;

  /// Opens the gateway WebSocket. The caller runs the hello/identify handshake.
  fn connect_gateway(&self) -> impl Future<Output = Result<Self::Gateway, ApiError>>;
  /// The token for the gateway's `identify`/`resume` frame.
  fn gateway_token(&self) -> Result<String, ApiError>;
}

/// One open gateway connection.
pub trait GatewayConnection {
  fn send(&mut self, message: &GatewayClientMessage) -> impl Future<Output = Result<(), ApiError>>;
  /// The next frame; `None` once the connection is closed.
  fn receive(&mut self) -> impl Future<Output = Option<Result<GatewayServerMessage, ApiError>>>;
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
  /// The server could not be reached; the request may be retried later.
  #[error("network error: {0}")]
  Network(String),
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

  /// Worth retrying later without changing the request (offline, server overloaded).
  pub fn is_transient(&self) -> bool {
    match self {
      Self::Network(_) => true,
      Self::Status { status, .. } => *status == 429 || *status >= 500,
      _ => false,
    }
  }

  pub(crate) fn status(status: u16, code: ErrorCode, message: impl Into<String>) -> Self {
    Self::Status {
      status,
      maybe_body: Some(ErrorBody::new(code, message)),
    }
  }
}
