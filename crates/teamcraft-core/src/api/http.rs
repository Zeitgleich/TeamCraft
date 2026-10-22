//! [`ServerApi`] over HTTP (reqwest; `fetch` in the browser) and the gateway WebSocket
//! (tokio-tungstenite on native targets, the browser's `WebSocket` in wasm).

use std::fmt;

use reqwest::{Method, RequestBuilder, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use teamcraft_proto::RendezvousClientMessage;
use teamcraft_proto::{
  AddConversationMemberRequest, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ConversationId,
  ConversationMemberResponse, ConversationResponse, CreateChannelRequest, CreateDmRequest, DeviceId,
  DeviceResponse, ErrorBody, GatewayClientMessage, GatewayServerMessage, GetMeResponse, IdentityKeyResponse,
  KeyBagResponse, KeyPackageStatusResponse, ListConversationMembersResponse, ListConversationsResponse,
  ListDevicesResponse, ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery,
  ListMlsMessagesResponse, ListSpacesResponse, MessageResponse, MlsMessageResponse, PostMlsCommitRequest,
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
use teamcraft_proto::{CallJoinTokenResponse, CreateCallJoinTokenRequest};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest,
  InviteResponse, ListCategoriesResponse, SpaceResponse, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};
use teamcraft_proto::{DeviceListResponse, PutDeviceListRequest};
use teamcraft_proto::{ListThreadsQuery, ListThreadsResponse, MessageId, UnreadResponse};
use teamcraft_proto::{SetPresenceRequest, SetReadPositionRequest, UpdateConversationMemberRequest};

#[cfg(target_arch = "wasm32")]
use super::web_socket::{BrowserSocket, SocketFrame};
use super::{ApiError, GatewayConnection, RendezvousConnection, RendezvousFrame, ServerApi};

/// Supplies a valid OIDC access token. The app owns login and refresh; the core only asks.
pub trait AccessTokenSource {
  fn access_token(&self) -> Result<String, ApiError>;
}

/// A fixed token (tests, command-line tools).
pub struct StaticToken(pub String);

pub struct HttpServerApi<T> {
  client: reqwest::Client,
  base_url: String,
  tokens: T,
}

impl<T: AccessTokenSource> HttpServerApi<T> {
  /// `base_url` without trailing slash, e.g. `https://chat.example.com`.
  pub fn new(base_url: impl Into<String>, tokens: T) -> Self {
    #[cfg(target_os = "android")]
    let client = super::tls::http_client();
    #[cfg(not(target_os = "android"))]
    let client = reqwest::Client::new();
    Self {
      client,
      base_url: base_url.into().trim_end_matches('/').to_string(),
      tokens,
    }
  }

  fn request(&self, method: Method, path: &str) -> Result<RequestBuilder, ApiError> {
    let token = self.tokens.access_token()?;
    Ok(
      self
        .client
        .request(method, format!("{}/api/v1{path}", self.base_url))
        .bearer_auth(token),
    )
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

  fn gateway_url(&self) -> String {
    self.websocket_url("/api/v1/gateway")
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

  async fn put_key_bag(&self, request: &PutKeyBagRequest) -> Result<KeyBagResponse, ApiError> {
    self.send_json(Method::PUT, "/me/key-bag", request).await
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
    let mut response = send(self.client.get(url)).await?;
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
    }
    Ok(blob)
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

  async fn connect_gateway(&self) -> Result<HttpGateway, ApiError> {
    HttpGateway::connect(&self.gateway_url()).await
  }

  async fn connect_rendezvous(&self) -> Result<HttpRendezvous, ApiError> {
    HttpRendezvous::connect(&self.websocket_url("/api/v1/rendezvous")).await
  }

  fn gateway_token(&self) -> Result<String, ApiError> {
    self.tokens.access_token()
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

/// The gateway WebSocket on native targets.
#[cfg(not(target_arch = "wasm32"))]
pub struct HttpGateway {
  socket: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
}

/// The gateway WebSocket in the browser.
#[cfg(target_arch = "wasm32")]
pub struct HttpGateway {
  socket: BrowserSocket,
}

#[cfg(not(target_arch = "wasm32"))]
impl HttpGateway {
  async fn connect(url: &str) -> Result<Self, ApiError> {
    Ok(Self {
      socket: connect_websocket(url).await?,
    })
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl GatewayConnection for HttpGateway {
  async fn send(&mut self, message: &GatewayClientMessage) -> Result<(), ApiError> {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;

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
      match self.socket.next().await? {
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
    })
  }
}

#[cfg(target_arch = "wasm32")]
impl GatewayConnection for HttpGateway {
  async fn send(&mut self, message: &GatewayClientMessage) -> Result<(), ApiError> {
    let text = serde_json::to_string(message).map_err(|error| ApiError::Decode(error.to_string()))?;
    self.socket.send_text(&text)
  }

  async fn receive(&mut self) -> Option<Result<GatewayServerMessage, ApiError>> {
    loop {
      match self.socket.next().await? {
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

#[cfg(not(target_arch = "wasm32"))]
type NativeSocket =
  tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

#[cfg(not(target_arch = "wasm32"))]
async fn connect_websocket(url: &str) -> Result<NativeSocket, ApiError> {
  #[cfg(target_os = "android")]
  let maybe_connector = Some(tokio_tungstenite::Connector::Rustls(super::tls::client_config()));
  #[cfg(not(target_os = "android"))]
  let maybe_connector = None;
  let (socket, _) = tokio_tungstenite::connect_async_tls_with_config(url, None, false, maybe_connector)
    .await
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
  fn token_is_redacted() {
    assert!(!format!("{:?}", StaticToken("secret".into())).contains("secret"));
  }

  /// Serves one HTTP response per connection: `body`, with or without a content length.
  async fn blob_server(body: Vec<u8>, has_length: bool) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("[::1]:0").await.unwrap();
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
}
