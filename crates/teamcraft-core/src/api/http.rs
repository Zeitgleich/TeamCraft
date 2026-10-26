//! [`ServerApi`] over HTTP (reqwest; `fetch` in the browser) and the gateway WebSocket
//! (tokio-tungstenite on native targets, the browser's `WebSocket` in wasm).

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use teamcraft_proto::RendezvousClientMessage;
use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse,
  ConversationId, ConversationMemberResponse, ConversationResponse, CreateChannelRequest, CreateDmRequest,
  CreateGroupDmRequest, DeviceId, DeviceResponse, ErrorBody, GatewayClientMessage, GatewayServerMessage,
  GetMeResponse, IdentityKeyResponse, InviteId, KeyBagResponse, KeyPackageStatusResponse,
  ListAdminConversationsResponse, ListConversationMembersResponse, ListConversationsResponse,
  ListDevicesResponse, ListInvitesResponse, ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery,
  ListMlsMessagesResponse, ListSpaceMembersResponse, ListSpacesResponse, MessageResponse, MlsMessageResponse,
  MoveChannelRequest, PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PutKeyBagRequest,
  RegisterDeviceRequest, SearchUsersResponse, SendMessageRequest, SpaceId, SpaceMemberResponse,
  UpdateSpaceMemberRequest, UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserSummary,
};
use teamcraft_proto::{
  AppendWorkspaceLogRequest, ListWorkspaceLogQuery, ListWorkspaceLogResponse, WorkspaceLogEntryResponse,
  WorkspaceLogKind,
};
use teamcraft_proto::{
  AttachmentDownloadResponse, AttachmentId, AttachmentPartUrlResponse, AttachmentResponse,
  CompleteMultipartAttachmentRequest, CreateAttachmentRequest, CreateAttachmentResponse,
  CreateMultipartAttachmentResponse, CreateShareLinkRequest, ListShareLinksResponse,
  PresignAttachmentPartRequest, RegisterPushRequest, ShareLinkResponse,
};
use teamcraft_proto::{
  BackgroundDownloadResponse, BackgroundId, CreateBackgroundUploadRequest, CreateBackgroundUploadResponse,
  CreateOrgBackgroundUploadRequest, CreateOrgBackgroundUploadResponse, OrgBackgroundDownloadResponse,
  OrgBackgroundId,
};
use teamcraft_proto::{
  Base64Bytes, GatewayInfoResponse, GatewayRevokeRequest, PushGatewayResponse, WebPushKeyResponse,
};
use teamcraft_proto::{
  CallJoinTokenResponse, ChangeWebPushSubscriptionRequest, CreateCallJoinTokenRequest, MoveCallRequest,
};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest,
  InviteResponse, ListCategoriesResponse, SpaceResponse, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};
use teamcraft_proto::{CreateInstantMeetingRequest, MeetingId, MeetingResponse};
use teamcraft_proto::{DeviceListResponse, PutDeviceListRequest, UserEnrollmentResponse};
use teamcraft_proto::{
  ListCallHistoryQuery, ListCallHistoryResponse, ListPresenceResponse, PrivacyResponse, UpdatePrivacyRequest,
};
use teamcraft_proto::{ListThreadsQuery, ListThreadsResponse, MessageId, UnreadResponse};
use teamcraft_proto::{NotificationSettings, TestPushResponse};
use teamcraft_proto::{SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest};

#[cfg(target_arch = "wasm32")]
use super::web_socket::{BrowserSocket, SocketFrame};
use super::{
  ApiError, DetachedDownload, DetachedPart, DetachedRequest, GatewayConnection, Interception,
  RendezvousConnection, RendezvousFrame, ServerApi,
};
use crate::clock::{Clock, SystemClock};
use crate::transfers::ByteCounter;
use teamcraft_proto::{CreateDeviceLinkRequest, DeviceLinkRequest, ListDeviceLinksResponse};

/// How long a TCP and TLS (or WebSocket) handshake may take.
#[cfg(not(target_arch = "wasm32"))]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Keepalive probes and HTTP/2 pings on open connections, below the idle timeout of common proxies.
#[cfg(not(target_arch = "wasm32"))]
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
#[cfg(not(target_arch = "wasm32"))]
const KEEPALIVE_RETRIES: u32 = 3;
/// The whole of a JSON API call (not blob transfers, which may be slow on a slow link). On macOS,
/// iOS and Windows a request on a connection that died mid-flight would otherwise hang until the OS
/// gives up: keepalive only probes idle sockets and there is no `TCP_USER_TIMEOUT`.
#[cfg(not(target_arch = "wasm32"))]
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long sent data may stay unacknowledged before the connection counts as dead.
#[cfg(any(target_os = "android", target_os = "linux"))]
const UNACKNOWLEDGED_TIMEOUT: Duration = Duration::from_secs(45);

/// Silence after which the gateway connection counts as dead: two heartbeat intervals (25 s, plus
/// up to one runner tick) and room for a slow round trip. The apps' `ConnectionSupervisor` detects
/// silence itself, with the interval from `hello`; this is the transport's backstop for clients that
/// drive the connection on their own (the integration bridge).
const GATEWAY_SILENCE_LIMIT_MS: i64 = 50_000;

/// Supplies a valid OIDC access token. The app owns login and refresh; the core only asks.
pub trait AccessTokenSource {
  fn access_token(&self) -> Result<String, ApiError>;
}

/// A fixed token (tests, command-line tools).
pub struct StaticToken(pub String);

/// Waits until the local store has persisted what was written so far. Hosts whose store writes
/// asynchronously (the browser's IndexedDB) set it, so nothing the server is told about can be
/// lost locally when the page dies right after (an MLS commit, uploaded key packages).
#[cfg(target_arch = "wasm32")]
pub type WriteBarrier = std::rc::Rc<dyn Fn() -> futures_util::future::LocalBoxFuture<'static, ()>>;

pub struct HttpServerApi<T> {
  client: reqwest::Client,
  /// Awaited before every request that changes server state.
  #[cfg(target_arch = "wasm32")]
  maybe_write_barrier: Option<WriteBarrier>,
  base_url: String,
  /// Why `base_url` is refused, checked once in [`HttpServerApi::new`].
  maybe_insecure: Option<String>,
  tokens: T,
  /// Set once `/enrollment` answered a bare 404 (a server from before the endpoint): later calls
  /// go to the single endpoints directly instead of probing first.
  lacks_enrollment_endpoint: AtomicBool,
}

/// What `GET /users/{id}/enrollment` answered.
enum Enrollment {
  Known(UserEnrollmentResponse),
  /// A server from before the endpoint: the single endpoints answer instead (404 for nothing).
  NoEndpoint,
}

impl<T: AccessTokenSource> HttpServerApi<T> {
  /// `base_url` without trailing slash, e.g. `https://chat.example.com`.
  pub fn new(base_url: impl Into<String>, tokens: T) -> Self {
    let client = http_client();
    let base_url = base_url.into().trim_end_matches('/').to_string();
    Self {
      client,
      maybe_insecure: insecure_reason(&base_url),
      base_url,
      tokens,
      lacks_enrollment_endpoint: AtomicBool::new(false),
      #[cfg(target_arch = "wasm32")]
      maybe_write_barrier: None,
    }
  }

  /// Awaits `barrier` before every request that changes server state.
  #[cfg(target_arch = "wasm32")]
  pub fn with_write_barrier(mut self, barrier: WriteBarrier) -> Self {
    self.maybe_write_barrier = Some(barrier);
    self
  }

  /// Local writes are durable before the server hears of anything that follows from them.
  async fn before_change(&self, method: &Method) {
    #[cfg(target_arch = "wasm32")]
    if *method != Method::GET
      && *method != Method::HEAD
      && let Some(barrier) = &self.maybe_write_barrier
    {
      barrier().await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = method;
  }

  fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ApiError> {
    self.require_secure_transport()?;
    let token = self.tokens.access_token()?;
    let request = self
      .client
      .request(method, format!("{}/api/v1{path}", self.base_url))
      .bearer_auth(token);
    #[cfg(not(target_arch = "wasm32"))]
    let request = request.timeout(API_REQUEST_TIMEOUT);
    Ok(request)
  }

  async fn get<R: DeserializeOwned>(&self, path: &str) -> Result<R, ApiError> {
    decode(send(self.request(Method::GET, path)?).await?).await
  }

  /// A user the server doesn't show the caller (404 with an error code) has published nothing, as
  /// the single endpoints said; a bare 404 is a server that doesn't have the endpoint yet.
  async fn enrollment(&self, user: UserId) -> Result<Enrollment, ApiError> {
    if self.lacks_enrollment_endpoint.load(Ordering::Relaxed) {
      return Ok(Enrollment::NoEndpoint);
    }
    match self.get(&format!("/users/{user}/enrollment")).await {
      Ok(enrollment) => Ok(Enrollment::Known(enrollment)),
      Err(error) if error.is_not_found() && error.error_code().is_none() => {
        self.lacks_enrollment_endpoint.store(true, Ordering::Relaxed);
        Ok(Enrollment::NoEndpoint)
      },
      Err(error) if error.is_not_found() => Ok(Enrollment::Known(UserEnrollmentResponse::default())),
      Err(error) => Err(error),
    }
  }

  async fn send_json<B: Serialize, R: DeserializeOwned>(
    &self,
    method: Method,
    path: &str,
    body: &B,
  ) -> Result<R, ApiError> {
    self.before_change(&method).await;
    decode(send(self.request(method, path)?.json(body)).await?).await
  }

  async fn send_without_response<B: Serialize>(
    &self,
    method: Method,
    path: &str,
    maybe_body: Option<&B>,
  ) -> Result<(), ApiError> {
    self.before_change(&method).await;
    let mut request = self.request(method, path)?;
    if let Some(body) = maybe_body {
      request = request.json(body);
    }
    send(request).await.map(|_| ())
  }

  /// An authenticated request to `/api/v1{path}` with a JSON body, answered with the raw JSON
  /// text (empty for no content): for shells that call plain server endpoints (notification
  /// settings, meetings) without a typed method here. `method` is an HTTP verb.
  pub async fn raw_request(
    &self,
    method: &str,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<String, ApiError> {
    if !path.starts_with('/') || path.contains("..") {
      return Err(ApiError::Decode(format!("not an API path: {path}")));
    }
    let method =
      Method::from_bytes(method.as_bytes()).map_err(|error| ApiError::Decode(error.to_string()))?;
    self.before_change(&method).await;
    let mut request = self.request(method, path)?;
    if let Some(body) = maybe_body {
      request = request
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body);
    }
    send(request)
      .await?
      .text()
      .await
      .map_err(|error| ApiError::Decode(error.to_string()))
  }

  fn gateway_url(&self) -> String {
    self.websocket_url("/api/v1/gateway")
  }

  /// Tokens and key material only travel over TLS; plain http only to a loopback dev server.
  fn require_secure_transport(&self) -> Result<(), ApiError> {
    match &self.maybe_insecure {
      Some(reason) => Err(ApiError::InsecureServerUrl(reason.clone())),
      None => Ok(()),
    }
  }

  fn websocket_url(&self, path: &str) -> String {
    let websocket_base = if let Some(rest) = self.base_url.strip_prefix("https://") {
      format!("wss://{rest}")
    } else if let Some(rest) = self.base_url.strip_prefix("http://") {
      format!("ws://{rest}")
    } else {
      self.base_url.clone()
    };
    format!("{websocket_base}{path}")
  }
}

impl<T: AccessTokenSource> ServerApi for HttpServerApi<T> {
  type Gateway = HttpGateway;
  type Rendezvous = HttpRendezvous;

  async fn me(&self) -> Result<GetMeResponse, ApiError> {
    self.get("/me").await
  }

  async fn user(&self, user: UserId) -> Result<UserSummary, ApiError> {
    self.get(&format!("/users/{user}")).await
  }

  async fn search_users(&self, query: &str) -> Result<SearchUsersResponse, ApiError> {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    self.get(&format!("/users?query={encoded}")).await
  }

  async fn register_device(&self, request: &RegisterDeviceRequest) -> Result<DeviceResponse, ApiError> {
    self.send_json(Method::POST, "/devices", request).await
  }

  async fn list_user_devices(&self, user: UserId) -> Result<ListDevicesResponse, ApiError> {
    self.get(&format!("/users/{user}/devices")).await
  }

  async fn put_identity_key(
    &self,
    request: &UploadIdentityKeyRequest,
  ) -> Result<IdentityKeyResponse, ApiError> {
    self.send_json(Method::PUT, "/me/identity-key", request).await
  }

  async fn identity_key(&self, user: UserId) -> Result<Option<IdentityKeyResponse>, ApiError> {
    match self.enrollment(user).await? {
      Enrollment::Known(enrollment) => Ok(enrollment.maybe_identity_key),
      Enrollment::NoEndpoint => found(self.get(&format!("/users/{user}/identity-key")).await),
    }
  }

  async fn device_list(&self, user: UserId) -> Result<Option<DeviceListResponse>, ApiError> {
    match self.enrollment(user).await? {
      Enrollment::Known(enrollment) => Ok(enrollment.maybe_device_list),
      Enrollment::NoEndpoint => found(self.get(&format!("/users/{user}/device-list")).await),
    }
  }

  async fn put_device_list(&self, request: &PutDeviceListRequest) -> Result<DeviceListResponse, ApiError> {
    self.send_json(Method::PUT, "/me/device-list", request).await
  }

  async fn revoke_device(&self, device: DeviceId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/devices/{device}"), None)
      .await
  }

  async fn announce_device_link(
    &self,
    request: &CreateDeviceLinkRequest,
  ) -> Result<DeviceLinkRequest, ApiError> {
    self.send_json(Method::POST, "/device-links", request).await
  }

  async fn device_links(&self) -> Result<Vec<DeviceLinkRequest>, ApiError> {
    Ok(
      self
        .get::<ListDeviceLinksResponse>("/device-links")
        .await?
        .requests,
    )
  }

  async fn reject_device_link(&self, request_id: &str) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::POST, &format!("/device-links/{request_id}/reject"), None)
      .await
  }

  async fn withdraw_device_link(&self, request_id: &str) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/device-links/{request_id}"), None)
      .await
  }

  async fn upload_key_packages(
    &self,
    device: DeviceId,
    request: &UploadKeyPackagesRequest,
  ) -> Result<KeyPackageStatusResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/devices/{device}/key-packages"), request)
      .await
  }

  async fn key_package_status(&self, device: DeviceId) -> Result<KeyPackageStatusResponse, ApiError> {
    self.get(&format!("/devices/{device}/key-packages")).await
  }

  async fn claim_key_packages(
    &self,
    user: UserId,
    request: &ClaimKeyPackagesRequest,
  ) -> Result<ClaimKeyPackagesResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/users/{user}/key-packages/claim"),
        request,
      )
      .await
  }

  async fn list_spaces(&self) -> Result<ListSpacesResponse, ApiError> {
    self.get("/spaces").await
  }

  async fn list_space_members(&self, space: SpaceId) -> Result<ListSpaceMembersResponse, ApiError> {
    self.get(&format!("/spaces/{space}/members")).await
  }

  async fn list_conversations(&self) -> Result<ListConversationsResponse, ApiError> {
    self.get("/conversations").await
  }

  async fn conversation(&self, conversation: ConversationId) -> Result<ConversationResponse, ApiError> {
    self.get(&format!("/conversations/{conversation}")).await
  }

  async fn list_members(
    &self,
    conversation: ConversationId,
  ) -> Result<ListConversationMembersResponse, ApiError> {
    self.get(&format!("/conversations/{conversation}/members")).await
  }

  async fn create_channel(
    &self,
    space: SpaceId,
    request: &CreateChannelRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/spaces/{space}/channels"), request)
      .await
  }

  async fn create_dm(&self, request: &CreateDmRequest) -> Result<ConversationResponse, ApiError> {
    self.send_json(Method::POST, "/dms", request).await
  }

  async fn create_group_dm(&self, request: &CreateGroupDmRequest) -> Result<ConversationResponse, ApiError> {
    self.send_json(Method::POST, "/group-dms", request).await
  }

  async fn add_member(
    &self,
    conversation: ConversationId,
    request: &AddConversationMemberRequest,
  ) -> Result<ConversationMemberResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/conversations/{conversation}/members"),
        request,
      )
      .await
  }

  async fn check_member_removal(&self, conversation: ConversationId, user: UserId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::GET,
        &format!("/conversations/{conversation}/members/{user}/removal"),
        None,
      )
      .await
  }

  async fn remove_member(&self, conversation: ConversationId, user: UserId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::DELETE,
        &format!("/conversations/{conversation}/members/{user}"),
        None,
      )
      .await
  }

  async fn admin_channels_of(&self, user: UserId) -> Result<Vec<ConversationId>, ApiError> {
    let mut channels = Vec::new();
    loop {
      let after = channels
        .last()
        .map(|last: &ConversationId| format!("&after={last}"))
        .unwrap_or_default();
      let page: ListAdminConversationsResponse = self
        .get(&format!("/admin/conversations?user_id={user}{after}"))
        .await?;
      channels.extend(page.conversations.iter().map(|channel| channel.id));
      if !page.has_more || page.conversations.is_empty() {
        return Ok(channels);
      }
    }
  }

  async fn admin_remove_from_chats(&self, user: UserId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::POST,
        &format!("/admin/users/{user}/remove-from-chats"),
        None,
      )
      .await
  }

  async fn update_member(
    &self,
    conversation: ConversationId,
    user: UserId,
    request: &UpdateConversationMemberRequest,
  ) -> Result<(), ApiError> {
    let path = format!("/conversations/{conversation}/members/{user}");
    self
      .send_without_response(Method::PATCH, &path, Some(request))
      .await
  }

  async fn set_read_position(
    &self,
    conversation: ConversationId,
    request: &SetReadPositionRequest,
  ) -> Result<(), ApiError> {
    let path = format!("/conversations/{conversation}/read-position");
    self
      .send_without_response(Method::PUT, &path, Some(request))
      .await
  }

  async fn set_thread_read(
    &self,
    conversation: ConversationId,
    thread: teamcraft_proto::ThreadId,
    request: &teamcraft_proto::SetThreadReadRequest,
  ) -> Result<(), ApiError> {
    let path = format!("/conversations/{conversation}/threads/{thread}/read");
    self
      .send_without_response(Method::PUT, &path, Some(request))
      .await
  }

  async fn set_presence(&self, request: &SetPresenceRequest) -> Result<(), ApiError> {
    self
      .send_without_response(Method::PUT, "/me/presence", Some(request))
      .await
  }

  async fn set_custom_status(
    &self,
    request: &teamcraft_proto::SetCustomStatusRequest,
  ) -> Result<(), ApiError> {
    self
      .send_without_response(Method::PUT, "/me/status", Some(request))
      .await
  }

  async fn clear_custom_status(&self) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, "/me/status", None)
      .await
  }

  async fn list_presence(&self, users: &[UserId]) -> Result<ListPresenceResponse, ApiError> {
    let ids: Vec<String> = users.iter().map(ToString::to_string).collect();
    let parameters = query_string(&[("user_ids", Some(ids.join(",")))]);
    self.get(&format!("/presence{parameters}")).await
  }

  async fn get_privacy(&self) -> Result<PrivacyResponse, ApiError> {
    self.get("/me/privacy").await
  }

  async fn put_privacy(&self, request: &UpdatePrivacyRequest) -> Result<PrivacyResponse, ApiError> {
    self.send_json(Method::PUT, "/me/privacy", request).await
  }

  async fn list_call_history(
    &self,
    query: &ListCallHistoryQuery,
  ) -> Result<ListCallHistoryResponse, ApiError> {
    let parameters = query_string(&[
      ("before", query.maybe_before.map(|id| id.to_string())),
      ("limit", query.maybe_limit.map(|limit| limit.to_string())),
    ]);
    self.get(&format!("/calls/history{parameters}")).await
  }

  async fn create_call_join_token(
    &self,
    conversation: ConversationId,
    request: &CreateCallJoinTokenRequest,
  ) -> Result<CallJoinTokenResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/conversations/{conversation}/call/join-token"),
        request,
      )
      .await
  }

  async fn create_instant_meeting(
    &self,
    request: &CreateInstantMeetingRequest,
  ) -> Result<MeetingResponse, ApiError> {
    self.send_json(Method::POST, "/meetings/instant", request).await
  }

  async fn cancel_meeting(&self, meeting: MeetingId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/meetings/{meeting}"), None)
      .await
  }

  async fn move_call(&self, conversation: ConversationId, request: &MoveCallRequest) -> Result<(), ApiError> {
    let path = format!("/conversations/{conversation}/call/move");
    self
      .send_without_response(Method::POST, &path, Some(request))
      .await
  }

  async fn send_message(
    &self,
    conversation: ConversationId,
    request: &SendMessageRequest,
  ) -> Result<MessageResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/conversations/{conversation}/messages"),
        request,
      )
      .await
  }

  async fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> Result<ListMessagesResponse, ApiError> {
    let parameters = query_string(&[
      ("before", query.maybe_before.map(|id| id.to_string())),
      ("after", query.maybe_after.map(|id| id.to_string())),
      ("limit", query.maybe_limit.map(|limit| limit.to_string())),
    ]);
    self
      .get(&format!("/conversations/{conversation}/messages{parameters}"))
      .await
  }

  async fn post_mls_commit(
    &self,
    conversation: ConversationId,
    request: &PostMlsCommitRequest,
  ) -> Result<PostMlsCommitResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/conversations/{conversation}/mls/commits"),
        request,
      )
      .await
  }

  async fn post_mls_message(
    &self,
    conversation: ConversationId,
    request: &PostMlsMessageRequest,
  ) -> Result<MlsMessageResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/conversations/{conversation}/mls/messages"),
        request,
      )
      .await
  }

  async fn list_mls_messages(
    &self,
    conversation: ConversationId,
    query: &ListMlsMessagesQuery,
  ) -> Result<ListMlsMessagesResponse, ApiError> {
    let parameters = mls_query(query);
    self
      .get(&format!("/conversations/{conversation}/mls{parameters}"))
      .await
  }

  async fn list_welcomes(
    &self,
    device: DeviceId,
    query: &ListMlsMessagesQuery,
  ) -> Result<ListMlsMessagesResponse, ApiError> {
    let parameters = mls_query(query);
    self.get(&format!("/devices/{device}/welcomes{parameters}")).await
  }

  async fn mls_group_info(
    &self,
    conversation: ConversationId,
  ) -> Result<teamcraft_proto::MlsGroupInfoResponse, ApiError> {
    self
      .get(&format!("/conversations/{conversation}/mls/group-info"))
      .await
  }

  async fn put_mls_group_info(
    &self,
    conversation: ConversationId,
    request: &teamcraft_proto::PutMlsGroupInfoRequest,
  ) -> Result<(), ApiError> {
    self
      .send_without_response(
        Method::PUT,
        &format!("/conversations/{conversation}/mls/group-info"),
        Some(request),
      )
      .await
  }

  async fn put_key_bag(&self, request: &PutKeyBagRequest) -> Result<KeyBagResponse, ApiError> {
    self.send_json(Method::PUT, "/me/key-bag", request).await
  }

  async fn join_space(&self, space: SpaceId) -> Result<SpaceResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/spaces/{space}/join"),
        &serde_json::json!({}),
      )
      .await
  }

  async fn add_space_member(
    &self,
    space: SpaceId,
    request: &AddSpaceMemberRequest,
  ) -> Result<SpaceMemberResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/spaces/{space}/members"), request)
      .await
  }

  async fn join_conversation(&self, conversation: ConversationId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::POST, &format!("/conversations/{conversation}/join"), None)
      .await
  }

  async fn update_conversation(
    &self,
    conversation: ConversationId,
    request: &UpdateConversationRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self
      .send_json(Method::PATCH, &format!("/conversations/{conversation}"), request)
      .await
  }

  async fn move_conversation(
    &self,
    conversation: ConversationId,
    request: &MoveChannelRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self
      .send_json(
        Method::PUT,
        &format!("/conversations/{conversation}/space"),
        request,
      )
      .await
  }

  async fn list_joinable(&self, space: SpaceId) -> Result<ListConversationsResponse, ApiError> {
    self
      .get(&format!("/conversations?space_id={space}&joinable=true"))
      .await
  }

  async fn create_space(&self, request: &CreateSpaceRequest) -> Result<SpaceResponse, ApiError> {
    self.send_json(Method::POST, "/spaces", request).await
  }

  async fn update_space(
    &self,
    space: SpaceId,
    request: &UpdateSpaceRequest,
  ) -> Result<SpaceResponse, ApiError> {
    self
      .send_json(Method::PATCH, &format!("/spaces/{space}"), request)
      .await
  }

  async fn list_categories(&self, space: SpaceId) -> Result<ListCategoriesResponse, ApiError> {
    self.get(&format!("/spaces/{space}/categories")).await
  }

  async fn create_category(
    &self,
    space: SpaceId,
    request: &CreateCategoryRequest,
  ) -> Result<CategoryResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/spaces/{space}/categories"), request)
      .await
  }

  async fn update_category(
    &self,
    space: SpaceId,
    category: CategoryId,
    request: &UpdateCategoryRequest,
  ) -> Result<CategoryResponse, ApiError> {
    self
      .send_json(
        Method::PATCH,
        &format!("/spaces/{space}/categories/{category}"),
        request,
      )
      .await
  }

  async fn delete_category(&self, space: SpaceId, category: CategoryId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::DELETE,
        &format!("/spaces/{space}/categories/{category}"),
        None,
      )
      .await
  }

  async fn create_invite(
    &self,
    space: SpaceId,
    request: &CreateInviteRequest,
  ) -> Result<InviteResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/spaces/{space}/invites"), request)
      .await
  }

  async fn list_invites(&self, space: SpaceId) -> Result<ListInvitesResponse, ApiError> {
    self.get(&format!("/spaces/{space}/invites")).await
  }

  async fn delete_invite(&self, space: SpaceId, invite: InviteId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/spaces/{space}/invites/{invite}"), None)
      .await
  }

  async fn update_space_member(
    &self,
    space: SpaceId,
    user: UserId,
    request: &UpdateSpaceMemberRequest,
  ) -> Result<SpaceMemberResponse, ApiError> {
    self
      .send_json(Method::PATCH, &format!("/spaces/{space}/members/{user}"), request)
      .await
  }

  async fn remove_space_member(&self, space: SpaceId, user: UserId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/spaces/{space}/members/{user}"), None)
      .await
  }

  async fn set_message_pinned(
    &self,
    conversation: ConversationId,
    message: MessageId,
    is_pinned: bool,
  ) -> Result<(), ApiError> {
    let method = if is_pinned { Method::PUT } else { Method::DELETE };
    self
      .send_without_response::<()>(
        method,
        &format!("/conversations/{conversation}/messages/{message}/pin"),
        None,
      )
      .await
  }

  async fn acknowledge_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::PUT,
        &format!("/conversations/{conversation}/messages/{message}/acknowledgement"),
        None,
      )
      .await
  }

  async fn list_threads(&self, query: &ListThreadsQuery) -> Result<ListThreadsResponse, ApiError> {
    let parameters = query_string(&[
      ("unread_only", query.unread_only.then(|| "true".to_string())),
      ("before", query.maybe_before.map(|id| id.to_string())),
      ("limit", query.maybe_limit.map(|limit| limit.to_string())),
    ]);
    self.get(&format!("/threads{parameters}")).await
  }

  async fn unread(&self) -> Result<UnreadResponse, ApiError> {
    self.get("/unread").await
  }

  async fn read_receipts(
    &self,
    conversation: ConversationId,
  ) -> Result<teamcraft_proto::ReadReceiptsResponse, ApiError> {
    self
      .get(&format!("/conversations/{conversation}/read-receipts"))
      .await
  }

  async fn key_bag(&self) -> Result<KeyBagResponse, ApiError> {
    self.get("/me/key-bag").await
  }

  async fn list_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    query: &ListWorkspaceLogQuery,
  ) -> Result<ListWorkspaceLogResponse, ApiError> {
    let parameters = query_string(&[
      ("after", query.maybe_after.map(|version| version.to_string())),
      ("limit", query.maybe_limit.map(|limit| limit.to_string())),
    ]);
    self
      .get(&format!("{}{parameters}", workspace_log_path(kind, false)))
      .await
  }

  async fn append_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    request: &AppendWorkspaceLogRequest,
  ) -> Result<WorkspaceLogEntryResponse, ApiError> {
    self
      .send_json(Method::POST, workspace_log_path(kind, true), request)
      .await
  }

  async fn create_attachment(
    &self,
    request: &CreateAttachmentRequest,
  ) -> Result<CreateAttachmentResponse, ApiError> {
    self.send_json(Method::POST, "/attachments", request).await
  }

  async fn complete_attachment(&self, attachment: AttachmentId) -> Result<AttachmentResponse, ApiError> {
    self.before_change(&Method::POST).await;
    decode(send(self.request(Method::POST, &format!("/attachments/{attachment}/complete"))?).await?).await
  }

  async fn create_multipart_attachment(
    &self,
    request: &CreateAttachmentRequest,
  ) -> Result<CreateMultipartAttachmentResponse, ApiError> {
    self
      .send_json(Method::POST, "/attachments/multipart", request)
      .await
  }

  async fn presign_attachment_part(
    &self,
    attachment: AttachmentId,
    request: &PresignAttachmentPartRequest,
  ) -> Result<AttachmentPartUrlResponse, ApiError> {
    self
      .send_json(Method::POST, &format!("/attachments/{attachment}/parts"), request)
      .await
  }

  async fn complete_multipart_attachment(
    &self,
    attachment: AttachmentId,
    request: &CompleteMultipartAttachmentRequest,
  ) -> Result<AttachmentResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/attachments/{attachment}/multipart/complete"),
        request,
      )
      .await
  }

  async fn abandon_attachment(&self, attachment: AttachmentId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/attachments/{attachment}"), None)
      .await
  }

  async fn download_attachment(
    &self,
    attachment: AttachmentId,
  ) -> Result<AttachmentDownloadResponse, ApiError> {
    self.get(&format!("/attachments/{attachment}/download")).await
  }

  async fn create_background_upload(
    &self,
    request: &CreateBackgroundUploadRequest,
  ) -> Result<CreateBackgroundUploadResponse, ApiError> {
    self.send_json(Method::POST, "/me/backgrounds", request).await
  }

  async fn complete_background_upload(&self, background: BackgroundId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::POST,
        &format!("/me/backgrounds/{background}/complete"),
        None,
      )
      .await
  }

  async fn download_background(
    &self,
    background: BackgroundId,
  ) -> Result<BackgroundDownloadResponse, ApiError> {
    self.get(&format!("/me/backgrounds/{background}/download")).await
  }

  async fn delete_background(&self, background: BackgroundId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/me/backgrounds/{background}"), None)
      .await
  }

  async fn create_org_background_upload(
    &self,
    request: &CreateOrgBackgroundUploadRequest,
  ) -> Result<CreateOrgBackgroundUploadResponse, ApiError> {
    self.send_json(Method::POST, "/admin/backgrounds", request).await
  }

  async fn complete_org_background_upload(&self, background: OrgBackgroundId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(
        Method::POST,
        &format!("/admin/backgrounds/{background}/complete"),
        None,
      )
      .await
  }

  async fn download_org_background(
    &self,
    background: OrgBackgroundId,
  ) -> Result<OrgBackgroundDownloadResponse, ApiError> {
    self.get(&format!("/org-backgrounds/{background}/download")).await
  }

  async fn delete_org_background(&self, background: OrgBackgroundId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/admin/backgrounds/{background}"), None)
      .await
  }

  async fn put_blob(&self, url: &str, bytes: Vec<u8>) -> Result<(), ApiError> {
    // Presigned: the URL carries the authorization, so no bearer token goes to object storage.
    send(self.client.put(url).body(bytes)).await.map(|_| ())
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn put_blob_detached(&self, url: &str, bytes: Vec<u8>, sent: ByteCounter) -> DetachedRequest {
    // Presigned URLs want the exact length; a streamed body would otherwise go chunked.
    let request = self
      .client
      .put(url)
      .header(reqwest::header::CONTENT_LENGTH, bytes.len())
      .body(counted_body(bytes, sent));
    Box::pin(async move { send(request).await.map(|_| ()) })
  }

  #[cfg(target_arch = "wasm32")]
  fn put_blob_detached(&self, url: &str, bytes: Vec<u8>, sent: ByteCounter) -> DetachedRequest {
    let url = url.to_owned();
    Box::pin(async move { super::xhr::put(&url, bytes, sent).await })
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn put_part_detached(&self, url: &str, bytes: Vec<u8>, sent: ByteCounter) -> DetachedPart {
    let request = self
      .client
      .put(url)
      .header(reqwest::header::CONTENT_LENGTH, bytes.len())
      .body(counted_body(bytes, sent));
    Box::pin(async move {
      let response = send(request).await?;
      response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::Decode("the storage answered a part without an ETag".into()))
    })
  }

  #[cfg(target_arch = "wasm32")]
  fn put_part_detached(&self, url: &str, bytes: Vec<u8>, sent: ByteCounter) -> DetachedPart {
    let url = url.to_owned();
    Box::pin(async move { super::xhr::put_part(&url, bytes, sent).await })
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn get_blob_detached(
    &self,
    url: &str,
    maybe_max_bytes: Option<u64>,
    received: ByteCounter,
  ) -> DetachedDownload {
    let request = self.client.get(url);
    Box::pin(async move {
      let mut response = send(request).await?;
      let max_bytes = maybe_max_bytes.unwrap_or(u64::MAX);
      if response.content_length().is_some_and(|length| length > max_bytes) {
        return Err(ApiError::BlobTooLarge { max_bytes });
      }
      let mut blob = Vec::new();
      while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ApiError::Network(error.to_string()))?
      {
        if blob.len() as u64 + chunk.len() as u64 > max_bytes {
          return Err(ApiError::BlobTooLarge { max_bytes });
        }
        blob.extend_from_slice(&chunk);
        received.add(chunk.len() as u64);
      }
      Ok(blob)
    })
  }

  #[cfg(target_arch = "wasm32")]
  fn get_blob_detached(
    &self,
    url: &str,
    maybe_max_bytes: Option<u64>,
    received: ByteCounter,
  ) -> DetachedDownload {
    let url = url.to_owned();
    Box::pin(async move { super::xhr::get(&url, maybe_max_bytes, received).await })
  }

  async fn get_blob(&self, url: &str) -> Result<Vec<u8>, ApiError> {
    let response = send(self.client.get(url)).await?;
    response
      .bytes()
      .await
      .map(|bytes| bytes.to_vec())
      .map_err(|error| ApiError::Network(error.to_string()))
  }

  async fn get_blob_capped(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, ApiError> {
    let response = send(self.client.get(url)).await?;
    if response.content_length().is_some_and(|length| length > max_bytes) {
      return Err(ApiError::BlobTooLarge { max_bytes });
    }
    read_capped(response, max_bytes).await
  }

  async fn create_share_link(
    &self,
    _attachment: AttachmentId,
    request: &CreateShareLinkRequest,
  ) -> Result<ShareLinkResponse, ApiError> {
    self.send_json(Method::POST, "/shares", request).await
  }

  async fn revoke_share_link(&self, share_id: &str) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/shares/{share_id}"), None)
      .await
  }

  async fn list_share_links(&self) -> Result<ListShareLinksResponse, ApiError> {
    self.get("/shares").await
  }

  async fn register_push(&self, device: DeviceId, request: &RegisterPushRequest) -> Result<(), ApiError> {
    self
      .send_without_response(Method::PUT, &format!("/devices/{device}/push"), Some(request))
      .await
  }

  async fn unregister_push(&self, device: DeviceId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/devices/{device}/push"), None)
      .await
  }

  async fn send_test_push(&self, device: DeviceId) -> Result<TestPushResponse, ApiError> {
    self
      .send_json(
        Method::POST,
        &format!("/devices/{device}/push/test"),
        &serde_json::json!({}),
      )
      .await
  }

  async fn notification_settings(&self) -> Result<NotificationSettings, ApiError> {
    self.get("/me/notification-settings").await
  }

  async fn push_gateway(&self) -> Result<PushGatewayResponse, ApiError> {
    self.get("/push/gateway").await
  }

  async fn web_push_key(&self) -> Result<WebPushKeyResponse, ApiError> {
    self.get("/push/web-key").await
  }

  async fn gateway_info(&self, gateway_url: &str) -> Result<GatewayInfoResponse, ApiError> {
    let base = gateway_base(gateway_url)?;
    decode(send(self.client.get(format!("{base}/v1/info"))).await?).await
  }

  async fn revoke_gateway_token(&self, gateway_url: &str, sealed_token: &[u8]) -> Result<(), ApiError> {
    let base = gateway_base(gateway_url)?;
    let body = GatewayRevokeRequest {
      bound_token: Base64Bytes::new(sealed_token.to_vec()),
    };
    send(self.client.post(format!("{base}/v1/tokens/revoke")).json(&body))
      .await
      .map(drop)
  }

  async fn connect_gateway(&self) -> Result<HttpGateway, ApiError> {
    self.require_secure_transport()?;
    HttpGateway::connect(&self.gateway_url()).await
  }

  async fn connect_rendezvous(&self) -> Result<HttpRendezvous, ApiError> {
    self.require_secure_transport()?;
    HttpRendezvous::connect(&self.websocket_url("/api/v1/rendezvous")).await
  }

  fn gateway_token(&self) -> Result<String, ApiError> {
    self.tokens.access_token()
  }
}

/// Sends the service worker's proof for a rotated Web Push subscription (e2ee.md §7). No access
/// token: the request authenticates itself (`ChangeWebPushSubscriptionRequest::mac`).
pub async fn change_web_push_subscription(
  server_url: &str,
  request: &ChangeWebPushSubscriptionRequest,
) -> Result<(), ApiError> {
  let base_url = server_url.trim_end_matches('/');
  if let Some(reason) = insecure_reason(base_url) {
    return Err(ApiError::InsecureServerUrl(reason));
  }
  let builder = http_client()
    .post(format!("{base_url}/api/v1/push/web/subscription-change"))
    .json(request);
  #[cfg(not(target_arch = "wasm32"))]
  let builder = builder.timeout(API_REQUEST_TIMEOUT);
  send(builder).await.map(drop)
}

/// The push gateway's base URL without trailing slash; device tokens go to it, so it must be
/// `https://` (or loopback, for development) like the server.
fn gateway_base(gateway_url: &str) -> Result<String, ApiError> {
  let base = gateway_url.trim_end_matches('/').to_owned();
  match insecure_reason(&base) {
    Some(reason) => Err(ApiError::InsecureServerUrl(reason)),
    None => Ok(base),
  }
}

/// `None` if `base_url` may carry tokens: `https://`, or `http://` to a loopback address.
fn insecure_reason(base_url: &str) -> Option<String> {
  let Ok(url) = url::Url::parse(base_url) else {
    return Some(format!("{base_url:?} is not a URL"));
  };
  let is_loopback = match url.host() {
    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
    Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
    None => false,
  };
  match url.scheme() {
    "https" => None,
    "http" if is_loopback => None,
    scheme => Some(format!("{scheme}:// to {}", url.host_str().unwrap_or(""))),
  }
}

impl<T> fmt::Debug for HttpServerApi<T> {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("HttpServerApi")
      .field("base_url", &self.base_url)
      .finish_non_exhaustive()
  }
}

impl AccessTokenSource for StaticToken {
  fn access_token(&self) -> Result<String, ApiError> {
    Ok(self.0.clone())
  }
}

impl fmt::Debug for StaticToken {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("StaticToken([redacted])")
  }
}

/// Reads a body chunk by chunk and stops as soon as it passes `max_bytes`.
#[cfg(not(target_arch = "wasm32"))]
async fn read_capped(mut response: reqwest::Response, max_bytes: u64) -> Result<Vec<u8>, ApiError> {
  let mut blob = Vec::new();
  while let Some(chunk) = response
    .chunk()
    .await
    .map_err(|error| ApiError::Network(error.to_string()))?
  {
    if blob.len() as u64 + chunk.len() as u64 > max_bytes {
      return Err(ApiError::BlobTooLarge { max_bytes });
    }
    blob.extend_from_slice(&chunk);
  }
  Ok(blob)
}

/// An upload body in chunks of 64 KiB, each counted into `sent` as the connection takes it.
#[cfg(not(target_arch = "wasm32"))]
fn counted_body(bytes: Vec<u8>, sent: ByteCounter) -> reqwest::Body {
  const CHUNK_BYTES: usize = 64 * 1024;
  let length = bytes.len();
  let chunks = futures_util::stream::iter((0..length).step_by(CHUNK_BYTES).map(move |start| {
    let end = start.saturating_add(CHUNK_BYTES).min(length);
    let chunk = bytes[start..end].to_vec();
    sent.add(chunk.len() as u64);
    Ok::<_, std::io::Error>(chunk)
  }));
  reqwest::Body::wrap_stream(chunks)
}

/// The browser's fetch has no chunked reads in reqwest, so the body is read whole; the
/// Content-Length check before it already refuses announced oversized bodies.
#[cfg(target_arch = "wasm32")]
async fn read_capped(response: reqwest::Response, max_bytes: u64) -> Result<Vec<u8>, ApiError> {
  let blob = response
    .bytes()
    .await
    .map_err(|error| ApiError::Network(error.to_string()))?;
  if blob.len() as u64 > max_bytes {
    return Err(ApiError::BlobTooLarge { max_bytes });
  }
  Ok(blob.to_vec())
}

/// The gateway WebSocket on native targets.
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpGateway {
  socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
  liveness: Liveness,
}

/// The gateway WebSocket in the browser.
#[cfg(target_arch = "wasm32")]
pub struct HttpGateway {
  socket: BrowserSocket,
  liveness: Liveness,
}

/// When the server was last heard from. Every client heartbeat is answered, so a connection that
/// stays silent for [`GATEWAY_SILENCE_LIMIT_MS`] is dead even if the socket never reports an error
/// (a half-open TCP connection after a network change). Wall-clock time on purpose: after a laptop
/// sleeps, the jump marks the connection as stale at once.
#[derive(Clone, Copy, Debug)]
struct Liveness {
  last_heard_ms: i64,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpGateway {
  async fn connect(url: &str) -> Result<Self, ApiError> {
    Ok(Self {
      socket: connect_websocket(url).await?,
      liveness: Liveness::new(SystemClock.now_ms()),
    })
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl GatewayConnection for HttpGateway {
  async fn send(&mut self, message: &GatewayClientMessage) -> Result<(), ApiError> {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

    self.liveness.check(SystemClock.now_ms())?;
    let text = serde_json::to_string(message).map_err(|error| ApiError::Decode(error.to_string()))?;
    self
      .socket
      .send(Message::text(text))
      .await
      .map_err(|error| ApiError::Network(error.to_string()))
  }

  async fn receive(&mut self) -> Option<Result<GatewayServerMessage, ApiError>> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;

    loop {
      let frame = self.socket.next().await?;
      self.liveness.heard(SystemClock.now_ms());
      match frame {
        Ok(Message::Text(text)) => {
          return Some(
            serde_json::from_str(text.as_str()).map_err(|error| ApiError::Decode(error.to_string())),
          );
        },
        Ok(Message::Close(_)) => return None,
        // Pings are answered by tungstenite; binary frames aren't used by the gateway.
        Ok(_) => {},
        Err(error) => return Some(Err(ApiError::Network(error.to_string()))),
      }
    }
  }
}

#[cfg(target_arch = "wasm32")]
impl HttpGateway {
  async fn connect(url: &str) -> Result<Self, ApiError> {
    Ok(Self {
      socket: BrowserSocket::connect(url).await?,
      liveness: Liveness::new(SystemClock.now_ms()),
    })
  }
}

#[cfg(target_arch = "wasm32")]
impl GatewayConnection for HttpGateway {
  async fn send(&mut self, message: &GatewayClientMessage) -> Result<(), ApiError> {
    self.liveness.check(SystemClock.now_ms())?;
    let text = serde_json::to_string(message).map_err(|error| ApiError::Decode(error.to_string()))?;
    self.socket.send_text(&text)
  }

  async fn receive(&mut self) -> Option<Result<GatewayServerMessage, ApiError>> {
    loop {
      let frame = self.socket.next().await?;
      self.liveness.heard(SystemClock.now_ms());
      match frame {
        Ok(SocketFrame::Text(text)) => {
          return Some(serde_json::from_str(&text).map_err(|error| ApiError::Decode(error.to_string())));
        },
        // The gateway doesn't use binary frames.
        Ok(SocketFrame::Binary(_)) => {},
        Err(error) => return Some(Err(error)),
      }
    }
  }
}

impl Liveness {
  fn new(now_ms: i64) -> Self {
    Self {
      last_heard_ms: now_ms,
    }
  }

  fn heard(&mut self, now_ms: i64) {
    self.last_heard_ms = now_ms;
  }

  /// Fails once the server has been silent for too long, so the caller reconnects as it does after
  /// any failed send.
  fn check(&self, now_ms: i64) -> Result<(), ApiError> {
    let silent_ms = now_ms.saturating_sub(self.last_heard_ms);
    if silent_ms > GATEWAY_SILENCE_LIMIT_MS {
      return Err(ApiError::Network(format!(
        "no frame from the gateway for {} s",
        silent_ms / 1000
      )));
    }
    Ok(())
  }
}

impl fmt::Debug for HttpGateway {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("HttpGateway")
  }
}

/// The device-linking relay on native targets.
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpRendezvous {
  socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

/// The device-linking relay in the browser.
#[cfg(target_arch = "wasm32")]
pub struct HttpRendezvous {
  socket: BrowserSocket,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpRendezvous {
  async fn connect(url: &str) -> Result<Self, ApiError> {
    Ok(Self {
      socket: connect_websocket(url).await?,
    })
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl RendezvousConnection for HttpRendezvous {
  async fn send_control(&mut self, message: &RendezvousClientMessage) -> Result<(), ApiError> {
    use futures_util::SinkExt;
    let text = serde_json::to_string(message).map_err(|error| ApiError::Decode(error.to_string()))?;
    self
      .socket
      .send(tokio_tungstenite::tungstenite::Message::text(text))
      .await
      .map_err(|error| ApiError::Network(error.to_string()))
  }

  async fn send_binary(&mut self, frame: Vec<u8>) -> Result<(), ApiError> {
    use futures_util::SinkExt;
    self
      .socket
      .send(tokio_tungstenite::tungstenite::Message::binary(frame))
      .await
      .map_err(|error| ApiError::Network(error.to_string()))
  }

  async fn receive(&mut self) -> Option<Result<RendezvousFrame, ApiError>> {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    loop {
      match self.socket.next().await? {
        Ok(Message::Text(text)) => {
          return Some(
            serde_json::from_str(text.as_str())
              .map(RendezvousFrame::Control)
              .map_err(|error| ApiError::Decode(error.to_string())),
          );
        },
        Ok(Message::Binary(bytes)) => return Some(Ok(RendezvousFrame::Binary(bytes.to_vec()))),
        Ok(Message::Close(_)) => return None,
        Ok(_) => {},
        Err(error) => return Some(Err(ApiError::Network(error.to_string()))),
      }
    }
  }
}

#[cfg(target_arch = "wasm32")]
impl HttpRendezvous {
  async fn connect(url: &str) -> Result<Self, ApiError> {
    Ok(Self {
      socket: BrowserSocket::connect(url).await?,
    })
  }
}

#[cfg(target_arch = "wasm32")]
impl RendezvousConnection for HttpRendezvous {
  async fn send_control(&mut self, message: &RendezvousClientMessage) -> Result<(), ApiError> {
    let text = serde_json::to_string(message).map_err(|error| ApiError::Decode(error.to_string()))?;
    self.socket.send_text(&text)
  }

  async fn send_binary(&mut self, frame: Vec<u8>) -> Result<(), ApiError> {
    self.socket.send_binary(&frame)
  }

  async fn receive(&mut self) -> Option<Result<RendezvousFrame, ApiError>> {
    Some(match self.socket.next().await? {
      Ok(SocketFrame::Text(text)) => serde_json::from_str(&text)
        .map(RendezvousFrame::Control)
        .map_err(|error| ApiError::Decode(error.to_string())),
      Ok(SocketFrame::Binary(bytes)) => Ok(RendezvousFrame::Binary(bytes)),
      Err(error) => Err(error),
    })
  }
}

impl fmt::Debug for HttpRendezvous {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("HttpRendezvous")
  }
}

/// The HTTP client of the native apps. reqwest has no timeouts by default, so a request on a
/// connection that died silently (Wi-Fi to mobile handover, laptop sleep) would hang until the
/// kernel gives up, which takes many minutes. There is deliberately no total or read timeout: a big
/// upload on a slow link is slow, not dead. Instead a connection is dropped when it stops making
/// progress (robustness.md, "Network loss and change").
#[cfg(not(target_arch = "wasm32"))]
fn http_client() -> reqwest::Client {
  match http_client_builder().build() {
    Ok(client) => client,
    // Only a reqwest/rustls version mismatch can cause this; the default client still works for
    // plain HTTP (development) and reports TLS errors per request.
    Err(error) => {
      tracing::error!(%error, "HTTP client configuration rejected, using the default client");
      reqwest::Client::new()
    },
  }
}

/// reqwest races IPv6 and IPv4 itself (hyper-util's Happy Eyeballs, 300 ms; issue #420), with the
/// connect timeout split across a family's addresses.
#[cfg(not(target_arch = "wasm32"))]
fn http_client_builder() -> reqwest::ClientBuilder {
  let builder = reqwest::Client::builder()
    .connect_timeout(CONNECT_TIMEOUT)
    .tcp_keepalive(KEEPALIVE_INTERVAL)
    .tcp_keepalive_interval(KEEPALIVE_INTERVAL)
    .tcp_keepalive_retries(KEEPALIVE_RETRIES)
    .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
    .http2_keep_alive_timeout(KEEPALIVE_INTERVAL)
    .http2_keep_alive_while_idle(true);
  // Data that the peer doesn't acknowledge for this long means the connection is gone, even while a
  // request is in flight (keepalive probes only run on an idle socket).
  #[cfg(any(target_os = "android", target_os = "linux"))]
  let builder = builder.tcp_user_timeout(UNACKNOWLEDGED_TIMEOUT);
  #[cfg(target_os = "android")]
  let builder = builder.tls_backend_preconfigured((*super::tls::client_config()).clone());
  // The same proxy as the WebSockets, also one the mobile app set (api/proxy.rs).
  builder
    .proxy(reqwest::Proxy::custom(super::proxy::proxy_for))
    .redirect(reqwest::redirect::Policy::custom(follow_redirect))
}

/// Redirects are followed up to [`MAX_REDIRECTS`] hops, but never from `https` down to `http`: a
/// presigned URL or a token-bearing request must not leave TLS because a server said so.
#[cfg(not(target_arch = "wasm32"))]
fn follow_redirect(attempt: reqwest::redirect::Attempt<'_>) -> reqwest::redirect::Action {
  if attempt.previous().len() >= MAX_REDIRECTS {
    return attempt.stop();
  }
  let comes_from_https = attempt
    .previous()
    .last()
    .is_some_and(|previous| previous.scheme() == "https");
  if is_downgrade(comes_from_https, attempt.url().scheme()) {
    return attempt.error("refusing a redirect from https to http");
  }
  attempt.follow()
}

#[cfg(not(target_arch = "wasm32"))]
fn is_downgrade(comes_from_https: bool, next_scheme: &str) -> bool {
  comes_from_https && next_scheme != "https"
}

/// reqwest's own default.
#[cfg(not(target_arch = "wasm32"))]
const MAX_REDIRECTS: usize = 10;

/// The browser's `fetch` has its own timeouts and connection handling.
#[cfg(target_arch = "wasm32")]
fn http_client() -> reqwest::Client {
  reqwest::Client::new()
}

#[cfg(not(target_arch = "wasm32"))]
type NativeSocket =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens a WebSocket: TCP with Happy Eyeballs, through the system or environment proxy with
/// `CONNECT` if one applies (issue #157), then TLS and the WebSocket handshake.
#[cfg(not(target_arch = "wasm32"))]
async fn connect_websocket(url: &str) -> Result<NativeSocket, ApiError> {
  // A black-holed server or proxy would otherwise stall the caller's loop (outbox, heartbeats) for
  // minutes.
  let parsed = url::Url::parse(url).map_err(|error| ApiError::Network(format!("{url}: {error}")))?;
  let maybe_proxy = super::proxy::tunnel_proxy_for(&parsed);
  tokio::time::timeout(CONNECT_TIMEOUT, open_websocket(&parsed, maybe_proxy.as_ref()))
    .await
    .map_err(|_| ApiError::Network("WebSocket connection timed out".into()))?
}

#[cfg(not(target_arch = "wasm32"))]
async fn open_websocket(url: &url::Url, maybe_proxy: Option<&url::Url>) -> Result<NativeSocket, ApiError> {
  let stream = super::connect::connect_via(url, maybe_proxy)
    .await
    .map_err(|error| ApiError::Network(error.to_string()))?;
  // Always an explicit configuration: tungstenite's default one panics in rustls (api/tls.rs).
  let maybe_connector = Some(tokio_tungstenite::Connector::Rustls(super::tls::client_config()));
  let (socket, _) =
    tokio_tungstenite::client_async_tls_with_config(url.as_str(), stream, None, maybe_connector)
      .await
      .map_err(websocket_error)?;
  Ok(socket)
}

/// A failed WebSocket handshake, told apart like [`network_error`]: a refused certificate, or an
/// HTTP answer instead of the upgrade that looks like a captive portal (a page, a redirect, 511).
#[cfg(not(target_arch = "wasm32"))]
fn websocket_error(error: tokio_tungstenite::tungstenite::Error) -> ApiError {
  use tokio_tungstenite::tungstenite::Error as WsError;
  if super::tls::is_certificate_rejection(&error) {
    return ApiError::Intercepted {
      kind: Interception::UntrustedCertificate,
      detail: error_chain(&error),
    };
  }
  if let WsError::Http(response) = &error {
    let status = response.status();
    let is_portal = status.is_success()
      || status.is_redirection()
      || status == reqwest::StatusCode::NETWORK_AUTHENTICATION_REQUIRED;
    if is_portal {
      return ApiError::Intercepted {
        kind: Interception::CaptivePortal,
        detail: format!("HTTP {} instead of a WebSocket", status.as_u16()),
      };
    }
  }
  ApiError::Network(error_chain(&error))
}

async fn send(request: RequestBuilder) -> Result<Response, ApiError> {
  let (client, request) = request.build_split();
  let request = request.map_err(|error| ApiError::Network(error.to_string()))?;
  let origin = request.url().origin();
  let response = client.execute(request).await.map_err(network_error)?;
  let status = response.status();
  if let Some(detail) = captive_portal_sign(&origin, &response) {
    return Err(ApiError::Intercepted {
      kind: Interception::CaptivePortal,
      detail,
    });
  }
  if status.is_success() {
    return Ok(response);
  }
  let maybe_body = response.json::<ErrorBody>().await.ok();
  Err(ApiError::Status {
    status: status.as_u16(),
    maybe_body,
  })
}

/// Why `response` comes from a captive portal rather than the server, if it does: HTTP 511, or a
/// successful answer that is an HTML page (the API and object storage never answer HTML), named
/// with the site it was redirected to. A redirect alone is no sign: object storage may hand a blob
/// over to a CDN. Error pages of the server's own reverse proxy (a 502 in HTML) are server trouble,
/// not a portal.
fn captive_portal_sign(origin: &url::Origin, response: &Response) -> Option<String> {
  let status = response.status();
  if status == reqwest::StatusCode::NETWORK_AUTHENTICATION_REQUIRED {
    return Some("HTTP 511 network authentication required".into());
  }
  if !status.is_success() {
    return None;
  }
  if !is_html(response.headers()) {
    return None;
  }
  if response.url().origin() != *origin {
    let host = response.url().host_str().unwrap_or_default();
    return Some(format!("redirected to an HTML page on {host}"));
  }
  Some(format!("HTTP {} with an HTML page", status.as_u16()))
}

fn is_html(headers: &reqwest::header::HeaderMap) -> bool {
  headers
    .get(reqwest::header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .is_some_and(|value| {
      let essence = value.split(';').next().unwrap_or_default().trim();
      essence.eq_ignore_ascii_case("text/html") || essence.eq_ignore_ascii_case("application/xhtml+xml")
    })
}

/// A request that got no answer: a certificate the device refuses means interception (or a server
/// that isn't who it claims), everything else is the network.
fn network_error(error: reqwest::Error) -> ApiError {
  #[cfg(not(target_arch = "wasm32"))]
  if super::tls::is_certificate_rejection(&error) {
    return ApiError::Intercepted {
      kind: Interception::UntrustedCertificate,
      detail: error_chain(&error),
    };
  }
  ApiError::Network(error_chain(&error))
}

/// The error and its causes, so the details name the actual reason ("certificate expired") and not
/// only reqwest's "error sending request".
fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
  let mut text = error.to_string();
  let mut maybe_source = error.source();
  while let Some(source) = maybe_source {
    let cause = source.to_string();
    if !text.contains(&cause) {
      text.push_str(": ");
      text.push_str(&cause);
    }
    maybe_source = source.source();
  }
  text
}

/// The single endpoints' 404: nothing published.
fn found<R>(result: Result<R, ApiError>) -> Result<Option<R>, ApiError> {
  match result {
    Ok(response) => Ok(Some(response)),
    Err(error) if error.is_not_found() => Ok(None),
    Err(error) => Err(error),
  }
}

async fn decode<R: DeserializeOwned>(response: Response) -> Result<R, ApiError> {
  response
    .json()
    .await
    .map_err(|error| ApiError::Decode(error.to_string()))
}

/// The roster log is read and appended at one path; the policy log is appended at
/// `/workspace/policy` and listed at `/workspace/policy/log`.
fn workspace_log_path(kind: &WorkspaceLogKind, is_append: bool) -> &'static str {
  match (kind, is_append) {
    (WorkspaceLogKind::Policy, true) => "/workspace/policy",
    (WorkspaceLogKind::Policy, false) => "/workspace/policy/log",
    _ => "/workspace/roster",
  }
}

fn mls_query(query: &ListMlsMessagesQuery) -> String {
  query_string(&[
    ("after", query.maybe_after.map(|id| id.to_string())),
    ("limit", query.maybe_limit.map(|limit| limit.to_string())),
  ])
}

/// `?a=1&b=2` from the present parameters. Values are IDs and numbers, so no escaping is needed.
fn query_string(parameters: &[(&str, Option<String>)]) -> String {
  let pairs: Vec<String> = parameters
    .iter()
    .filter_map(|(name, maybe_value)| maybe_value.as_ref().map(|value| format!("{name}={value}")))
    .collect();
  if pairs.is_empty() {
    String::new()
  } else {
    format!("?{}", pairs.join("&"))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(not(target_arch = "wasm32"))]
  #[test]
  fn a_redirect_never_leaves_https() {
    assert!(is_downgrade(true, "http"));
    assert!(!is_downgrade(true, "https"));
    assert!(!is_downgrade(false, "http"));
  }

  #[test]
  fn query_string_skips_absent_parameters() {
    assert_eq!(
      query_string(&[("before", None), ("limit", Some("50".into()))]),
      "?limit=50"
    );
    assert_eq!(query_string(&[("after", None)]), "");
  }

  #[test]
  fn gateway_url_follows_the_scheme() {
    let api = HttpServerApi::new("https://chat.example.com/", StaticToken("t".into()));
    assert_eq!(api.gateway_url(), "wss://chat.example.com/api/v1/gateway");
    let api = HttpServerApi::new("http://127.0.0.1:8081", StaticToken("t".into()));
    assert_eq!(api.gateway_url(), "ws://127.0.0.1:8081/api/v1/gateway");
  }

  #[test]
  fn only_https_or_loopback_http_carries_tokens() {
    for allowed in [
      "https://chat.example.com",
      "http://127.0.0.1:8081",
      "http://[::1]:8081",
      "http://localhost:8080",
    ] {
      assert_eq!(insecure_reason(allowed), None, "{allowed}");
    }
    for refused in [
      "http://chat.example.com",
      "http://10.0.0.5",
      "http://localhost.example.com",
      "ftp://chat.example.com",
      "chat.example.com",
      "",
    ] {
      assert!(insecure_reason(refused).is_some(), "{refused}");
    }
  }

  #[cfg(not(target_arch = "wasm32"))]
  #[tokio::test]
  async fn a_plain_http_server_gets_no_request_and_no_token() {
    let api = HttpServerApi::new("http://chat.example.com", PanickingToken);
    assert!(matches!(
      api.get::<serde_json::Value>("/me").await,
      Err(ApiError::InsecureServerUrl(_))
    ));
    assert!(matches!(
      api.connect_gateway().await,
      Err(ApiError::InsecureServerUrl(_))
    ));
  }

  #[cfg(not(target_arch = "wasm32"))]
  #[tokio::test(start_paused = true)]
  async fn a_server_that_never_answers_the_websocket_handshake_times_out() {
    let listener = loopback_listener().await;
    let address = listener.local_addr().unwrap();
    // Accepts the TCP connection and then stays silent, like a black-holed proxy.
    tokio::spawn(async move {
      let (_socket, _) = listener.accept().await.unwrap();
      std::future::pending::<()>().await;
    });
    // Paused time jumps ahead while everything waits, so the timeout fires without a real wait.
    let result = connect_websocket(&format!("ws://{address}/api/v1/gateway")).await;
    assert!(matches!(result, Err(ApiError::Network(message)) if message.contains("timed out")));
  }

  #[cfg(not(target_arch = "wasm32"))]
  #[tokio::test(start_paused = true)]
  async fn an_api_call_to_a_server_that_never_answers_times_out() {
    let listener = loopback_listener().await;
    let address = listener.local_addr().unwrap();
    // Takes the request and never answers, like a connection that died mid-flight.
    tokio::spawn(async move {
      let (_socket, _) = listener.accept().await.unwrap();
      std::future::pending::<()>().await;
    });
    let api = HttpServerApi::new(format!("http://{address}"), StaticToken("token".into()));
    let result = api.me().await;
    assert!(matches!(result, Err(ApiError::Network(_))), "{result:?}");
  }

  #[cfg(not(target_arch = "wasm32"))]
  mod hostile_networks {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    /// A mandatory company proxy: the gateway goes through `CONNECT` to the server.
    #[tokio::test]
    async fn the_gateway_connects_through_a_connect_proxy() {
      let server = loopback_listener().await;
      let server_address = server.local_addr().unwrap();
      tokio::spawn(async move {
        let (socket, _) = server.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
        use futures_util::SinkExt;
        socket
          .send(tokio_tungstenite::tungstenite::Message::text(
            "hello through the proxy",
          ))
          .await
          .unwrap();
      });
      let (proxy_url, requests) = connect_proxy().await;
      let url = url::Url::parse(&format!("ws://{server_address}/api/v1/gateway")).unwrap();
      let mut socket = open_websocket(&url, Some(&proxy_url)).await.unwrap();
      use futures_util::StreamExt;
      let frame = socket.next().await.unwrap().unwrap();
      assert_eq!(frame.into_text().unwrap().as_str(), "hello through the proxy");
      let request = requests.await.unwrap();
      let authority = server_address.to_string();
      assert!(
        request.starts_with(&format!("CONNECT {authority} HTTP/1.1\r\n")),
        "{request}"
      );
    }

    /// A captive portal: the login page arrives with 200 where the API answers JSON. That's a
    /// network problem to retry (the outbox waits), not an unreadable answer that fails for good.
    #[tokio::test]
    async fn an_html_page_instead_of_json_is_a_captive_portal_and_retried() {
      let base = canned_server(
        "200 OK",
        "text/html; charset=utf-8",
        "<html>Sign in to the Wi-Fi</html>",
      )
      .await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      let error = api.me().await.unwrap_err();
      assert_eq!(error.interception(), Some(Interception::CaptivePortal), "{error}");
      assert!(error.is_transient() && error.is_retry_later() && error.is_unreachable());
      // Also where no answer body is read (read markers, uploads' completion).
      let device =
        <DeviceId as teamcraft_proto::Id>::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap());
      let error = api.unregister_push(device).await.unwrap_err();
      assert_eq!(error.interception(), Some(Interception::CaptivePortal), "{error}");
    }

    #[tokio::test]
    async fn http_511_is_a_captive_portal() {
      let base = canned_server("511 Network Authentication Required", "text/plain", "sign in").await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      assert_eq!(
        api.me().await.unwrap_err().interception(),
        Some(Interception::CaptivePortal)
      );
    }

    #[tokio::test]
    async fn an_html_error_page_of_the_reverse_proxy_is_server_trouble() {
      let base = canned_server("502 Bad Gateway", "text/html", "<html>bad gateway</html>").await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      let error = api.me().await.unwrap_err();
      assert!(matches!(error, ApiError::Status { status: 502, .. }), "{error}");
      assert!(error.is_transient());
    }

    #[tokio::test]
    async fn a_portal_answering_the_websocket_handshake_is_a_captive_portal() {
      let base = canned_server(
        "302 Found\r\nlocation: http://portal.example.com/login",
        "text/html",
        "",
      )
      .await;
      let url = url::Url::parse(&base.replace("http://", "ws://")).unwrap();
      let error = open_websocket(&url, None).await.unwrap_err();
      assert_eq!(error.interception(), Some(Interception::CaptivePortal), "{error}");
    }

    /// A proxy that inspects TLS with its own CA: the certificate it shows isn't trusted, both for
    /// REST (reqwest) and the gateway WebSocket (tungstenite).
    #[tokio::test]
    async fn a_certificate_nobody_trusts_is_named_as_interception() {
      let base = untrusted_tls_server().await;
      let api = HttpServerApi::new(base.clone(), StaticToken("t".into()));
      let error = api.me().await.unwrap_err();
      assert_eq!(
        error.interception(),
        Some(Interception::UntrustedCertificate),
        "{error}"
      );
      assert!(error.is_transient());
      let url = url::Url::parse(&format!("{}/api/v1/gateway", base.replace("https://", "wss://"))).unwrap();
      let error = open_websocket(&url, None).await.unwrap_err();
      assert_eq!(
        error.interception(),
        Some(Interception::UntrustedCertificate),
        "{error}"
      );
    }

    /// The portal sends the request to its own site, which answers its login page.
    #[tokio::test]
    async fn a_redirect_to_an_html_page_elsewhere_is_a_captive_portal() {
      let portal = canned_server("200 OK", "text/html", "<html>Accept the terms</html>").await;
      let redirect: &'static str =
        Box::leak(format!("302 Found\r\nlocation: {portal}/login").into_boxed_str());
      let base = canned_server(redirect, "text/plain", "").await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      let error = api.me().await.unwrap_err();
      assert_eq!(error.interception(), Some(Interception::CaptivePortal), "{error}");
      assert!(error.to_string().contains("redirected"), "{error}");
    }

    /// Object storage may hand a blob over to a CDN: a redirect to bytes is no portal.
    #[tokio::test]
    async fn a_redirect_to_a_blob_elsewhere_is_fine() {
      let cdn = canned_server("200 OK", "application/octet-stream", "ciphertext").await;
      let redirect: &'static str = Box::leak(format!("302 Found\r\nlocation: {cdn}/blob").into_boxed_str());
      let storage = canned_server(redirect, "text/plain", "").await;
      let api = HttpServerApi::new("http://[::1]:1", StaticToken("t".into()));
      assert_eq!(
        api.get_blob(&format!("{storage}/blob")).await.unwrap(),
        b"ciphertext"
      );
    }

    /// Serves TLS on localhost with a fresh self-signed certificate and answers nothing useful.
    async fn untrusted_tls_server() -> String {
      let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
      let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
      let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
      let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certified.cert.der().clone()], key)
        .unwrap();
      let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
      let listener = loopback_listener().await;
      let port = listener.local_addr().unwrap().port();
      tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
          // The handshake fails on the client's side; nothing to serve.
          let _ = acceptor.accept(socket).await;
        }
      });
      format!("https://localhost:{port}")
    }

    /// The 1.0.2 failure (issue #420) for REST: the server's name resolves to an IPv6 address that
    /// swallows the SYN and an IPv4 address that works. The request goes out over IPv4.
    #[tokio::test]
    async fn rest_falls_back_to_ipv4_when_ipv6_is_black_holed() {
      let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
      let v4 = listener.local_addr().unwrap();
      tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
          let mut request = [0u8; 4096];
          let _ = socket.read(&mut request).await;
          let body = r#"{"ok":true}"#;
          let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
          );
          let _ = socket.write_all(response.as_bytes()).await;
        }
      });
      let Some(black_hole) = crate::api::connect::black_hole::BlackHole::ipv6(v4.port()).await else {
        return;
      };
      let client = http_client_builder()
        .dns_resolver(Arc::new(FixedResolver(vec![black_hole.address, v4])))
        .build()
        .unwrap();
      let started = std::time::Instant::now();
      let request = client
        .get(format!("http://chat.example.com:{}/api/v1/me", v4.port()))
        .send();
      let response = tokio::time::timeout(std::time::Duration::from_secs(5), request)
        .await
        .expect("fell back to IPv4 within the timeout")
        .unwrap();
      assert!(response.status().is_success());
      // IPv6 was tried first and swallowed the attempt.
      assert!(started.elapsed() >= std::time::Duration::from_millis(250));
    }

    /// Resolves every name to the same addresses, in this order.
    struct FixedResolver(Vec<std::net::SocketAddr>);

    impl reqwest::dns::Resolve for FixedResolver {
      fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let addresses = self.0.clone();
        Box::pin(async move { Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs) })
      }
    }

    /// A local `CONNECT` proxy: tunnels one connection and hands back the request head it got.
    async fn connect_proxy() -> (url::Url, tokio::task::JoinHandle<String>) {
      let listener = loopback_listener().await;
      let address = listener.local_addr().unwrap();
      let requests = tokio::spawn(async move {
        let (mut client, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
          let mut byte = [0u8; 1];
          client.read_exact(&mut byte).await.unwrap();
          head.push(byte[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let target = head.split_whitespace().nth(1).unwrap().to_owned();
        let mut upstream = tokio::net::TcpStream::connect(target).await.unwrap();
        client
          .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
          .await
          .unwrap();
        tokio::spawn(async move {
          let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        });
        head
      });
      (url::Url::parse(&format!("http://{address}")).unwrap(), requests)
    }

    /// Answers every request with `status`, `content_type` and `body`.
    async fn canned_server(status: &'static str, content_type: &'static str, body: &'static str) -> String {
      let listener = loopback_listener().await;
      let address = listener.local_addr().unwrap();
      tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
          let mut request = [0u8; 4096];
          let _ = socket.read(&mut request).await;
          let response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
          );
          let _ = socket.write_all(response.as_bytes()).await;
        }
      });
      format!("http://{address}")
    }
  }

  mod liveness {
    use super::*;

    #[test]
    fn a_connection_that_hears_heartbeat_acks_stays_alive() {
      let mut liveness = Liveness::new(0);
      for now_ms in (25_000..=250_000).step_by(30_000) {
        assert!(liveness.check(now_ms).is_ok(), "{now_ms}");
        liveness.heard(now_ms + 200);
      }
    }

    #[test]
    fn a_silent_connection_is_dead_after_the_limit() {
      let liveness = Liveness::new(0);
      assert!(liveness.check(GATEWAY_SILENCE_LIMIT_MS).is_ok());
      assert!(matches!(
        liveness.check(GATEWAY_SILENCE_LIMIT_MS + 1),
        Err(ApiError::Network(_))
      ));
    }

    #[test]
    fn a_wall_clock_jump_after_sleep_marks_the_connection_dead() {
      let liveness = Liveness::new(1_000_000);
      // The laptop slept over the weekend; the first heartbeat after waking fails at once.
      assert!(liveness.check(1_000_000 + 2 * 86_400_000).is_err());
    }
  }

  #[test]
  fn token_is_redacted() {
    assert!(!format!("{:?}", StaticToken("secret".into())).contains("secret"));
  }

  /// Serves one HTTP response per connection: `body`, with or without a content length.
  /// IPv6 loopback, or IPv4 on hosts whose kernel has no IPv6 (some CI sandboxes).
  #[cfg(not(target_arch = "wasm32"))]
  async fn loopback_listener() -> tokio::net::TcpListener {
    match tokio::net::TcpListener::bind("[::1]:0").await {
      Ok(listener) => listener,
      Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
    }
  }

  #[cfg(not(target_arch = "wasm32"))]
  async fn blob_server(body: Vec<u8>, has_length: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = loopback_listener().await;
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
      while let Ok((mut socket, _)) = listener.accept().await {
        let mut request = [0u8; 1024];
        let _ = socket.read(&mut request).await;
        let head = if has_length {
          format!(
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
          )
        } else {
          "HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_owned()
        };
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket.write_all(&body).await;
      }
    });
    format!("http://{address}/blob")
  }

  #[cfg(not(target_arch = "wasm32"))]
  #[tokio::test]
  async fn capped_downloads_stop_at_the_cap() {
    let api = HttpServerApi::new("http://[::1]:1", StaticToken("t".into()));
    let declared = blob_server(vec![7; 100], true).await;
    assert!(matches!(
      api.get_blob_capped(&declared, 50).await,
      Err(ApiError::BlobTooLarge { max_bytes: 50 })
    ));
    assert_eq!(api.get_blob_capped(&declared, 100).await.unwrap().len(), 100);
    // Without a content length the stream is counted as it arrives.
    let undeclared = blob_server(vec![7; 100_000], false).await;
    assert!(matches!(
      api.get_blob_capped(&undeclared, 1000).await,
      Err(ApiError::BlobTooLarge { .. })
    ));
  }

  #[cfg(not(target_arch = "wasm32"))]
  mod enrollment {
    use teamcraft_proto::{Id, Snowflake};

    use super::*;

    const USER: i64 = 7;

    #[tokio::test]
    async fn a_user_without_keys_is_not_enrolled_without_a_404() {
      let base = route_server(&[("/api/v1/users/7/enrollment", 200, "{}")]).await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      assert_eq!(api.identity_key(user()).await.unwrap(), None);
      assert_eq!(api.device_list(user()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_published_keys_come_from_the_enrollment() {
      let body = r#"{"device_list":{"sequence":2,"signed_list":"AQID"}}"#;
      let base = route_server(&[("/api/v1/users/7/enrollment", 200, body)]).await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      assert_eq!(api.identity_key(user()).await.unwrap(), None);
      assert_eq!(api.device_list(user()).await.unwrap().unwrap().sequence, 2);
    }

    #[tokio::test]
    async fn an_unknown_user_is_not_enrolled() {
      let not_found = r#"{"error_code":"user_not_found","message":"no such user"}"#;
      let base = route_server(&[("/api/v1/users/7/enrollment", 404, not_found)]).await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      assert_eq!(api.identity_key(user()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_server_without_the_endpoint_is_asked_the_old_way() {
      let key = r#"{"user_id":"7","public_key":"AQID","signature":"BAUG","created_at":1}"#;
      let missing = r#"{"error_code":"device_list_not_found","message":"none"}"#;
      let base = route_server(&[
        ("/api/v1/users/7/identity-key", 200, key),
        ("/api/v1/users/7/device-list", 404, missing),
      ])
      .await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      let identity_key = api.identity_key(user()).await.unwrap().unwrap();
      assert_eq!(identity_key.public_key.as_slice(), [1, 2, 3]);
      assert_eq!(api.device_list(user()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_old_server_is_probed_only_once() {
      let key = r#"{"user_id":"7","public_key":"AQID","signature":"BAUG","created_at":1}"#;
      let base = route_server(&[("/api/v1/users/7/identity-key", 200, key)]).await;
      let api = HttpServerApi::new(base, StaticToken("t".into()));
      assert!(!api.lacks_enrollment_endpoint.load(Ordering::Relaxed));
      api.identity_key(user()).await.unwrap();
      assert!(api.lacks_enrollment_endpoint.load(Ordering::Relaxed));
      api.identity_key(user()).await.unwrap();
    }

    fn user() -> UserId {
      UserId::from_snowflake(Snowflake::new(USER).unwrap())
    }

    /// Answers `(path, status, body)`; any other path gets a bare 404, like an unknown API route.
    async fn route_server(routes: &[(&'static str, u16, &'static str)]) -> String {
      use tokio::io::{AsyncReadExt, AsyncWriteExt};

      let routes = routes.to_vec();
      let listener = loopback_listener().await;
      let address = listener.local_addr().unwrap();
      tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
          let mut request = [0u8; 4096];
          let read = socket.read(&mut request).await.unwrap_or(0);
          let head = String::from_utf8_lossy(&request[..read]);
          let path = head.split_whitespace().nth(1).unwrap_or_default().to_owned();
          let (status, body) = routes
            .iter()
            .find(|(route, ..)| *route == path)
            .map_or((404, ""), |(_, status, body)| (*status, *body));
          let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
          );
          let _ = socket.write_all(response.as_bytes()).await;
        }
      });
      format!("http://{address}")
    }
  }

  #[cfg(not(target_arch = "wasm32"))]
  struct PanickingToken;

  #[cfg(not(target_arch = "wasm32"))]
  impl AccessTokenSource for PanickingToken {
    fn access_token(&self) -> Result<String, ApiError> {
      panic!("no token may be read for an insecure server");
    }
  }
}
