//! An in-process stand-in for `teamcraft-server`, for tests of the sync engine.
//!
//! It mirrors the server's observable behaviour that the client depends on: snowflake IDs in
//! creation order, membership checks, atomic key package claims, the MLS queue with "first commit
//! per epoch wins", welcomes per device, key bag compare-and-swap, and gateway sessions with
//! sequence numbers and resume. It never looks inside payloads, like the real server. Tests can take
//! a client offline or drop its gateway to exercise retries and resume.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt;
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ChannelModeration, ChannelRole, ClaimKeyPackagesRequest,
  ClaimKeyPackagesResponse, ClaimedKeyPackage, ConversationId, ConversationKind, ConversationMemberResponse,
  ConversationResponse, ConversationVisibility, CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest,
  DeviceId, DeviceResponse, ErrorCode, GatewayClientMessage, GatewayEvent, GatewayServerMessage,
  GetMeResponse, HistoryPolicy, Id, IdentityKeyResponse, JoinPolicy, KeyBagResponse,
  KeyPackageStatusResponse, ListConversationMembersResponse, ListConversationsResponse, ListDevicesResponse,
  ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse,
  ListSpaceMembersResponse, ListSpacesResponse, MessageId, MessageResponse, MlsMessageKind,
  MlsMessageResponse, MoveChannelRequest, MyMembership, NotificationLevel, PostMlsCommitRequest,
  PostMlsCommitResponse, PostMlsMessageRequest, PutKeyBagRequest, RegisterDeviceRequest, Role,
  SendMessageRequest, SetPresenceRequest, SetReadPositionRequest, Snowflake, SpaceId, SpaceMemberResponse,
  SpaceResponse, SpaceRole, UnixMillis, UpdateConversationMemberRequest, UploadIdentityKeyRequest,
  UploadKeyPackagesRequest, UserId, UserKind, UserSummary,
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
use teamcraft_proto::{CallId, CallJoinTokenResponse, CreateCallJoinTokenRequest};
use teamcraft_proto::{
  CategoryId, CategoryResponse, CreateCategoryRequest, CreateInviteRequest, CreateSpaceRequest,
  InviteResponse, ListCategoriesResponse, UpdateCategoryRequest, UpdateConversationRequest,
  UpdateSpaceRequest,
};
use teamcraft_proto::{DeviceListResponse, PutDeviceListRequest};
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
const HEARTBEAT_INTERVAL_MS: u64 = 30_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// The server's defaults: key packages are handed out while they stay valid this long, and count as
/// "expiring soon" within the renewal window (e2ee.md §4.3).
const KEY_PACKAGE_MIN_VALIDITY_MS: i64 = 42 * DAY_MS;
const KEY_PACKAGE_RENEWAL_WINDOW_MS: i64 = 56 * DAY_MS;

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
  /// Users whose REST calls fail while their gateway still connects.
  rest_failing_users: HashSet<UserId>,
  /// Users whose access token the server rejects (401), as after an expired session.
  signed_out_users: HashSet<UserId>,
  /// Heartbeats received, to check that a busy client keeps sending them.
  heartbeats: u64,
  /// A malicious server: the next registered device gets this ID instead of a fresh one.
  maybe_forced_device_id: Option<DeviceId>,
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
  attachments: HashMap<AttachmentId, FakeAttachment>,
  push_registrations: HashMap<DeviceId, RegisterPushRequest>,
  notification_settings: HashMap<UserId, NotificationSettings>,
  /// What `GET /push/gateway` answers (`None`: the server sends to APNs and FCM itself).
  push_gateway: Option<PushGatewayBinding>,
  /// Sealed tokens that were revoked at the (fake) gateway.
  revoked_gateway_tokens: Vec<Vec<u8>>,
  revoked_gateway_urls: Vec<String>,
  relays: HashMap<String, Relay>,
  /// Relayed binary frames still to be lost on the way (a flaky connection during linking).
  relay_frames_to_lose: u32,
  roster_log: Vec<WorkspaceLogEntryResponse>,
  policy_log: Vec<WorkspaceLogEntryResponse>,
  /// Larger attachments are refused like the server's `max_attachment_bytes`; `None`: no limit.
  maybe_max_attachment_bytes: Option<u64>,
  share_links: HashMap<String, AttachmentId>,
  backgrounds: HashMap<BackgroundId, FakeBackground>,
  org_backgrounds: HashMap<OrgBackgroundId, FakeBackground>,
  /// Spaces besides the default one, and changes to the default one.
  spaces: BTreeMap<SpaceId, SpaceResponse>,
  categories: BTreeMap<CategoryId, CategoryResponse>,
  invites: Vec<InviteResponse>,
  /// Calls served by `list_call_history` to everybody who took part (newest first).
  call_history: Vec<teamcraft_proto::CallHistoryEntry>,
  /// When people were last active, and who chose to hide it.
  last_seen: HashMap<UserId, i64>,
  hidden_last_seen: HashSet<UserId>,
  /// How far the server's clock runs ahead of the system clock ([`FakeServer::advance_clock`]).
  clock_offset_ms: i64,
  /// Revoked devices stay listed with their key packages, as a stale or malicious server would.
  is_ignoring_revocations: bool,
  /// The next commit of these users is stored and fanned out, but the answer is lost.
  losing_commit_answers: HashSet<UserId>,
  /// The next commit of these users is lost on the way (never stored).
  dropping_commits: HashSet<UserId>,
  /// These users get the next commit (of anyone) garbled, so their client refuses it.
  garbling_next_commit_for: HashSet<UserId>,
  /// MLS entries each user gets garbled (gateway and backlog alike).
  garbled_entries: HashMap<UserId, HashSet<teamcraft_proto::MlsMessageId>>,
  /// Group information served whatever the group's epoch ([`FakeServer::serve_group_info`]).
  forced_group_infos: HashMap<ConversationId, teamcraft_proto::MlsGroupInfoResponse>,
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
  pinned: HashSet<MessageId>,
  /// The newest MLS group information and the epoch it is for.
  maybe_group_info: Option<(u64, Base64Bytes)>,
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
      },
    );
    drop(state);
    FakeServerApi {
      server: self.clone(),
      user: id,
    }
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

  /// How many gateway connections were opened so far.
  pub fn gateway_connect_count(&self) -> u64 {
    self.lock().gateway_connects
  }

  /// How many gateway heartbeats arrived so far.
  pub fn heartbeat_count(&self) -> u64 {
    self.lock().heartbeats
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

  /// The next `count` binary frames sent over any relay are lost.
  pub fn lose_relay_frames(&self, count: u32) {
    self.lock().relay_frames_to_lose = count;
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

  /// The user's next commit is stored and fanned out, but the answer to the client is lost (a
  /// timeout or a killed app right after sending).
  pub fn lose_next_commit_answer(&self, user: UserId) {
    self.lock().losing_commit_answers.insert(user);
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

  /// Refuses attachments larger than `limit` bytes, like the server's `max_attachment_bytes`.
  pub fn set_max_attachment_bytes(&self, limit: u64) {
    self.lock().maybe_max_attachment_bytes = Some(limit);
  }

  /// What the public download page of a share link serves: the ciphertext, no account needed.
  pub fn shared_blob(&self, share_id: &str) -> Option<Vec<u8>> {
    let state = self.lock();
    let attachment = state.share_links.get(share_id)?;
    state.attachments.get(attachment)?.maybe_blob.clone()
  }

  /// The push registration of a device (to seal wake-ups in tests).
  pub fn push_registration(&self, device: DeviceId) -> Option<RegisterPushRequest> {
    self.lock().push_registrations.get(&device).cloned()
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
    if state.offline_users.contains(&self.user) || state.rest_failing_users.contains(&self.user) {
      return Err(ApiError::Network("fake server: client is offline".into()));
    }
    if state.signed_out_users.contains(&self.user) {
      return Err(ApiError::status(401, ErrorCode::Unauthorized, "token expired"));
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
    self.with(|state, _| state.user(user).cloned())
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
    self.with(|state, _| {
      Ok(ListDevicesResponse {
        devices: state
          .devices_of(user)
          .map(|device| device.response.clone())
          .collect(),
      })
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

  async fn identity_key(&self, user: UserId) -> Result<IdentityKeyResponse, ApiError> {
    self.with(|state, _| {
      state
        .identity_keys
        .get(&user)
        .cloned()
        .ok_or_else(|| ApiError::status(404, ErrorCode::IdentityKeyNotFound, "no key"))
    })
  }

  async fn device_list(&self, user: UserId) -> Result<DeviceListResponse, ApiError> {
    self.with(|state, _| {
      state
        .device_lists
        .get(&user)
        .cloned()
        .ok_or_else(|| ApiError::status(404, ErrorCode::DeviceNotFound, "no device list"))
    })
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
    self.with(|state, me| {
      if state.devices.get(&device).is_none_or(|entry| entry.user != me) {
        return Err(ApiError::status(404, ErrorCode::DeviceNotFound, "no such device"));
      }
      state.revoke_device(me, device);
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
      Ok(packages.status(now))
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

  /// Every known user is in every fake space.
  async fn list_space_members(&self, space: SpaceId) -> Result<ListSpaceMembersResponse, ApiError> {
    self.with(|state, _| {
      state.default_space_entry();
      if !state.spaces.contains_key(&space) {
        return Err(ApiError::status(404, ErrorCode::SpaceNotFound, "no such space"));
      }
      let members = state
        .users
        .values()
        .map(|user| SpaceMemberResponse {
          user: user.clone(),
          role: SpaceRole::Member,
          maybe_expires_at: None,
          maybe_sponsor_user_id: None,
        })
        .collect();
      Ok(ListSpaceMembersResponse { members })
    })
  }

  async fn list_conversations(&self) -> Result<ListConversationsResponse, ApiError> {
    self.with(|state, me| {
      let conversations = state
        .conversations
        .values()
        .filter(|conversation| conversation.members.contains_key(&me))
        .map(|conversation| conversation.view(me))
        .collect();
      Ok(ListConversationsResponse { conversations })
    })
  }

  async fn conversation(&self, conversation: ConversationId) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| Ok(state.member_conversation(conversation, me)?.view(me)))
  }

  async fn list_members(
    &self,
    conversation: ConversationId,
  ) -> Result<ListConversationMembersResponse, ApiError> {
    self.with(|state, me| {
      let members = state
        .member_conversation(conversation, me)?
        .members
        .iter()
        .map(|(user, (role, _))| (*user, role.clone()))
        .collect::<Vec<_>>();
      let members = members
        .into_iter()
        .map(|(user, role)| {
          Ok(ConversationMemberResponse {
            user: state.user(user)?.clone(),
            role,
          })
        })
        .collect::<Result<_, ApiError>>()?;
      Ok(ListConversationMembersResponse { members })
    })
  }

  async fn create_channel(
    &self,
    space: SpaceId,
    request: &CreateChannelRequest,
  ) -> Result<ConversationResponse, ApiError> {
    self.with(|state, me| {
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
      entry.response.visibility = request.visibility.clone();
      entry.response.maybe_topic = request.maybe_topic.clone();
      entry.response.maybe_category_id = request.maybe_category_id;
      state.publish([me], &GatewayEvent::ConversationUpdate { conversation_id: id });
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
      });
      if let Some(conversation) = existing {
        return Ok(conversation.view(me));
      }
      let id = state.create_conversation(
        ConversationKind::Dm,
        None,
        None,
        HistoryPolicy::Full,
        &[me, request.user_id],
      );
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

  async fn remove_member(&self, conversation: ConversationId, user: UserId) -> Result<(), ApiError> {
    self.with(|state, me| {
      // Server admins may remove people from conversations they aren't in.
      let entry = if state.admins.contains(&me) {
        state.conversation_mut(conversation)?
      } else {
        state.member_conversation_mut(conversation, me)?
      };
      let mut recipients: Vec<UserId> = entry.members.keys().copied().collect();
      if entry.members.remove(&user).is_none() {
        return Err(ApiError::status(404, ErrorCode::UserNotFound, "not a member"));
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
      state
        .member_conversation_mut(conversation, me)?
        .read_positions
        .insert(me, request.message_id);
      Ok(())
    })
  }

  async fn set_presence(&self, _request: &SetPresenceRequest) -> Result<(), ApiError> {
    self.with(|_, _| Ok(()))
  }

  async fn list_presence(&self, users: &[UserId]) -> Result<teamcraft_proto::ListPresenceResponse, ApiError> {
    self.with(|state, _| {
      Ok(teamcraft_proto::ListPresenceResponse {
        presences: users
          .iter()
          .map(|user| teamcraft_proto::UserPresence {
            user_id: *user,
            state: teamcraft_proto::PresenceState::Offline,
            maybe_status: None,
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
      Ok(CallJoinTokenResponse {
        call_id: CallId::from_snowflake(conversation.snowflake()),
        token: format!("fake:{conversation}"),
        sfu_url: "https://sfu.invalid/call".to_owned(),
        expires_at: UnixMillis(0),
      })
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
      let id = state.next_id();
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

  async fn list_mls_messages(
    &self,
    conversation: ConversationId,
    query: &ListMlsMessagesQuery,
  ) -> Result<ListMlsMessagesResponse, ApiError> {
    self.with(|state, me| {
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
      Ok(ListConversationsResponse { conversations })
    })
  }

  async fn create_space(&self, request: &CreateSpaceRequest) -> Result<SpaceResponse, ApiError> {
    self.with(|state, _| {
      state.default_space_entry();
      let id: SpaceId = state.next_id();
      let space = SpaceResponse {
        id,
        name: request.name.clone(),
        maybe_icon: request.maybe_icon.clone(),
        maybe_color: request.maybe_color.clone(),
        maybe_description: request.maybe_description.clone(),
        join_policy: request.join_policy.clone(),
        is_archived: false,
        maybe_brand_id: None,
        maybe_my_role: Some(SpaceRole::SpaceAdmin),
        maybe_my_notifications: None,
      };
      state.spaces.insert(id, space.clone());
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
      Ok(invite)
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

  /// The fake doesn't track read positions; nothing is ever unread.
  async fn unread(&self) -> Result<UnreadResponse, ApiError> {
    self.with(|_, _| {
      Ok(UnreadResponse {
        conversations: Vec::new(),
        unread_thread_count: 0,
        thread_mention_count: 0,
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

  async fn put_blob(&self, url: &str, bytes: Vec<u8>) -> Result<(), ApiError> {
    if let Some(raw) = url.strip_prefix(ORG_BACKGROUND_SCHEME) {
      return self.with(|state, _| {
        let entry = org_background_blob(state, raw)?;
        if bytes.len() as u64 != entry.size_bytes || entry.is_complete {
          return Err(ApiError::Status {
            status: 403,
            maybe_body: None,
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
          });
        }
        entry.maybe_blob = Some(bytes);
        Ok(())
      });
    }
    self.with(|state, _| {
      let entry = blob_entry(state, url)?;
      if bytes.len() as u64 != entry.size_bytes || entry.is_complete {
        return Err(ApiError::Status {
          status: 403,
          maybe_body: None,
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
      GatewayClientMessage::Typing { conversation_id } => {
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
          },
        );
      },
      GatewayClientMessage::SetActivity { .. }
      | GatewayClientMessage::Attention { .. }
      | GatewayClientMessage::Unknown => {},
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
      join_policy: JoinPolicy::Open,
      is_archived: false,
      maybe_brand_id: None,
      maybe_my_role: Some(SpaceRole::Member),
      maybe_my_notifications: None,
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
      maybe_my_membership: None,
      participants: Vec::new(),
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
        pinned: HashSet::new(),
        maybe_group_info: None,
      },
    );
    id
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

const PENDING_PREFIX: &str = "pending-";
const BLOB_SCHEME: &str = "fake-blob://";
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
    })
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
    })?;
  state.attachments.get_mut(&id).ok_or(ApiError::Status {
    status: 404,
    maybe_body: None,
  })
}

fn pending_session(connection: u64) -> String {
  format!("{PENDING_PREFIX}{connection}")
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
  /// Threads `user` follows (started or replied to), without read positions.
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
          unread_count: 0,
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
  }
}
