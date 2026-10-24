//! [`ServerApi`] over HTTP (reqwest; `fetch` in the browser) and the gateway WebSocket
//! (tokio-tungstenite on native targets, the browser's `WebSocket` in wasm).

use std::fmt;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use teamcraft_proto::RendezvousClientMessage;
use teamcraft_proto::{
  AddConversationMemberRequest, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ConversationId,
  ConversationMemberResponse, ConversationResponse, CreateChannelRequest, CreateDmRequest,
  CreateGroupDmRequest, DeviceId, DeviceResponse, ErrorBody, GatewayClientMessage, GatewayServerMessage,
  GetMeResponse, IdentityKeyResponse, KeyBagResponse, KeyPackageStatusResponse,
  ListConversationMembersResponse, ListConversationsResponse, ListDevicesResponse, ListMessagesQuery,
  ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListSpaceMembersResponse,
  ListSpacesResponse, MessageResponse, MlsMessageResponse, MoveChannelRequest, PostMlsCommitRequest,
  PostMlsCommitResponse, PostMlsMessageRequest, PutKeyBagRequest, RegisterDeviceRequest, SendMessageRequest,
  SpaceId, UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserSummary,
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
use teamcraft_proto::{
  Base64Bytes, GatewayInfoResponse, GatewayRevokeRequest, PushGatewayResponse, WebPushKeyResponse,
};
use teamcraft_proto::{CallJoinTokenResponse, CreateCallJoinTokenRequest};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest,
  InviteResponse, ListCategoriesResponse, SpaceResponse, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};
use teamcraft_proto::{DeviceListResponse, PutDeviceListRequest};
use teamcraft_proto::{
  ListCallHistoryQuery, ListCallHistoryResponse, ListPresenceResponse, PrivacyResponse, UpdatePrivacyRequest,
};
use teamcraft_proto::{ListThreadsQuery, ListThreadsResponse, MessageId, UnreadResponse};
use teamcraft_proto::{NotificationSettings, TestPushResponse};
use teamcraft_proto::{SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest};

#[cfg(target_arch = "wasm32")]
use super::web_socket::{BrowserSocket, SocketFrame};
use super::{ApiError, GatewayConnection, RendezvousConnection, RendezvousFrame, ServerApi};
use crate::clock::{Clock, SystemClock};

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

pub struct HttpServerApi<T> {
  client: reqwest::Client,
  base_url: String,
  /// Why `base_url` is refused, checked once in [`HttpServerApi::new`].
  maybe_insecure: Option<String>,
  tokens: T,
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
    }
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

  async fn send_json<B: Serialize, R: DeserializeOwned>(
    &self,
    method: Method,
    path: &str,
    body: &B,
  ) -> Result<R, ApiError> {
    decode(send(self.request(method, path)?.json(body)).await?).await
  }

  async fn send_without_response<B: Serialize>(
    &self,
    method: Method,
    path: &str,
    maybe_body: Option<&B>,
  ) -> Result<(), ApiError> {
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

  async fn identity_key(&self, user: UserId) -> Result<IdentityKeyResponse, ApiError> {
    self.get(&format!("/users/{user}/identity-key")).await
  }

  async fn device_list(&self, user: UserId) -> Result<DeviceListResponse, ApiError> {
    self.get(&format!("/users/{user}/device-list")).await
  }

  async fn put_device_list(&self, request: &PutDeviceListRequest) -> Result<DeviceListResponse, ApiError> {
    self.send_json(Method::PUT, "/me/device-list", request).await
  }

  async fn revoke_device(&self, device: DeviceId) -> Result<(), ApiError> {
    self
      .send_without_response::<()>(Method::DELETE, &format!("/devices/{device}"), None)
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

  async fn set_presence(&self, request: &SetPresenceRequest) -> Result<(), ApiError> {
    self
      .send_without_response(Method::PUT, "/me/presence", Some(request))
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
    decode(send(self.request(Method::POST, &format!("/attachments/{attachment}/complete"))?).await?).await
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
  match builder.build() {
    Ok(client) => client,
    // Only a reqwest/rustls version mismatch can cause this; the default client still works for
    // plain HTTP (development) and reports TLS errors per request.
    Err(error) => {
      tracing::error!(%error, "HTTP client configuration rejected, using the default client");
      reqwest::Client::new()
    },
  }
}

/// The browser's `fetch` has its own timeouts and connection handling.
#[cfg(target_arch = "wasm32")]
fn http_client() -> reqwest::Client {
  reqwest::Client::new()
}

#[cfg(not(target_arch = "wasm32"))]
type NativeSocket =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(not(target_arch = "wasm32"))]
async fn connect_websocket(url: &str) -> Result<NativeSocket, ApiError> {
  // Always an explicit configuration: tungstenite's default one panics in rustls (api/tls.rs).
  let maybe_connector = Some(tokio_tungstenite::Connector::Rustls(super::tls::client_config()));
  let connecting = tokio_tungstenite::connect_async_tls_with_config(url, None, false, maybe_connector);
  // A black-holed server would otherwise stall the caller's loop (outbox, heartbeats) for minutes.
  let (socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
    .await
    .map_err(|_| ApiError::Network("WebSocket connection timed out".into()))?
    .map_err(|error| ApiError::Network(error.to_string()))?;
  Ok(socket)
}

async fn send(request: RequestBuilder) -> Result<Response, ApiError> {
  let response = request
    .send()
    .await
    .map_err(|error| ApiError::Network(error.to_string()))?;
  let status = response.status();
  if status.is_success() {
    return Ok(response);
  }
  let maybe_body = response.json::<ErrorBody>().await.ok();
  Err(ApiError::Status {
    status: status.as_u16(),
    maybe_body,
  })
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
  async fn loopback_listener() -> tokio::net::TcpListener {
    match tokio::net::TcpListener::bind("[::1]:0").await {
      Ok(listener) => listener,
      Err(_) => tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(),
    }
  }

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

  struct PanickingToken;

  impl AccessTokenSource for PanickingToken {
    fn access_token(&self) -> Result<String, ApiError> {
      panic!("no token may be read for an insecure server");
    }
  }
}
