//! An in-process stand-in for `teamcraft-server`, for tests of the sync engine.
//!
//! It mirrors the server's observable behaviour that the client depends on: snowflake IDs in
//! creation order, membership checks, atomic key package claims, the MLS queue with "first commit
//! per epoch wins", welcomes per device, key bag compare-and-swap, and gateway sessions with
//! sequence numbers and resume. It never looks inside payloads, like the real server. Tests can take
//! a client offline or drop its gateway to exercise retries and resume.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt;
use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, Base64Bytes, ChannelModeration, ChannelRole,
  ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClaimedKeyPackage, ConversationHead, ConversationId,
  ConversationKind, ConversationMemberResponse, ConversationResponse, ConversationVisibility,
  CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, DeviceActivity, DeviceId, DeviceResponse,
  ErrorCode, GatewayClientMessage, GatewayEvent, GatewayServerMessage, GetMeResponse, HistoryPolicy, Id,
  IdempotencyKey, IdentityKeyResponse, JoinPolicy, KeyBagResponse, KeyPackageStatusResponse,
  ListConversationMembersResponse, ListConversationsResponse, ListDevicesResponse, ListMessagesQuery,
  ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListSpaceMembersResponse,
  ListSpacesResponse, MessageId, MessageResponse, MlsMessageId, MlsMessageKind, MlsMessageResponse,
  MoveChannelRequest, MyMembership, NotificationLevel, PostMlsCommitRequest, PostMlsCommitResponse,
  PostMlsMessageRequest, PutKeyBagRequest, RegisterDeviceRequest, Role, SendMessageRequest,
  SetPresenceRequest, SetReadPositionRequest, Snowflake, SpaceHead, SpaceId, SpaceMemberResponse,
  SpaceResponse, SpaceRole, SyncHeadsResponse, UnixMillis, UpdateConversationMemberRequest,
  UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserKind, UserSummary,
};
use teamcraft_proto::{AdminJoinRequestChange, AdminJoinRequestResponse};
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
  CallId, CallJoinTokenResponse, CallRosterEntry, CallStateResponse, CreateCallJoinTokenRequest,
  MoveCallRequest,
};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest,
  InviteResponse, ListCategoriesResponse, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};
use teamcraft_proto::{CreateDeviceLinkRequest, DeviceLinkClosedReason, DeviceLinkRequest};
use teamcraft_proto::{CreateInstantMeetingRequest, MeetingKind, MeetingResponse, MeetingStatus};
use teamcraft_proto::{DeviceListResponse, PutDeviceListRequest};
use teamcraft_proto::{DevicePushHealth, PushDeliveryCounts, PushHealthResponse, UpdateDeviceRequest};
use teamcraft_proto::{ErrorBody, RendezvousClientMessage, RendezvousServerMessage};
use teamcraft_proto::{
  GatewayInfoResponse, GatewayLimits, GatewayTokenKey, PushGatewayBinding, PushGatewayResponse, PushProvider,
  WebPushKeyResponse,
};
use teamcraft_proto::{
  ListThreadsQuery, ListThreadsResponse, ThreadFollowReason, ThreadId, ThreadSummary, UnreadResponse,
};
use teamcraft_proto::{NotificationSettings, PushKind, PushMode, TestPushOutcome, TestPushResponse};

use super::{ApiError, GatewayConnection, RendezvousConnection, RendezvousFrame, ServerApi};
use crate::clock::{Clock, SystemClock};

const PAGE_SIZE: usize = 50;
/// People per directory answer (the server's default `max_directory_results`).
const DIRECTORY_PAGE: usize = 50;
const HEARTBEAT_INTERVAL_MS: u64 = 30_000;
/// The lifetime a relay reports on join, as the server's default.
pub const RELAY_TTL_SECS: u64 = 300;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The server's defaults: key packages are handed out while they stay valid this long, and count as
/// "expiring soon" within the renewal window (e2ee.md §4.3).
const KEY_PACKAGE_MIN_VALIDITY_MS: i64 = 42 * DAY_MS;
const KEY_PACKAGE_RENEWAL_WINDOW_MS: i64 = 56 * DAY_MS;
/// The server's default lifetime of an admin's request to join a private channel.
const ADMIN_JOIN_REQUEST_MS: i64 = 7 * DAY_MS;

/// The seed of the fake gateway's token key, to open what the client sealed (tests).
pub const FAKE_GATEWAY_TOKEN_SEED: [u8; 32] = [0x42; 32];
/// The VAPID key the fake server hands to browsers (not a real key; browser tests fake the push service).
pub const FAKE_WEB_PUSH_KEY: &str = "fake-web-push-key";

/// The shared server state. Clone it to hand the same server to several clients.
#[derive(Clone, Default)]

pub struct FakeServer {
  state: Arc<Mutex<State>>,
}

/// One user's view of the server, as an authenticated client sees it.
#[derive(Clone)]
pub struct FakeServerApi {
  server: FakeServer,
  user: UserId,
}

pub struct FakeGateway {
  server: FakeServer,
  user: UserId,
  connection: u64,
  receiver: UnboundedReceiver<GatewayServerMessage>,
}

#[derive(Default)]
struct State {
  next_id: i64,
  /// `GET /sync/heads` answers a bare 404, like a server from before the endpoint (issue #714):
  /// a full sync then reads everything.
  lacks_sync_heads: bool,
  /// `…/role-log` answers 404, like a server from before signed channel roles (issue #380).
  lacks_role_log: bool,
  directory_searches: u64,
  /// Conversations whose history answers 403 (a removal the list doesn't show yet, a policy).
  forbidden_histories: HashSet<ConversationId>,
  /// Conversations whose member list answers with an unreadable response.
  broken_member_lists: HashSet<ConversationId>,
  /// Conversations whose read position writes answer 503.
  failing_read_positions: HashSet<ConversationId>,
  /// `PUT read-position` requests that arrived, to check that queued ones are sent once.
  read_position_writes: u64,
  /// Meetings cancelled through the API, in order.
  cancelled_meetings: Vec<teamcraft_proto::MeetingId>,
  /// The running call of each conversation (a new one after [`FakeServer::end_call`]).
  calls: HashMap<ConversationId, CallId>,
  /// Who is in each conversation's call, as the SFU reported it ([`FakeServer::join_call`]); a
  /// conversation is listed only while somebody is in its call.
  call_rosters: BTreeMap<ConversationId, CallStateResponse>,
  /// Instant meetings by the idempotency key they were created with.
  instant_meetings: HashMap<IdempotencyKey, MeetingResponse>,
  /// Calls moved into a meeting (`move_call`), in order.
  call_moves: Vec<(ConversationId, MoveCallRequest)>,
  /// REST calls per user, also those that failed (tests about idle traffic).
  rest_calls: HashMap<UserId, u64>,
  users: BTreeMap<UserId, UserSummary>,
  devices: BTreeMap<DeviceId, Device>,
  identity_keys: HashMap<UserId, IdentityKeyResponse>,
  key_packages: HashMap<DeviceId, KeyPackages>,
  space: Option<SpaceId>,
  conversations: BTreeMap<ConversationId, Conversation>,
  welcomes: Vec<MlsMessageResponse>,
  key_bags: HashMap<UserId, KeyBagResponse>,
  device_lists: HashMap<UserId, DeviceListResponse>,
  sessions: HashMap<String, Session>,
  offline_users: HashSet<UserId>,
  /// Users whose blob uploads the network refuses (a browser's CSP, a proxy) while REST works.
  blob_refused_users: HashSet<UserId>,
  /// Users whose REST calls fail while their gateway still connects.
  rest_failing_users: HashSet<UserId>,
  /// Users the server answers 503 with this `Retry-After` (seconds): a dependency outage or a
  /// restart behind the proxy (`set_unavailable`).
  unavailable_users: HashMap<UserId, u64>,
  /// Gateway connects answered with that 503.
  unavailable_connects: u64,
  /// Users whose access token the server rejects (401), as after an expired session.
  signed_out_users: HashSet<UserId>,
  /// Users whose REST calls a captive portal answers (`set_behind_captive_portal`).
  portal_users: HashSet<UserId>,
  /// Heartbeats received, to check that a busy client keeps sending them.
  heartbeats: u64,
  /// `set_activity` frames per user, in order (presence after an away timeout, issue #472).
  activity_reports: HashMap<UserId, Vec<DeviceActivity>>,
  /// `typing` frames per user, in order: the conversations typed in.
  typing_reports: HashMap<UserId, Vec<ConversationId>>,
  /// What `GET /unread` answers per user; without an entry nothing is unread.
  unread: HashMap<UserId, UnreadResponse>,
  /// A malicious server: the next registered device gets this ID instead of a fresh one.
  maybe_forced_device_id: Option<DeviceId>,
  /// A malicious server: the next message gets this ID instead of a fresh one.
  maybe_forced_message_id: Option<MessageId>,
  /// A network cut armed per user: after the given number of calls, the next one fails.
  network_cuts: HashMap<UserId, (u32, NetworkCut)>,
  /// Users whose gateway connections stay open but carry nothing either way: no `hello`, no
  /// answers, no events (a half-open connection after a network change, a stalled server).
  silent_gateway_users: HashSet<UserId>,
  /// Connections that died without the client noticing: frames sent on them vanish and nothing
  /// arrives on them any more.
  frozen_connections: HashSet<u64>,
  /// Handshakes refused because the token was (as `set_signed_out`), to check a client stops trying.
  gateway_refusals: u64,
  /// Gateway connections opened so far.
  gateway_connects: u64,
  next_connection: u64,
  /// Users with workspace admin rights (permission checks only; trust comes from the roster).
  admins: HashSet<UserId>,
  /// Direct messages deleted with a deleted account, per remaining participant.
  deleted_conversations: HashMap<UserId, Vec<ConversationId>>,
  attachments: HashMap<AttachmentId, FakeAttachment>,
  push_registrations: HashMap<DeviceId, RegisterPushRequest>,
  notification_settings: HashMap<UserId, NotificationSettings>,
  /// What `GET /push/gateway` answers (`None`: the server sends to APNs and FCM itself).
  push_gateway: Option<PushGatewayBinding>,
  /// Sealed tokens that were revoked at the (fake) gateway.
  revoked_gateway_tokens: Vec<Vec<u8>>,
  revoked_gateway_urls: Vec<String>,
  relays: HashMap<String, Relay>,
  /// Open device link requests by relay ID (the server keeps them next to the relays).
  link_requests: HashMap<String, FakeLinkRequest>,
  /// Requests of admins to join a private channel (admin.md §2.4.1), kept after a decision like
  /// the server keeps them until they expire.
  admin_join_requests: BTreeMap<(ConversationId, UserId), FakeJoinRequest>,
  /// Relayed binary frames still to be lost on the way (a flaky connection during linking).
  relay_frames_to_lose: u32,
  /// Like the server: a repeated idempotency key returns the first space / channel.
  created_spaces: HashMap<(UserId, IdempotencyKey), SpaceId>,
  created_channels: HashMap<(UserId, IdempotencyKey), ConversationId>,
  /// Create answers still to be lost after the resource was made (a timeout).
  create_answers_to_lose: u32,
  /// Key package uploads still to be lost after the server stored them (a timeout).
  key_package_answers_to_lose: u32,
  roster_log: Vec<WorkspaceLogEntryResponse>,
  policy_log: Vec<WorkspaceLogEntryResponse>,
  /// Larger attachments are refused like the server's `max_attachment_bytes`; `None`: no limit.
  maybe_max_attachment_bytes: Option<u64>,
  /// Parts stored by uploads in parts, counted over the whole run.
  part_uploads: u64,
  /// Unfinished uploads the clients gave up (`abandon_attachment`).
  abandoned_attachments: Vec<AttachmentId>,
  /// Uploads in parts keep only the parts' sizes, not their bytes (memory tests with large files).
  is_discarding_parts: bool,
  share_links: HashMap<String, AttachmentId>,
  /// Links that were revoked; the server still lists them, as revoked.
  revoked_share_links: std::collections::HashSet<String>,
  backgrounds: HashMap<BackgroundId, FakeBackground>,
  org_backgrounds: HashMap<OrgBackgroundId, FakeBackground>,
  /// Spaces besides the default one, and changes to the default one.
  spaces: BTreeMap<SpaceId, SpaceResponse>,
  categories: BTreeMap<CategoryId, CategoryResponse>,
  invites: Vec<InviteResponse>,
  /// People added to spaces through `add_space_member`, in order.
  space_member_adds: Vec<(SpaceId, AddSpaceMemberRequest)>,
  /// Known users who are in no space until `add_space_member` adds them.
  space_outsiders: HashSet<UserId>,
  /// Space roles other than plain member, set through `update_space_member`.
  space_roles: HashMap<(SpaceId, UserId), SpaceRole>,
  /// The personal message of each invitation, in order (the response doesn't carry it).
  invite_messages: Vec<Option<String>>,
  /// Calls served by `list_call_history` to everybody who took part (newest first).
  call_history: Vec<teamcraft_proto::CallHistoryEntry>,
  /// When people were last active, and who chose to hide it.
  last_seen: HashMap<UserId, i64>,
  hidden_last_seen: HashSet<UserId>,
  /// Custom statuses set through `PUT /me/status`.
  custom_statuses: HashMap<UserId, teamcraft_proto::CustomStatus>,
  /// The workspace allows only presets: `PUT /me/status` answers 403 ([`FakeServer::refuse_custom_statuses`]).
  refuses_custom_statuses: bool,
  /// How far the server's clock runs ahead of the system clock ([`FakeServer::advance_clock`]).
  clock_offset_ms: i64,
  /// Users whose conversation list answers only after this long (a slow server in a big sync).
  conversation_list_delays: HashMap<UserId, std::time::Duration>,
  /// Users whose member lists answer only after this long.
  member_list_delays: HashMap<UserId, std::time::Duration>,
  /// Users whose reads of a user's registered devices answer only after this long.
  user_device_delays: HashMap<UserId, std::time::Duration>,
  /// Users whose reads of a user's registered devices are refused (403).
  user_device_refusals: HashSet<UserId>,
  /// The gateway's heartbeat timeout on a clock of the test's ([`FakeServer::enforce_heartbeat_timeout`]).
  maybe_heartbeat_timeout: Option<(Arc<dyn Clock + Send + Sync>, i64)>,
  /// When each gateway connection last sent a frame, while the timeout is enforced.
  last_client_frames: HashMap<u64, i64>,
  /// Gateway connections closed because the client went silent for longer than the timeout.
  heartbeat_timeouts: u64,
  /// Revoked devices stay listed with their key packages, as a stale or malicious server would.
  is_ignoring_revocations: bool,
  /// The next commit of these users is stored and fanned out, but the answer is lost.
  losing_commit_answers: HashSet<UserId>,
  /// Users whose removals the server refuses (a moderator removing an admin, the last admin).
  refusing_removals_by: HashSet<UserId>,
  /// The next member removal or device revocation of these users fails this way.
  removal_cuts: HashMap<UserId, NetworkCut>,
  /// The next commit of these users is lost on the way (never stored).
  dropping_commits: HashSet<UserId>,
  /// These users get the next commit (of anyone) garbled, so their client refuses it.
  garbling_next_commit_for: HashSet<UserId>,
  /// MLS entries each user gets garbled (gateway and backlog alike).
  garbled_entries: HashMap<UserId, HashSet<teamcraft_proto::MlsMessageId>>,
  /// Group information served whatever the group's epoch ([`FakeServer::serve_group_info`]).
  forced_group_infos: HashMap<ConversationId, teamcraft_proto::MlsGroupInfoResponse>,
  /// How many more group information reads answer "not found" per conversation
  /// ([`FakeServer::hide_group_info_reads`]).
  hidden_group_info_reads: HashMap<ConversationId, u32>,
  /// Users whose member removals (`DELETE …/members/{user}`) are refused, the check before them
  /// still allowed ([`FakeServer::refuse_removal_steps_by`]).
  refusing_removal_steps_by: HashSet<UserId>,
  /// Member removals requested so far, refused or not.
  removal_requests: u64,
  /// The next proposal of these users fails this way ([`FakeServer::cut_next_proposal`]).
  proposal_cuts: HashMap<UserId, NetworkCut>,
  /// The next application message of these users fails this way
  /// ([`FakeServer::cut_next_application`]).
  application_cuts: HashMap<UserId, NetworkCut>,
  /// The next application message of these users is accepted but published only on
  /// [`FakeServer::release_held_messages`].
  holding_application_of: HashSet<UserId>,
  /// The next proposal of these users is accepted but published only on
  /// [`FakeServer::release_held_messages`].
  holding_proposal_of: HashSet<UserId>,
  /// Held application messages and proposals, in the order they were posted.
  held_applications: Vec<(UserId, ConversationId, PostMlsMessageRequest)>,
  /// Users whose messages are answered as stored but never kept or delivered
  /// ([`FakeServer::drop_messages_of`]).
  dropping_messages_of: HashSet<UserId>,
  /// Members refused their groups' queues ([`FakeServer::refuse_queue_to`]).
  queue_refused_to: HashSet<UserId>,
  /// The next identity key lookup of these users fails with a network error.
  failing_identity_lookups: HashSet<UserId>,
  /// Users the server answers "not found" for when asked who they are
  /// ([`FakeServer::hide_user`]).
  hidden_users: HashSet<UserId>,
}

/// How an armed network cut ([`FakeServer::cut_network_after`]) hits the call it catches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkCut {
  /// The request never reaches the server.
  BeforeRequest,
  /// The server handles the request, the response is lost.
  ResponseLost,
}

/// A device-linking relay: at most two endpoints of one user, frames held for an absent peer.
#[derive(Default)]
struct Relay {
  endpoints: Vec<(u64, UserId, UnboundedSender<RendezvousFrame>)>,
  held: Vec<(u64, Vec<u8>)>,
}

struct FakeLinkRequest {
  user: UserId,
  request: DeviceLinkRequest,
}

/// One device's end of a fake relay.
pub struct FakeRendezvous {
  server: FakeServer,
  user: UserId,
  connection: u64,
  maybe_rendezvous_id: Option<String>,
  sender: UnboundedSender<RendezvousFrame>,
  receiver: UnboundedReceiver<RendezvousFrame>,
}

struct FakeBackground {
  owner: UserId,
  size_bytes: u64,
  maybe_blob: Option<Vec<u8>>,
  is_complete: bool,
}

struct FakeAttachment {
  conversation: ConversationId,
  size_bytes: u64,
  maybe_blob: Option<Vec<u8>>,
  is_complete: bool,
  /// An upload in parts: the parts stored so far, by part number.
  maybe_parts: Option<BTreeMap<u16, FakePart>>,
}

struct FakePart {
  etag: String,
  size_bytes: u64,
  /// `None` while the server discards parts (`discard_parts`).
  maybe_bytes: Option<Vec<u8>>,
}

struct Device {
  user: UserId,
  response: DeviceResponse,
}

#[derive(Default)]
struct KeyPackages {
  regular: VecDeque<StoredKeyPackage>,
  maybe_last_resort: Option<StoredKeyPackage>,
}

/// A key package with its expiry on the server's clock.
#[derive(Clone)]
struct StoredKeyPackage {
  bytes: Base64Bytes,
  expires_at_ms: i64,
}

struct Conversation {
  response: ConversationResponse,
  /// Member and the MLS entry ID before they joined (they see later entries only).
  members: BTreeMap<UserId, (ChannelRole, i64)>,
  epoch: u64,
  mls: Vec<MlsMessageResponse>,
  messages: Vec<MessageResponse>,
  favorites: HashMap<UserId, bool>,
  read_positions: HashMap<UserId, MessageId>,
  /// How far each member read each thread.
  thread_read_positions: HashMap<(UserId, ThreadId), MessageId>,
  pinned: HashSet<MessageId>,
  /// The newest MLS group information and the epoch it is for.
  maybe_group_info: Option<(u64, Base64Bytes)>,
  /// The signed channel role log (issue #380), oldest first.
  role_log: Vec<teamcraft_proto::ChannelRoleLogEntryResponse>,
}

struct Session {
  user: UserId,
  seq: u64,
  buffer: Vec<(u64, GatewayEvent)>,
  maybe_live: Option<(u64, UnboundedSender<GatewayServerMessage>)>,
}

impl FakeServer {
  pub fn new() -> Self {
    Self::default()
  }

  /// Creates a user and returns the API handle a client of that user would use.
  pub fn add_user(&self, name: &str) -> FakeServerApi {
    let mut state = self.lock();
    let id = state.next_id();
    state.users.insert(
      id,
      UserSummary {
        id,
        maybe_username: Some(name.to_lowercase()),
        display_name: name.to_string(),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_deactivated: false,
        is_deleted: false,
      },
    );
    drop(state);
    FakeServerApi {
      server: self.clone(),
      user: id,
    }
  }

  /// The user's name changes (at the identity provider, say): everyone is told, like the server's
  /// `ProfileUpdate`.
  pub fn rename_user(&self, user: UserId, display_name: &str, username: &str) {
    let mut state = self.lock();
    if let Some(summary) = state.users.get_mut(&user) {
      summary.display_name = display_name.to_owned();
      summary.maybe_username = Some(username.to_owned());
    }
    let everyone: Vec<UserId> = state.users.keys().copied().collect();
    state.publish(everyone, &GatewayEvent::ProfileUpdate { user_id: user });
  }

  /// An admin deactivates the user: like the server, `GET /users/{id}` no longer finds them, and
  /// everyone is told.
  pub fn deactivate_user(&self, user: UserId) {
    let mut state = self.lock();
    if let Some(summary) = state.users.get_mut(&user) {
      summary.is_deactivated = true;
    }
    let everyone: Vec<UserId> = state.users.keys().copied().collect();
    state.publish(everyone, &GatewayEvent::ProfileUpdate { user_id: user });
  }

  /// How many device link requests the server holds.
  pub fn link_requests_held(&self) -> usize {
    self.lock().link_requests.len()
  }

  /// Creates a meeting guest: a user of kind `guest`, as the server makes one for a redeemed link.
  pub fn add_guest(&self, name: &str) -> FakeServerApi {
    let api = self.add_user(name);
    if let Some(user) = self.lock().users.get_mut(&api.user) {
      user.kind = UserKind::Guest;
    }
    api
  }

  /// The server shows `user` as a guest (kind `guest`) from now on, whatever they were.
  pub fn show_as_guest(&self, user: UserId) {
    if let Some(entry) = self.lock().users.get_mut(&user) {
      entry.kind = UserKind::Guest;
    }
  }

  /// From now on the server answers "not found" when asked who `user` is (`GET /users/{id}`).
  pub fn hide_user(&self, user: UserId) {
    self.lock().hidden_users.insert(user);
  }

  /// While offline, every REST call of this user fails with a network error. Coming back online
  /// also disarms a network cut that hasn't hit yet.
  pub fn set_offline(&self, user: UserId, is_offline: bool) {
    let mut state = self.lock();
    if is_offline {
      state.offline_users.insert(user);
    } else {
      state.offline_users.remove(&user);
      state.network_cuts.remove(&user);
    }
  }

  /// While set, this user's REST calls are answered with 401 (an expired access token).
  pub fn set_signed_out(&self, user: UserId, is_signed_out: bool) {
    let mut state = self.lock();
    if is_signed_out {
      state.signed_out_users.insert(user);
    } else {
      state.signed_out_users.remove(&user);
    }
  }

  /// While set, this user's gateway connections go silent: new ones never get `hello`, frames sent
  /// on open ones are swallowed, and no events or answers arrive (they are still buffered for a
  /// resume).
  pub fn set_gateway_silent(&self, user: UserId, is_silent: bool) {
    let mut state = self.lock();
    if is_silent {
      state.silent_gateway_users.insert(user);
    } else {
      state.silent_gateway_users.remove(&user);
    }
  }

  /// The open gateway connections of a user die silently (half-open TCP after a network change):
  /// the client's sends still succeed but nothing reaches either side. New connections work.
  pub fn freeze_gateways(&self, user: UserId) {
    let mut state = self.lock();
    let open: Vec<u64> = state
      .sessions
      .values()
      .filter(|session| session.user == user)
      .filter_map(|session| session.maybe_live.as_ref().map(|(connection, _)| *connection))
      .collect();
    state.frozen_connections.extend(open);
  }

  /// How many gateway handshakes were refused for a bad token so far.
  pub fn gateway_refusal_count(&self) -> u64 {
    self.lock().gateway_refusals
  }

  /// How many REST calls `user` made so far, including failed ones.
  pub fn rest_call_count(&self, user: UserId) -> u64 {
    self.lock().rest_calls.get(&user).copied().unwrap_or(0)
  }

  /// How many directory searches (`GET /users?query=`) arrived so far.
  pub fn directory_search_count(&self) -> u64 {
    self.lock().directory_searches
  }

  /// Makes the conversation's history answer 403 (or work again).
  pub fn forbid_history(&self, conversation: ConversationId, is_forbidden: bool) {
    let mut state = self.lock();
    if is_forbidden {
      state.forbidden_histories.insert(conversation);
    } else {
      state.forbidden_histories.remove(&conversation);
    }
  }

  /// Makes the conversation's member list answer with an unreadable response (or work again).
  pub fn break_member_list(&self, conversation: ConversationId, is_broken: bool) {
    let mut state = self.lock();
    if is_broken {
      state.broken_member_lists.insert(conversation);
    } else {
      state.broken_member_lists.remove(&conversation);
    }
  }

  /// Makes writes of the conversation's read position answer 503 (or work again).
  pub fn fail_read_positions(&self, conversation: ConversationId, is_failing: bool) {
    let mut state = self.lock();
    if is_failing {
      state.failing_read_positions.insert(conversation);
    } else {
      state.failing_read_positions.remove(&conversation);
    }
  }

  /// How many read positions were written so far (requests, not changes).
  pub fn read_position_writes(&self) -> u64 {
    self.lock().read_position_writes
  }

  /// How far the user has read the conversation, as the server stores it.
  pub fn read_position(&self, conversation: ConversationId, user: UserId) -> Option<MessageId> {
    self
      .lock()
      .conversations
      .get(&conversation)
      .and_then(|entry| entry.read_positions.get(&user).copied())
  }

  /// How many gateway connections were opened so far.
  pub fn gateway_connect_count(&self) -> u64 {
    self.lock().gateway_connects
  }

  /// Like the server, closes a gateway connection whose client sent nothing for longer than
  /// `timeout` on `clock`. Checked when the next frame arrives: that frame is lost and the
  /// connection closes.
  pub fn enforce_heartbeat_timeout(&self, clock: Arc<dyn Clock + Send + Sync>, timeout: std::time::Duration) {
    let timeout_ms = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
    self.lock().maybe_heartbeat_timeout = Some((clock, timeout_ms));
  }

  /// How many connections [`Self::enforce_heartbeat_timeout`] closed.
  pub fn heartbeat_timeout_count(&self) -> u64 {
    self.lock().heartbeat_timeouts
  }

  /// The user's member lists answer only after `delay` (a catch-up of many conversations).
  pub fn delay_member_lists(&self, user: UserId, delay: std::time::Duration) {
    self.lock().member_list_delays.insert(user, delay);
  }

  /// The user's reads of registered devices (`GET /users/{id}/devices`) answer only after `delay`.
  pub fn delay_user_devices(&self, user: UserId, delay: std::time::Duration) {
    self.lock().user_device_delays.insert(user, delay);
  }

  /// While set, the user's reads of registered devices are refused with 403, a refusal that a
  /// retry doesn't change.
  pub fn refuse_user_devices(&self, user: UserId, is_refusing: bool) {
    let mut state = self.lock();
    if is_refusing {
      state.user_device_refusals.insert(user);
    } else {
      state.user_device_refusals.remove(&user);
    }
  }

  /// Answers `GET /sync/heads` with a bare 404 from now on, like an older server (issue #714).
  pub fn stop_serving_sync_heads(&self) {
    self.lock().lacks_sync_heads = true;
  }

  /// Answers the channel role log with 404 while `is_lacking`, like a server from before signed
  /// channel roles (issue #380).
  pub fn set_role_log_lacking(&self, is_lacking: bool) {
    self.lock().lacks_role_log = is_lacking;
  }

  /// The user's `GET /conversations` answers only after `delay` (a long full sync).
  pub fn delay_conversation_list(&self, user: UserId, delay: std::time::Duration) {
    self.lock().conversation_list_delays.insert(user, delay);
  }

  /// How many gateway heartbeats arrived so far.
  pub fn heartbeat_count(&self) -> u64 {
    self.lock().heartbeats
  }

  /// The `set_activity` frames `user`'s devices sent so far, in order.
  pub fn activity_reports(&self, user: UserId) -> Vec<DeviceActivity> {
    self
      .lock()
      .activity_reports
      .get(&user)
      .cloned()
      .unwrap_or_default()
  }

  /// The conversations of the `typing` frames `user`'s devices sent so far, in order.
  pub fn typing_reports(&self, user: UserId) -> Vec<ConversationId> {
    self.lock().typing_reports.get(&user).cloned().unwrap_or_default()
  }

  /// While set, a captive portal answers this user's REST calls (hotel Wi-Fi before signing in).
  pub fn set_behind_captive_portal(&self, user: UserId, is_behind: bool) {
    let mut state = self.lock();
    if is_behind {
      state.portal_users.insert(user);
    } else {
      state.portal_users.remove(&user);
    }
  }

  /// While set, this user's REST calls fail but the gateway still connects (REST timing out
  /// behind a flaky proxy, or the sync after `ready` being cut off).
  pub fn set_rest_failing(&self, user: UserId, is_failing: bool) {
    let mut state = self.lock();
    if is_failing {
      state.rest_failing_users.insert(user);
    } else {
      state.rest_failing_users.remove(&user);
    }
  }

  /// While set, the server answers this user's REST calls and gateway connects with 503, code
  /// `unavailable` and `Retry-After: retry_after` (a dependency outage, a restart behind the proxy).
  pub fn set_unavailable(&self, user: UserId, retry_after: std::time::Duration) {
    self.lock().unavailable_users.insert(user, retry_after.as_secs());
  }

  /// Ends [`Self::set_unavailable`].
  pub fn clear_unavailable(&self, user: UserId) {
    self.lock().unavailable_users.remove(&user);
  }

  /// How many gateway connects [`Self::set_unavailable`] answered with 503.
  pub fn unavailable_connects(&self) -> u64 {
    self.lock().unavailable_connects
  }

  /// Serves another identity key for a user, as a server substituting its own would.
  /// Lets `calls` more REST calls of `user` through, then cuts the network: the next call fails as
  /// `cut` says and the user stays offline until [`Self::set_offline`] brings them back.
  pub fn cut_network_after(&self, user: UserId, calls: u32, cut: NetworkCut) {
    self.lock().network_cuts.insert(user, (calls, cut));
  }

  /// Hands the next registered device the ID `id` (of another device), as a malicious server might.
  pub fn force_next_device_id(&self, id: DeviceId) {
    self.lock().maybe_forced_device_id = Some(id);
  }

  /// Gives the next message the ID `id` (one the server gave something else), as a malicious
  /// server might.
  pub fn force_next_message_id(&self, id: MessageId) {
    self.lock().maybe_forced_message_id = Some(id);
  }

  /// The next `count` binary frames sent over any relay are lost.
  pub fn lose_relay_frames(&self, count: u32) {
    self.lock().relay_frames_to_lose = count;
  }

  /// Every relay's lifetime ends, as the server's timer would end it.
  pub fn expire_relays(&self) {
    let mut state = self.lock();
    for (_, relay) in std::mem::take(&mut state.relays) {
      for (_, _, endpoint) in relay.endpoints {
        let _ = endpoint.unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::Expired));
      }
    }
  }

  /// The user's devices that are not revoked.
  pub fn active_devices(&self, user: UserId) -> Vec<DeviceId> {
    self
      .lock()
      .devices_of(user)
      .map(|device| device.response.id)
      .collect()
  }

  pub fn identity_key_of(&self, user: UserId) -> Option<IdentityKeyResponse> {
    self.lock().identity_keys.get(&user).cloned()
  }

  /// Drops the user's key bag, as on a server where an older client enrolled without one.
  pub fn forget_key_bag(&self, user: UserId) {
    self.lock().key_bags.remove(&user);
  }

  pub fn replace_identity_key(&self, user: UserId, public_key: Vec<u8>) {
    if let Some(entry) = self.lock().identity_keys.get_mut(&user) {
      entry.public_key = Base64Bytes::new(public_key);
    }
  }

  /// Forgets a user's identity key, so the server answers "not found" as a lying server would.
  pub fn forget_identity_key(&self, user: UserId) {
    self.lock().identity_keys.remove(&user);
  }

  /// Sets what `GET /unread` answers the user (the fake keeps no read positions of its own).
  pub fn set_unread(&self, user: UserId, unread: UnreadResponse) {
    self.lock().unread.insert(user, unread);
  }

  /// Closes all gateway connections of a user, as a network drop would. Sessions stay resumable.
  pub fn drop_gateways(&self, user: UserId) {
    for session in self
      .lock()
      .sessions
      .values_mut()
      .filter(|session| session.user == user)
    {
      session.maybe_live = None;
    }
  }

  /// Grants a user the server-side admin permissions (adding people to channels they're not in,
  /// posting admin proposals).
  /// Adds a call to the history that `list_call_history` serves to everybody (newest added first).
  pub fn add_call_history(&self, entry: teamcraft_proto::CallHistoryEntry) {
    let mut state = self.lock();
    state.call_history.insert(0, entry);
  }

  /// Sets when the user was last active (Unix milliseconds).
  pub fn set_last_seen(&self, user: UserId, at: i64) {
    self.lock().last_seen.insert(user, at);
  }

  /// The custom status the server keeps for the user (set through `PUT /me/status`).
  pub fn custom_status(&self, user: UserId) -> Option<teamcraft_proto::CustomStatus> {
    self.lock().custom_statuses.get(&user).cloned()
  }

  /// The server's clock now (the system clock plus [`FakeServer::advance_clock`]).
  pub fn server_now_ms(&self) -> i64 {
    self.lock().now_ms()
  }

  /// A malicious server makes up a request of `user` to join `conversation`, whatever it is, shows
  /// it to `recipients` (and tells them) and lets any of them "approve" it.
  pub fn plant_admin_join_request(&self, conversation: ConversationId, user: UserId, recipients: &[UserId]) {
    let mut state = self.lock();
    let mut request = FakeJoinRequest::open(state.now_ms());
    request.planted_for = recipients.iter().copied().collect();
    state.admin_join_requests.insert((conversation, user), request);
    state.publish(
      recipients.iter().copied(),
      &GatewayEvent::AdminJoinRequestUpdate {
        conversation_id: conversation,
        user_id: user,
        change: AdminJoinRequestChange::Requested,
      },
    );
  }

  /// From now on `PUT /me/status` answers 403, as with custom statuses switched off.
  pub fn refuse_custom_statuses(&self, is_refused: bool) {
    self.lock().refuses_custom_statuses = is_refused;
  }

  /// What `GET /me/notification-settings` answers for the user from now on.
  pub fn set_notification_settings(&self, user: UserId, settings: NotificationSettings) {
    self.lock().notification_settings.insert(user, settings);
  }

  pub fn set_admin(&self, user: UserId) {
    self.lock().admins.insert(user);
  }

  /// Revokes a device, as the device list or an admin would, and announces it like the server.
  pub fn revoke_device(&self, user: UserId, device: DeviceId) {
    self.lock().revoke_device(user, device);
  }

  /// Moves the server's clock forward (key package expiry). Packages keep the validity they had
  /// left when they were uploaded, measured on this clock: the fake can't make openmls, which
  /// checks lifetimes against the system clock, see the future.
  pub fn advance_clock(&self, by: std::time::Duration) {
    let by_ms = i64::try_from(by.as_millis()).unwrap_or(i64::MAX);
    let mut state = self.lock();
    state.clock_offset_ms = state.clock_offset_ms.saturating_add(by_ms);
  }

  /// The next `count` space / channel creations are made, but the answer never reaches the client.
  pub fn lose_create_answers(&self, count: u32) {
    self.lock().create_answers_to_lose = count;
  }

  /// The next key package upload is stored, but the answer never reaches the client.
  pub fn lose_next_key_package_answer(&self) {
    self.lock().key_package_answers_to_lose += 1;
  }

  /// A meeting's chat as the server creates it with the meeting (docs/meetings.md): the organizer
  /// is admin, the invitees are members, and no client has founded its encryption group.
  pub fn create_meeting_chat(&self, organizer: UserId, invitees: &[UserId]) -> ConversationId {
    let mut state = self.lock();
    let mut members = invitees.to_vec();
    members.push(organizer);
    let id = state.create_conversation(
      ConversationKind::Meeting,
      None,
      Some("Meeting".to_owned()),
      HistoryPolicy::Full,
      &members,
    );
    if let Some(conversation) = state.conversations.get_mut(&id)
      && let Some(entry) = conversation.members.get_mut(&organizer)
    {
      entry.0 = ChannelRole::Admin;
    }
    if let Some(conversation) = state.conversations.get_mut(&id) {
      conversation.response.maybe_organizer_user_id = Some(organizer);
    }
    state.publish(members, &GatewayEvent::ConversationUpdate { conversation_id: id });
    id
  }

  /// An entry the server puts into a channel's role log without any check: a hostile server, or
  /// a client that skipped them. Returns its version.
  pub fn plant_role_log_entry(
    &self,
    conversation: ConversationId,
    payload: Vec<u8>,
    signature: Vec<u8>,
  ) -> u64 {
    let mut state = self.lock();
    let created_at = UnixMillis(state.next_id::<UserId>().snowflake().unix_millis());
    let Some(entry) = state.conversations.get_mut(&conversation) else {
      return 0;
    };
    let version = entry.role_log.len() as u64 + 1;
    entry.role_log.push(teamcraft_proto::ChannelRoleLogEntryResponse {
      version,
      payload: Base64Bytes::new(payload),
      signature: Base64Bytes::new(signature),
      maybe_signer_device_id: None,
      created_at,
    });
    version
  }

  /// The channel's role log as stored.
  pub fn role_log(&self, conversation: ConversationId) -> Vec<teamcraft_proto::ChannelRoleLogEntryResponse> {
    self
      .lock()
      .conversations
      .get(&conversation)
      .map(|entry| entry.role_log.clone())
      .unwrap_or_default()
  }

  /// Sets a member's role on the server alone, as an older client or a hostile server would: no
  /// signed entry goes with it.
  pub fn set_channel_role_unsigned(&self, conversation: ConversationId, user: UserId, role: ChannelRole) {
    if let Some(entry) = self.lock().conversations.get_mut(&conversation)
      && let Some(held) = entry.members.get_mut(&user)
    {
      held.0 = role;
    }
  }

  /// Group information the server holds for the conversation's current epoch although no member
  /// published it: a member or the server planting information nobody can join.
  pub fn plant_group_info(&self, conversation: ConversationId, group_info: &[u8]) {
    if let Some(entry) = self.lock().conversations.get_mut(&conversation) {
      entry.maybe_group_info = Some((entry.epoch, Base64Bytes::new(group_info.to_vec())));
    }
  }

  /// The conversation's call ends: the next join token opens a new call with a new ID.
  pub fn end_call(&self, conversation: ConversationId) {
    let mut state = self.lock();
    state.calls.remove(&conversation);
    state.call_rosters.remove(&conversation);
  }

  /// The SFU reports that `device` of `user` joined the conversation's call: the first one starts it.
  /// Members hear `call_start` or `call_update`, as from the server.
  pub fn join_call(&self, conversation: ConversationId, user: UserId, device: DeviceId) {
    let mut state = self.lock();
    let call_id = match state.calls.get(&conversation) {
      Some(call_id) => *call_id,
      None => {
        let call_id = state.next_id();
        state.calls.insert(conversation, call_id);
        call_id
      },
    };
    let entry = CallRosterEntry {
      user_id: user,
      device_id: device,
    };
    let is_start = !state.call_rosters.contains_key(&conversation);
    let roster = state
      .call_rosters
      .entry(conversation)
      .or_insert_with(|| CallStateResponse {
        call_id,
        conversation_id: conversation,
        started_by: user,
        started_at: UnixMillis(0),
        is_ringing: false,
        participants: Vec::new(),
      });
    if roster.participants.contains(&entry) {
      return;
    }
    roster.participants.push(entry);
    let roster = roster.clone();
    let event = if is_start {
      GatewayEvent::CallStart {
        conversation_id: conversation,
        call_id,
        started_by: roster.started_by,
        started_at: roster.started_at,
        is_ringing: roster.is_ringing,
        participants: roster.participants,
      }
    } else {
      GatewayEvent::CallUpdate {
        conversation_id: conversation,
        call_id,
        is_ringing: roster.is_ringing,
        participants: roster.participants,
      }
    };
    let members = state.member_ids(conversation);
    state.publish(members, &event);
  }

  /// The SFU reports that `device` of `user` left the conversation's call: exactly that device
  /// leaves the roster, the last one ends the call. Members hear `call_update` or `call_end`.
  pub fn leave_call(&self, conversation: ConversationId, user: UserId, device: DeviceId) {
    let mut state = self.lock();
    if let Some(event) = state.leave_call(conversation, user, device) {
      let members = state.member_ids(conversation);
      state.publish(members, &event);
    }
  }

  /// Like [`Self::leave_call`], but nobody hears of it: the event the server should have sent got
  /// lost (a server that restarted while it ended, a report it couldn't place).
  pub fn leave_call_unannounced(&self, conversation: ConversationId, user: UserId, device: DeviceId) {
    self.lock().leave_call(conversation, user, device);
  }

  /// The calls moved into a meeting so far, in order.
  pub fn call_moves(&self) -> Vec<(ConversationId, MoveCallRequest)> {
    self.lock().call_moves.clone()
  }

  /// How many channels have this name.
  pub fn channel_count(&self, name: &str) -> usize {
    let state = self.lock();
    state
      .conversations
      .values()
      .filter(|c| c.response.maybe_name.as_deref() == Some(name))
      .count()
  }

  /// The user's next commit is stored and fanned out, but the answer to the client is lost (a
  /// timeout or a killed app right after sending).
  pub fn lose_next_commit_answer(&self, user: UserId) {
    self.lock().losing_commit_answers.insert(user);
  }

  /// From now on the server refuses `user`'s removals (and the check before them) with `403`, as
  /// for a member without the right to remove somebody.
  pub fn refuse_removals_by(&self, user: UserId) {
    self.lock().refusing_removals_by.insert(user);
  }

  /// The user's next member removal (`DELETE …/members/{user}`) or device revocation fails as
  /// `cut` says, without taking the user offline: the app was killed before sending it, or right
  /// after the server handled it.
  pub fn cut_next_removal(&self, user: UserId, cut: NetworkCut) {
    self.lock().removal_cuts.insert(user, cut);
  }

  /// While `is_refusing`, the server refuses `user`'s member removals with `403` but still allows
  /// the check before them: the remover lost the right between the two.
  pub fn refuse_removal_steps_by(&self, user: UserId, is_refusing: bool) {
    let mut state = self.lock();
    if is_refusing {
      state.refusing_removal_steps_by.insert(user);
    } else {
      state.refusing_removal_steps_by.remove(&user);
    }
  }

  /// Member removals (`DELETE …/members/{user}`) requested so far, refused or not.
  pub fn removal_requests(&self) -> u64 {
    self.lock().removal_requests
  }

  /// The user's next MLS proposal (a signed change, a rejoin request) fails as `cut` says.
  pub fn cut_next_proposal(&self, user: UserId, cut: NetworkCut) {
    self.lock().proposal_cuts.insert(user, cut);
  }

  /// The user's next MLS application message (content keys) fails as `cut` says.
  pub fn cut_next_application(&self, user: UserId, cut: NetworkCut) {
    self.lock().application_cuts.insert(user, cut);
  }

  /// The user's next MLS application message is accepted but reaches the group's queue only on
  /// [`Self::release_held_messages`]: two members sending at once, each before seeing the other's.
  pub fn hold_next_application(&self, user: UserId) {
    self.lock().holding_application_of.insert(user);
  }

  /// The user's next MLS proposal (a rejoin request, a signed change) is accepted but reaches the
  /// group's queue only on [`Self::release_held_messages`]: a server that holds a request back and
  /// delivers it later.
  pub fn hold_next_proposal(&self, user: UserId) {
    self.lock().holding_proposal_of.insert(user);
  }

  /// Publishes the held application messages and proposals, after everything posted meanwhile.
  pub fn release_held_messages(&self) {
    let held = std::mem::take(&mut self.lock().held_applications);
    for (user, conversation, request) in held {
      let mut state = self.lock();
      let message = MlsMessageResponse {
        id: state.next_id(),
        conversation_id: conversation,
        epoch: request.epoch,
        kind: request.kind.clone(),
        sender_device_id: request.device_id,
        maybe_recipient_device_id: None,
        payload: request.payload.clone(),
      };
      let Ok(entry) = state.actor_conversation_mut(conversation, user) else {
        continue;
      };
      entry.mls.push(message.clone());
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(members, &GatewayEvent::MlsMessage { message });
    }
  }

  /// While `is_dropping`, the server answers `user`'s messages as stored but neither keeps nor
  /// delivers them: a server that keeps a message from the members (or a sender that crashed
  /// before sending it, as the members see it).
  pub fn drop_messages_of(&self, user: UserId, is_dropping: bool) {
    let mut state = self.lock();
    if is_dropping {
      state.dropping_messages_of.insert(user);
    } else {
      state.dropping_messages_of.remove(&user);
    }
  }

  /// From now on the server refuses `user` the queues of their groups (`403`) while still listing
  /// them as a member: a server that wants a member to stop following a group.
  pub fn refuse_queue_to(&self, user: UserId) {
    self.lock().queue_refused_to.insert(user);
  }

  /// The next lookup of `user`'s identity key (by anyone) fails with a network error.
  pub fn fail_next_identity_lookup(&self, user: UserId) {
    self.lock().failing_identity_lookups.insert(user);
  }

  /// Moves the group's epoch forward as if members had committed `by` times (that this fake
  /// doesn't see): for a device outside the group, which only learns the epoch.
  pub fn bump_epoch(&self, conversation: ConversationId, by: u64) {
    if let Some(entry) = self.lock().conversations.get_mut(&conversation) {
      entry.epoch = entry.epoch.saturating_add(by);
    }
  }

  /// Hands `device` a welcome to `conversation` that no commit made, as a malicious server might.
  pub fn inject_welcome(&self, conversation: ConversationId, device: DeviceId, epoch: u64, payload: Vec<u8>) {
    let mut state = self.lock();
    let id = state.next_id();
    state.welcomes.push(MlsMessageResponse {
      id,
      conversation_id: conversation,
      epoch,
      kind: MlsMessageKind::Welcome,
      sender_device_id: device,
      maybe_recipient_device_id: Some(device),
      payload: Base64Bytes::new(payload),
    });
  }

  /// The welcomes stored for `device`, oldest first.
  pub fn welcomes_for(&self, device: DeviceId) -> Vec<MlsMessageResponse> {
    self
      .lock()
      .welcomes
      .iter()
      .filter(|welcome| welcome.maybe_recipient_device_id == Some(device))
      .cloned()
      .collect()
  }

  /// The user's next commit is lost on the way: never stored, the client sees a network error.
  pub fn drop_next_commit(&self, user: UserId) {
    self.lock().dropping_commits.insert(user);
  }

  /// The next commit anyone makes reaches `user` garbled, so that client refuses it while the
  /// others merge it: the device falls out of the group (a bug, a rolled-back store).
  pub fn garble_next_commit_for(&self, user: UserId) {
    self.lock().garbling_next_commit_for.insert(user);
  }

  /// From now on serves `response` as the conversation's group information, whatever the group's
  /// epoch: a server handing out an old one.
  pub fn serve_group_info(
    &self,
    conversation: ConversationId,
    response: teamcraft_proto::MlsGroupInfoResponse,
  ) {
    self.lock().forced_group_infos.insert(conversation, response);
  }

  /// The next `count` reads of the conversation's group information answer "not found" although the
  /// server holds it: two devices checking at the same moment, before either has published.
  pub fn hide_group_info_reads(&self, conversation: ConversationId, count: u32) {
    self.lock().hidden_group_info_reads.insert(conversation, count);
  }

  /// Forgets every stored MLS group information, as for groups whose last commit came from a
  /// client that sent none.
  pub fn forget_group_infos(&self) {
    for conversation in self.lock().conversations.values_mut() {
      conversation.maybe_group_info = None;
    }
  }

  /// From now on revoked devices stay listed and keep their key packages (a stale server).
  pub fn ignore_revocations(&self) {
    self.lock().is_ignoring_revocations = true;
  }

  /// Replaces the device's regular key packages with garbage, as a broken or malicious server
  /// could.
  pub fn corrupt_key_packages(&self, device: DeviceId) {
    if let Some(packages) = self.lock().key_packages.get_mut(&device) {
      for package in &mut packages.regular {
        package.bytes = Base64Bytes::new(b"not a key package".to_vec());
      }
    }
  }

  /// Regular key packages the device has on the server, expired or not.
  pub fn stored_key_packages(&self, device: DeviceId) -> usize {
    self
      .lock()
      .key_packages
      .get(&device)
      .map_or(0, |packages| packages.regular.len())
  }

  /// Serves `list` as the user's device list, as a server holding back a newer one would.
  pub fn serve_device_list(&self, user: UserId, list: DeviceListResponse) {
    self.lock().device_lists.insert(user, list);
  }

  pub fn device_list_of(&self, user: UserId) -> Option<DeviceListResponse> {
    self.lock().device_lists.get(&user).cloned()
  }

  /// Forgets the user's signed device list, as a server that lost it (or never kept one) would.
  pub fn forget_device_list(&self, user: UserId) {
    self.lock().device_lists.remove(&user);
  }

  /// Replaces a stored organization background, as a compromised server could.
  pub fn swap_org_background(&self, id: OrgBackgroundId, picture: Vec<u8>) {
    if let Some(entry) = self.lock().org_backgrounds.get_mut(&id) {
      entry.size_bytes = picture.len() as u64;
      entry.maybe_blob = Some(picture);
    }
  }

  /// Sends an event as the server would (for events whose endpoints the fake doesn't have, like
  /// read receipts and acknowledgements).
  pub fn publish_event(&self, users: &[UserId], event: &GatewayEvent) {
    self.lock().publish(users.iter().copied(), event);
  }

  /// Removes a member on the server's own authority (admin action, deactivation).
  pub fn remove_member_as_admin(&self, conversation: ConversationId, user: UserId) {
    let mut state = self.lock();
    let Some(entry) = state.conversations.get_mut(&conversation) else {
      return;
    };
    entry.members.remove(&user);
    let mut audience: Vec<UserId> = entry.members.keys().copied().collect();
    audience.push(user);
    state.publish(
      audience,
      &GatewayEvent::MembershipRemove {
        conversation_id: conversation,
        user_id: user,
      },
    );
  }

  /// Appends a log entry without any checks, as a compromised server could.
  pub fn inject_workspace_entry(&self, kind: WorkspaceLogKind, payload: Vec<u8>, signature: Vec<u8>) {
    let mut state = self.lock();
    let log = state.log_mut(&kind);
    let version = log.len() as u64 + 1;
    log.push(WorkspaceLogEntryResponse {
      kind: kind.clone(),
      version,
      payload: Base64Bytes::new(payload),
      signature: Base64Bytes::new(signature),
      maybe_signer_device_id: None,
      created_at: UnixMillis(i64::try_from(version).unwrap_or(0)),
    });
    let everyone: Vec<UserId> = state.users.keys().copied().collect();
    state.publish(everyone, &GatewayEvent::WorkspaceUpdate { kind, version });
  }

  /// A user's wrapped backup keys, as the (still missing) recovery endpoint would hand them to the
  /// approving admins.
  pub fn wrapped_backup_keys_of(&self, user: UserId) -> Option<Vec<u8>> {
    self
      .lock()
      .key_bags
      .get(&user)
      .map(|bag| bag.wrapped_backup_keys.as_slice().to_vec())
  }

  /// The key bag stored for a user.
  pub fn key_bag_of(&self, user: UserId) -> Option<KeyBagResponse> {
    self.lock().key_bags.get(&user).cloned()
  }

  /// Serves `bag` as the user's key bag with a newer version number, as a server rolling back to
  /// an older upload (or swapping the wrapped keys) would.
  pub fn serve_key_bag(&self, user: UserId, mut bag: KeyBagResponse) {
    let mut state = self.lock();
    bag.version = state.key_bags.get(&user).map_or(1, |current| current.version + 1);
    state.key_bags.insert(user, bag);
  }

  /// Takes a known user out of every space, so `add_space_member` has someone to add.
  pub fn make_space_outsider(&self, user: UserId) {
    self.lock().space_outsiders.insert(user);
  }

  /// People added to spaces so far (`add_space_member`), in order.
  pub fn space_member_adds(&self) -> Vec<(SpaceId, AddSpaceMemberRequest)> {
    self.lock().space_member_adds.clone()
  }

  /// The personal messages of the invitations sent so far, in order.
  pub fn invite_messages(&self) -> Vec<Option<String>> {
    self.lock().invite_messages.clone()
  }

  /// Space invitations sent so far.
  pub fn invites(&self) -> Vec<InviteResponse> {
    self.lock().invites.clone()
  }

  /// Marks a conversation as having guests in it, as the server reports once a guest joins.
  pub fn set_contains_guests(&self, conversation: ConversationId) {
    if let Some(entry) = self.lock().conversations.get_mut(&conversation) {
      entry.response.contains_guests = true;
    }
  }

  /// What the server answers as `my_permissions` for the conversation from now on (the same for
  /// every member); `None` answers like an older server.
  pub fn set_my_permissions(
    &self,
    conversation: ConversationId,
    maybe_permissions: Option<Vec<teamcraft_proto::Permission>>,
  ) {
    if let Some(entry) = self.lock().conversations.get_mut(&conversation) {
      entry.response.maybe_my_permissions = maybe_permissions;
    }
  }

  /// Refuses attachments larger than `limit` bytes, like the server's `max_attachment_bytes`.
  /// While set, this user's blob uploads fail like a refused request; the REST API still answers.
  pub fn refuse_blob_uploads(&self, user: UserId, is_refused: bool) {
    let mut state = self.lock();
    if is_refused {
      state.blob_refused_users.insert(user);
    } else {
      state.blob_refused_users.remove(&user);
    }
  }

  /// Attachments announced so far, finished or not.
  pub fn attachment_count(&self) -> usize {
    self.lock().attachments.len()
  }

  /// Parts stored by uploads in parts so far (a part sent again counts again).
  pub fn part_upload_count(&self) -> u64 {
    self.lock().part_uploads
  }

  /// Unfinished uploads the clients gave up, in order.
  pub fn abandoned_attachments(&self) -> Vec<AttachmentId> {
    self.lock().abandoned_attachments.clone()
  }

  /// While set, uploads in parts keep only the parts' sizes: a test of a large file then measures
  /// the client's memory, not the fake storage's.
  pub fn discard_parts(&self, is_discarding: bool) {
    self.lock().is_discarding_parts = is_discarding;
  }

  pub fn set_max_attachment_bytes(&self, limit: u64) {
    self.lock().maybe_max_attachment_bytes = Some(limit);
  }

  /// What the public download page of a share link serves: the ciphertext, no account needed.
  pub fn shared_blob(&self, share_id: &str) -> Option<Vec<u8>> {
    let state = self.lock();
    if state.revoked_share_links.contains(share_id) {
      return None;
    }
    let attachment = state.share_links.get(share_id)?;
    state.attachments.get(attachment)?.maybe_blob.clone()
  }

  /// A share link revoked behind the client's back (an admin, another device).
  pub fn revoke_shared_link(&self, share_id: &str) {
    self.lock().revoked_share_links.insert(share_id.to_owned());
  }

  /// The push registration of a device (to seal wake-ups in tests).
  pub fn push_registration(&self, device: DeviceId) -> Option<RegisterPushRequest> {
    self.lock().push_registrations.get(&device).cloned()
  }

  /// Forgets a device's push registration, as the server does when a push service calls it gone.
  pub fn forget_push_registration(&self, device: DeviceId) {
    self.lock().push_registrations.remove(&device);
  }

  /// Makes the server send through a push gateway (apps then seal their device tokens).
  pub fn set_push_gateway(&self, maybe_binding: Option<PushGatewayBinding>) {
    self.lock().push_gateway = maybe_binding;
  }

  /// The public key the fake gateway publishes (any fixed key; tests open sealed tokens with
  /// [`FAKE_GATEWAY_TOKEN_SEED`]).
  pub fn gateway_token_public_key() -> [u8; 32] {
    teamcraft_crypto::push_gateway::token_public_key(&FAKE_GATEWAY_TOKEN_SEED).unwrap_or([0; 32])
  }

  /// The gateway URLs the revocations went to.
  pub fn revoked_gateway_urls(&self) -> Vec<String> {
    self.lock().revoked_gateway_urls.clone()
  }

  /// Sealed tokens revoked so far.
  pub fn revoked_gateway_tokens(&self) -> Vec<Vec<u8>> {
    self.lock().revoked_gateway_tokens.clone()
  }

  /// Flips a byte in every stored blob, as a malicious storage backend could.
  pub fn tamper_blobs(&self) {
    for attachment in self.lock().attachments.values_mut() {
      if let Some(byte) = attachment.maybe_blob.as_mut().and_then(|blob| blob.last_mut()) {
        *byte ^= 1;
      }
    }
  }

  /// The mention lists the server stored (plaintext metadata, e2ee.md §4.4).
  pub fn stored_mentions(&self) -> Vec<Vec<UserId>> {
    let state = self.lock();
    state
      .conversations
      .values()
      .flat_map(|conversation| {
        conversation
          .messages
          .iter()
          .map(|message| message.mentioned_user_ids.clone())
      })
      .collect()
  }

  /// Every blob in object storage, to check that nothing readable is uploaded.
  pub fn stored_blobs(&self) -> Vec<Vec<u8>> {
    self
      .lock()
      .attachments
      .values()
      .filter_map(|attachment| attachment.maybe_blob.clone())
      .collect()
  }

  /// Forgets all sessions, so resuming fails with `session_invalid` (e.g. after a server restart).
  pub fn expire_sessions(&self) {
    self.lock().sessions.clear();
  }

  /// All stored message ciphertexts, to check that nothing readable reaches the server.
  pub fn stored_ciphertexts(&self) -> Vec<Vec<u8>> {
    let state = self.lock();
    state
      .conversations
      .values()
      .flat_map(|conversation| {
        conversation
          .messages
          .iter()
          .map(|message| message.ciphertext.as_slice().to_vec())
      })
      .collect()
  }

  fn lock(&self) -> MutexGuard<'_, State> {
    // A poisoned lock only means a test thread panicked; the state is still usable.
    self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl FakeServerApi {
  pub fn user_id(&self) -> UserId {
    self.user
  }

  pub fn server(&self) -> &FakeServer {
    &self.server
  }

  /// Runs `action` on the state if the user is online.
  fn with<R>(&self, action: impl FnOnce(&mut State, UserId) -> Result<R, ApiError>) -> Result<R, ApiError> {
    let mut state = self.server.lock();
    *state.rest_calls.entry(self.user).or_default() += 1;
    if state.offline_users.contains(&self.user) || state.rest_failing_users.contains(&self.user) {
      return Err(ApiError::Network("fake server: client is offline".into()));
    }
    if let Some(error) = state.unavailable_answer(self.user) {
      return Err(error);
    }
    if state.signed_out_users.contains(&self.user) {
      return Err(ApiError::status(401, ErrorCode::Unauthorized, "token expired"));
    }
    if state.portal_users.contains(&self.user) {
      return Err(ApiError::Intercepted {
        kind: super::Interception::CaptivePortal,
        detail: "fake server: HTTP 200 with an HTML page".into(),
      });
    }
    let Some(cut) = state.take_network_cut(self.user) else {
      return action(&mut state, self.user);
    };
    if cut == NetworkCut::ResponseLost {
      // Deliberately ignored: the client never learns the outcome.
      let _ = action(&mut state, self.user);
    }
    Err(ApiError::Network("fake server: network cut".into()))
  }

  fn post_mls_message_now(
    &self,
    conversation: ConversationId,
    request: &PostMlsMessageRequest,
  ) -> Result<MlsMessageResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.device_id)?;
      // Non-members may only post proposals, and only with admin rights (admin.md §2.2).
      let is_member = state.member_conversation(conversation, me).is_ok();
      if !is_member && !(request.kind == MlsMessageKind::Proposal && state.admins.contains(&me)) {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "only members can post",
        ));
      }
      let current_epoch = state.conversation(conversation)?.epoch;
      if request.epoch != current_epoch {
        return Err(ApiError::status(
          409,
          ErrorCode::EpochConflict,
          format!("the group is in epoch {current_epoch}"),
        ));
      }
      let message = MlsMessageResponse {
        id: state.next_id(),
        conversation_id: conversation,
        epoch: request.epoch,
        kind: request.kind.clone(),
        sender_device_id: request.device_id,
        maybe_recipient_device_id: None,
        payload: request.payload.clone(),
      };
      let entry = state.actor_conversation_mut(conversation, me)?;
      entry.mls.push(message.clone());
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(
        members,
        &GatewayEvent::MlsMessage {
          message: message.clone(),
        },
      );
      Ok(message)
    })
  }

  /// Accepts `request` without publishing it until [`FakeServer::release_held_messages`].
  fn hold(
    &self,
    state: &mut State,
    conversation: ConversationId,
    request: &PostMlsMessageRequest,
  ) -> MlsMessageResponse {
    let id = state.next_id();
    state
      .held_applications
      .push((self.user, conversation, request.clone()));
    MlsMessageResponse {
      id,
      conversation_id: conversation,
      epoch: request.epoch,
      kind: request.kind.clone(),
      sender_device_id: request.device_id,
      maybe_recipient_device_id: None,
      payload: request.payload.clone(),
    }
  }

  /// [`Self::with`], failing as [`FakeServer::cut_next_removal`] asked.
  fn with_removal_cut<R>(
    &self,
    action: impl FnOnce(&mut State, UserId) -> Result<R, ApiError>,
  ) -> Result<R, ApiError> {
    let maybe_cut = self.server.lock().removal_cuts.remove(&self.user);
    match maybe_cut {
      None => self.with(action),
      Some(NetworkCut::BeforeRequest) => Err(ApiError::Network("fake server: never sent".into())),
      Some(NetworkCut::ResponseLost) => {
        // Deliberately ignored: the client never learns the outcome.
        let _ = self.with(action);
        Err(ApiError::Network("fake server: answer lost".into()))
      },
    }
  }
}

impl ServerApi for FakeServerApi {
  type Gateway = FakeGateway;
  type Rendezvous = FakeRendezvous;

  async fn me(&self) -> Result<GetMeResponse, ApiError> {
    self.with(|state, me| {
      let user = state.user(me)?;
      Ok(GetMeResponse {
        id: me,
        maybe_username: user.maybe_username.clone(),
        display_name: user.display_name.clone(),
        role: Role::Member,
      })
    })
  }

  async fn user(&self, user: UserId) -> Result<UserSummary, ApiError> {
    self.with(|state, _| {
      if state.hidden_users.contains(&user) {
        return Err(ApiError::status(404, ErrorCode::UserNotFound, "unknown user"));
      }
      let summary = state.user(user)?;
      // Like the server: a deleted account's tombstone is shown, a deactivated one is not.
      if summary.is_deactivated && !summary.is_deleted {
        return Err(ApiError::status(404, ErrorCode::UserNotFound, "unknown user"));
      }
      Ok(summary.clone())
    })
  }

  async fn search_users(&self, query: &str) -> Result<teamcraft_proto::SearchUsersResponse, ApiError> {
    let needle = query.trim().to_lowercase();
    self.with(|state, _| {
      state.directory_searches += 1;
      let mut users: Vec<UserSummary> = state
        .users
        .values()
        .filter(|user| {
          user.display_name.to_lowercase().contains(&needle)
            || user
              .maybe_username
              .as_deref()
              .is_some_and(|name| name.to_lowercase().contains(&needle))
        })
        .cloned()
        .collect();
      users.sort_by(|a, b| (&a.display_name, a.id).cmp(&(&b.display_name, b.id)));
      users.truncate(DIRECTORY_PAGE);
      for user in &mut users {
        user.maybe_has_devices = Some(state.devices_of(user.id).next().is_some());
      }
      Ok(teamcraft_proto::SearchUsersResponse { users })
    })
  }

  async fn register_device(&self, request: &RegisterDeviceRequest) -> Result<DeviceResponse, ApiError> {
    self.with(|state, me| {
      let id = match state.maybe_forced_device_id.take() {
        Some(forced) => forced,
        None => state.next_id(),
      };
      let response = DeviceResponse {
        id,
        name: request.name.clone(),
        public_key: request.public_key.clone(),
        cross_signature: request.cross_signature.clone(),
        maybe_push_provider: None,
        created_at: UnixMillis(0),
      };
      state.devices.insert(
        id,
        Device {
          user: me,
          response: response.clone(),
        },
      );
      // Like the server: everybody sharing a conversation, and the user's other devices.
      let mut audience = state.users_sharing_conversation(me);
      audience.insert(me);
      state.publish(
        audience,
        &GatewayEvent::DeviceAdd {
          user_id: me,
          device_id: id,
        },
      );
      Ok(response)
    })
  }

  async fn list_user_devices(&self, user: UserId) -> Result<ListDevicesResponse, ApiError> {
    let maybe_delay = self.server.lock().user_device_delays.get(&self.user).copied();
    if let Some(delay) = maybe_delay {
      crate::platform::sleep(delay).await;
    }
    self.with(|state, me| {
      if state.user_device_refusals.contains(&me) {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "not allowed"));
      }
      Ok(ListDevicesResponse {
        devices: state
          .devices_of(user)
          .map(|device| {
            // Like the server: other users get the keys, not the name or the push service.
            if user == me {
              device.response.clone()
            } else {
              DeviceResponse {
                name: String::new(),
                maybe_push_provider: None,
                ..device.response.clone()
              }
            }
          })
          .collect(),
      })
    })
  }

  async fn update_device(
    &self,
    device: DeviceId,
    request: &UpdateDeviceRequest,
  ) -> Result<DeviceResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      let name = request.name.trim();
      if name.is_empty() {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "empty device name",
        ));
      }
      let stored = state
        .devices
        .get_mut(&device)
        .ok_or_else(|| ApiError::status(404, ErrorCode::DeviceNotFound, "unknown device"))?;
      stored.response.name = name.to_owned();
      Ok(stored.response.clone())
    })
  }

  async fn put_identity_key(
    &self,
    request: &UploadIdentityKeyRequest,
  ) -> Result<IdentityKeyResponse, ApiError> {
    self.with(|state, me| {
      if state.identity_keys.contains_key(&me) && !request.replace {
        return Err(ApiError::status(409, ErrorCode::Conflict, "identity key exists"));
      }
      // Like the server: the key and its first key bag in one transaction.
      let key_bag = request.maybe_key_bag.as_ref().ok_or_else(|| {
        ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "a new identity key needs its key bag",
        )
      })?;
      let version = state.key_bags.get(&me).map_or(0, |bag| bag.version) + 1;
      state.key_bags.insert(
        me,
        KeyBagResponse {
          version,
          encrypted_key_bag: key_bag.encrypted_key_bag.clone(),
          wrapped_backup_keys: key_bag.wrapped_backup_keys.clone(),
          updated_at: UnixMillis(0),
        },
      );
      let stored = IdentityKeyResponse {
        user_id: me,
        public_key: request.public_key.clone(),
        signature: request.signature.clone(),
        maybe_attestation: request.maybe_attestation.clone(),
        created_at: UnixMillis(0),
        maybe_key_bag_version: None,
      };
      state.identity_keys.insert(me, stored.clone());
      Ok(IdentityKeyResponse {
        maybe_key_bag_version: Some(version),
        ..stored
      })
    })
  }

  async fn identity_key(&self, user: UserId) -> Result<Option<IdentityKeyResponse>, ApiError> {
    self.with(|state, _| {
      if state.failing_identity_lookups.remove(&user) {
        return Err(ApiError::Network("fake server: identity lookup cut".into()));
      }
      Ok(state.identity_keys.get(&user).cloned())
    })
  }

  async fn device_list(&self, user: UserId) -> Result<Option<DeviceListResponse>, ApiError> {
    self.with(|state, _| Ok(state.device_lists.get(&user).cloned()))
  }

  async fn put_device_list(&self, request: &PutDeviceListRequest) -> Result<DeviceListResponse, ApiError> {
    self.with(|state, me| {
      let current = state.device_lists.get(&me).map_or(0, |list| list.sequence);
      if request.expected_sequence != current || request.sequence <= current {
        return Err(ApiError::status(
          409,
          ErrorCode::VersionConflict,
          format!("stored device list is {current}"),
        ));
      }
      let stored = DeviceListResponse {
        sequence: request.sequence,
        signed_list: request.signed_list.clone(),
      };
      state.device_lists.insert(me, stored.clone());
      Ok(stored)
    })
  }

  async fn revoke_device(&self, device: DeviceId) -> Result<(), ApiError> {
    self.with_removal_cut(|state, me| {
      if state.devices.get(&device).is_none_or(|entry| entry.user != me) {
        return Err(ApiError::status(404, ErrorCode::DeviceNotFound, "no such device"));
      }
      state.revoke_device(me, device);
      Ok(())
    })
  }

  async fn announce_device_link(
    &self,
    request: &CreateDeviceLinkRequest,
  ) -> Result<DeviceLinkRequest, ApiError> {
    self.with(|state, me| {
      let is_waiting = state.relays.get(&request.rendezvous_id).is_some_and(|relay| {
        relay.endpoints.len() == 1 && relay.endpoints.iter().all(|(_, user, _)| *user == me)
      });
      if !is_waiting || state.link_requests.contains_key(&request.rendezvous_id) {
        return Err(ApiError::status(
          409,
          ErrorCode::Conflict,
          "no device is waiting at that rendezvous",
        ));
      }
      let announced = DeviceLinkRequest {
        id: request.rendezvous_id.clone(),
        key: request.key.clone(),
        device_name: request.device_name.clone(),
        platform: request.platform.clone(),
        expires_in_secs: 300,
      };
      state.link_requests.insert(
        announced.id.clone(),
        FakeLinkRequest {
          user: me,
          request: announced.clone(),
        },
      );
      state.publish(
        [me],
        &GatewayEvent::DeviceLinkRequested {
          request: announced.clone(),
        },
      );
      Ok(announced)
    })
  }

  async fn device_links(&self) -> Result<Vec<DeviceLinkRequest>, ApiError> {
    self.with(|state, me| {
      Ok(
        state
          .link_requests
          .values()
          .filter(|entry| entry.user == me)
          .map(|entry| entry.request.clone())
          .collect(),
      )
    })
  }

  async fn reject_device_link(&self, request_id: &str) -> Result<(), ApiError> {
    self.with(|state, me| {
      if state
        .link_requests
        .get(request_id)
        .is_none_or(|entry| entry.user != me)
      {
        return Err(ApiError::status(
          404,
          ErrorCode::DeviceLinkNotFound,
          "no such link request",
        ));
      }
      state.link_requests.remove(request_id);
      if let Some(relay) = state.relays.get(request_id) {
        for (_, _, waiting) in &relay.endpoints {
          let _ = waiting.unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::Rejected));
        }
      }
      state.publish(
        [me],
        &GatewayEvent::DeviceLinkClosed {
          request_id: request_id.to_owned(),
          reason: DeviceLinkClosedReason::Rejected,
        },
      );
      Ok(())
    })
  }

  async fn admin_join_requests(&self) -> Result<Vec<AdminJoinRequestResponse>, ApiError> {
    self.with(|state, me| {
      let mut requests = Vec::new();
      for ((conversation, user), request) in &state.admin_join_requests {
        let Some(entry) = state.conversations.get(conversation) else {
          continue;
        };
        let is_owner = entry
          .members
          .get(&me)
          .is_some_and(|(role, _)| *role == ChannelRole::Admin);
        let is_shown = *user == me || is_owner || request.planted_for.contains(&me);
        if request.state == FakeJoinState::Open && is_shown && !entry.members.contains_key(user) {
          requests.push(state.admin_join_response(*conversation, *user, request.requested_at)?);
        }
      }
      Ok(requests)
    })
  }

  async fn request_admin_join(
    &self,
    conversation: ConversationId,
  ) -> Result<AdminJoinRequestResponse, ApiError> {
    self.with(|state, me| {
      if !state.admins.contains(&me) {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "admins only"));
      }
      let entry = state.conversation(conversation)?;
      if entry.response.visibility != ConversationVisibility::Private || entry.members.contains_key(&me) {
        return Err(ApiError::status(409, ErrorCode::Conflict, "nothing to ask for"));
      }
      let key = (conversation, me);
      match state.admin_join_requests.get(&key).map(|request| request.state) {
        Some(FakeJoinState::Open) => {},
        Some(FakeJoinState::Declined | FakeJoinState::Withdrawn) => {
          return Err(ApiError::status(
            409,
            ErrorCode::AdminJoinRequestTooSoon,
            "asked recently",
          ));
        },
        Some(FakeJoinState::Approved) | None => {
          let request = FakeJoinRequest::open(state.now_ms());
          state.admin_join_requests.insert(key, request);
          state.publish_admin_join(conversation, me, AdminJoinRequestChange::Requested);
        },
      }
      let requested_at = state.admin_join_requests[&key].requested_at;
      state.admin_join_response(conversation, me, requested_at)
    })
  }

  async fn approve_admin_join(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<ConversationMemberResponse, ApiError> {
    self.with(|state, me| {
      let key = (conversation, user);
      let is_planted_for_me = state
        .admin_join_requests
        .get(&key)
        .is_some_and(|request| request.planted_for.contains(&me));
      // A planted request: the malicious server lets anybody "approve" it.
      if !is_planted_for_me {
        state.require_channel_owner(conversation, me)?;
      }
      let summary = state.user(user)?.clone();
      let answer = ConversationMemberResponse {
        user: summary.clone(),
        role: ChannelRole::Member,
      };
      match state.admin_join_requests.get_mut(&key) {
        Some(request) if request.state == FakeJoinState::Open => {
          request.state = FakeJoinState::Approved;
          request.maybe_decided_by = Some(me);
        },
        // This owner's retry: answered again, nothing changes.
        Some(request) if request.state == FakeJoinState::Approved && request.maybe_decided_by == Some(me) => {
          return Ok(answer);
        },
        _ => {
          return Err(ApiError::status(
            404,
            ErrorCode::AdminJoinRequestNotFound,
            "no open request",
          ));
        },
      }
      let floor = state.peek_id();
      let entry = state.conversation_mut(conversation)?;
      if entry.members.insert(user, (ChannelRole::Member, floor)).is_none() {
        let members: Vec<UserId> = entry.members.keys().copied().collect();
        state.publish(
          members,
          &GatewayEvent::MembershipAdd {
            conversation_id: conversation,
            user: summary,
          },
        );
      }
      state.publish_admin_join(conversation, user, AdminJoinRequestChange::Approved);
      Ok(answer)
    })
  }

  async fn close_admin_join(&self, conversation: ConversationId, user: UserId) -> Result<(), ApiError> {
    self.with(|state, me| {
      let (closed, change) = if user == me {
        (FakeJoinState::Withdrawn, AdminJoinRequestChange::Withdrawn)
      } else {
        state.require_channel_owner(conversation, me)?;
        (FakeJoinState::Declined, AdminJoinRequestChange::Declined)
      };
      if let Some(request) = state
        .admin_join_requests
        .get_mut(&(conversation, user))
        .filter(|request| request.state == FakeJoinState::Open)
      {
        request.state = closed;
        request.maybe_decided_by = Some(me);
        state.publish_admin_join(conversation, user, change);
      }
      Ok(())
    })
  }

  async fn withdraw_device_link(&self, request_id: &str) -> Result<(), ApiError> {
    self.with(|state, me| {
      if state
        .link_requests
        .get(request_id)
        .is_none_or(|entry| entry.user != me)
      {
        return Err(ApiError::status(
          404,
          ErrorCode::DeviceLinkNotFound,
          "no such link request",
        ));
      }
      state.link_requests.remove(request_id);
      state.publish(
        [me],
        &GatewayEvent::DeviceLinkClosed {
          request_id: request_id.to_owned(),
          reason: DeviceLinkClosedReason::Withdrawn,
        },
      );
      Ok(())
    })
  }

  async fn upload_key_packages(
    &self,
    device: DeviceId,
    request: &UploadKeyPackagesRequest,
  ) -> Result<KeyPackageStatusResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      let now = state.now_ms();
      let regular = request
        .key_packages
        .iter()
        .map(|bytes| stored_key_package(bytes, false, now))
        .collect::<Result<Vec<_>, _>>()?;
      let maybe_last_resort = request
        .maybe_last_resort
        .as_ref()
        .map(|bytes| stored_key_package(bytes, true, now))
        .transpose()?;
      let packages = state.key_packages.entry(device).or_default();
      packages.regular.extend(regular);
      if let Some(last_resort) = maybe_last_resort {
        packages.maybe_last_resort = Some(last_resort);
      }
      let status = packages.status(now);
      if state.key_package_answers_to_lose > 0 {
        state.key_package_answers_to_lose -= 1;
        return Err(ApiError::Network("fake answer lost".into()));
      }
      Ok(status)
    })
  }

  async fn key_package_status(&self, device: DeviceId) -> Result<KeyPackageStatusResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      let now = state.now_ms();
      Ok(
        state
          .key_packages
          .get(&device)
          .map(|packages| packages.status(now))
          .unwrap_or(KeyPackageStatusResponse {
            available: 0,
            expiring_soon: 0,
            has_last_resort: false,
          }),
      )
    })
  }

  async fn claim_key_packages(
    &self,
    user: UserId,
    request: &ClaimKeyPackagesRequest,
  ) -> Result<ClaimKeyPackagesResponse, ApiError> {
    self.with(|state, me| {
      let conversation = state.conversation(request.conversation_id)?;
      if !conversation.members.contains_key(&me) {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "only members can add devices",
        ));
      }
      if !conversation.members.contains_key(&user) {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "user is not a member",
        ));
      }
      let valid_until = state.now_ms().saturating_add(KEY_PACKAGE_MIN_VALIDITY_MS);
      let devices: Vec<DeviceId> = state.devices_of(user).map(|device| device.response.id).collect();
      let mut packages = Vec::new();
      for device in devices {
        let Some(stock) = state.key_packages.get_mut(&device) else {
          continue;
        };
        // Like the server: packages expiring too soon are deleted, never handed out.
        stock
          .regular
          .retain(|package| package.expires_at_ms > valid_until);
        if let Some(key_package) = stock.regular.pop_front() {
          packages.push(ClaimedKeyPackage {
            device_id: device,
            key_package: key_package.bytes,
            is_last_resort: false,
          });
        } else if let Some(last_resort) = stock
          .maybe_last_resort
          .as_ref()
          .filter(|package| package.expires_at_ms > valid_until)
        {
          packages.push(ClaimedKeyPackage {
            device_id: device,
            key_package: last_resort.bytes.clone(),
            is_last_resort: true,
          });
        }
      }
      if packages.is_empty() {
        return Err(ApiError::status(
          409,
          ErrorCode::KeyPackageExhausted,
          "no key package left",
        ));
      }
      Ok(ClaimKeyPackagesResponse { packages })
    })
  }

  async fn list_spaces(&self) -> Result<ListSpacesResponse, ApiError> {
    self.with(|state, _| {
      state.default_space_entry();
      Ok(ListSpacesResponse {
        spaces: state.spaces.values().cloned().collect(),
      })
    })
  }

  /// Every known user is in every fake space, except the outsiders.
  async fn list_space_members(&self, space: SpaceId) -> Result<ListSpaceMembersResponse, ApiError> {
    self.with(|state, _| state.space_members_answer(space))
  }

  async fn list_conversations(&self) -> Result<ListConversationsResponse, ApiError> {
    let maybe_delay = self
      .server
      .lock()
      .conversation_list_delays
      .get(&self.user)
      .copied();
    if let Some(delay) = maybe_delay {
      crate::platform::sleep(delay).await;
    }
    self.with(|state, me| {
      let conversations = state
        .conversations
        .values()
        .filter(|conversation| conversation.members.contains_key(&me))
        .map(|conversation| conversation.view(me))
        .collect();
      Ok(ListConversationsResponse {
        conversations,
        deleted_conversation_ids: state.deleted_conversations.get(&me).cloned().unwrap_or_default(),
      })
    })
  }

  async fn conversation(&self, conversation: ConversationId) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| Ok(state.member_conversation(conversation, me)?.view(me)))
  }

  async fn list_members(
    &self,
    conversation: ConversationId,
  ) -> Result<ListConversationMembersResponse, ApiError> {
    let maybe_delay = self.server.lock().member_list_delays.get(&self.user).copied();
    if let Some(delay) = maybe_delay {
      crate::platform::sleep(delay).await;
    }
    self.with(|state, me| state.members_answer(conversation, me))
  }

  async fn create_channel(
    &self,
    space: SpaceId,
    request: &CreateChannelRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
      if let Some(key) = request.maybe_idempotency_key
        && let Some(existing) = state.created_channels.get(&(me, key))
      {
        return Ok(state.conversation(*existing)?.view(me));
      }
      let history = request
        .maybe_history_policy
        .clone()
        .unwrap_or(HistoryPolicy::Full);
      let id = state.create_conversation(
        ConversationKind::Channel,
        Some(space),
        Some(request.name.clone()),
        history,
        &[me],
      );
      let entry = state.conversation_mut(id)?;
      // The creator administers the channel.
      if let Some((role, _)) = entry.members.get_mut(&me) {
        *role = ChannelRole::Admin;
      }
      entry.response.visibility = request.visibility.clone();
      entry.response.maybe_topic = request.maybe_topic.clone();
      entry.response.maybe_category_id = request.maybe_category_id;
      if let Some(key) = request.maybe_idempotency_key {
        state.created_channels.insert((me, key), id);
      }
      state.publish([me], &GatewayEvent::ConversationUpdate { conversation_id: id });
      if state.create_answers_to_lose > 0 {
        state.create_answers_to_lose -= 1;
        return Err(ApiError::Network("fake answer lost".into()));
      }
      Ok(state.conversation(id)?.view(me))
    })
  }

  async fn create_dm(&self, request: &CreateDmRequest) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
      state.user(request.user_id)?;
      let existing = state.conversations.values().find(|conversation| {
        conversation.response.kind == ConversationKind::Dm
          && conversation.members.contains_key(&me)
          && conversation.members.contains_key(&request.user_id)
          // A DM with oneself has one member (notes to self).
          && (request.user_id != me || conversation.members.len() == 1)
      });
      if let Some(conversation) = existing {
        return Ok(conversation.view(me));
      }
      let participants = if request.user_id == me {
        vec![me]
      } else {
        vec![me, request.user_id]
      };
      let id = state.create_conversation(
        ConversationKind::Dm,
        None,
        None,
        HistoryPolicy::Full,
        &participants,
      );
      // Like the server: a DM's participants are the people it was opened for (issue #643).
      let summaries = participants
        .iter()
        .map(|user| state.user(*user).cloned())
        .collect::<Result<Vec<_>, _>>()?;
      if let Some(conversation) = state.conversations.get_mut(&id) {
        conversation.response.participants = summaries;
      }
      state.publish(
        [me, request.user_id],
        &GatewayEvent::ConversationUpdate { conversation_id: id },
      );
      Ok(state.conversation(id)?.view(me))
    })
  }

  async fn create_group_dm(&self, request: &CreateGroupDmRequest) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
      let mut participants: Vec<UserId> = request.user_ids.clone();
      participants.push(me);
      participants.sort();
      participants.dedup();
      for user in &participants {
        state.user(*user)?;
      }
      let existing = state.conversations.values().find(|conversation| {
        conversation.response.kind == ConversationKind::Group
          && conversation.members.len() == participants.len()
          && participants
            .iter()
            .all(|user| conversation.members.contains_key(user))
      });
      if let Some(conversation) = existing {
        return Ok(conversation.view(me));
      }
      let id = state.create_conversation(
        ConversationKind::Group,
        None,
        None,
        HistoryPolicy::Full,
        &participants,
      );
      state.publish(
        participants.clone(),
        &GatewayEvent::ConversationUpdate { conversation_id: id },
      );
      Ok(state.conversation(id)?.view(me))
    })
  }

  async fn add_member(
    &self,
    conversation: ConversationId,
    request: &AddConversationMemberRequest,
  ) -> Result<ConversationMemberResponse, ApiError> {
    self.with(|state, me| {
      let floor = state.peek_id();
      let user = state.user(request.user_id)?.clone();
      let entry = state.actor_conversation_mut(conversation, me)?;
      let role = request.maybe_role.clone().unwrap_or(ChannelRole::Member);
      entry.members.insert(user.id, (role.clone(), floor));
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(
        members,
        &GatewayEvent::MembershipAdd {
          conversation_id: conversation,
          user: user.clone(),
        },
      );
      Ok(ConversationMemberResponse { user, role })
    })
  }

  async fn admin_channels_of(&self, user: UserId) -> Result<Vec<ConversationId>, ApiError> {
    self.with(|state, me| {
      if !state.admins.contains(&me) {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "administrators only"));
      }
      Ok(
        state
          .conversations
          .iter()
          .filter(|(_, entry)| {
            entry.response.kind == teamcraft_proto::ConversationKind::Channel
              && entry.members.contains_key(&user)
          })
          .map(|(id, _)| *id)
          .collect(),
      )
    })
  }

  async fn admin_remove_from_chats(&self, user: UserId) -> Result<(), ApiError> {
    let me = self.user;
    if !self.server.lock().admins.contains(&me) {
      return Err(ApiError::status(403, ErrorCode::Forbidden, "administrators only"));
    }
    let conversations: Vec<ConversationId> = self
      .server
      .lock()
      .conversations
      .iter()
      .filter(|(_, entry)| entry.members.contains_key(&user))
      .map(|(id, _)| *id)
      .collect();
    for conversation in conversations {
      self.server.remove_member_as_admin(conversation, user);
    }
    Ok(())
  }

  async fn admin_delete_user(&self, user: UserId) -> Result<(), ApiError> {
    let me = self.user;
    let mut state = self.server.lock();
    if !state.admins.contains(&me) {
      return Err(ApiError::status(403, ErrorCode::Forbidden, "administrators only"));
    }
    if user == me {
      return Err(ApiError::status(
        409,
        ErrorCode::CannotDeleteSelf,
        "admins can't delete their own account",
      ));
    }
    state.delete_user(user);
    Ok(())
  }

  async fn check_member_removal(&self, conversation: ConversationId, _user: UserId) -> Result<(), ApiError> {
    self.with(|state, me| state.removal_target(conversation, me).map(|_| ()))
  }

  async fn remove_member(&self, conversation: ConversationId, user: UserId) -> Result<(), ApiError> {
    self.with_removal_cut(|state, me| {
      state.removal_requests += 1;
      if state.refusing_removal_steps_by.contains(&me) {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "may not remove members",
        ));
      }
      let entry = state.removal_target(conversation, me)?;
      let mut recipients: Vec<UserId> = entry.members.keys().copied().collect();
      if entry.members.remove(&user).is_none() {
        // Idempotent like the server: already gone.
        return Ok(());
      }
      recipients.retain(|member| *member != user);
      // The removed user learns about it once, then stops receiving the conversation's events.
      recipients.push(user);
      state.publish(
        recipients,
        &GatewayEvent::MembershipRemove {
          conversation_id: conversation,
          user_id: user,
        },
      );
      Ok(())
    })
  }

  async fn update_member(
    &self,
    conversation: ConversationId,
    user: UserId,
    request: &UpdateConversationMemberRequest,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      let is_instance_admin = state.admins.contains(&me);
      if let Some(role) = &request.maybe_role {
        // Like the server: channel admins and workspace admins manage roles; guests stay members;
        // a channel keeps an admin. (The clients also never change their own role.)
        if user == me {
          return Err(ApiError::status(
            403,
            ErrorCode::Forbidden,
            "people can't change their own role",
          ));
        }
        let is_guest = state.user(user)?.kind == UserKind::Guest
          || state
            .conversation(conversation)?
            .response
            .maybe_space_id
            .and_then(|space| state.space_roles.get(&(space, user)))
            .is_some_and(|role| *role == SpaceRole::Guest);
        if is_guest && *role != ChannelRole::Member {
          return Err(ApiError::status(
            400,
            ErrorCode::InvalidRequest,
            "guests can't be channel admins or moderators",
          ));
        }
        let entry = state.actor_conversation_mut(conversation, me)?;
        let is_channel_admin = entry
          .members
          .get(&me)
          .is_some_and(|(own, _)| *own == ChannelRole::Admin);
        if !is_channel_admin && !is_instance_admin {
          return Err(ApiError::status(
            403,
            ErrorCode::Forbidden,
            "missing permission manage_channel_roles",
          ));
        }
        if matches!(role, ChannelRole::Unknown(_)) {
          return Err(ApiError::status(
            400,
            ErrorCode::InvalidRequest,
            "unknown channel role",
          ));
        }
        let admins = entry
          .members
          .values()
          .filter(|(held, _)| *held == ChannelRole::Admin)
          .count();
        let Some((held, _)) = entry.members.get_mut(&user) else {
          return Err(ApiError::status(404, ErrorCode::UserNotFound, "not a member"));
        };
        if *held == ChannelRole::Admin && *role != ChannelRole::Admin && admins <= 1 {
          return Err(ApiError::status(
            409,
            ErrorCode::Conflict,
            "a channel needs at least one admin",
          ));
        }
        *held = role.clone();
        return Ok(());
      }
      let entry = state.member_conversation_mut(conversation, me)?;
      if let Some(is_favorite) = request.maybe_is_favorite {
        if user != me {
          return Err(ApiError::status(
            403,
            ErrorCode::Forbidden,
            "only own preferences",
          ));
        }
        entry.favorites.insert(me, is_favorite);
      }
      Ok(())
    })
  }

  async fn set_read_position(
    &self,
    conversation: ConversationId,
    request: &SetReadPositionRequest,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      state.read_position_writes += 1;
      if state.failing_read_positions.contains(&conversation) {
        return Err(ApiError::status(503, ErrorCode::Unavailable, "try again later"));
      }
      // Like the server's `GREATEST`: a late or repeated write never moves the position back.
      let position = state
        .member_conversation_mut(conversation, me)?
        .read_positions
        .entry(me)
        .or_insert(request.message_id);
      *position = (*position).max(request.message_id);
      Ok(())
    })
  }

  async fn set_thread_read(
    &self,
    conversation: ConversationId,
    thread: ThreadId,
    request: &teamcraft_proto::SetThreadReadRequest,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      // Like the server's `GREATEST`: a late or repeated write never moves the position back.
      let positions = &mut state
        .member_conversation_mut(conversation, me)?
        .thread_read_positions;
      let position = positions.entry((me, thread)).or_insert(request.message_id);
      *position = (*position).max(request.message_id);
      Ok(())
    })
  }

  async fn set_presence(&self, _request: &SetPresenceRequest) -> Result<(), ApiError> {
    self.with(|_, _| Ok(()))
  }

  async fn set_custom_status(
    &self,
    request: &teamcraft_proto::SetCustomStatusRequest,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      if state.refuses_custom_statuses {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "custom statuses are switched off; choose one of the presets",
        ));
      }
      // Like the server: a duration ends on this clock and wins over the client's end.
      let now = state.now_ms();
      let maybe_expires_at = match (request.maybe_expires_in_seconds, request.maybe_expires_at) {
        (Some(seconds), _) => Some(teamcraft_proto::UnixMillis(
          now.saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX).saturating_mul(1000)),
        )),
        (None, Some(at)) if at.0 <= now => {
          return Err(ApiError::status(
            400,
            ErrorCode::InvalidRequest,
            "expires_at must be in the future",
          ));
        },
        (None, maybe_at) => maybe_at,
      };
      let status = teamcraft_proto::CustomStatus {
        maybe_emoji: request.maybe_emoji.clone(),
        maybe_text: request.maybe_text.clone(),
        maybe_expires_at,
        maybe_activity: None,
      };
      state.custom_statuses.insert(me, status);
      Ok(())
    })
  }

  async fn clear_custom_status(&self) -> Result<(), ApiError> {
    self.with(|state, me| {
      state.custom_statuses.remove(&me);
      Ok(())
    })
  }

  async fn list_presence(&self, users: &[UserId]) -> Result<teamcraft_proto::ListPresenceResponse, ApiError> {
    self.with(|state, _| {
      Ok(teamcraft_proto::ListPresenceResponse {
        presences: users
          .iter()
          .map(|user| teamcraft_proto::UserPresence {
            user_id: *user,
            state: teamcraft_proto::PresenceState::Offline,
            maybe_status: state.custom_statuses.get(user).cloned(),
            maybe_last_seen: state
              .last_seen
              .get(user)
              .filter(|_| !state.hidden_last_seen.contains(user))
              .copied()
              .map(teamcraft_proto::UnixMillis),
          })
          .collect(),
      })
    })
  }

  async fn get_privacy(&self) -> Result<teamcraft_proto::PrivacyResponse, ApiError> {
    self.with(|state, me| Ok(privacy_of(state, me)))
  }

  async fn put_privacy(
    &self,
    request: &teamcraft_proto::UpdatePrivacyRequest,
  ) -> Result<teamcraft_proto::PrivacyResponse, ApiError> {
    self.with(|state, me| {
      if request.last_seen == teamcraft_proto::LastSeenVisibility::Nobody {
        state.hidden_last_seen.insert(me);
      } else {
        state.hidden_last_seen.remove(&me);
      }
      Ok(privacy_of(state, me))
    })
  }

  async fn list_call_history(
    &self,
    query: &teamcraft_proto::ListCallHistoryQuery,
  ) -> Result<teamcraft_proto::ListCallHistoryResponse, ApiError> {
    // The fake has no call membership: everybody sees every call of the history.
    self.with(|state, _| {
      let limit = query.maybe_limit.map_or(PAGE_SIZE, |limit| limit as usize);
      let mut calls: Vec<_> = state
        .call_history
        .iter()
        .filter(|call| query.maybe_before.is_none_or(|before| call.call_id < before))
        .cloned()
        .collect();
      let has_more = calls.len() > limit;
      calls.truncate(limit);
      Ok(teamcraft_proto::ListCallHistoryResponse { calls, has_more })
    })
  }

  /// A placeholder token: the fake has no SFU, so nothing can join with it.
  async fn create_call_join_token(
    &self,
    conversation: ConversationId,
    request: &CreateCallJoinTokenRequest,
  ) -> Result<CallJoinTokenResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.device_id)?;
      if !state.conversations.contains_key(&conversation) {
        return Err(ApiError::status(
          404,
          ErrorCode::ChannelNotFound,
          "no such conversation",
        ));
      }
      let call_id = match state.calls.get(&conversation) {
        Some(call_id) => *call_id,
        None => {
          let call_id = state.next_id();
          state.calls.insert(conversation, call_id);
          call_id
        },
      };
      Ok(CallJoinTokenResponse {
        call_id,
        token: format!("fake:{conversation}"),
        sfu_url: "https://sfu.invalid/call".to_owned(),
        expires_at: UnixMillis(0),
        sfu_certificate_sha256: Vec::new(),
        ends_when_one_side_leaves: false,
      })
    })
  }

  /// A call some member's join token started; the fake keeps no participants.
  async fn running_call(&self, conversation: ConversationId) -> Result<Option<CallStateResponse>, ApiError> {
    self.with(|state, me| {
      Ok(state.calls.get(&conversation).map(|call_id| CallStateResponse {
        call_id: *call_id,
        conversation_id: conversation,
        started_by: me,
        started_at: UnixMillis(0),
        is_ringing: false,
        participants: Vec::new(),
      }))
    })
  }

  /// The rosters of the calls in the caller's conversations ([`FakeServer::join_call`]).
  async fn running_calls(&self) -> Result<Option<Vec<CallStateResponse>>, ApiError> {
    self.with(|state, me| {
      Ok(Some(
        state
          .call_rosters
          .values()
          .filter(|roster| {
            state
              .conversations
              .get(&roster.conversation_id)
              .is_some_and(|entry| entry.members.contains_key(&me))
          })
          .cloned()
          .collect(),
      ))
    })
  }

  /// A meeting chat like the server's (the caller is admin, the invitees members), without a
  /// calendar; a retry with the same key returns the same meeting.
  async fn create_instant_meeting(
    &self,
    request: &CreateInstantMeetingRequest,
  ) -> Result<MeetingResponse, ApiError> {
    self.with(|state, me| {
      if let Some(existing) = request
        .maybe_idempotency_key
        .as_ref()
        .and_then(|key| state.instant_meetings.get(key))
      {
        return Ok(existing.clone());
      }
      let mut members: Vec<UserId> = request
        .invitees
        .iter()
        .copied()
        .filter(|user| *user != me)
        .collect();
      members.push(me);
      let title = request
        .maybe_title
        .clone()
        .unwrap_or_else(|| "Instant meeting".to_owned());
      let conversation_id = state.create_conversation(
        ConversationKind::Meeting,
        None,
        Some(title.clone()),
        HistoryPolicy::Full,
        &members,
      );
      if let Some(conversation) = state.conversations.get_mut(&conversation_id)
        && let Some(entry) = conversation.members.get_mut(&me)
      {
        entry.0 = ChannelRole::Admin;
      }
      state.publish(members, &GatewayEvent::ConversationUpdate { conversation_id });
      let meeting = MeetingResponse {
        id: state.next_id(),
        title,
        maybe_description: None,
        starts_at: UnixMillis(0),
        ends_at: UnixMillis(0),
        kind: MeetingKind::Instant,
        status: MeetingStatus::Scheduled,
        has_lobby: true,
        organizer_user_id: me,
        conversation_id,
        join_url: format!("https://chat.example.com/meeting/{conversation_id}"),
        maybe_recurrence: None,
        attendees: Vec::new(),
        undelivered_invitations: Vec::new(),
      };
      if let Some(key) = &request.maybe_idempotency_key {
        state.instant_meetings.insert(*key, meeting.clone());
      }
      Ok(meeting)
    })
  }

  /// Notes the cancellation; the fake keeps no calendar.
  async fn cancel_meeting(&self, meeting: teamcraft_proto::MeetingId) -> Result<(), ApiError> {
    self.with(|state, _| {
      state.cancelled_meetings.push(meeting);
      Ok(())
    })
  }

  /// Tells the conversation's members that its call moved, like the server; the fake has no call
  /// roster, so being a member is enough.
  async fn move_call(&self, conversation: ConversationId, request: &MoveCallRequest) -> Result<(), ApiError> {
    self.with(|state, me| {
      let members: Vec<UserId> = state
        .conversation(conversation)?
        .members
        .keys()
        .copied()
        .collect();
      if !members.contains(&me) {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "not a member"));
      }
      state.call_moves.push((conversation, request.clone()));
      state.publish(
        members,
        &GatewayEvent::CallMove {
          conversation_id: conversation,
          call_id: state
            .calls
            .get(&conversation)
            .copied()
            .unwrap_or_else(|| CallId::from_snowflake(conversation.snowflake())),
          to_conversation_id: request.to_conversation_id,
          moved_by: me,
        },
      );
      Ok(())
    })
  }

  async fn send_message(
    &self,
    conversation: ConversationId,
    request: &SendMessageRequest,
  ) -> Result<MessageResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.device_id)?;
      let existing = state
        .member_conversation(conversation, me)?
        .messages
        .iter()
        .find(|message| {
          message.client_message_id == request.client_message_id && message.sender_user_id == me
        })
        .cloned();
      // Idempotent retries: the same client message ID returns the stored message.
      if let Some(message) = existing {
        return Ok(message);
      }
      let id = match state.maybe_forced_message_id.take() {
        Some(id) => id,
        None => state.next_id(),
      };
      let message = MessageResponse {
        id,
        client_message_id: request.client_message_id,
        conversation_id: conversation,
        sender_user_id: me,
        sender_device_id: request.device_id,
        maybe_thread_id: request.maybe_thread_id,
        mentioned_user_ids: request.mentioned_user_ids.clone(),
        is_urgent: request.is_urgent,
        content_key_id: request.content_key_id.clone(),
        ciphertext: request.ciphertext.clone(),
      };
      if state.dropping_messages_of.contains(&me) {
        return Ok(message);
      }
      let entry = state.member_conversation_mut(conversation, me)?;
      entry.messages.push(message.clone());
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(
        members,
        &GatewayEvent::MessageCreate {
          message: message.clone(),
        },
      );
      if let Some(thread) = message.maybe_thread_id {
        let followers: HashSet<UserId> = state
          .thread_participants(conversation, thread)
          .into_iter()
          .collect();
        state.publish(
          followers,
          &GatewayEvent::ThreadUpdate {
            conversation_id: conversation,
            thread_id: thread,
          },
        );
      }
      Ok(message)
    })
  }

  async fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> Result<ListMessagesResponse, ApiError> {
    self.with(|state, me| {
      let limit = query
        .maybe_limit
        .map_or(PAGE_SIZE, |limit| limit as usize)
        .clamp(1, PAGE_SIZE);
      if state.forbidden_histories.contains(&conversation) {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "history not allowed"));
      }
      let messages = &state.member_conversation(conversation, me)?.messages;
      let selected: Vec<MessageResponse> = match (query.maybe_before, query.maybe_after) {
        (_, Some(after)) => messages
          .iter()
          .filter(|message| message.id > after)
          .take(limit + 1)
          .cloned()
          .collect(),
        (maybe_before, None) => {
          let older: Vec<&MessageResponse> = messages
            .iter()
            .filter(|message| maybe_before.is_none_or(|before| message.id < before))
            .collect();
          let start = older.len().saturating_sub(limit + 1);
          older[start..]
            .iter()
            .rev()
            .map(|message| (*message).clone())
            .collect()
        },
      };
      let has_more = selected.len() > limit;
      let mut page: Vec<MessageResponse> = selected.into_iter().take(limit).collect();
      page.sort_by_key(|message| message.id);
      Ok(ListMessagesResponse {
        messages: page,
        has_more,
      })
    })
  }

  async fn post_mls_commit(
    &self,
    conversation: ConversationId,
    request: &PostMlsCommitRequest,
  ) -> Result<PostMlsCommitResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.device_id)?;
      if state.dropping_commits.remove(&me) {
        return Err(ApiError::Network("fake server: commit lost on the way".into()));
      }
      let current_epoch = state.member_conversation(conversation, me)?.epoch;
      if request.epoch != current_epoch {
        return Err(ApiError::status(
          409,
          ErrorCode::EpochConflict,
          format!("the group is in epoch {current_epoch}"),
        ));
      }
      let commit_id = state.next_id();
      let commit = MlsMessageResponse {
        id: commit_id,
        conversation_id: conversation,
        epoch: request.epoch,
        kind: MlsMessageKind::Commit,
        sender_device_id: request.device_id,
        maybe_recipient_device_id: None,
        payload: request.commit.clone(),
      };
      let mut welcomes = Vec::new();
      for welcome in &request.welcomes {
        let owner = state.devices.get(&welcome.device_id).map(|device| device.user);
        let id = state.next_id();
        welcomes.push((
          owner,
          MlsMessageResponse {
            id,
            conversation_id: conversation,
            epoch: request.epoch + 1,
            kind: MlsMessageKind::Welcome,
            sender_device_id: request.device_id,
            maybe_recipient_device_id: Some(welcome.device_id),
            payload: welcome.welcome.clone(),
          },
        ));
      }
      let entry = state.member_conversation_mut(conversation, me)?;
      entry.epoch += 1;
      entry.mls.push(commit.clone());
      let new_epoch = entry.epoch;
      if let Some(group_info) = &request.maybe_group_info {
        entry.maybe_group_info = Some((new_epoch, group_info.clone()));
      }
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      let garbled_for: Vec<UserId> = state.garbling_next_commit_for.drain().collect();
      for user in &garbled_for {
        state.garbled_entries.entry(*user).or_default().insert(commit_id);
      }
      let (garbled, plain): (Vec<UserId>, Vec<UserId>) =
        members.into_iter().partition(|user| garbled_for.contains(user));
      state.publish(
        plain,
        &GatewayEvent::MlsMessage {
          message: commit.clone(),
        },
      );
      state.publish(
        garbled,
        &GatewayEvent::MlsMessage {
          message: garble(commit.clone()),
        },
      );
      for (maybe_owner, welcome) in welcomes {
        state.welcomes.push(welcome.clone());
        if let Some(owner) = maybe_owner {
          state.publish([owner], &GatewayEvent::MlsMessage { message: welcome });
        }
      }
      if state.losing_commit_answers.remove(&me) {
        return Err(ApiError::Network("fake server: the answer was lost".into()));
      }
      Ok(PostMlsCommitResponse { new_epoch, commit })
    })
  }

  async fn post_mls_message(
    &self,
    conversation: ConversationId,
    request: &PostMlsMessageRequest,
  ) -> Result<MlsMessageResponse, ApiError> {
    let maybe_cut = match request.kind {
      MlsMessageKind::Proposal => {
        let mut state = self.server.lock();
        if state.holding_proposal_of.remove(&self.user) {
          return Ok(self.hold(&mut state, conversation, request));
        }
        state.proposal_cuts.remove(&self.user)
      },
      MlsMessageKind::Application => {
        let mut state = self.server.lock();
        if state.holding_application_of.remove(&self.user) {
          return Ok(self.hold(&mut state, conversation, request));
        }
        state.application_cuts.remove(&self.user)
      },
      _ => None,
    };
    match maybe_cut {
      None => self.post_mls_message_now(conversation, request),
      Some(NetworkCut::BeforeRequest) => Err(ApiError::Network("fake server: never sent".into())),
      Some(NetworkCut::ResponseLost) => {
        // Deliberately ignored: the client never learns the outcome.
        let _ = self.post_mls_message_now(conversation, request);
        Err(ApiError::Network("fake server: answer lost".into()))
      },
    }
  }

  async fn list_mls_messages(
    &self,
    conversation: ConversationId,
    query: &ListMlsMessagesQuery,
  ) -> Result<ListMlsMessagesResponse, ApiError> {
    self.with(|state, me| {
      if state.queue_refused_to.contains(&me) {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "only members can read the group's messages",
        ));
      }
      let entry = state.member_conversation(conversation, me)?;
      let floor = entry.members.get(&me).map_or(0, |(_, floor)| *floor);
      let maybe_garbled = state.garbled_entries.get(&me);
      let visible = entry
        .mls
        .iter()
        .filter(|message| {
          message.id.snowflake().get() > floor && query.maybe_after.is_none_or(|after| message.id > after)
        })
        .map(|message| match maybe_garbled {
          Some(garbled) if garbled.contains(&message.id) => garble(message.clone()),
          _ => message.clone(),
        });
      Ok(page(visible.collect(), query.maybe_limit, entry.epoch))
    })
  }

  async fn mls_group_info(
    &self,
    conversation: ConversationId,
  ) -> Result<teamcraft_proto::MlsGroupInfoResponse, ApiError> {
    self.with(|state, me| {
      if let Some(forced) = state.forced_group_infos.get(&conversation) {
        return Ok(forced.clone());
      }
      if let Some(hidden) = state.hidden_group_info_reads.get_mut(&conversation)
        && *hidden > 0
      {
        *hidden -= 1;
        return Err(ApiError::status(
          404,
          ErrorCode::MlsGroupInfoNotFound,
          "no group information for the current epoch",
        ));
      }
      let entry = state.member_conversation(conversation, me)?;
      match &entry.maybe_group_info {
        Some((epoch, group_info)) if *epoch == entry.epoch => Ok(teamcraft_proto::MlsGroupInfoResponse {
          epoch: *epoch,
          group_info: group_info.clone(),
        }),
        _ => Err(ApiError::status(
          404,
          ErrorCode::MlsGroupInfoNotFound,
          "no group information for the current epoch",
        )),
      }
    })
  }

  async fn put_mls_group_info(
    &self,
    conversation: ConversationId,
    request: &teamcraft_proto::PutMlsGroupInfoRequest,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.device_id)?;
      let entry = state.member_conversation_mut(conversation, me)?;
      if request.epoch != entry.epoch {
        return Err(ApiError::status(
          409,
          ErrorCode::EpochConflict,
          format!("the group is in epoch {}", entry.epoch),
        ));
      }
      // Like the server: a meeting chat's first information comes from the organizer's devices only.
      if request.epoch == 0
        && entry.response.kind == ConversationKind::Meeting
        && entry.response.maybe_organizer_user_id != Some(me)
      {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "only the organizer publishes a meeting chat's first group information",
        ));
      }
      // At epoch 0 the first information wins (two founders' groups differ), unless the
      // organizer replaces it.
      if request.epoch == 0
        && !request.is_replacement
        && entry
          .maybe_group_info
          .as_ref()
          .is_some_and(|(_, stored)| *stored != request.group_info)
      {
        return Err(ApiError::status(
          409,
          ErrorCode::EpochConflict,
          "another group information holds epoch 0",
        ));
      }
      entry.maybe_group_info = Some((request.epoch, request.group_info.clone()));
      Ok(())
    })
  }

  async fn list_welcomes(
    &self,
    device: DeviceId,
    query: &ListMlsMessagesQuery,
  ) -> Result<ListMlsMessagesResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      let visible = state.welcomes.iter().filter(|welcome| {
        welcome.maybe_recipient_device_id == Some(device)
          && query.maybe_after.is_none_or(|after| welcome.id > after)
      });
      Ok(page(visible.cloned().collect(), query.maybe_limit, 0))
    })
  }

  async fn put_key_bag(&self, request: &PutKeyBagRequest) -> Result<KeyBagResponse, ApiError> {
    self.with(|state, me| {
      let current = state.key_bags.get(&me).map_or(0, |bag| bag.version);
      if request.expected_version != current {
        return Err(ApiError::status(
          409,
          ErrorCode::VersionConflict,
          format!("stored version is {current}"),
        ));
      }
      let stored = KeyBagResponse {
        version: current + 1,
        encrypted_key_bag: request.encrypted_key_bag.clone(),
        wrapped_backup_keys: request.wrapped_backup_keys.clone(),
        updated_at: UnixMillis(0),
      };
      state.key_bags.insert(me, stored.clone());
      // Like the server: the user's devices learn about the new version.
      state.publish(
        [me],
        &GatewayEvent::KeyBagUpdate {
          version: stored.version,
        },
      );
      Ok(stored)
    })
  }

  async fn join_space(&self, space: SpaceId) -> Result<SpaceResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .spaces
        .get_mut(&space)
        .ok_or_else(|| ApiError::status(404, ErrorCode::SpaceNotFound, "no such space"))?;
      if entry.join_policy != JoinPolicy::Open || entry.is_archived {
        return Err(ApiError::status(403, ErrorCode::Forbidden, "invitation only"));
      }
      entry.maybe_my_role.get_or_insert(SpaceRole::Member);
      Ok(entry.clone())
    })
  }

  async fn add_space_member(
    &self,
    space: SpaceId,
    request: &AddSpaceMemberRequest,
  ) -> Result<SpaceMemberResponse, ApiError> {
    self.with(|state, _| {
      state.default_space_entry();
      if !state.spaces.contains_key(&space) {
        return Err(ApiError::status(404, ErrorCode::SpaceNotFound, "no such space"));
      }
      let user = state.user(request.user_id)?.clone();
      // Like the server: never a role change through an add.
      if !state.space_outsiders.contains(&request.user_id) {
        let current = state
          .space_roles
          .get(&(space, request.user_id))
          .cloned()
          .unwrap_or(SpaceRole::Member);
        if current != request.role {
          return Err(ApiError::status(409, ErrorCode::Conflict, "already a member"));
        }
        return Ok(SpaceMemberResponse {
          user,
          role: current,
          maybe_expires_at: None,
          maybe_sponsor_user_id: None,
        });
      }
      state.space_outsiders.remove(&request.user_id);
      state.space_member_adds.push((space, request.clone()));
      Ok(SpaceMemberResponse {
        user,
        role: request.role.clone(),
        maybe_expires_at: request.maybe_expires_at,
        maybe_sponsor_user_id: None,
      })
    })
  }

  async fn join_conversation(&self, conversation: ConversationId) -> Result<(), ApiError> {
    self.with(|state, me| {
      let floor = state.peek_id();
      let user = state.user(me)?.clone();
      let entry = state.conversation_mut(conversation)?;
      if entry.response.visibility != ConversationVisibility::Public || entry.response.is_archived {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "not a public channel",
        ));
      }
      if entry.members.contains_key(&me) {
        return Err(ApiError::status(
          409,
          ErrorCode::AlreadyJoined,
          "already a member",
        ));
      }
      entry.members.insert(me, (ChannelRole::Member, floor));
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(
        members,
        &GatewayEvent::MembershipAdd {
          conversation_id: conversation,
          user,
        },
      );
      Ok(())
    })
  }

  async fn update_conversation(
    &self,
    conversation: ConversationId,
    request: &UpdateConversationRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
      let entry = state.member_conversation_mut(conversation, me)?;
      let response = &mut entry.response;
      if let Some(name) = &request.maybe_name {
        response.maybe_name = Some(name.clone());
      }
      if let Some(topic) = &request.maybe_topic {
        response.maybe_topic = (!topic.is_empty()).then(|| topic.clone());
      }
      if let Some(description) = &request.maybe_description {
        response.maybe_description = (!description.is_empty()).then(|| description.clone());
      }
      if let Some(visibility) = &request.maybe_visibility {
        response.visibility = visibility.clone();
      }
      if let Some(is_archived) = request.maybe_is_archived {
        response.is_archived = is_archived;
      }
      if let Some(category) = request.maybe_category_id {
        response.maybe_category_id = Some(category);
      } else if request.clear_category {
        response.maybe_category_id = None;
      }
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      let view = entry.view(me);
      state.publish(
        members,
        &GatewayEvent::ConversationUpdate {
          conversation_id: conversation,
        },
      );
      Ok(view)
    })
  }

  async fn move_conversation(
    &self,
    conversation: ConversationId,
    request: &MoveChannelRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
      let entry = state.member_conversation_mut(conversation, me)?;
      entry.response.maybe_space_id = Some(request.space_id);
      entry.response.maybe_category_id = request.maybe_category_id;
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      let view = entry.view(me);
      state.publish(
        members,
        &GatewayEvent::ConversationUpdate {
          conversation_id: conversation,
        },
      );
      Ok(view)
    })
  }

  async fn list_joinable(&self, space: SpaceId) -> Result<ListConversationsResponse, ApiError> {
    self.with(|state, me| {
      let conversations = state
        .conversations
        .values()
        .filter(|conversation| {
          conversation.response.maybe_space_id == Some(space)
            && conversation.response.visibility == ConversationVisibility::Public
            && !conversation.members.contains_key(&me)
        })
        .map(|conversation| conversation.view(me))
        .collect();
      Ok(ListConversationsResponse {
        conversations,
        deleted_conversation_ids: Vec::new(),
      })
    })
  }

  async fn create_space(&self, request: &CreateSpaceRequest) -> Result<SpaceResponse, ApiError> {
    self.with(|state, me| {
      state.default_space_entry();
      if let Some(key) = request.maybe_idempotency_key
        && let Some(existing) = state.created_spaces.get(&(me, key))
        && let Some(space) = state.spaces.get(existing)
      {
        return Ok(space.clone());
      }
      let id: SpaceId = state.next_id();
      let space = SpaceResponse {
        id,
        name: request.name.clone(),
        maybe_icon: request.maybe_icon.clone(),
        maybe_color: request.maybe_color.clone(),
        maybe_description: request.maybe_description.clone(),
        maybe_image: None,
        join_policy: request.join_policy.clone(),
        is_archived: false,
        maybe_brand_id: None,
        maybe_my_role: Some(SpaceRole::SpaceAdmin),
        maybe_my_notifications: None,
        member_count: 1,
        can_add_members: true,
        can_invite_guests: true,
        can_manage_space: true,
        can_create_channels: true,
      };
      state.spaces.insert(id, space.clone());
      if let Some(key) = request.maybe_idempotency_key {
        state.created_spaces.insert((me, key), id);
      }
      Ok(space)
    })
  }

  async fn update_space(
    &self,
    space: SpaceId,
    request: &UpdateSpaceRequest,
  ) -> Result<SpaceResponse, ApiError> {
    self.with(|state, _| {
      state.default_space_entry();
      let entry = state
        .spaces
        .get_mut(&space)
        .ok_or_else(|| ApiError::status(404, ErrorCode::SpaceNotFound, "no such space"))?;
      if let Some(name) = &request.maybe_name {
        entry.name = name.clone();
      }
      if let Some(description) = &request.maybe_description {
        entry.maybe_description = (!description.is_empty()).then(|| description.clone());
      }
      if let Some(color) = &request.maybe_color {
        entry.maybe_color = (!color.is_empty()).then(|| color.clone());
      }
      if let Some(policy) = &request.maybe_join_policy {
        entry.join_policy = policy.clone();
      }
      if let Some(is_archived) = request.maybe_is_archived {
        entry.is_archived = is_archived;
      }
      Ok(entry.clone())
    })
  }

  async fn list_categories(&self, space: SpaceId) -> Result<ListCategoriesResponse, ApiError> {
    self.with(|state, _| {
      let mut categories: Vec<CategoryResponse> = state
        .categories
        .values()
        .filter(|category| category.space_id == space)
        .cloned()
        .collect();
      categories.sort_by_key(|category| category.position);
      Ok(ListCategoriesResponse { categories })
    })
  }

  async fn create_category(
    &self,
    space: SpaceId,
    request: &CreateCategoryRequest,
  ) -> Result<CategoryResponse, ApiError> {
    self.with(|state, _| {
      let id: CategoryId = state.next_id();
      let position = request
        .maybe_position
        .unwrap_or_else(|| i32::try_from(state.categories.len()).unwrap_or(i32::MAX));
      let category = CategoryResponse {
        id,
        space_id: space,
        name: request.name.clone(),
        position,
      };
      state.categories.insert(id, category.clone());
      Ok(category)
    })
  }

  async fn update_category(
    &self,
    space: SpaceId,
    category: CategoryId,
    request: &UpdateCategoryRequest,
  ) -> Result<CategoryResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .categories
        .get_mut(&category)
        .filter(|entry| entry.space_id == space)
        .ok_or_else(|| ApiError::status(404, ErrorCode::CategoryNotFound, "no such category"))?;
      if let Some(name) = &request.maybe_name {
        entry.name = name.clone();
      }
      if let Some(position) = request.maybe_position {
        entry.position = position;
      }
      Ok(entry.clone())
    })
  }

  async fn delete_category(&self, space: SpaceId, category: CategoryId) -> Result<(), ApiError> {
    self.with(|state, _| match state.categories.get(&category) {
      Some(entry) if entry.space_id == space => {
        state.categories.remove(&category);
        Ok(())
      },
      _ => Err(ApiError::status(
        404,
        ErrorCode::CategoryNotFound,
        "no such category",
      )),
    })
  }

  async fn create_invite(
    &self,
    space: SpaceId,
    request: &CreateInviteRequest,
  ) -> Result<InviteResponse, ApiError> {
    self.with(|state, me| {
      let invite = InviteResponse {
        id: state.next_id(),
        space_id: space,
        email: request.email.clone(),
        role: request.role.clone(),
        channel_ids: request.channel_ids.clone(),
        sponsor_user_id: me,
        maybe_expires_at: request.maybe_expires_at,
      };
      state.invites.push(invite.clone());
      state.invite_messages.push(request.maybe_message.clone());
      Ok(invite)
    })
  }

  async fn list_invites(&self, space: SpaceId) -> Result<teamcraft_proto::ListInvitesResponse, ApiError> {
    self.with(|state, _| {
      Ok(teamcraft_proto::ListInvitesResponse {
        invites: state
          .invites
          .iter()
          .filter(|invite| invite.space_id == space)
          .cloned()
          .collect(),
      })
    })
  }

  async fn delete_invite(&self, space: SpaceId, invite: teamcraft_proto::InviteId) -> Result<(), ApiError> {
    self.with(|state, _| {
      let before = state.invites.len();
      state
        .invites
        .retain(|candidate| !(candidate.id == invite && candidate.space_id == space));
      if state.invites.len() == before {
        return Err(ApiError::status(404, ErrorCode::InviteNotFound, "no such invite"));
      }
      Ok(())
    })
  }

  async fn update_space_member(
    &self,
    space: SpaceId,
    user: UserId,
    request: &teamcraft_proto::UpdateSpaceMemberRequest,
  ) -> Result<SpaceMemberResponse, ApiError> {
    self.with(|state, _| {
      let summary = state.user(user)?.clone();
      state.space_roles.insert((space, user), request.role.clone());
      Ok(SpaceMemberResponse {
        user: summary,
        role: request.role.clone(),
        maybe_expires_at: request.maybe_expires_at,
        maybe_sponsor_user_id: None,
      })
    })
  }

  async fn remove_space_member(&self, space: SpaceId, user: UserId) -> Result<(), ApiError> {
    self.with(|state, _| {
      state.user(user)?;
      state.space_roles.remove(&(space, user));
      state.space_outsiders.insert(user);
      Ok(())
    })
  }

  async fn set_message_pinned(
    &self,
    conversation: ConversationId,
    message: MessageId,
    is_pinned: bool,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      let entry = state.member_conversation_mut(conversation, me)?;
      if !entry.messages.iter().any(|stored| stored.id == message) {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "unknown message",
        ));
      }
      if is_pinned {
        entry.pinned.insert(message);
      } else {
        entry.pinned.remove(&message);
      }
      Ok(())
    })
  }

  /// Like the server: the sender and the acknowledging user's devices hear about it.
  async fn acknowledge_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<(), ApiError> {
    self.with(|state, me| {
      let sender = state
        .member_conversation(conversation, me)?
        .messages
        .iter()
        .find(|stored| stored.id == message)
        .map(|stored| stored.sender_user_id)
        .ok_or_else(|| ApiError::status(400, ErrorCode::InvalidRequest, "unknown message"))?;
      state.publish(
        [sender, me],
        &GatewayEvent::MessageAck {
          conversation_id: conversation,
          message_id: message,
          user_id: me,
        },
      );
      Ok(())
    })
  }

  async fn list_threads(&self, query: &ListThreadsQuery) -> Result<ListThreadsResponse, ApiError> {
    self.with(|state, me| {
      let mut threads: Vec<ThreadSummary> = state
        .conversations
        .values()
        .filter(|conversation| conversation.members.contains_key(&me))
        .flat_map(|conversation| conversation.threads(me))
        .collect();
      threads
        .sort_by_key(|summary| std::cmp::Reverse(summary.maybe_last_reply.as_ref().map(|reply| reply.id)));
      let limit = query.maybe_limit.map_or(PAGE_SIZE, |limit| limit as usize);
      let has_more = threads.len() > limit;
      threads.truncate(limit);
      Ok(ListThreadsResponse { threads, has_more })
    })
  }

  /// Like the server: the newest message and MLS entry of each of the user's conversations (from
  /// the join on), and digests of the member lists as `list_members` and `list_space_members`
  /// would answer them.
  async fn sync_heads(&self) -> Result<SyncHeadsResponse, ApiError> {
    let spaces = self.with(|state, me| {
      if state.lacks_sync_heads {
        return Err(ApiError::Status {
          status: 404,
          maybe_body: None,
          maybe_retry_after: None,
        });
      }
      let _ = me;
      state.default_space_entry();
      Ok(
        state
          .spaces
          .values()
          .filter(|space| space.maybe_my_role.is_some())
          .map(|space| space.id)
          .collect::<Vec<_>>(),
      )
    })?;
    let mut space_heads = Vec::new();
    for space in spaces {
      let answer = self.server.lock().space_members_answer(space);
      if let Ok(members) = answer {
        space_heads.push(SpaceHead {
          space_id: space,
          members_digest: fake_digest(&members),
        });
      }
    }
    let conversations: Vec<(ConversationId, Option<MessageId>, Option<MlsMessageId>)> =
      self.with(|state, me| {
        Ok(
          state
            .conversations
            .values()
            .filter_map(|entry| {
              let (_, floor) = entry.members.get(&me)?;
              let maybe_last_mls = entry
                .mls
                .iter()
                .filter(|message| message.id.snowflake().get() > *floor)
                .map(|message| message.id)
                .max();
              let maybe_last_message = entry.messages.iter().map(|message| message.id).max();
              Some((entry.response.id, maybe_last_message, maybe_last_mls))
            })
            .collect(),
        )
      })?;
    let mut heads = Vec::new();
    for (conversation, maybe_last_message_id, maybe_last_mls_message_id) in conversations {
      let mut state = self.server.lock();
      // An unreadable list never matches, so the client reads it (and fails) every time.
      let members_digest = match state.members_answer(conversation, self.user) {
        Ok(members) => fake_digest(&members),
        Err(_) => format!("unreadable-{}", state.next_id::<MessageId>()),
      };
      drop(state);
      heads.push(ConversationHead {
        conversation_id: conversation,
        maybe_last_message_id,
        maybe_last_mls_message_id,
        members_digest,
      });
    }
    Ok(SyncHeadsResponse {
      conversations: heads,
      spaces: space_heads,
    })
  }

  /// The fake doesn't track read positions: nothing is unread unless a test set it.
  async fn unread(&self) -> Result<UnreadResponse, ApiError> {
    self.with(|state, me| {
      Ok(state.unread.get(&me).cloned().unwrap_or(UnreadResponse {
        conversations: Vec::new(),
        unread_thread_count: 0,
        thread_mention_count: 0,
        maybe_max_counted: None,
        maybe_counted_through: None,
      }))
    })
  }

  /// Like the server: only in DMs and group DMs, the others' positions, and nothing for (or about)
  /// members who turned receipts off.
  async fn read_receipts(
    &self,
    conversation: ConversationId,
  ) -> Result<teamcraft_proto::ReadReceiptsResponse, ApiError> {
    self.with(|state, me| {
      let found = state.member_conversation(conversation, me)?;
      let sends_receipts = |user: &UserId| {
        state
          .notification_settings
          .get(user)
          .is_none_or(|settings| settings.send_read_receipts)
      };
      let has_receipts = matches!(
        found.response.kind,
        ConversationKind::Dm | ConversationKind::Group
      );
      let receipts = if has_receipts && sends_receipts(&me) {
        found
          .read_positions
          .iter()
          .filter(|(user, _)| **user != me && sends_receipts(user))
          .map(|(user, message)| teamcraft_proto::ReadReceipt {
            user_id: *user,
            message_id: *message,
          })
          .collect()
      } else {
        Vec::new()
      };
      Ok(teamcraft_proto::ReadReceiptsResponse {
        conversation_id: conversation,
        receipts,
      })
    })
  }

  async fn key_bag(&self) -> Result<KeyBagResponse, ApiError> {
    self.with(|state, me| {
      state
        .key_bags
        .get(&me)
        .cloned()
        .ok_or_else(|| ApiError::status(404, ErrorCode::KeyBagNotFound, "no key bag"))
    })
  }

  async fn list_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    query: &ListWorkspaceLogQuery,
  ) -> Result<ListWorkspaceLogResponse, ApiError> {
    self.with(|state, _| {
      let after = query.maybe_after.unwrap_or(0);
      let limit = query
        .maybe_limit
        .map_or(PAGE_SIZE, |limit| limit as usize)
        .clamp(1, PAGE_SIZE);
      let mut entries: Vec<WorkspaceLogEntryResponse> = state
        .log_mut(kind)
        .iter()
        .filter(|entry| entry.version > after)
        .cloned()
        .collect();
      let has_more = entries.len() > limit;
      entries.truncate(limit);
      Ok(ListWorkspaceLogResponse { entries, has_more })
    })
  }

  async fn append_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    request: &AppendWorkspaceLogRequest,
  ) -> Result<WorkspaceLogEntryResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, request.signer_device_id)?;
      // Like the server: the root of trust must be recoverable.
      if *kind == WorkspaceLogKind::Roster
        && request.expected_version == 0
        && !(state.identity_keys.contains_key(&me) && state.key_bags.contains_key(&me))
      {
        return Err(ApiError::status(
          409,
          ErrorCode::Conflict,
          "no key bag for the root",
        ));
      }
      let created_at = UnixMillis(state.next_id::<UserId>().snowflake().get());
      let log = state.log_mut(kind);
      let current = log.len() as u64;
      if request.expected_version != current {
        return Err(ApiError::status(
          409,
          ErrorCode::VersionConflict,
          format!("stored version is {current}"),
        ));
      }
      let entry = WorkspaceLogEntryResponse {
        kind: kind.clone(),
        version: current + 1,
        payload: request.payload.clone(),
        signature: request.signature.clone(),
        maybe_signer_device_id: Some(request.signer_device_id),
        created_at,
      };
      log.push(entry.clone());
      let everyone: Vec<UserId> = state.users.keys().copied().collect();
      state.publish(
        everyone,
        &GatewayEvent::WorkspaceUpdate {
          kind: kind.clone(),
          version: entry.version,
        },
      );
      Ok(entry)
    })
  }

  async fn list_channel_role_log(
    &self,
    conversation: ConversationId,
    query: &teamcraft_proto::ListChannelRoleLogQuery,
  ) -> Result<teamcraft_proto::ListChannelRoleLogResponse, ApiError> {
    self.with(|state, me| {
      if state.lacks_role_log {
        return Err(ApiError::Status {
          status: 404,
          maybe_body: None,
          maybe_retry_after: None,
        });
      }
      let entry = state.actor_conversation_mut(conversation, me)?;
      let after = query.maybe_after.unwrap_or(0);
      let limit = query
        .maybe_limit
        .map_or(PAGE_SIZE, |limit| limit as usize)
        .clamp(1, PAGE_SIZE);
      let mut entries: Vec<_> = entry
        .role_log
        .iter()
        .filter(|entry| entry.version > after)
        .cloned()
        .collect();
      let has_more = entries.len() > limit;
      entries.truncate(limit);
      Ok(teamcraft_proto::ListChannelRoleLogResponse { entries, has_more })
    })
  }

  async fn append_channel_role_log(
    &self,
    conversation: ConversationId,
    request: &teamcraft_proto::AppendChannelRoleLogRequest,
  ) -> Result<teamcraft_proto::ChannelRoleLogEntryResponse, ApiError> {
    self.with(|state, me| {
      if state.lacks_role_log {
        return Err(ApiError::Status {
          status: 404,
          maybe_body: None,
          maybe_retry_after: None,
        });
      }
      state.ensure_own_device(me, request.signer_device_id)?;
      let is_instance_admin = state.admins.contains(&me);
      let created_at = UnixMillis(state.next_id::<UserId>().snowflake().unix_millis());
      let entry = state.actor_conversation_mut(conversation, me)?;
      // Like the server: open channels only, by channel admins and workspace admins; the payload
      // names this channel and the next version. Signatures are the clients' business.
      if entry.response.kind != ConversationKind::Channel || entry.response.is_archived {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "not an open channel",
        ));
      }
      let is_channel_admin = entry
        .members
        .get(&me)
        .is_some_and(|(own, _)| *own == ChannelRole::Admin);
      if !is_channel_admin && !is_instance_admin {
        return Err(ApiError::status(
          403,
          ErrorCode::Forbidden,
          "missing permission manage_channel_roles",
        ));
      }
      let document: teamcraft_proto::ChannelRolesDocument =
        serde_json::from_slice(request.payload.as_slice())
          .map_err(|error| ApiError::status(400, ErrorCode::InvalidRequest, error.to_string()))?;
      let current = entry.role_log.len() as u64;
      if document.conversation_id != conversation || document.version != request.expected_version + 1 {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "the document names another channel or version",
        ));
      }
      if request.expected_version != current {
        return Err(ApiError::status(
          409,
          ErrorCode::VersionConflict,
          format!("stored version is {current}"),
        ));
      }
      let appended = teamcraft_proto::ChannelRoleLogEntryResponse {
        version: current + 1,
        payload: request.payload.clone(),
        signature: request.signature.clone(),
        maybe_signer_device_id: Some(request.signer_device_id),
        created_at,
      };
      entry.role_log.push(appended.clone());
      let members: Vec<UserId> = entry.members.keys().copied().collect();
      state.publish(
        members,
        &GatewayEvent::ChannelRolesUpdate {
          conversation_id: conversation,
          version: appended.version,
        },
      );
      Ok(appended)
    })
  }

  async fn create_attachment(
    &self,
    request: &CreateAttachmentRequest,
  ) -> Result<CreateAttachmentResponse, ApiError> {
    self.with(|state, me| {
      state.member_conversation(request.conversation_id, me)?;
      if state
        .maybe_max_attachment_bytes
        .is_some_and(|limit| request.size_bytes > limit)
      {
        return Err(ApiError::status(
          413,
          ErrorCode::PayloadTooLarge,
          "attachment too large",
        ));
      }
      let id: AttachmentId = state.next_id();
      state.attachments.insert(
        id,
        FakeAttachment {
          conversation: request.conversation_id,
          size_bytes: request.size_bytes,
          maybe_blob: None,
          is_complete: false,
          maybe_parts: None,
        },
      );
      Ok(CreateAttachmentResponse {
        id,
        upload_url: format!("{BLOB_SCHEME}{id}"),
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn create_share_link(
    &self,
    attachment: AttachmentId,
    request: &CreateShareLinkRequest,
  ) -> Result<ShareLinkResponse, ApiError> {
    self.with(|state, me| {
      let conversation = state
        .attachments
        .get(&attachment)
        .filter(|stored| stored.is_complete)
        .map(|stored| stored.conversation)
        .ok_or_else(|| ApiError::status(404, ErrorCode::AttachmentNotFound, "no such attachment"))?;
      state.member_conversation(conversation, me)?;
      let share_id = format!("share{}", state.next_id::<AttachmentId>());
      state.share_links.insert(share_id.clone(), attachment);
      Ok(ShareLinkResponse {
        maybe_url: Some(format!("https://chat.example.com/f/{share_id}")),
        id: share_id,
        attachment_id: attachment,
        created_at: UnixMillis(0),
        expires_at: request.maybe_expires_at.unwrap_or(UnixMillis(i64::MAX)),
        maybe_max_downloads: request.maybe_max_downloads,
        is_password_protected: request.is_password_protected,
        download_count: 0,
        maybe_last_downloaded_at: None,
        is_revoked: false,
      })
    })
  }

  async fn revoke_share_link(&self, share_id: &str) -> Result<(), ApiError> {
    self.with(|state, _| {
      if !state.share_links.contains_key(share_id) {
        return Err(ApiError::status(
          404,
          ErrorCode::AttachmentNotFound,
          "no such link",
        ));
      }
      if !state.revoked_share_links.insert(share_id.to_owned()) {
        return Err(ApiError::status(
          409,
          ErrorCode::Conflict,
          "the link was revoked already",
        ));
      }
      Ok(())
    })
  }

  async fn list_share_links(&self) -> Result<ListShareLinksResponse, ApiError> {
    self.with(|state, _| {
      let mut links: Vec<ShareLinkResponse> = state
        .share_links
        .iter()
        .map(|(id, attachment)| ShareLinkResponse {
          maybe_url: Some(format!("https://chat.example.com/f/{id}")),
          id: id.clone(),
          attachment_id: *attachment,
          created_at: UnixMillis(0),
          expires_at: UnixMillis(i64::MAX),
          maybe_max_downloads: None,
          is_password_protected: false,
          download_count: 0,
          maybe_last_downloaded_at: None,
          is_revoked: state.revoked_share_links.contains(id),
        })
        .collect();
      links.sort_by(|a, b| a.id.cmp(&b.id));
      Ok(ListShareLinksResponse { links })
    })
  }

  async fn complete_attachment(&self, attachment: AttachmentId) -> Result<AttachmentResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .attachments
        .get_mut(&attachment)
        .ok_or_else(|| ApiError::status(404, ErrorCode::AttachmentNotFound, "unknown"))?;
      if entry.maybe_blob.as_ref().map(|blob| blob.len() as u64) != Some(entry.size_bytes) {
        return Err(ApiError::status(
          400,
          ErrorCode::InvalidRequest,
          "upload missing or of the wrong size",
        ));
      }
      entry.is_complete = true;
      Ok(AttachmentResponse {
        id: attachment,
        conversation_id: entry.conversation,
        size_bytes: entry.size_bytes,
        is_complete: true,
      })
    })
  }

  async fn create_multipart_attachment(
    &self,
    request: &CreateAttachmentRequest,
  ) -> Result<CreateMultipartAttachmentResponse, ApiError> {
    let created = self.create_attachment(request).await?;
    self.with(|state, _| {
      if let Some(entry) = state.attachments.get_mut(&created.id) {
        entry.maybe_parts = Some(BTreeMap::new());
      }
      Ok(CreateMultipartAttachmentResponse {
        id: created.id,
        min_part_size_bytes: FAKE_MIN_PART_BYTES,
        max_part_size_bytes: 16 * 1024 * 1024,
        max_parts: 10_000,
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn presign_attachment_part(
    &self,
    attachment: AttachmentId,
    request: &PresignAttachmentPartRequest,
  ) -> Result<AttachmentPartUrlResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .attachments
        .get(&attachment)
        .filter(|entry| entry.maybe_parts.is_some() && !entry.is_complete)
        .ok_or_else(|| ApiError::status(404, ErrorCode::AttachmentNotFound, "no upload in parts"))?;
      if request.part_number == 0 || request.size_bytes == 0 || request.size_bytes > entry.size_bytes {
        return Err(ApiError::status(400, ErrorCode::InvalidRequest, "bad part"));
      }
      Ok(AttachmentPartUrlResponse {
        upload_url: format!(
          "{PART_SCHEME}{attachment}/{}/{}",
          request.part_number, request.size_bytes
        ),
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn complete_multipart_attachment(
    &self,
    attachment: AttachmentId,
    request: &CompleteMultipartAttachmentRequest,
  ) -> Result<AttachmentResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .attachments
        .get_mut(&attachment)
        .ok_or_else(|| ApiError::status(404, ErrorCode::AttachmentNotFound, "unknown"))?;
      if !entry.is_complete {
        let parts = entry
          .maybe_parts
          .as_ref()
          .ok_or_else(|| ApiError::status(409, ErrorCode::Conflict, "not an upload in parts"))?;
        let joined = join_fake_parts(parts, request, entry.size_bytes)?;
        entry.maybe_blob = joined;
        entry.maybe_parts = None;
        entry.is_complete = true;
      }
      Ok(AttachmentResponse {
        id: attachment,
        conversation_id: entry.conversation,
        size_bytes: entry.size_bytes,
        is_complete: true,
      })
    })
  }

  async fn abandon_attachment(&self, attachment: AttachmentId) -> Result<(), ApiError> {
    self.with(|state, _| {
      if state
        .attachments
        .get(&attachment)
        .is_some_and(|entry| entry.is_complete)
      {
        return Err(ApiError::status(409, ErrorCode::Conflict, "completed"));
      }
      state.attachments.remove(&attachment);
      state.abandoned_attachments.push(attachment);
      Ok(())
    })
  }

  async fn download_attachment(
    &self,
    attachment: AttachmentId,
  ) -> Result<AttachmentDownloadResponse, ApiError> {
    self.with(|state, me| {
      let entry = state
        .attachments
        .get(&attachment)
        .ok_or_else(|| ApiError::status(404, ErrorCode::AttachmentNotFound, "unknown"))?;
      let size_bytes = entry.size_bytes;
      state.member_conversation(entry.conversation, me)?;
      Ok(AttachmentDownloadResponse {
        download_url: format!("{BLOB_SCHEME}{attachment}"),
        size_bytes,
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn create_background_upload(
    &self,
    request: &CreateBackgroundUploadRequest,
  ) -> Result<CreateBackgroundUploadResponse, ApiError> {
    self.with(|state, me| {
      let id: BackgroundId = state.next_id();
      state.backgrounds.insert(
        id,
        FakeBackground {
          owner: me,
          size_bytes: request.size_bytes,
          maybe_blob: None,
          is_complete: false,
        },
      );
      Ok(CreateBackgroundUploadResponse {
        id,
        upload_url: format!("{BACKGROUND_SCHEME}{id}"),
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn complete_background_upload(&self, background: BackgroundId) -> Result<(), ApiError> {
    self.with(|state, me| {
      let entry = own_background(state, me, background)?;
      if entry.maybe_blob.as_ref().map(|blob| blob.len() as u64) != Some(entry.size_bytes) {
        return Err(ApiError::status(
          409,
          ErrorCode::Conflict,
          "upload missing or of the wrong size",
        ));
      }
      entry.is_complete = true;
      Ok(())
    })
  }

  async fn download_background(
    &self,
    background: BackgroundId,
  ) -> Result<BackgroundDownloadResponse, ApiError> {
    self.with(|state, me| {
      let entry = own_background(state, me, background)?;
      Ok(BackgroundDownloadResponse {
        download_url: format!("{BACKGROUND_SCHEME}{background}"),
        size_bytes: entry.size_bytes,
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn delete_background(&self, background: BackgroundId) -> Result<(), ApiError> {
    self.with(|state, me| {
      own_background(state, me, background)?;
      state.backgrounds.remove(&background);
      Ok(())
    })
  }

  async fn create_org_background_upload(
    &self,
    request: &CreateOrgBackgroundUploadRequest,
  ) -> Result<CreateOrgBackgroundUploadResponse, ApiError> {
    self.with(|state, me| {
      let id: OrgBackgroundId = state.next_id();
      state.org_backgrounds.insert(
        id,
        FakeBackground {
          owner: me,
          size_bytes: request.size_bytes,
          maybe_blob: None,
          is_complete: false,
        },
      );
      Ok(CreateOrgBackgroundUploadResponse {
        id,
        upload_url: format!("{ORG_BACKGROUND_SCHEME}{id}"),
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn complete_org_background_upload(&self, background: OrgBackgroundId) -> Result<(), ApiError> {
    self.with(|state, _| {
      let entry = state
        .org_backgrounds
        .get_mut(&background)
        .ok_or_else(|| ApiError::status(404, ErrorCode::BackgroundNotFound, "unknown"))?;
      if entry.maybe_blob.as_ref().map(|blob| blob.len() as u64) != Some(entry.size_bytes) {
        return Err(ApiError::status(
          409,
          ErrorCode::Conflict,
          "upload missing or of the wrong size",
        ));
      }
      entry.is_complete = true;
      Ok(())
    })
  }

  async fn download_org_background(
    &self,
    background: OrgBackgroundId,
  ) -> Result<OrgBackgroundDownloadResponse, ApiError> {
    self.with(|state, _| {
      let entry = state
        .org_backgrounds
        .get(&background)
        .filter(|entry| entry.is_complete)
        .ok_or_else(|| ApiError::status(404, ErrorCode::BackgroundNotFound, "unknown"))?;
      Ok(OrgBackgroundDownloadResponse {
        download_url: format!("{ORG_BACKGROUND_SCHEME}{background}"),
        size_bytes: entry.size_bytes,
        expires_at: UnixMillis(0),
      })
    })
  }

  async fn delete_org_background(&self, background: OrgBackgroundId) -> Result<(), ApiError> {
    self.with(|state, _| {
      state
        .org_backgrounds
        .remove(&background)
        .map(drop)
        .ok_or_else(|| ApiError::status(404, ErrorCode::BackgroundNotFound, "unknown"))
    })
  }

  fn put_blob_detached(
    &self,
    url: &str,
    bytes: Vec<u8>,
    sent: crate::transfers::ByteCounter,
  ) -> super::DetachedRequest {
    let (api, url) = (self.clone(), url.to_owned());
    Box::pin(async move {
      let length = bytes.len() as u64;
      api.put_blob(&url, bytes).await?;
      sent.add(length);
      Ok(())
    })
  }

  fn put_part_detached(
    &self,
    url: &str,
    bytes: Vec<u8>,
    sent: crate::transfers::ByteCounter,
  ) -> super::DetachedPart {
    let (api, url) = (self.clone(), url.to_owned());
    Box::pin(async move {
      let length = bytes.len() as u64;
      let etag = api.with(|state, me| {
        if state.blob_refused_users.contains(&me) {
          return Err(ApiError::Network("fake server: the upload was refused".into()));
        }
        store_fake_part(state, &url, bytes)
      })?;
      sent.add(length);
      Ok(etag)
    })
  }

  fn get_blob_detached(
    &self,
    url: &str,
    maybe_max_bytes: Option<u64>,
    received: crate::transfers::ByteCounter,
  ) -> super::DetachedDownload {
    let (api, url) = (self.clone(), url.to_owned());
    Box::pin(async move {
      let blob = match maybe_max_bytes {
        Some(max_bytes) => api.get_blob_capped(&url, max_bytes).await?,
        None => api.get_blob(&url).await?,
      };
      received.add(blob.len() as u64);
      Ok(blob)
    })
  }

  async fn put_blob(&self, url: &str, bytes: Vec<u8>) -> Result<(), ApiError> {
    if let Some(raw) = url.strip_prefix(ORG_BACKGROUND_SCHEME) {
      return self.with(|state, _| {
        let entry = org_background_blob(state, raw)?;
        if bytes.len() as u64 != entry.size_bytes || entry.is_complete {
          return Err(ApiError::Status {
            status: 403,
            maybe_body: None,
            maybe_retry_after: None,
          });
        }
        entry.maybe_blob = Some(bytes);
        Ok(())
      });
    }
    if let Some(raw) = url.strip_prefix(BACKGROUND_SCHEME) {
      return self.with(|state, _| {
        let entry = background_blob(state, raw)?;
        if bytes.len() as u64 != entry.size_bytes || entry.is_complete {
          return Err(ApiError::Status {
            status: 403,
            maybe_body: None,
            maybe_retry_after: None,
          });
        }
        entry.maybe_blob = Some(bytes);
        Ok(())
      });
    }
    self.with(|state, me| {
      if state.blob_refused_users.contains(&me) {
        return Err(ApiError::Network("fake server: the upload was refused".into()));
      }
      let entry = blob_entry(state, url)?;
      if bytes.len() as u64 != entry.size_bytes || entry.is_complete {
        return Err(ApiError::Status {
          status: 403,
          maybe_body: None,
          maybe_retry_after: None,
        });
      }
      entry.maybe_blob = Some(bytes);
      Ok(())
    })
  }

  async fn get_blob_capped(&self, url: &str, max_bytes: u64) -> Result<Vec<u8>, ApiError> {
    let blob = self.get_blob(url).await?;
    if blob.len() as u64 > max_bytes {
      return Err(ApiError::BlobTooLarge { max_bytes });
    }
    Ok(blob)
  }

  async fn get_blob(&self, url: &str) -> Result<Vec<u8>, ApiError> {
    if let Some(raw) = url.strip_prefix(ORG_BACKGROUND_SCHEME) {
      return self.with(|state, _| {
        org_background_blob(state, raw)?
          .maybe_blob
          .clone()
          .ok_or(ApiError::Status {
            status: 404,
            maybe_body: None,
            maybe_retry_after: None,
          })
      });
    }
    if let Some(raw) = url.strip_prefix(BACKGROUND_SCHEME) {
      return self.with(|state, _| {
        background_blob(state, raw)?
          .maybe_blob
          .clone()
          .ok_or(ApiError::Status {
            status: 404,
            maybe_body: None,
            maybe_retry_after: None,
          })
      });
    }
    self.with(|state, _| {
      blob_entry(state, url)?
        .maybe_blob
        .clone()
        .ok_or(ApiError::Status {
          status: 404,
          maybe_body: None,
          maybe_retry_after: None,
        })
    })
  }

  async fn register_push(&self, device: DeviceId, request: &RegisterPushRequest) -> Result<(), ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      state.push_registrations.insert(device, request.clone());
      Ok(())
    })
  }

  async fn push_gateway(&self) -> Result<PushGatewayResponse, ApiError> {
    self.with(|state, _| {
      Ok(PushGatewayResponse {
        maybe_gateway: state.push_gateway.clone(),
      })
    })
  }

  async fn web_push_key(&self) -> Result<WebPushKeyResponse, ApiError> {
    Ok(WebPushKeyResponse {
      maybe_public_key: Some(FAKE_WEB_PUSH_KEY.to_owned()),
    })
  }

  async fn gateway_info(&self, _gateway_url: &str) -> Result<GatewayInfoResponse, ApiError> {
    let public = FakeServer::gateway_token_public_key();
    Ok(GatewayInfoResponse {
      version: "1.0.0".into(),
      providers: vec![PushProvider::Apns, PushProvider::Fcm],
      is_registration_open: true,
      limits: GatewayLimits {
        pushes_per_second: 50,
        burst: 200,
        pushes_per_day: 200_000,
        pushes_per_token_per_minute: 30,
      },
      token_key: GatewayTokenKey {
        key_id: teamcraft_crypto::push_gateway::token_key_id(&public)
          .iter()
          .map(|byte| format!("{byte:02x}"))
          .collect(),
        algorithm: "hpke-x25519-hkdf-sha256-chacha20poly1305".into(),
        public_key: Base64Bytes::new(public.to_vec()),
      },
      are_bound_tokens_required: true,
      max_token_lifetime_secs: 90 * 24 * 3600,
      kinds: vec![
        PushKind::Message,
        PushKind::Mention,
        PushKind::Call,
        PushKind::Dismiss,
        PushKind::Test,
      ],
    })
  }

  async fn revoke_gateway_token(&self, gateway_url: &str, sealed_token: &[u8]) -> Result<(), ApiError> {
    self.with(|state, _| {
      state.revoked_gateway_urls.push(gateway_url.to_owned());
      state.revoked_gateway_tokens.push(sealed_token.to_vec());
      Ok(())
    })
  }

  async fn unregister_push(&self, device: DeviceId) -> Result<(), ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      state.push_registrations.remove(&device);
      Ok(())
    })
  }

  async fn send_test_push(&self, device: DeviceId) -> Result<TestPushResponse, ApiError> {
    self.with(|state, me| {
      state.ensure_own_device(me, device)?;
      let maybe_provider = state
        .push_registrations
        .get(&device)
        .map(|registration| registration.provider.clone());
      Ok(TestPushResponse {
        outcome: if maybe_provider.is_some() {
          TestPushOutcome::Sent
        } else {
          TestPushOutcome::NotRegistered
        },
        maybe_provider,
        sent_at: UnixMillis(0),
      })
    })
  }

  async fn my_push_health(&self) -> Result<PushHealthResponse, ApiError> {
    self.with(|state, me| {
      let devices = state
        .devices_of(me)
        .map(|device| {
          let maybe_provider = state
            .push_registrations
            .get(&device.response.id)
            .map(|registration| registration.provider.clone());
          DevicePushHealth {
            device_id: device.response.id,
            counts: PushDeliveryCounts {
              sent: u64::from(maybe_provider.is_some()),
              maybe_last_sent_at: maybe_provider.as_ref().map(|_| UnixMillis(1_000)),
              ..PushDeliveryCounts::default()
            },
            maybe_provider,
          }
        })
        .collect();
      Ok(PushHealthResponse { devices })
    })
  }

  async fn notification_settings(&self) -> Result<NotificationSettings, ApiError> {
    self.with(|state, me| {
      Ok(
        state
          .notification_settings
          .get(&me)
          .cloned()
          .unwrap_or_else(default_notification_settings),
      )
    })
  }

  async fn connect_gateway(&self) -> Result<FakeGateway, ApiError> {
    let (sender, receiver) = unbounded();
    let connection = {
      let mut state = self.server.lock();
      if state.offline_users.contains(&self.user) {
        return Err(ApiError::Network("fake server: client is offline".into()));
      }
      if let Some(error) = state.unavailable_answer(self.user) {
        state.unavailable_connects += 1;
        return Err(error);
      }
      state.next_connection += 1;
      state.gateway_connects += 1;
      state.next_connection
    };
    let is_silent = self.server.lock().silent_gateway_users.contains(&self.user);
    let gateway = FakeGateway {
      server: self.server.clone(),
      user: self.user,
      connection,
      receiver,
    };
    // Before identify the connection has no session; keep the sender until it picks one.
    self.server.lock().sessions.insert(
      pending_session(connection),
      Session {
        user: self.user,
        seq: 0,
        buffer: Vec::new(),
        maybe_live: Some((connection, sender.clone())),
      },
    );
    if !is_silent {
      let _ = sender.unbounded_send(GatewayServerMessage::Hello {
        heartbeat_interval_ms: HEARTBEAT_INTERVAL_MS,
      });
    }
    Ok(gateway)
  }

  async fn connect_rendezvous(&self) -> Result<FakeRendezvous, ApiError> {
    let connection = self.with(|state, _| {
      state.next_connection += 1;
      Ok(state.next_connection)
    })?;
    let (sender, receiver) = unbounded();
    Ok(FakeRendezvous {
      server: self.server.clone(),
      user: self.user,
      connection,
      maybe_rendezvous_id: None,
      sender,
      receiver,
    })
  }

  fn gateway_token(&self) -> Result<String, ApiError> {
    Ok(format!("fake-token-{}", self.user))
  }
}

impl GatewayConnection for FakeGateway {
  async fn send(&mut self, message: &GatewayClientMessage) -> Result<(), ApiError> {
    let mut state = self.server.lock();
    let Some(sender) = state
      .sessions
      .get(&pending_session(self.connection))
      .and_then(|session| session.maybe_live.clone())
      .map(|(_, sender)| sender)
      .or_else(|| state.live_sender(self.connection))
    else {
      return Err(ApiError::Network("fake gateway closed".into()));
    };
    if state.silent_gateway_users.contains(&self.user) || state.frozen_connections.contains(&self.connection)
    {
      return Ok(());
    }
    if state.is_heartbeat_late(self.connection) {
      state.heartbeat_timeouts += 1;
      for session in state.sessions.values_mut() {
        if session
          .maybe_live
          .as_ref()
          .is_some_and(|(live, _)| *live == self.connection)
        {
          session.maybe_live = None;
        }
      }
      return Ok(());
    }
    let is_handshake = matches!(
      message,
      GatewayClientMessage::Identify { .. } | GatewayClientMessage::Resume { .. }
    );
    if is_handshake && state.signed_out_users.contains(&self.user) {
      // Like the server: an error frame, then the connection closes.
      state.gateway_refusals += 1;
      state.sessions.remove(&pending_session(self.connection));
      let _ = sender.unbounded_send(GatewayServerMessage::Error {
        error: ErrorBody::new(ErrorCode::InvalidToken, "invalid token: token expired"),
      });
      return Ok(());
    }
    match message {
      GatewayClientMessage::Identify { .. } => {
        state.sessions.remove(&pending_session(self.connection));
        let session_id = format!("session-{}-{}", self.user, self.connection);
        state.sessions.insert(
          session_id.clone(),
          Session {
            user: self.user,
            seq: 0,
            buffer: Vec::new(),
            maybe_live: Some((self.connection, sender.clone())),
          },
        );
        let user = state.user(self.user)?.clone();
        let _ = sender.unbounded_send(GatewayServerMessage::Ready { session_id, user });
      },
      GatewayClientMessage::Resume {
        session_id, last_seq, ..
      } => {
        match state.sessions.get_mut(session_id) {
          Some(session) if session.user == self.user => {
            session.maybe_live = Some((self.connection, sender.clone()));
            let missed: Vec<(u64, GatewayEvent)> = session
              .buffer
              .iter()
              .filter(|(seq, _)| seq > last_seq)
              .cloned()
              .collect();
            let _ = sender.unbounded_send(GatewayServerMessage::Resumed {
              replayed: missed.len() as u64,
            });
            for (seq, event) in missed {
              let _ = sender.unbounded_send(GatewayServerMessage::Event { seq, event });
            }
            state.sessions.remove(&pending_session(self.connection));
          },
          _ => {
            // Like the server: `session_invalid`, then the connection closes; the client starts over
            // with `identify` on a new one.
            let _ = sender.unbounded_send(GatewayServerMessage::SessionInvalid);
            state.sessions.remove(&pending_session(self.connection));
          },
        }
      },
      GatewayClientMessage::Heartbeat => {
        state.heartbeats += 1;
        let _ = sender.unbounded_send(GatewayServerMessage::HeartbeatAck);
      },
      GatewayClientMessage::Typing {
        conversation_id,
        maybe_thread_id,
      } => {
        state
          .typing_reports
          .entry(self.user)
          .or_default()
          .push(*conversation_id);
        // Members only, like the server; the typist doesn't get their own event.
        let others: Vec<UserId> = state
          .conversations
          .get(conversation_id)
          .filter(|conversation| conversation.members.contains_key(&self.user))
          .map(|conversation| {
            conversation
              .members
              .keys()
              .copied()
              .filter(|member| *member != self.user)
              .collect()
          })
          .unwrap_or_default();
        state.publish(
          others,
          &GatewayEvent::TypingStart {
            conversation_id: *conversation_id,
            user_id: self.user,
            maybe_thread_id: *maybe_thread_id,
          },
        );
      },
      GatewayClientMessage::SetActivity { activity } => {
        state
          .activity_reports
          .entry(self.user)
          .or_default()
          .push(activity.clone());
      },
      GatewayClientMessage::Attention { .. } | GatewayClientMessage::Unknown => {},
    }
    Ok(())
  }

  async fn receive(&mut self) -> Option<Result<GatewayServerMessage, ApiError>> {
    self.receiver.next().await.map(Ok)
  }
}

impl RendezvousConnection for FakeRendezvous {
  async fn send_control(&mut self, message: &RendezvousClientMessage) -> Result<(), ApiError> {
    let RendezvousClientMessage::Join { rendezvous_id, .. } = message else {
      return Ok(());
    };
    let mut state = self.server.lock();
    let relay = state.relays.entry(rendezvous_id.clone()).or_default();
    // One user's two devices only, like the server.
    if relay.endpoints.len() >= 2 || relay.endpoints.iter().any(|(_, user, _)| *user != self.user) {
      let error = ErrorBody::new(ErrorCode::Forbidden, "relay is taken");
      let _ = self
        .sender
        .unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::Error { error }));
      return Ok(());
    }
    let is_peer_present = !relay.endpoints.is_empty();
    let is_claim = is_peer_present;
    for (_, _, peer) in &relay.endpoints {
      let _ = peer.unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::PeerJoined));
    }
    relay
      .endpoints
      .push((self.connection, self.user, self.sender.clone()));
    let _ = self
      .sender
      .unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::Joined {
        peer_present: is_peer_present,
        maybe_expires_in_secs: Some(RELAY_TTL_SECS),
      }));
    let held = std::mem::take(&mut relay.held);
    for (from, frame) in held {
      if from == self.connection {
        relay.held.push((from, frame));
      } else {
        let _ = self.sender.unbounded_send(RendezvousFrame::Binary(frame));
      }
    }
    self.maybe_rendezvous_id = Some(rendezvous_id.clone());
    // Like the server: the second device takes the request up, the prompts go.
    if is_claim && state.link_requests.remove(rendezvous_id).is_some() {
      state.publish(
        [self.user],
        &GatewayEvent::DeviceLinkClosed {
          request_id: rendezvous_id.clone(),
          reason: DeviceLinkClosedReason::Claimed,
        },
      );
    }
    Ok(())
  }

  async fn send_binary(&mut self, frame: Vec<u8>) -> Result<(), ApiError> {
    let id = self
      .maybe_rendezvous_id
      .clone()
      .ok_or_else(|| ApiError::Gateway("join first".into()))?;
    let mut state = self.server.lock();
    if state.relay_frames_to_lose > 0 {
      state.relay_frames_to_lose -= 1;
      return Ok(());
    }
    let relay = state.relays.entry(id).or_default();
    match relay
      .endpoints
      .iter()
      .find(|(connection, _, _)| *connection != self.connection)
    {
      Some((_, _, peer)) => {
        let _ = peer.unbounded_send(RendezvousFrame::Binary(frame));
      },
      None => relay.held.push((self.connection, frame)),
    }
    Ok(())
  }

  async fn receive(&mut self) -> Option<Result<RendezvousFrame, ApiError>> {
    self.receiver.next().await.map(Ok)
  }
}

/// Like the server: the other device learns that this one left, and the relay closes.
impl Drop for FakeRendezvous {
  fn drop(&mut self) {
    let Some(id) = self.maybe_rendezvous_id.take() else {
      return;
    };
    let mut state = self.server.lock();
    if state.link_requests.remove(&id).is_some() {
      state.publish(
        [self.user],
        &GatewayEvent::DeviceLinkClosed {
          request_id: id.clone(),
          reason: DeviceLinkClosedReason::Withdrawn,
        },
      );
    }
    if let Some(relay) = state.relays.remove(&id) {
      for (connection, _, peer) in relay.endpoints {
        if connection != self.connection {
          let _ = peer.unbounded_send(RendezvousFrame::Control(RendezvousServerMessage::PeerLeft));
        }
      }
    }
  }
}

impl FakeGateway {
  pub fn user_id(&self) -> UserId {
    self.user
  }
}

impl State {
  /// What `GET /spaces/{id}/members` answers.
  fn space_members_answer(&mut self, space: SpaceId) -> Result<ListSpaceMembersResponse, ApiError> {
    self.default_space_entry();
    if !self.spaces.contains_key(&space) {
      return Err(ApiError::status(404, ErrorCode::SpaceNotFound, "no such space"));
    }
    let members = self
      .users
      .values()
      .filter(|user| !self.space_outsiders.contains(&user.id))
      .map(|user| SpaceMemberResponse {
        user: user.clone(),
        role: self
          .space_roles
          .get(&(space, user.id))
          .cloned()
          .unwrap_or(SpaceRole::Member),
        maybe_expires_at: None,
        maybe_sponsor_user_id: None,
      })
      .collect();
    Ok(ListSpaceMembersResponse { members })
  }

  /// What `GET …/members` answers `me`.
  fn members_answer(
    &self,
    conversation: ConversationId,
    me: UserId,
  ) -> Result<ListConversationMembersResponse, ApiError> {
    if self.broken_member_lists.contains(&conversation) {
      return Err(ApiError::Decode("fake server: unreadable member list".into()));
    }
    // Instance admins may list any conversation's members, like the server.
    let entry = if self.admins.contains(&me) {
      self.conversation(conversation)?
    } else {
      self.member_conversation(conversation, me)?
    };
    let members = entry
      .members
      .iter()
      .map(|(user, (role, _))| (*user, role.clone()))
      .collect::<Vec<_>>();
    let members = members
      .into_iter()
      .map(|(user, role)| {
        Ok(ConversationMemberResponse {
          user: self.user(user)?.clone(),
          role,
        })
      })
      .collect::<Result<_, ApiError>>()?;
    Ok(ListConversationMembersResponse { members })
  }

  /// Time-sortable like the server's: the server's clock in the time bits, so clients reading an
  /// ID's time get the server's time.
  fn next_id<T: Id>(&mut self) -> T {
    let shift = teamcraft_proto::SNOWFLAKE_SEQUENCE_BITS + teamcraft_proto::SNOWFLAKE_WORKER_BITS;
    let since_epoch = self
      .now_ms()
      .saturating_sub(teamcraft_proto::SNOWFLAKE_EPOCH_MS)
      .max(0);
    self.next_id = self.next_id.saturating_add(1).max(since_epoch << shift);
    T::from_snowflake(snowflake(self.next_id))
  }

  /// The ID the next created object gets, minus one: "everything created so far".
  fn peek_id(&self) -> i64 {
    self.next_id
  }

  fn admin_join_response(
    &self,
    conversation: ConversationId,
    user: UserId,
    requested_at: i64,
  ) -> Result<AdminJoinRequestResponse, ApiError> {
    Ok(AdminJoinRequestResponse {
      conversation_id: conversation,
      user: self.user(user)?.clone(),
      requested_at: UnixMillis(requested_at),
      expires_at: UnixMillis(requested_at.saturating_add(ADMIN_JOIN_REQUEST_MS)),
    })
  }

  /// Requests are decided by the channel's owners (members with the channel role `admin`).
  fn require_channel_owner(&self, conversation: ConversationId, me: UserId) -> Result<(), ApiError> {
    let is_owner = self
      .conversation(conversation)?
      .members
      .get(&me)
      .is_some_and(|(role, _)| *role == ChannelRole::Admin);
    if is_owner {
      Ok(())
    } else {
      Err(ApiError::status(403, ErrorCode::Forbidden, "owners only"))
    }
  }

  /// Tells the channel's owners and the admin, as the server does.
  fn publish_admin_join(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    change: AdminJoinRequestChange,
  ) {
    let mut recipients: Vec<UserId> = self
      .conversations
      .get(&conversation)
      .map(|entry| {
        entry
          .members
          .iter()
          .filter(|(_, (role, _))| *role == ChannelRole::Admin)
          .map(|(member, _)| *member)
          .collect()
      })
      .unwrap_or_default();
    recipients.push(user);
    self.publish(
      recipients,
      &GatewayEvent::AdminJoinRequestUpdate {
        conversation_id: conversation,
        user_id: user,
        change,
      },
    );
  }

  fn user(&self, user: UserId) -> Result<&UserSummary, ApiError> {
    self
      .users
      .get(&user)
      .ok_or_else(|| ApiError::status(404, ErrorCode::UserNotFound, "unknown user"))
  }

  /// Counts a call against the user's armed network cut; returns the cut when it hits this call
  /// (the user is offline from then on).
  fn take_network_cut(&mut self, user: UserId) -> Option<NetworkCut> {
    let (calls_left, cut) = self.network_cuts.get_mut(&user)?;
    if *calls_left > 0 {
      *calls_left -= 1;
      return None;
    }
    let cut = *cut;
    self.network_cuts.remove(&user);
    self.offline_users.insert(user);
    Some(cut)
  }

  /// Notes a frame from `connection`; `true` if it came later than the enforced heartbeat timeout.
  fn is_heartbeat_late(&mut self, connection: u64) -> bool {
    let Some((clock, timeout_ms)) = &self.maybe_heartbeat_timeout else {
      return false;
    };
    let now = clock.now_ms();
    let timeout_ms = *timeout_ms;
    self
      .last_client_frames
      .insert(connection, now)
      .is_some_and(|last| now.saturating_sub(last) > timeout_ms)
  }

  /// The server's clock: the system clock plus [`FakeServer::advance_clock`].
  fn now_ms(&self) -> i64 {
    SystemClock.now_ms().saturating_add(self.clock_offset_ms)
  }

  fn devices_of(&self, user: UserId) -> impl Iterator<Item = &Device> {
    self.devices.values().filter(move |device| device.user == user)
  }

  fn ensure_own_device(&self, me: UserId, device: DeviceId) -> Result<(), ApiError> {
    match self.devices.get(&device) {
      Some(found) if found.user == me => Ok(()),
      _ => Err(ApiError::status(404, ErrorCode::DeviceNotFound, "unknown device")),
    }
  }

  /// The default space as an entry of `spaces`, created on first use.
  fn default_space_entry(&mut self) {
    let id = self.space();
    self.spaces.entry(id).or_insert_with(|| SpaceResponse {
      id,
      name: "Space A".into(),
      maybe_icon: None,
      maybe_color: None,
      maybe_description: None,
      maybe_image: None,
      join_policy: JoinPolicy::Open,
      is_archived: false,
      maybe_brand_id: None,
      maybe_my_role: Some(SpaceRole::Member),
      maybe_my_notifications: None,
      member_count: 0,
      can_add_members: false,
      can_invite_guests: false,
      can_manage_space: false,
      can_create_channels: false,
    });
  }

  fn space(&mut self) -> SpaceId {
    if let Some(space) = self.space {
      return space;
    }
    let space = self.next_id();
    self.space = Some(space);
    space
  }

  fn revoke_device(&mut self, user: UserId, device: DeviceId) {
    let state = self;
    if !state.is_ignoring_revocations {
      state.devices.remove(&device);
      state.key_packages.remove(&device);
    }
    let mut audience: HashSet<UserId> = state
      .conversations
      .values()
      .filter(|conversation| conversation.members.contains_key(&user))
      .flat_map(|conversation| conversation.members.keys().copied())
      .collect();
    audience.insert(user);
    state.publish(
      audience,
      &GatewayEvent::DeviceRemove {
        user_id: user,
        device_id: device,
      },
    );
  }

  fn conversation_mut(&mut self, conversation: ConversationId) -> Result<&mut Conversation, ApiError> {
    self
      .conversations
      .get_mut(&conversation)
      .ok_or_else(|| ApiError::status(404, ErrorCode::ChannelNotFound, "unknown conversation"))
  }

  fn conversation(&self, conversation: ConversationId) -> Result<&Conversation, ApiError> {
    self
      .conversations
      .get(&conversation)
      .ok_or_else(|| ApiError::status(404, ErrorCode::ChannelNotFound, "unknown conversation"))
  }

  fn users_sharing_conversation(&self, user: UserId) -> HashSet<UserId> {
    self
      .conversations
      .values()
      .filter(|conversation| conversation.members.contains_key(&user))
      .flat_map(|conversation| conversation.members.keys().copied())
      .collect()
  }

  /// Authors of a thread's root and replies: the users following it.
  fn thread_participants(&self, conversation: ConversationId, thread: ThreadId) -> Vec<UserId> {
    self
      .conversations
      .get(&conversation)
      .map(|entry| {
        entry
          .messages
          .iter()
          .filter(|message| message.maybe_thread_id == Some(thread) || is_root(message, thread))
          .map(|message| message.sender_user_id)
          .collect()
      })
      .unwrap_or_default()
  }

  fn member_conversation(&self, conversation: ConversationId, me: UserId) -> Result<&Conversation, ApiError> {
    let found = self.conversation(conversation)?;
    if found.members.contains_key(&me) {
      Ok(found)
    } else {
      Err(ApiError::status(404, ErrorCode::ChannelNotFound, "not a member"))
    }
  }

  /// The conversation `me` may remove somebody from: as a member, or as a server admin (also in
  /// conversations they aren't in).
  fn removal_target(
    &mut self,
    conversation: ConversationId,
    me: UserId,
  ) -> Result<&mut Conversation, ApiError> {
    if self.refusing_removals_by.contains(&me) {
      return Err(ApiError::status(
        403,
        ErrorCode::Forbidden,
        "may not remove members",
      ));
    }
    if self.admins.contains(&me) {
      self.conversation_mut(conversation)
    } else {
      self.member_conversation_mut(conversation, me)
    }
  }

  fn member_conversation_mut(
    &mut self,
    conversation: ConversationId,
    me: UserId,
  ) -> Result<&mut Conversation, ApiError> {
    match self.conversations.get_mut(&conversation) {
      Some(found) if found.members.contains_key(&me) => Ok(found),
      _ => Err(ApiError::status(404, ErrorCode::ChannelNotFound, "not a member")),
    }
  }

  /// The conversation if `me` is a member or a workspace admin (who may manage every channel).
  fn actor_conversation_mut(
    &mut self,
    conversation: ConversationId,
    me: UserId,
  ) -> Result<&mut Conversation, ApiError> {
    let is_admin = self.admins.contains(&me);
    match self.conversations.get_mut(&conversation) {
      Some(found) if found.members.contains_key(&me) || is_admin => Ok(found),
      _ => Err(ApiError::status(404, ErrorCode::ChannelNotFound, "not a member")),
    }
  }

  fn log_mut(&mut self, kind: &WorkspaceLogKind) -> &mut Vec<WorkspaceLogEntryResponse> {
    match kind {
      WorkspaceLogKind::Policy => &mut self.policy_log,
      _ => &mut self.roster_log,
    }
  }

  fn create_conversation(
    &mut self,
    kind: ConversationKind,
    maybe_space: Option<SpaceId>,
    maybe_name: Option<String>,
    history_policy: HistoryPolicy,
    members: &[UserId],
  ) -> ConversationId {
    let id = self.next_id();
    let floor = self.peek_id();
    let response = ConversationResponse {
      id,
      maybe_space_id: maybe_space,
      kind,
      maybe_name,
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy,
      maybe_category_id: None,
      is_archived: false,
      moderation: ChannelModeration::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: None,
    };
    let members = members
      .iter()
      .map(|user| (*user, (ChannelRole::Member, floor)))
      .collect();
    self.conversations.insert(
      id,
      Conversation {
        response,
        members,
        epoch: 0,
        mls: Vec::new(),
        messages: Vec::new(),
        favorites: HashMap::new(),
        read_positions: HashMap::new(),
        thread_read_positions: HashMap::new(),
        pinned: HashSet::new(),
        maybe_group_info: None,
        role_log: Vec::new(),
      },
    );
    id
  }

  /// What `DELETE /admin/users/{id}` does: 1:1 DMs (and group DMs that would keep one other
  /// person) go, the user leaves every other conversation, their devices and keys go, and the
  /// account becomes a tombstone. Idempotent.
  fn delete_user(&mut self, user: UserId) {
    let theirs: Vec<ConversationId> = self
      .conversations
      .iter()
      .filter(|(_, entry)| entry.members.contains_key(&user))
      .map(|(id, _)| *id)
      .collect();
    let mut co_members = BTreeSet::new();
    for conversation in theirs {
      let Some(entry) = self.conversations.get_mut(&conversation) else {
        continue;
      };
      entry.members.remove(&user);
      let remaining: Vec<UserId> = entry.members.keys().copied().collect();
      co_members.extend(remaining.iter().copied());
      let is_direct = matches!(
        entry.response.kind,
        ConversationKind::Dm | ConversationKind::Group
      );
      if is_direct && (entry.response.kind == ConversationKind::Dm || remaining.len() <= 1) {
        self.conversations.remove(&conversation);
        for other in &remaining {
          self
            .deleted_conversations
            .entry(*other)
            .or_default()
            .push(conversation);
        }
        self.publish(
          remaining,
          &GatewayEvent::ConversationDelete {
            conversation_id: conversation,
          },
        );
      } else {
        self.publish(
          remaining,
          &GatewayEvent::MembershipRemove {
            conversation_id: conversation,
            user_id: user,
          },
        );
      }
    }
    let devices: Vec<DeviceId> = self
      .devices
      .iter()
      .filter(|(_, device)| device.user == user)
      .map(|(id, _)| *id)
      .collect();
    for device in devices {
      self.devices.remove(&device);
      self.key_packages.remove(&device);
      self.publish(
        co_members.iter().copied(),
        &GatewayEvent::DeviceRemove {
          user_id: user,
          device_id: device,
        },
      );
    }
    self.identity_keys.remove(&user);
    self.key_bags.remove(&user);
    self.admins.remove(&user);
    if let Some(summary) = self.users.get_mut(&user) {
      summary.is_deactivated = true;
      summary.is_deleted = true;
      summary.display_name = "Deleted user".to_owned();
      summary.maybe_username = None;
    }
    let everyone: Vec<UserId> = self.users.keys().copied().collect();
    self.publish(everyone, &GatewayEvent::ProfileUpdate { user_id: user });
  }

  /// Takes `device` out of the conversation's call roster; the event members should hear, if the
  /// roster changed.
  fn leave_call(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
  ) -> Option<GatewayEvent> {
    let entry = CallRosterEntry {
      user_id: user,
      device_id: device,
    };
    let roster = self.call_rosters.get_mut(&conversation)?;
    let position = roster.participants.iter().position(|known| *known == entry)?;
    roster.participants.remove(position);
    let call_id = roster.call_id;
    if roster.participants.is_empty() {
      self.call_rosters.remove(&conversation);
      self.calls.remove(&conversation);
      return Some(GatewayEvent::CallEnd {
        conversation_id: conversation,
        call_id,
        cause: teamcraft_proto::CallEndCause::LastLeft,
      });
    }
    Some(GatewayEvent::CallUpdate {
      conversation_id: conversation,
      call_id,
      is_ringing: roster.is_ringing,
      participants: roster.participants.clone(),
    })
  }

  /// The members of a conversation (none for an unknown one).
  fn member_ids(&self, conversation: ConversationId) -> Vec<UserId> {
    self
      .conversations
      .get(&conversation)
      .map(|entry| entry.members.keys().copied().collect())
      .unwrap_or_default()
  }

  fn publish(&mut self, users: impl IntoIterator<Item = UserId>, event: &GatewayEvent) {
    let users: HashSet<UserId> = users.into_iter().collect();
    let silent = &self.silent_gateway_users;
    let frozen = &self.frozen_connections;
    for (id, session) in self.sessions.iter_mut() {
      if !users.contains(&session.user) || id.starts_with(PENDING_PREFIX) {
        continue;
      }
      session.seq += 1;
      session.buffer.push((session.seq, event.clone()));
      let is_frozen = session
        .maybe_live
        .as_ref()
        .is_some_and(|(connection, _)| frozen.contains(connection));
      if silent.contains(&session.user) || is_frozen {
        continue;
      }
      if let Some((_, sender)) = &session.maybe_live
        && sender
          .unbounded_send(GatewayServerMessage::Event {
            seq: session.seq,
            event: event.clone(),
          })
          .is_err()
      {
        session.maybe_live = None;
      }
    }
  }

  fn live_sender(&self, connection: u64) -> Option<UnboundedSender<GatewayServerMessage>> {
    self
      .sessions
      .values()
      .find_map(|session| match &session.maybe_live {
        Some((live, sender)) if *live == connection => Some(sender.clone()),
        _ => None,
      })
  }
}

impl Conversation {
  fn view(&self, me: UserId) -> ConversationResponse {
    let mut response = self.response.clone();
    response.maybe_my_membership = self.members.get(&me).map(|(role, _)| MyMembership {
      role: role.clone(),
      notification_level: NotificationLevel::All,
      is_favorite: self.favorites.get(&me).copied().unwrap_or(false),
      maybe_last_read_message_id: self.read_positions.get(&me).copied(),
      maybe_muted_until: None,
      is_notification_inherited: false,
    });
    response
  }
}

impl State {
  /// The 503 the server gives `user` while [`FakeServer::set_unavailable`] holds.
  fn unavailable_answer(&self, user: UserId) -> Option<ApiError> {
    let retry_after_secs = *self.unavailable_users.get(&user)?;
    Some(ApiError::Status {
      status: 503,
      maybe_body: Some(
        ErrorBody::new(ErrorCode::Unavailable, "fake server: a dependency is down")
          .with_retry_after_secs(retry_after_secs),
      ),
      // The server sends it as a header too.
      maybe_retry_after: Some(std::time::Duration::from_secs(retry_after_secs)),
    })
  }
}

impl KeyPackages {
  /// Like the server: packages expiring within the renewal window are "expiring soon".
  fn status(&self, now_ms: i64) -> KeyPackageStatusResponse {
    let renew_before = now_ms.saturating_add(KEY_PACKAGE_RENEWAL_WINDOW_MS);
    let count = |is_expiring: bool| {
      let matching = self
        .regular
        .iter()
        .filter(|package| package.expires_at_ms > now_ms)
        .filter(|package| (package.expires_at_ms <= renew_before) == is_expiring)
        .count();
      u32::try_from(matching).unwrap_or(u32::MAX)
    };
    KeyPackageStatusResponse {
      available: count(false),
      expiring_soon: count(true),
      has_last_resort: self
        .maybe_last_resort
        .as_ref()
        .is_some_and(|package| package.expires_at_ms > renew_before),
    }
  }
}

/// An MLS entry with its payload broken, as one client gets it ([`FakeServer::garble_next_commit_for`]).
fn garble(mut message: MlsMessageResponse) -> MlsMessageResponse {
  let mut payload = message.payload.into_vec();
  if let Some(last) = payload.last_mut() {
    *last ^= 0xff;
  }
  message.payload = Base64Bytes::new(payload);
  message
}

/// Checks an uploaded package like the server and records its expiry on the fake's clock: the
/// validity it has left now on the system clock, from `now_ms`.
fn stored_key_package(
  bytes: &Base64Bytes,
  is_last_resort: bool,
  now_ms: i64,
) -> Result<StoredKeyPackage, ApiError> {
  let invalid = |message: String| ApiError::status(400, ErrorCode::InvalidKeyPackage, message);
  let info = teamcraft_crypto::mls::inspect_key_package(bytes.as_slice())
    .map_err(|error| invalid(error.to_string()))?;
  if info.is_last_resort != is_last_resort {
    return Err(invalid("the last_resort extension does not match".into()));
  }
  let not_after_ms = i64::try_from(info.not_after_unix_secs)
    .unwrap_or(i64::MAX)
    .saturating_mul(1000);
  let left_ms = not_after_ms.saturating_sub(SystemClock.now_ms());
  if left_ms <= KEY_PACKAGE_MIN_VALIDITY_MS {
    return Err(invalid("the key package expires too soon".into()));
  }
  Ok(StoredKeyPackage {
    bytes: bytes.clone(),
    expires_at_ms: now_ms.saturating_add(left_ms),
  })
}

/// An admin's request to join, as the fake server keeps it.
#[derive(Clone, Debug)]
struct FakeJoinRequest {
  requested_at: i64,
  state: FakeJoinState,
  maybe_decided_by: Option<UserId>,
  /// Made up by a "malicious" server for these users ([`FakeServer::plant_admin_join_request`]).
  planted_for: HashSet<UserId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FakeJoinState {
  Open,
  Approved,
  Declined,
  Withdrawn,
}

impl FakeJoinRequest {
  fn open(requested_at: i64) -> Self {
    Self {
      requested_at,
      state: FakeJoinState::Open,
      maybe_decided_by: None,
      planted_for: HashSet::new(),
    }
  }
}

const PENDING_PREFIX: &str = "pending-";
const BLOB_SCHEME: &str = "fake-blob://";
/// Presigned part URLs: `fake-part://{attachment}/{part number}/{size}`.
const PART_SCHEME: &str = "fake-part://";
/// Like S3's 5 MiB, scaled down so tests stay small.
pub const FAKE_MIN_PART_BYTES: u64 = 1024;
const BACKGROUND_SCHEME: &str = "fake-background://";
const ORG_BACKGROUND_SCHEME: &str = "fake-org-background://";

fn org_background_blob<'a>(state: &'a mut State, raw: &str) -> Result<&'a mut FakeBackground, ApiError> {
  raw
    .parse::<i64>()
    .ok()
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(OrgBackgroundId::from_snowflake)
    .and_then(|id| state.org_backgrounds.get_mut(&id))
    .ok_or(ApiError::Status {
      status: 404,
      maybe_body: None,
      maybe_retry_after: None,
    })
}

/// The caller's own background; others' look missing, like on the server.
fn own_background(
  state: &mut State,
  me: UserId,
  background: BackgroundId,
) -> Result<&mut FakeBackground, ApiError> {
  state
    .backgrounds
    .get_mut(&background)
    .filter(|entry| entry.owner == me)
    .ok_or_else(|| ApiError::status(404, ErrorCode::BackgroundNotFound, "unknown"))
}

/// A background's object, by the presigned URL's ID (the URL is the credential).
fn background_blob<'a>(state: &'a mut State, raw: &str) -> Result<&'a mut FakeBackground, ApiError> {
  raw
    .parse::<i64>()
    .ok()
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(BackgroundId::from_snowflake)
    .and_then(|id| state.backgrounds.get_mut(&id))
    .ok_or(ApiError::Status {
      status: 404,
      maybe_body: None,
      maybe_retry_after: None,
    })
}

/// Stores a part sent to a fake presigned part URL; answers its `ETag`.
fn store_fake_part(state: &mut State, url: &str, bytes: Vec<u8>) -> Result<String, ApiError> {
  let refused = || ApiError::Status {
    status: 404,
    maybe_body: None,
    maybe_retry_after: None,
  };
  let mut fields = url.strip_prefix(PART_SCHEME).ok_or_else(refused)?.split('/');
  let (Some(id), Some(number), Some(size)) = (fields.next(), fields.next(), fields.next()) else {
    return Err(refused());
  };
  let id = id
    .parse::<i64>()
    .ok()
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(AttachmentId::from_snowflake)
    .ok_or_else(refused)?;
  let number: u16 = number.parse().map_err(|_| refused())?;
  if size.parse::<u64>().ok() != Some(bytes.len() as u64) {
    return Err(ApiError::Status {
      status: 403,
      maybe_body: None,
      maybe_retry_after: None,
    });
  }
  let is_discarding = state.is_discarding_parts;
  let parts = state
    .attachments
    .get_mut(&id)
    .and_then(|entry| entry.maybe_parts.as_mut())
    .ok_or_else(refused)?;
  let checksum = bytes.iter().fold(0u64, |sum, &byte| {
    sum.wrapping_mul(31).wrapping_add(u64::from(byte))
  });
  let etag = format!("\"{number}-{checksum:x}\"");
  parts.insert(
    number,
    FakePart {
      etag: etag.clone(),
      size_bytes: bytes.len() as u64,
      maybe_bytes: (!is_discarding).then_some(bytes),
    },
  );
  state.part_uploads += 1;
  Ok(etag)
}

/// The blob the listed parts make, like S3's CompleteMultipartUpload: every part listed with its
/// current `ETag`, numbered from 1 without gaps, of the announced size together. `None` for parts
/// the server discarded.
fn join_fake_parts(
  parts: &BTreeMap<u16, FakePart>,
  request: &CompleteMultipartAttachmentRequest,
  size_bytes: u64,
) -> Result<Option<Vec<u8>>, ApiError> {
  let invalid = |message: &'static str| ApiError::status(409, ErrorCode::Conflict, message);
  let mut blob = Some(Vec::new());
  let mut total = 0;
  for (index, listed) in request.parts.iter().enumerate() {
    let part = parts
      .get(&listed.part_number)
      .filter(|part| usize::from(listed.part_number) == index + 1 && part.etag == listed.etag)
      .ok_or_else(|| invalid("a part is missing or out of order"))?;
    total += part.size_bytes;
    blob = match (blob, &part.maybe_bytes) {
      (Some(mut joined), Some(bytes)) => {
        joined.extend_from_slice(bytes);
        Some(joined)
      },
      _ => None,
    };
  }
  if total != size_bytes {
    return Err(invalid("the parts don't add up to the announced size"));
  }
  Ok(blob)
}

/// The attachment behind a fake presigned URL.
fn blob_entry<'a>(state: &'a mut State, url: &str) -> Result<&'a mut FakeAttachment, ApiError> {
  let id = url
    .strip_prefix(BLOB_SCHEME)
    .and_then(|raw| raw.parse::<i64>().ok())
    .and_then(|raw| Snowflake::new(raw).ok())
    .map(AttachmentId::from_snowflake)
    .ok_or(ApiError::Status {
      status: 404,
      maybe_body: None,
      maybe_retry_after: None,
    })?;
  state.attachments.get_mut(&id).ok_or(ApiError::Status {
    status: 404,
    maybe_body: None,
    maybe_retry_after: None,
  })
}

fn pending_session(connection: u64) -> String {
  format!("{PENDING_PREFIX}{connection}")
}

fn fake_digest(answer: &impl serde::Serialize) -> String {
  teamcraft_crypto::change_digest::change_digest(&serde_json::to_vec(answer).unwrap_or_default())
}

fn page(
  mut messages: Vec<MlsMessageResponse>,
  maybe_limit: Option<u32>,
  current_epoch: u64,
) -> ListMlsMessagesResponse {
  let limit = maybe_limit
    .map_or(PAGE_SIZE, |limit| limit as usize)
    .clamp(1, PAGE_SIZE);
  let has_more = messages.len() > limit;
  messages.truncate(limit);
  ListMlsMessagesResponse {
    messages,
    has_more,
    current_epoch,
  }
}

fn snowflake(raw: i64) -> Snowflake {
  // Invariant: the counter starts at 1 and only grows, so it is always a valid snowflake.
  Snowflake::new(raw).unwrap_or_else(|_| unreachable!("positive counter"))
}

impl Conversation {
  /// Threads `user` follows (started or replied to), with their unread replies.
  fn threads(&self, user: UserId) -> Vec<ThreadSummary> {
    let mut by_thread: BTreeMap<ThreadId, Vec<&MessageResponse>> = BTreeMap::new();
    for message in &self.messages {
      if let Some(thread) = message.maybe_thread_id {
        by_thread.entry(thread).or_default().push(message);
      }
    }
    by_thread
      .into_iter()
      .filter_map(|(thread, replies)| {
        let is_starter = self
          .messages
          .iter()
          .any(|message| is_root(message, thread) && message.sender_user_id == user);
        let has_replied = replies.iter().any(|reply| reply.sender_user_id == user);
        let maybe_read = self.thread_read_positions.get(&(user, thread)).copied();
        let follow_reason = match (is_starter, has_replied) {
          (true, _) => ThreadFollowReason::Started,
          (false, true) => ThreadFollowReason::Replied,
          (false, false) => return None,
        };
        Some(ThreadSummary {
          conversation_id: self.response.id,
          thread_id: thread,
          follow_reason,
          reply_count: u32::try_from(replies.len()).unwrap_or(u32::MAX),
          unread_count: u32::try_from(
            replies
              .iter()
              .filter(|reply| reply.sender_user_id != user && maybe_read.is_none_or(|read| reply.id > read))
              .count(),
          )
          .unwrap_or(u32::MAX),
          mention_count: 0,
          maybe_last_reply: replies
            .iter()
            .max_by_key(|reply| reply.id)
            .map(|reply| (*reply).clone()),
        })
      })
      .collect()
  }
}

fn is_root(message: &MessageResponse, thread: ThreadId) -> bool {
  message.id.snowflake() == thread.snowflake()
}

fn privacy_of(state: &State, user: UserId) -> teamcraft_proto::PrivacyResponse {
  teamcraft_proto::PrivacyResponse {
    last_seen: if state.hidden_last_seen.contains(&user) {
      teamcraft_proto::LastSeenVisibility::Nobody
    } else {
      teamcraft_proto::LastSeenVisibility::Everyone
    },
    is_last_seen_available: true,
  }
}

fn default_notification_settings() -> NotificationSettings {
  NotificationSettings {
    auto_follow_mentioned_threads: true,
    push_mode: PushMode::AwayOnly,
    send_read_receipts: true,
    maybe_dnd_schedule: None,
    maybe_uses_desktop_attention: None,
    maybe_level: None,
    maybe_default_level: None,
  }
}
