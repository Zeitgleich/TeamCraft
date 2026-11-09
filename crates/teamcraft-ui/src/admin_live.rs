//! The administration console on the real admin API (docs/admin.md): channels, users and devices,
//! the audit log and the workspace policy, all as metadata. Reads are cached after a refresh, so the
//! synchronous [`crate::backend::AdminBackend`] can answer from them; every change goes to the
//! server and then triggers a refresh. The signed policy is the exception: only a client with the
//! owner's keys (the desktop core) can sign it, so it is handed to the chat backend.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use futures_util::future::{join, join5};
use futures_util::stream::{self, StreamExt};
use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use teamcraft_proto::{
  AdminBrandsResponse, AdminCalendarSettingsResponse, AdminConversationSummary, AdminDeviceResponse,
  AdminHealthResponse, AdminInstanceResponse, AdminMailSettingsResponse, AdminOidcSyncPreviewResponse,
  AdminOidcSyncResponse, AdminSpaceSummary, AdminUserSummary, AnnouncementResponse,
  AssignChannelOwnerRequest, AuditEntryResponse, BrandAssetSlot, ChannelModeration, ConversationVisibility,
  CreateBrandRequest, ErrorBody, ErrorCode, ExtendGuestRequest, GetAnnouncementResponse, HistoryPolicy,
  ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminGuestsQuery, ListAdminGuestsResponse,
  ListAdminSpacesResponse, ListAdminUsersResponse, ListAuditResponse, ListConversationMembersResponse,
  MAX_ADMIN_IDS, PermissionSchemeResponse, RetentionSetting, RetentionStatusResponse, Role,
  RuntimeSettingsKey, RuntimeSettingsResponse, RuntimeSettingsSection, SchemeChange, SendTestMailRequest,
  SetAnnouncementRequest, SetChannelModerationRequest, SetRetentionRequest, SettingsTestResponse, SpaceId,
  UnixMillis, UpdateBrandRequest, UpdateCalendarSettingsRequest, UpdateConversationRequest,
  UpdateMailSettingsRequest, UpdatePermissionSchemeRequest, UpdateSpaceRequest, UserKind, WorkspacePolicy,
  WorkspacePolicyResponse,
};

use crate::admin::{
  AdminChannel, AdminChannelFilter, AdminChannelQuery, AdminLoad, AdminSection, AdminSpace,
  AdminSpaceChanges, AdminUser, AdminUserQuery, AuditAction, AuditEntry, AuditFilter, DeviceInfo,
  DeviceVerification, INACTIVE_AFTER_DAYS, RATE_LIMITED_CODE, READ_FAILED_CODE, Retention, SettingsTestKind,
  UNREACHABLE_CODE,
};
use crate::auth::{ApiMethod, Authenticator, BinaryBody};
use crate::clock::now_ms;
use crate::model::{
  ConversationId, ConversationKind, CustomStatus, PresenceStatus, User, UserId, id_from_raw, raw_id,
};
use crate::server_backend::hue_from_seed;

const REFRESH_DELAY: Duration = Duration::from_millis(500);
const HEALTH_PATH: &str = "/api/v1/admin/health";
const OIDC_SYNC_PATH: &str = "/api/v1/admin/oidc-sync";
const LIST_LIMIT: u32 = 200;
/// Guest memberships the overview shows at once; the filters narrow a longer list, and the console
/// says that it is cut.
const GUEST_LIST_LIMIT: u32 = 200;
/// Channels (and users) per page of the Channels, Retention and Users lists. The first page loads
/// when the page opens, the next ones when the admin asks for more (issue #1241).
const PAGE_SIZE: u32 = 100;
/// Pages the timer and a refresh read again at most: the head of what the admin loaded. Pages
/// loaded beyond stay as they were read, so the cost of a refresh depends on this and not on how
/// far the admin scrolled, still less on the size of the workspace.
const MAX_REFRESH_PAGES: usize = 3;
/// Pages of owners (and of admins) read for the roster proposals: nobody is left out of them
/// silently, and their number is far below this in practice.
const MAX_ROLE_PAGES: usize = 5;
/// The search box asks after its own debounce; the read follows at once.
const QUERY_DELAY: Duration = Duration::ZERO;
const MORE_CHANNELS_KEY: &str = "more:channels";
const MORE_USERS_KEY: &str = "more:users";
/// Servers from before the list carried owner and retention (`maybe_retention` absent): only this
/// many channels get them, one request each, so an old server isn't flooded.
const LEGACY_DETAIL_LIMIT: usize = 100;
/// Requests in flight at once when the details of many channels are read from such a server.
const DETAIL_CONCURRENCY: usize = 8;
/// How long the per-channel details read from such a server stay as they are before the next
/// look: on the pages that show them, not on every refresh.
const LEGACY_DETAIL_TTL_MS: i64 = 10 * 60 * 1000;
/// The shortest and the longest the console stays quiet after the server refused it with 429.
const MIN_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 120;
/// Without a `retry_after_secs` in the refusal.
const DEFAULT_BACKOFF_SECS: u64 = 30;
/// Signing and publishing happens in the core; the server shows the result after this long.
const PUBLISH_SETTLE: Duration = Duration::from_millis(1500);

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct AdminLive {
  auth: Rc<dyn Authenticator>,
  data: Rc<RefCell<AdminData>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  refresh_queued: Rc<Cell<bool>>,
  /// A refresh is running; another request meanwhile makes it run once more, never in parallel.
  is_refreshing: Rc<Cell<bool>>,
  is_refresh_wanted_again: Rc<Cell<bool>>,
  /// What the next refresh reads: the union of what was asked for since the last one.
  wanted: Rc<Cell<Reads>>,
  /// The server refused with 429; nothing is read before this time (ms since the epoch).
  quiet_until: Rc<Cell<i64>>,
  /// The console has read everything once this session; from then on opening a section reads that
  /// section's data only.
  has_read_all: Rc<Cell<bool>>,
  /// The section the console showed last: a health event refreshes the open health page, and only
  /// counts the alerts otherwise.
  maybe_open_section: Rc<Cell<Option<AdminSection>>>,
  /// How many health checks were alerting at the last read, for the badges. Read without the
  /// console's chunk: a number out of one small answer.
  open_alert_count: Rc<Cell<usize>>,
  is_health_count_queued: Rc<Cell<bool>>,
}

/// What the last refresh returned.
#[derive(Default)]
pub struct AdminData {
  load: AdminLoad,
  /// The pages of channels loaded so far for [`AdminData::channel_query`].
  channels: Vec<AdminChannel>,
  channel_paging: Paging,
  channel_query: AdminChannelQuery,
  /// The pages of users loaded so far for [`AdminData::user_query`].
  users: Vec<AdminUser>,
  user_paging: Paging,
  user_query: AdminUserQuery,
  /// Owners and admins, read by role: the roster proposals and the names of those who act in the
  /// audit log, whatever page of the user list is open.
  role_holders: Vec<AdminUser>,
  /// Channels and accounts that notices refer to (unconfirmed removals, people left in a group)
  /// and that the loaded pages don't hold, read by ID (issue #1433).
  referenced_channels: HashMap<ConversationId, AdminChannel>,
  referenced_users: HashMap<UserId, AdminUser>,
  /// What was asked by ID already, found or not: a notice about a deleted channel asks once.
  asked_channels: HashSet<ConversationId>,
  asked_users: HashSet<UserId>,
  /// Devices of the users whose details were opened; refreshed with the console.
  devices: HashMap<UserId, Vec<AdminDeviceResponse>>,
  /// Owner and retention of channels, read one by one from a server that doesn't send them in the
  /// list, with the time they were read.
  legacy_details: HashMap<ConversationId, (i64, ChannelDetails)>,
  audit: Vec<AuditEntry>,
  policy: WorkspacePolicy,
  policy_version: u64,
  spaces: Vec<AdminSpace>,
  announcement: Option<AnnouncementResponse>,
  instance: Option<AdminInstanceResponse>,
  health: Option<AdminHealthResponse>,
  oidc_sync: Option<AdminOidcSyncResponse>,
  oidc_sync_preview: Option<AdminOidcSyncPreviewResponse>,
  brands: Option<AdminBrandsResponse>,
  mail: Option<AdminMailSettingsResponse>,
  /// Loaded when the console shows the notetaker page.
  notetaker: Option<teamcraft_proto::AdminNotetakerResponse>,
  has_asked_notetaker: bool,
  calendar: Option<AdminCalendarSettingsResponse>,
  /// Loaded when the console shows the webhooks page; dropped after every change, so it is read
  /// again.
  webhooks: Option<teamcraft_proto::AdminWebhooksResponse>,
  has_asked_webhooks: bool,
  webhook_deliveries: HashMap<teamcraft_proto::WebhookId, teamcraft_proto::WebhookDeliveriesResponse>,
  /// Logs asked for since the last change: a refused one is not asked again on every render.
  asked_webhook_deliveries: HashSet<teamcraft_proto::WebhookId>,
  /// The secret of the webhook just created or rotated, until the admin dismisses it.
  revealed_webhook_secret: Option<(teamcraft_proto::WebhookId, teamcraft_proto::WebhookSecretText)>,
  runtime: Option<RuntimeSettingsResponse>,
  last_test: Option<(SettingsTestKind, SettingsTestResponse)>,
  /// `data:` URLs of brand images by `{brand}/{slot}`; loaded when the editor shows them.
  brand_previews: HashMap<String, String>,
  audit_filter: AuditFilter,
  guests: ListAdminGuestsResponse,
  guest_query: ListAdminGuestsQuery,
  /// Loaded on demand, when the console shows them; cleared when changed.
  schemes: HashMap<Option<SpaceId>, PermissionSchemeResponse>,
  moderation: HashMap<ConversationId, ChannelModeration>,
  members: HashMap<ConversationId, Vec<UserId>>,
  /// Requests for on-demand data that are under way.
  loading: HashSet<String>,
  maybe_error: Option<String>,
}

impl AdminLive {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self {
      auth,
      data: Rc::new(RefCell::new(AdminData::default())),
      notify: Rc::new(RefCell::new(None)),
      refresh_queued: Rc::new(Cell::new(false)),
      is_refreshing: Rc::new(Cell::new(false)),
      is_refresh_wanted_again: Rc::new(Cell::new(false)),
      wanted: Rc::new(Cell::new(Reads::NONE)),
      quiet_until: Rc::new(Cell::new(0)),
      has_read_all: Rc::new(Cell::new(false)),
      maybe_open_section: Rc::new(Cell::new(None)),
      open_alert_count: Rc::new(Cell::new(0)),
      is_health_count_queued: Rc::new(Cell::new(false)),
    }
  }

  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  pub fn channels(&self) -> Vec<AdminChannel> {
    self.data.borrow().channels.clone()
  }

  pub fn users(&self) -> Vec<AdminUser> {
    let data = self.data.borrow();
    data
      .users
      .iter()
      .map(|user| match data.devices.get(&user.user.id) {
        Some(devices) => AdminUser {
          devices: devices_from_wire(devices),
          ..user.clone()
        },
        None => user.clone(),
      })
      .collect()
  }

  /// The accounts the console knows by name and role besides the Users list's pages: the page
  /// loaded now and the owners and admins.
  pub fn known_users(&self) -> Vec<AdminUser> {
    let mut known = self.users();
    let listed: HashSet<UserId> = known.iter().map(|user| user.user.id).collect();
    known.extend(
      self
        .data
        .borrow()
        .role_holders
        .iter()
        .filter(|user| !listed.contains(&user.user.id))
        .cloned(),
    );
    let listed: HashSet<UserId> = known.iter().map(|user| user.user.id).collect();
    let data = self.data.borrow();
    known.extend(
      data
        .referenced_users
        .values()
        .filter(|user| !listed.contains(&user.user.id))
        .cloned(),
    );
    known
  }

  /// The channels notices refer to that the loaded pages don't hold, see
  /// [`Self::resolve_references`].
  pub fn referenced_channels(&self) -> Vec<AdminChannel> {
    let data = self.data.borrow();
    let listed: HashSet<ConversationId> = data.channels.iter().map(|channel| channel.id).collect();
    data
      .referenced_channels
      .values()
      .filter(|channel| !listed.contains(&channel.id))
      .cloned()
      .collect()
  }

  /// Reads the channels and accounts a notice refers to by ID, those the loaded pages and earlier
  /// reads don't hold, so a name on a later page still resolves. They arrive with the next change
  /// notification. Each ID is asked once.
  pub fn resolve_references(&self, channels: Vec<ConversationId>, users: Vec<UserId>) {
    let (channels, users) = self.unasked_references(channels, users);
    if !channels.is_empty() || !users.is_empty() {
      self.run(AdminTask::References { channels, users });
    }
  }

  /// What of `channels` and `users` isn't loaded or asked for already; marks it as asked.
  fn unasked_references(
    &self,
    channels: Vec<ConversationId>,
    users: Vec<UserId>,
  ) -> (Vec<ConversationId>, Vec<UserId>) {
    let mut data = self.data.borrow_mut();
    let known_users: HashSet<UserId> = data
      .users
      .iter()
      .chain(data.role_holders.iter())
      .map(|user| user.user.id)
      .collect();
    let known_channels: HashSet<ConversationId> = data.channels.iter().map(|channel| channel.id).collect();
    let channels = channels
      .into_iter()
      .filter(|id| !known_channels.contains(id) && data.asked_channels.insert(*id))
      .collect();
    let users = users
      .into_iter()
      .filter(|id| !known_users.contains(id) && data.asked_users.insert(*id))
      .collect();
    (channels, users)
  }

  pub fn channel_query(&self) -> AdminChannelQuery {
    self.data.borrow().channel_query.clone()
  }

  pub fn user_query(&self) -> AdminUserQuery {
    self.data.borrow().user_query.clone()
  }

  pub fn channels_have_more(&self) -> bool {
    self.data.borrow().channel_paging.has_more
  }

  pub fn users_have_more(&self) -> bool {
    self.data.borrow().user_paging.has_more
  }

  /// Lists the channels of `query` from the first page on. What was loaded stays on screen until
  /// the answer comes, and an answer to an earlier query is dropped.
  pub fn set_channel_query(&self, query: AdminChannelQuery) {
    if self.apply_channel_query(query) {
      self.queue_reads(Reads::CONVERSATIONS, QUERY_DELAY);
    }
  }

  /// Whether the query differs from the one the list holds; the list starts over if it does.
  fn apply_channel_query(&self, query: AdminChannelQuery) -> bool {
    let mut data = self.data.borrow_mut();
    if data.channel_query == query {
      return false;
    }
    data.channel_query = query;
    data.channel_paging = data.channel_paging.restarted();
    true
  }

  pub fn set_user_query(&self, query: AdminUserQuery) {
    if self.apply_user_query(query) {
      self.queue_reads(Reads::USERS, QUERY_DELAY);
    }
  }

  fn apply_user_query(&self, query: AdminUserQuery) -> bool {
    let mut data = self.data.borrow_mut();
    if data.user_query == query {
      return false;
    }
    data.user_query = query;
    data.user_paging = data.user_paging.restarted();
    true
  }

  /// A page after the last entry is being read.
  pub fn is_loading_more(&self) -> bool {
    let data = self.data.borrow();
    data.loading.contains(MORE_CHANNELS_KEY) || data.loading.contains(MORE_USERS_KEY)
  }

  /// Reads the page after the last channel loaded; one at a time.
  pub fn load_more_channels(&self) {
    let mut data = self.data.borrow_mut();
    if data.channel_paging.has_more && data.loading.insert(MORE_CHANNELS_KEY.into()) {
      drop(data);
      self.changed();
      self.run(AdminTask::MoreChannels);
    }
  }

  pub fn load_more_users(&self) {
    let mut data = self.data.borrow_mut();
    if data.user_paging.has_more && data.loading.insert(MORE_USERS_KEY.into()) {
      drop(data);
      self.changed();
      self.run(AdminTask::MoreUsers);
    }
  }

  /// Loads the devices of `user` once; they stay cached and current with the refreshes.
  pub fn load_devices(&self, user: UserId) {
    if self.data.borrow().devices.contains_key(&user) {
      return;
    }
    self.load(
      format!("devices:{user}"),
      format!("/api/v1/admin/users/{user}/devices"),
      Loaded::Devices(user),
    );
  }

  pub fn audit(&self) -> Vec<AuditEntry> {
    self.data.borrow().audit.clone()
  }

  pub fn policy(&self) -> WorkspacePolicy {
    self.data.borrow().policy.clone()
  }

  pub fn policy_version(&self) -> u64 {
    self.data.borrow().policy_version
  }

  pub fn spaces(&self) -> Vec<AdminSpace> {
    self.data.borrow().spaces.clone()
  }

  pub fn list_load(&self) -> AdminLoad {
    self.data.borrow().load.clone()
  }

  /// Renames, archives or restores a space, or changes its join policy. The server checks that
  /// the caller may; the answer shows up with the refresh.
  pub fn change_space(&self, space: SpaceId, changes: AdminSpaceChanges) {
    let request = UpdateSpaceRequest {
      maybe_name: changes.maybe_name,
      maybe_join_policy: changes.maybe_join_policy,
      maybe_is_archived: changes.maybe_is_archived,
      ..UpdateSpaceRequest::default()
    };
    if let Ok(body) = serde_json::to_string(&request) {
      self.send(ApiMethod::Patch, format!("/api/v1/spaces/{space}"), Some(body));
    }
  }

  pub fn announcement(&self) -> Option<AnnouncementResponse> {
    self.data.borrow().announcement.clone()
  }

  pub fn instance(&self) -> Option<AdminInstanceResponse> {
    self.data.borrow().instance.clone()
  }

  pub fn health(&self) -> Option<AdminHealthResponse> {
    self.data.borrow().health.clone()
  }

  pub fn oidc_sync(&self) -> Option<AdminOidcSyncResponse> {
    self.data.borrow().oidc_sync.clone()
  }

  pub fn oidc_sync_preview(&self) -> Option<AdminOidcSyncPreviewResponse> {
    self.data.borrow().oidc_sync_preview.clone()
  }

  /// Asks what the sync would change now; the answer is kept for the console.
  pub fn run_oidc_sync_preview(&self) {
    self.send_stored(
      ApiMethod::Post,
      format!("{OIDC_SYNC_PATH}/preview"),
      Some("{}".to_owned()),
      Stored::OidcSyncPreview,
    );
  }

  /// Confirms the deactivations the safety brake holds back.
  pub fn confirm_oidc_sync_brake(&self, revision: i64) {
    let body = serde_json::json!({ "revision": revision }).to_string();
    self.send_stored(
      ApiMethod::Post,
      format!("{OIDC_SYNC_PATH}/brake/confirm"),
      Some(body),
      Stored::OidcSync,
    );
  }

  /// How many health checks are alerting, as of the last read.
  pub fn open_alert_count(&self) -> usize {
    self.open_alert_count.get()
  }

  pub fn audit_filter(&self) -> AuditFilter {
    self.data.borrow().audit_filter
  }

  pub fn brands(&self) -> Option<AdminBrandsResponse> {
    self.data.borrow().brands.clone()
  }

  pub fn create_brand(&self, request: CreateBrandRequest) {
    self.send_brands(ApiMethod::Post, "/api/v1/admin/brands".to_owned(), &request, None);
  }

  pub fn update_brand(&self, id: &str, request: UpdateBrandRequest) {
    self.send_brands(
      ApiMethod::Put,
      format!("/api/v1/admin/brands/{id}"),
      &request,
      None,
    );
  }

  pub fn delete_brand(&self, id: &str) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/brands/{id}"),
      None,
      Stored::Brands {
        maybe_forgotten: Some(id.to_owned()),
      },
    );
  }

  pub fn take_over_brand(&self, id: &str) {
    self.send_stored(
      ApiMethod::Post,
      format!("/api/v1/admin/brands/{id}/take-over"),
      Some("{}".to_owned()),
      Stored::Brands {
        maybe_forgotten: None,
      },
    );
  }

  /// The image as a `data:` URL once it is loaded; the first call starts the request.
  pub fn brand_asset_preview(&self, id: &str, slot: &BrandAssetSlot) -> Option<String> {
    let key = preview_key(id, slot);
    if let Some(cached) = self.data.borrow().brand_previews.get(&key) {
      return Some(cached.clone());
    }
    // A failed load is remembered as an empty entry, so a missing image is not asked for again.
    if !self.data.borrow_mut().loading.insert(format!("preview:{key}")) {
      return None;
    }
    let path = format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str());
    self.run(AdminTask::BrandPreview { key, path });
    None
  }

  pub fn upload_brand_asset(&self, id: &str, slot: &BrandAssetSlot, bytes: Vec<u8>, content_type: String) {
    self.run(AdminTask::UploadBrandAsset {
      id: id.to_owned(),
      slot: slot.clone(),
      bytes,
      content_type,
    });
  }

  async fn upload_brand_asset_now(
    &self,
    id: String,
    slot: BrandAssetSlot,
    bytes: Vec<u8>,
    content_type: String,
  ) {
    let path = format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str());
    let body = BinaryBody { content_type, bytes };
    let message = match self
      .auth
      .api_request_bytes(ApiMethod::Put, &path, Some(body))
      .await
    {
      Ok(response) if response.status < 300 => {
        let text = String::from_utf8_lossy(&response.body);
        if let Ok(brands) = serde_json::from_str::<AdminBrandsResponse>(&text) {
          self.data.borrow_mut().brands = Some(brands);
        }
        None
      },
      Ok(response) => Some(format!(
        "{path}: {}",
        self.refusal_of(&String::from_utf8_lossy(&response.body), response.status)
      )),
      Err(error) => Some(error.to_string()),
    };
    {
      let mut data = self.data.borrow_mut();
      data.maybe_error = message;
      forget_previews(&mut data, &id);
    }
    self.changed();
    self.queue_refresh();
  }

  pub fn delete_brand_asset(&self, id: &str, slot: &BrandAssetSlot) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str()),
      None,
      Stored::Brands {
        maybe_forgotten: Some(id.to_owned()),
      },
    );
  }

  pub fn save_brand_mail_server(&self, id: &str, request: &teamcraft_proto::UpdateBrandMailServerRequest) {
    self.send_brands(
      ApiMethod::Put,
      format!("/api/v1/admin/brands/{id}/mail-server"),
      request,
      None,
    );
  }

  pub fn remove_brand_mail_server(&self, id: &str) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/brands/{id}/mail-server"),
      None,
      Stored::Brands {
        maybe_forgotten: None,
      },
    );
  }

  pub fn send_brand_test_mail(&self, id: &str, request: SendTestMailRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.run_settings_test(
        SettingsTestKind::BrandMail(id.to_owned()),
        format!("/api/v1/admin/brands/{id}/mail-server/test"),
        body,
      );
    }
  }

  pub fn mail_settings(&self) -> Option<AdminMailSettingsResponse> {
    self.data.borrow().mail.clone()
  }

  /// Whether the notetaker's key is stored; loaded once the console shows its page.
  pub fn notetaker_settings(&self) -> Option<teamcraft_proto::AdminNotetakerResponse> {
    let maybe_settings = self.data.borrow().notetaker.clone();
    // Once per console session: a refusal (no `manage_workspace`) must not ask again on every render.
    let is_first_ask = !std::mem::replace(&mut self.data.borrow_mut().has_asked_notetaker, true);
    if maybe_settings.is_none() && is_first_ask {
      self.load(
        "notetaker".to_owned(),
        "/api/v1/admin/notetaker".to_owned(),
        Loaded::Notetaker,
      );
    }
    maybe_settings
  }

  pub fn set_notetaker_key(&self, key: teamcraft_proto::SecretUpdate) {
    let request = teamcraft_proto::PutNotetakerKeyRequest { key };
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Put,
        "/api/v1/admin/notetaker/key".to_owned(),
        Some(body),
        Stored::Notetaker,
      );
    }
  }

  pub fn save_mail_settings(&self, request: UpdateMailSettingsRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Put,
        "/api/v1/admin/settings/mail".to_owned(),
        Some(body),
        Stored::Mail,
      );
    }
  }

  pub fn reset_mail_settings(&self) {
    self.send_stored(
      ApiMethod::Delete,
      "/api/v1/admin/settings/mail".to_owned(),
      None,
      Stored::Mail,
    );
  }

  pub fn send_test_mail(&self, request: SendTestMailRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.run_settings_test(
        SettingsTestKind::Mail,
        "/api/v1/admin/settings/mail/test".to_owned(),
        body,
      );
    }
  }

  pub fn calendar_settings(&self) -> Option<AdminCalendarSettingsResponse> {
    self.data.borrow().calendar.clone()
  }

  pub fn save_calendar_settings(&self, request: UpdateCalendarSettingsRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Put,
        "/api/v1/admin/settings/calendar".to_owned(),
        Some(body),
        Stored::Calendar,
      );
    }
  }

  pub fn reset_calendar_settings(&self) {
    self.send_stored(
      ApiMethod::Delete,
      "/api/v1/admin/settings/calendar".to_owned(),
      None,
      Stored::Calendar,
    );
  }

  pub fn test_calendar(&self) {
    self.run_settings_test(
      SettingsTestKind::Calendar,
      "/api/v1/admin/settings/calendar/test".to_owned(),
      "{}".to_owned(),
    );
  }

  pub fn last_settings_test(&self) -> Option<(SettingsTestKind, SettingsTestResponse)> {
    self.data.borrow().last_test.clone()
  }

  /// The webhooks; loaded once the console shows their page, and again after each change.
  pub fn admin_webhooks(&self) -> Option<teamcraft_proto::AdminWebhooksResponse> {
    let maybe_webhooks = self.data.borrow().webhooks.clone();
    // Once until the next change: a refusal must not ask again on every render.
    let is_first_ask = !std::mem::replace(&mut self.data.borrow_mut().has_asked_webhooks, true);
    if maybe_webhooks.is_none() && is_first_ask {
      self.load(
        "webhooks".to_owned(),
        "/api/v1/admin/webhooks".to_owned(),
        Loaded::Webhooks,
      );
    }
    maybe_webhooks
  }

  pub fn create_webhook(&self, request: teamcraft_proto::CreateWebhookRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Post,
        "/api/v1/admin/webhooks".to_owned(),
        Some(body),
        Stored::Webhooks { has_secret: true },
      );
    }
  }

  pub fn update_webhook(
    &self,
    id: teamcraft_proto::WebhookId,
    request: teamcraft_proto::UpdateWebhookRequest,
  ) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Put,
        format!("/api/v1/admin/webhooks/{id}"),
        Some(body),
        Stored::Webhooks { has_secret: false },
      );
    }
  }

  pub fn delete_webhook(&self, id: teamcraft_proto::WebhookId) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/webhooks/{id}"),
      None,
      Stored::Webhooks { has_secret: false },
    );
  }

  pub fn rotate_webhook_secret(&self, id: teamcraft_proto::WebhookId) {
    self.send_stored(
      ApiMethod::Post,
      format!("/api/v1/admin/webhooks/{id}/secret"),
      Some("{}".to_owned()),
      Stored::Webhooks { has_secret: true },
    );
  }

  pub fn test_webhook(&self, id: teamcraft_proto::WebhookId) {
    self.run_settings_test(
      SettingsTestKind::Webhook(id),
      format!("/api/v1/admin/webhooks/{id}/test"),
      "{}".to_owned(),
    );
  }

  pub fn webhook_deliveries(
    &self,
    id: teamcraft_proto::WebhookId,
  ) -> Option<teamcraft_proto::WebhookDeliveriesResponse> {
    let maybe_log = self.data.borrow().webhook_deliveries.get(&id).cloned();
    let is_first_ask = self.data.borrow_mut().asked_webhook_deliveries.insert(id);
    if maybe_log.is_none() && is_first_ask {
      self.load(
        format!("webhook-deliveries-{id}"),
        format!("/api/v1/admin/webhooks/{id}/deliveries"),
        Loaded::WebhookDeliveries(id),
      );
    }
    maybe_log
  }

  pub fn revealed_webhook_secret(
    &self,
  ) -> Option<(teamcraft_proto::WebhookId, teamcraft_proto::WebhookSecretText)> {
    self.data.borrow().revealed_webhook_secret.clone()
  }

  pub fn dismiss_webhook_secret(&self) {
    self.data.borrow_mut().revealed_webhook_secret = None;
    self.changed();
  }

  pub fn runtime_settings(&self) -> Option<RuntimeSettingsResponse> {
    self.data.borrow().runtime.clone()
  }

  pub fn save_runtime_settings(&self, section: RuntimeSettingsSection) {
    if let Ok(body) = serde_json::to_string(&section) {
      self.send_stored(
        ApiMethod::Put,
        "/api/v1/admin/settings".to_owned(),
        Some(body),
        Stored::Runtime,
      );
    }
  }

  pub fn reset_runtime_settings(&self, key: RuntimeSettingsKey) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/settings/{}", key.as_str()),
      None,
      Stored::Runtime,
    );
  }

  pub fn guests(&self) -> ListAdminGuestsResponse {
    self.data.borrow().guests.clone()
  }

  pub fn set_guest_query(&self, query: ListAdminGuestsQuery) {
    self.data.borrow_mut().guest_query = query;
    self.queue_refresh();
  }

  pub fn extend_guests(&self, guests: Vec<(SpaceId, UserId, i64)>) {
    let requests = guests
      .into_iter()
      .filter_map(|(space, user, expires_at_ms)| {
        let request = ExtendGuestRequest {
          expires_at: UnixMillis(expires_at_ms),
        };
        let body = serde_json::to_string(&request).ok()?;
        Some((
          ApiMethod::Post,
          format!("/api/v1/spaces/{space}/guests/{user}/extend"),
          Some(body),
        ))
      })
      .collect();
    self.run(AdminTask::SendEach { requests });
  }

  pub fn remove_guests(&self, guests: Vec<(SpaceId, UserId)>) {
    let requests = guests
      .into_iter()
      .map(|(space, user)| {
        (
          ApiMethod::Delete,
          format!("/api/v1/spaces/{space}/members/{user}"),
          None,
        )
      })
      .collect();
    self.run(AdminTask::SendEach { requests });
  }

  /// Archives or restores a channel. The server checks that the caller may; members' apps name the
  /// change from the server's record (e2ee.md §4.10).
  pub fn set_channel_archived(&self, channel: ConversationId, is_archived: bool) {
    let request = UpdateConversationRequest {
      maybe_is_archived: Some(is_archived),
      ..UpdateConversationRequest::default()
    };
    if let Ok(body) = serde_json::to_string(&request) {
      self.send(
        ApiMethod::Patch,
        format!("/api/v1/conversations/{channel}"),
        Some(body),
      );
    }
  }

  /// Keeps a channel open for the policy's snooze period (issue #212); the console shows the new date
  /// with the refresh.
  pub fn snooze_channel(&self, channel: ConversationId) {
    let request = teamcraft_proto::SnoozeInactivityRequest::default();
    if let Ok(body) = serde_json::to_string(&request) {
      self.send(
        ApiMethod::Post,
        format!("/api/v1/conversations/{channel}/inactivity/snooze"),
        Some(body),
      );
    }
  }

  pub fn set_audit_filter(&self, filter: AuditFilter) {
    self.data.borrow_mut().audit_filter = filter;
    self.queue_refresh();
  }

  /// The scheme at the scope; empty until the first request has answered.
  pub fn permission_scheme(&self, maybe_space: Option<SpaceId>) -> PermissionSchemeResponse {
    let cached = self.data.borrow().schemes.get(&maybe_space).cloned();
    if let Some(scheme) = cached {
      return scheme;
    }
    let path = match maybe_space {
      Some(space) => format!("/api/v1/admin/spaces/{space}/permission-scheme"),
      None => "/api/v1/admin/permission-scheme".to_owned(),
    };
    self.load(
      format!("scheme:{maybe_space:?}"),
      path,
      Loaded::Scheme(maybe_space),
    );
    PermissionSchemeResponse { roles: Vec::new() }
  }

  pub fn change_permissions(&self, maybe_space: Option<SpaceId>, changes: Vec<SchemeChange>) {
    let path = match maybe_space {
      Some(space) => format!("/api/v1/admin/spaces/{space}/permission-scheme"),
      None => "/api/v1/admin/permission-scheme".to_owned(),
    };
    if let Ok(body) = serde_json::to_string(&UpdatePermissionSchemeRequest { changes }) {
      self.send(ApiMethod::Put, path, Some(body));
    }
  }

  pub fn channel_moderation(&self, channel: ConversationId) -> ChannelModeration {
    let cached = self.data.borrow().moderation.get(&channel).cloned();
    if let Some(moderation) = cached {
      return moderation;
    }
    self.load(
      format!("moderation:{channel}"),
      format!("/api/v1/admin/conversations/{channel}/moderation"),
      Loaded::Moderation(channel),
    );
    ChannelModeration::default()
  }

  pub fn set_channel_moderation(&self, channel: ConversationId, moderation: ChannelModeration) {
    if let Ok(body) = serde_json::to_string(&SetChannelModerationRequest {
      moderation: moderation.clone(),
    }) {
      // Show the new toggles at once; the next refresh confirms or corrects them.
      self.data.borrow_mut().moderation.insert(channel, moderation);
      self.changed();
      self.send(
        ApiMethod::Put,
        format!("/api/v1/admin/conversations/{channel}/moderation"),
        Some(body),
      );
    }
  }

  pub fn channel_member_candidates(&self, channel: ConversationId) -> Vec<UserId> {
    let cached = self.data.borrow().members.get(&channel).cloned();
    if let Some(members) = cached {
      return members;
    }
    self.load(
      format!("members:{channel}"),
      format!("/api/v1/admin/conversations/{channel}/members"),
      Loaded::Members(channel),
    );
    Vec::new()
  }

  pub fn assign_channel_owner(&self, channel: ConversationId, user: UserId) {
    self.data.borrow_mut().legacy_details.remove(&channel);
    if let Ok(body) = serde_json::to_string(&AssignChannelOwnerRequest { user_id: user }) {
      self.send(
        ApiMethod::Post,
        format!("/api/v1/admin/conversations/{channel}/owner"),
        Some(body),
      );
    }
  }

  pub fn set_announcement(&self, announcement: SetAnnouncementRequest) {
    if let Ok(body) = serde_json::to_string(&announcement) {
      self.send(
        ApiMethod::Put,
        "/api/v1/admin/announcement".to_owned(),
        Some(body),
      );
    }
  }

  pub fn clear_announcement(&self) {
    self.send(ApiMethod::Delete, "/api/v1/admin/announcement".to_owned(), None);
  }

  pub fn maybe_error(&self) -> Option<String> {
    self.data.borrow().maybe_error.clone()
  }

  /// Owners and admins as the server lists them (active accounts only, each once): proposed for the
  /// first roster, and as additions to a signed one (issue #765).
  pub fn roster_candidates(&self) -> (Vec<UserId>, Vec<UserId>) {
    teamcraft_client_api::admin::role_holders(&self.data.borrow().role_holders)
  }

  /// Fetches what was asked for now; a part that fails keeps its previous value and shows its error.
  /// Only one refresh runs at a time (the console polls); a request meanwhile reruns it once. While
  /// the server asks the console to wait (429), nothing is read until then.
  pub async fn refresh(&self) {
    if self.is_refreshing.replace(true) {
      self.is_refresh_wanted_again.set(true);
      return;
    }
    loop {
      self.is_refresh_wanted_again.set(false);
      if let Some(wait) = self.time_to_quiet_end() {
        // The wanted reads stay; one refresh follows when the server allows again.
        self.queue_refresh_after(wait);
        break;
      }
      // The reading and its wire types are in the console's chunk (`areas::admin_refresh`): an
      // admin's first visit doesn't load them.
      match crate::areas::admin_refresh(self.clone()).await {
        Ok(refresh) => refresh.await,
        Err(error) => self.chunk_failed(&error.to_string()),
      }
      // Refused meanwhile: the same reads follow once the server allows.
      if let Some(wait) = self.time_to_quiet_end() {
        self.queue_refresh_after(wait);
        break;
      }
      // async-ok: a Cell, not a signal.
      if !self.is_refresh_wanted_again.get() {
        break;
      }
    }
    // async-ok: a Cell, not a signal.
    self.is_refreshing.set(false);
  }

  /// The console's chunk didn't load (offline, a deploy in between): the lists say so, as a failed
  /// read would.
  fn chunk_failed(&self, error: &str) {
    tracing::warn!(%error, "the administration console's data did not load");
    let detail = format!("{UNREACHABLE_CODE}:");
    {
      let mut data = self.data.borrow_mut();
      if data.load != AdminLoad::Loaded {
        data.load = AdminLoad::Failed(detail.clone());
      }
      data.maybe_error = Some(detail);
    }
    self.changed();
  }

  /// The lists first and all at once, so the console shows spaces and channels after one round
  /// trip. Only what was asked for is read (`Reads`): the timer asks for the open section's data,
  /// not for everything. The lists carry owner and retention of every channel; devices load when a
  /// user's details open.
  pub(crate) async fn refresh_once(&self) {
    let mut reads = self.wanted.replace(Reads::NONE);
    // The first load didn't finish (or failed): the lists are read again whatever section asks.
    // So is everything the first time, and when nothing in particular was asked for.
    // async-ok: a Cell, not a signal.
    if reads == Reads::NONE || self.data.borrow().load != AdminLoad::Loaded || !self.has_read_all.get() {
      reads = Reads::ALL;
    }
    let mut errors: Vec<String> = Vec::new();
    let (
      ((spaces, conversations, users, policy, announcement), (instance, brands, mail, calendar, runtime)),
      (health, oidc_sync),
    ) = join(
      join(
        join5(
          self.fetch_if::<ListAdminSpacesResponse>(reads.has(Reads::SPACES), "/api/v1/admin/spaces"),
          self.read_conversations_if(reads.has(Reads::CONVERSATIONS)),
          self.read_users_if(reads.has(Reads::USERS)),
          self.fetch_if::<WorkspacePolicyResponse>(reads.has(Reads::POLICY), "/api/v1/workspace/policy"),
          self.fetch_if::<GetAnnouncementResponse>(reads.has(Reads::ANNOUNCEMENT), "/api/v1/announcement"),
        ),
        join5(
          self.fetch_if::<AdminInstanceResponse>(reads.has(Reads::INSTANCE), "/api/v1/admin/instance"),
          self.fetch_if::<AdminBrandsResponse>(reads.has(Reads::BRANDS), "/api/v1/admin/brands"),
          self.fetch_if::<AdminMailSettingsResponse>(reads.has(Reads::MAIL), "/api/v1/admin/settings/mail"),
          self.fetch_if::<AdminCalendarSettingsResponse>(
            reads.has(Reads::CALENDAR),
            "/api/v1/admin/settings/calendar",
          ),
          self.fetch_if::<RuntimeSettingsResponse>(reads.has(Reads::RUNTIME), "/api/v1/admin/settings"),
        ),
      ),
      join(
        self.fetch_if::<AdminHealthResponse>(reads.has(Reads::HEALTH), HEALTH_PATH),
        self.fetch_if::<AdminOidcSyncResponse>(reads.has(Reads::HEALTH), OIDC_SYNC_PATH),
      ),
    )
    .await;
    let spaces = settle(spaces, &mut errors).flatten();
    let conversations = settle(conversations, &mut errors).flatten();
    let users = settle(users, &mut errors).flatten();
    let policy = settle(policy, &mut errors).flatten();
    let announcement = settle(announcement, &mut errors).flatten();
    let instance = settle(instance, &mut errors).flatten();
    let health = settle(health, &mut errors).flatten();
    let oidc_sync = settle(oidc_sync, &mut errors).flatten();
    let brands = settle(brands, &mut errors).flatten();
    let mail = settle(mail, &mut errors).flatten();
    let calendar = settle(calendar, &mut errors).flatten();
    let runtime = settle(runtime, &mut errors).flatten();

    // What the lists are combined with: read now, or as the last refresh left it.
    let space_names: HashMap<SpaceId, String> = match &spaces {
      Some(spaces) => spaces
        .spaces
        .iter()
        .map(|space| (space.id, space.name.clone()))
        .collect(),
      None => self
        .data
        .borrow()
        .spaces
        .iter()
        .map(|space| (space.id, space.name.clone()))
        .collect(),
    };
    let (policy_default_history, keeps_pinned) = match &policy {
      Some(policy) => (
        policy.policy.history_default.clone(),
        policy.policy.retention.keep_pinned,
      ),
      None => {
        let data = self.data.borrow();
        (
          data.policy.history_default.clone(),
          data.policy.retention.keep_pinned,
        )
      },
    };

    // Show the lists now; a server that doesn't send owner and retention in them is asked below.
    {
      let mut data = self.data.borrow_mut();
      let is_first_load = data.load != AdminLoad::Loaded;
      if is_first_load {
        if let Some(list) = &conversations {
          let channels = channels_of(
            &list.items,
            &space_names,
            &data.legacy_details,
            &policy_default_history,
            keeps_pinned,
          );
          data.channels = channels;
          data.channel_paging.pages = list.pages;
          data.channel_paging.has_more = list.has_more;
        }
        if let Some(spaces) = &spaces {
          data.spaces = spaces.spaces.iter().map(space_from_wire).collect();
        }
        // Every first read asks for the channels (ALL), so a missing list is a failed one.
        data.load = match (&spaces, &conversations) {
          (Some(_), Some(_)) => AdminLoad::Loaded,
          _ => AdminLoad::Failed(errors.first().cloned().unwrap_or_default()),
        };
      }
    }
    self.changed();

    // The server asked to wait: keep what the lists gave and read nothing more until it allows.
    if !self.is_quiet() {
      let mut reload_errors = Vec::new();
      self.reload_cached(reads, &mut reload_errors).await;
      errors.extend(reload_errors);
    }
    if let Some(list) = &conversations
      && reads.has(Reads::LEGACY_DETAILS)
      && !self.is_quiet()
    {
      self.read_legacy_details(&list.items).await;
    }
    let role_holders = if reads.has(Reads::ROLE_HOLDERS) && !self.is_quiet() {
      settle(self.read_role_holders().await, &mut errors)
    } else {
      None
    };
    let guests = if reads.has(Reads::GUESTS) && !self.is_quiet() {
      let guests_path = guests_path(&self.data.borrow().guest_query);
      settle(
        self.fetch_result::<ListAdminGuestsResponse>(guests_path).await,
        &mut errors,
      )
    } else {
      None
    };
    let audit = if reads.has(Reads::AUDIT) && !self.is_quiet() {
      let audit_path = audit_path(self.data.borrow().audit_filter);
      settle(
        self.fetch_result::<ListAuditResponse>(audit_path).await,
        &mut errors,
      )
    } else {
      None
    };

    let has_conversations = conversations.is_some();
    {
      let mut data = self.data.borrow_mut();
      if let Some(list) = guests {
        data.guests = list;
      }
      if let Some(list) = conversations {
        let channels = list.map(|summaries| {
          channels_of(
            &summaries,
            &space_names,
            &data.legacy_details,
            &policy_default_history,
            keeps_pinned,
          )
        });
        let (current, paging) = (std::mem::take(&mut data.channels), data.channel_paging);
        match refreshed(&current, paging, channels, |channel| raw_id(channel.id)) {
          Some((items, paging)) => {
            data.channels = items;
            data.channel_paging = paging;
          },
          // The admin searched for something else meanwhile; its answer follows.
          None => data.channels = current,
        }
      }
      if let Some(list) = users {
        let list = list.map(|summaries| summaries.iter().map(user_from_wire).collect());
        let (current, paging) = (std::mem::take(&mut data.users), data.user_paging);
        match refreshed(&current, paging, list, |user: &AdminUser| raw_id(user.user.id)) {
          Some((items, paging)) => {
            data.users = items;
            data.user_paging = paging;
          },
          None => data.users = current,
        }
      }
      if let Some(holders) = role_holders {
        data.role_holders = holders.iter().map(user_from_wire).collect();
      }
      if let Some(policy) = policy {
        data.policy = policy.policy;
        data.policy_version = policy.version;
      }
      if let Some(spaces) = &spaces {
        data.spaces = spaces.spaces.iter().map(space_from_wire).collect();
      }
      if let Some(announcement) = announcement {
        data.announcement = announcement.maybe_announcement;
      }
      if instance.is_some() {
        data.instance = instance;
      }
      if let Some(health) = health {
        self.open_alert_count.set(health.open_alert_count()); // async-ok: a Cell, not a signal.
        data.health = Some(health);
      }
      if oidc_sync.is_some() {
        data.oidc_sync = oidc_sync;
      }
      if brands.is_some() {
        data.brands = brands;
      }
      if mail.is_some() {
        data.mail = mail;
      }
      if calendar.is_some() {
        data.calendar = calendar;
      }
      if runtime.is_some() {
        data.runtime = runtime;
      }
      if let Some(audit) = audit {
        let channels = data.channels.clone();
        let users: Vec<AdminUser> = {
          let mut known = data.users.clone();
          known.extend(data.role_holders.iter().cloned());
          known
        };
        data.audit = audit
          .entries
          .iter()
          .map(|entry| audit_from_wire(entry, &space_names, &channels, &users))
          .collect();
      }
      if data.load != AdminLoad::Loaded && spaces.is_some() && has_conversations {
        data.load = AdminLoad::Loaded;
      }
      // A refusal to wait for is what the admin should see first, not a failure it caused.
      errors.sort_by_key(|error| !error.starts_with(RATE_LIMITED_CODE));
      data.maybe_error = errors.into_iter().next();
    }
    // Refused meanwhile: the same reads are wanted again when the server allows.
    if self.is_quiet() {
      self.wanted.set(self.wanted.get() | reads); // async-ok: a Cell, not a signal.
    } else if reads == Reads::ALL {
      self.has_read_all.set(true); // async-ok: a Cell, not a signal.
    }
    self.changed();
  }

  /// Schedules a refresh of everything soon; calls while one is scheduled collapse into it. For the
  /// admin's own changes, opening the console and the retry button.
  pub fn queue_refresh(&self) {
    self.queue_reads(Reads::ALL, REFRESH_DELAY);
  }

  /// The console's timer: reads what `section` shows and nothing else.
  pub fn queue_refresh_section(&self, section: AdminSection) {
    self.maybe_open_section.set(Some(section));
    self.queue_reads(Reads::of(section), REFRESH_DELAY);
  }

  /// The server told the admin that an alert was raised or resolved (or the connection came back,
  /// so the news may have been missed): the open health page reads the checks, anywhere else only
  /// the number for the badges is read. One small request either way, never a poll.
  pub fn health_changed(&self) {
    if self.maybe_open_section.get() == Some(AdminSection::Health) {
      self.queue_reads(Reads::HEALTH, REFRESH_DELAY);
    } else {
      self.queue_health_count();
    }
  }

  /// Reads how many checks are alerting, for the badges. Calls while one is scheduled collapse.
  pub fn queue_health_count(&self) {
    if self.is_health_count_queued.replace(true) {
      return;
    }
    let live = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell, not a signal.
        live.is_health_count_queued.set(false);
        spawn_local(async move { live.read_health_count().await });
      },
      REFRESH_DELAY,
    );
  }

  async fn read_health_count(&self) {
    if let Some(wait) = self.time_to_quiet_end() {
      self.is_health_count_queued.set(true); // async-ok: a Cell, not a signal.
      let live = self.clone();
      set_timeout(
        move || {
          // async-ok: a Cell, not a signal.
          live.is_health_count_queued.set(false);
          spawn_local(async move { live.read_health_count().await });
        },
        wait,
      );
      return;
    }
    let count = match self.auth.api_get(HEALTH_PATH).await {
      Ok(response) if response.status == 200 => alerting_checks(&response.body),
      Ok(response) => {
        self.read_failure(response.status, &response.body);
        None
      },
      Err(error) => {
        tracing::debug!(%error, "the health count could not be read");
        None
      },
    };
    if let Some(count) = count
      && self.open_alert_count.replace(count) != count
    {
      self.changed();
    }
  }

  /// A refresh after the core had time to sign and publish a policy.
  pub fn queue_refresh_after_publish(&self) {
    self.queue_reads(Reads::ALL, PUBLISH_SETTLE);
  }

  fn queue_reads(&self, reads: Reads, delay: Duration) {
    self.wanted.set(self.wanted.get() | reads);
    self.queue_refresh_after(delay);
  }

  fn queue_refresh_after(&self, delay: Duration) {
    if self.refresh_queued.replace(true) {
      return;
    }
    let live = self.clone();
    set_timeout(
      move || {
        // async-ok: a Cell, not a signal.
        live.refresh_queued.set(false);
        spawn_local(async move { live.refresh().await });
      },
      delay,
    );
  }

  /// The server answered 429: no read starts for `seconds` (its `Retry-After`).
  fn back_off(&self, seconds: u64) {
    let seconds = seconds.clamp(MIN_BACKOFF_SECS, MAX_BACKOFF_SECS);
    let until = now_ms() + i64::try_from(seconds).unwrap_or(0) * 1000;
    self.quiet_until.set(self.quiet_until.get().max(until));
  }

  fn is_quiet(&self) -> bool {
    self.time_to_quiet_end().is_some()
  }

  /// How long the console still waits, with a second of margin so the retry doesn't land on the
  /// last moment of the refusal.
  fn time_to_quiet_end(&self) -> Option<Duration> {
    let left = self.quiet_until.get() - now_ms();
    (left > 0).then(|| Duration::from_millis(u64::try_from(left).unwrap_or(0) + 1000))
  }

  pub fn set_user_active(&self, user: UserId, is_active: bool) {
    let verb = if is_active { "reactivate" } else { "deactivate" };
    self.send(
      ApiMethod::Post,
      format!("/api/v1/admin/users/{user}/{verb}"),
      Some("{}".to_owned()),
    );
  }

  pub fn remove_device(&self, user: UserId, device: crate::model::DeviceId) {
    self.send(
      ApiMethod::Delete,
      format!("/api/v1/admin/users/{user}/devices/{device}"),
      None,
    );
  }

  pub fn set_channel_retention(&self, channel: ConversationId, retention: Retention) {
    self.data.borrow_mut().legacy_details.remove(&channel);
    let request = SetRetentionRequest {
      retention: RetentionSetting {
        maybe_max_age_days: retention.maybe_days,
        maybe_max_messages: retention.maybe_max_messages.map(u64::from),
      },
    };
    if let Ok(body) = serde_json::to_string(&request) {
      self.send(
        ApiMethod::Put,
        format!("/api/v1/conversations/{channel}/retention"),
        Some(body),
      );
    }
  }

  /// [`refusal_text`], and a 429 also makes the console wait.
  fn refusal_of(&self, body: &str, status: u16) -> String {
    let text = refusal_text(body, status);
    if text.starts_with(RATE_LIMITED_CODE) {
      self.back_off(retry_after_of(body));
    }
    text
  }

  fn changed(&self) {
    let notify = self.notify.borrow().clone();
    if let Some(notify) = notify {
      notify();
    }
  }

  /// Owner and retention of channels from a server that doesn't send them in the list: one request
  /// each for the first [`LEGACY_DETAIL_LIMIT`] channels, with bounded concurrency, and again only
  /// after [`LEGACY_DETAIL_TTL_MS`] (or after the admin changed that channel). The pages that show
  /// them ask for this; the timer on the other pages doesn't.
  async fn read_legacy_details(&self, summaries: &[AdminConversationSummary]) {
    let now = now_ms();
    let wanted: Vec<&AdminConversationSummary> = {
      let data = self.data.borrow();
      summaries
        .iter()
        .take(LEGACY_DETAIL_LIMIT)
        .filter(|summary| summary.maybe_retention.is_none())
        .filter(|summary| {
          data
            .legacy_details
            .get(&summary.id)
            .is_none_or(|(read_at, _)| now - read_at > LEGACY_DETAIL_TTL_MS)
        })
        .collect()
    };
    let read = stream::iter(wanted.into_iter().map(|summary| async move {
      if self.is_quiet() {
        return None;
      }
      Some((summary.id, self.channel_details(summary.id).await))
    }))
    .buffered(DETAIL_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut data = self.data.borrow_mut();
    for (channel, details) in read.into_iter().flatten() {
      data.legacy_details.insert(channel, (now, details));
    }
  }

  async fn channel_details(&self, channel: ConversationId) -> ChannelDetails {
    let mut ignored = Vec::new();
    let members = self
      .fetch::<ListConversationMembersResponse>(
        &format!("/api/v1/admin/conversations/{channel}/members"),
        &mut ignored,
      )
      .await;
    let retention = self
      .fetch::<RetentionStatusResponse>(
        &format!("/api/v1/conversations/{channel}/retention"),
        &mut ignored,
      )
      .await;
    ChannelDetails {
      maybe_owner: members.and_then(|list| {
        list
          .members
          .into_iter()
          .find(|member| member.role == teamcraft_proto::ChannelRole::Admin)
          .map(|member| member.user.id)
      }),
      maybe_retention: retention.map(|status| status.effective),
    }
  }

  /// Re-reads the on-demand data that is cached, so changes of other admins show up: only the kinds
  /// the open section shows (`reads`), and only what was looked at before.
  async fn reload_cached(&self, reads: Reads, errors: &mut Vec<String>) {
    let (scopes, channels, users): (Vec<Option<SpaceId>>, Vec<ConversationId>, Vec<UserId>) = {
      let data = self.data.borrow();
      (
        data.schemes.keys().copied().collect(),
        data.moderation.keys().copied().collect(),
        data.devices.keys().copied().collect(),
      )
    };
    if reads.has(Reads::CACHED_SCHEMES) {
      for scope in scopes {
        if self.is_quiet() {
          return;
        }
        let path = match scope {
          Some(space) => format!("/api/v1/admin/spaces/{space}/permission-scheme"),
          None => "/api/v1/admin/permission-scheme".to_owned(),
        };
        if let Some(scheme) = self.fetch::<PermissionSchemeResponse>(&path, errors).await {
          self.data.borrow_mut().schemes.insert(scope, scheme);
        }
      }
    }
    if reads.has(Reads::CACHED_MODERATION) {
      for channel in channels {
        if self.is_quiet() {
          return;
        }
        let path = format!("/api/v1/admin/conversations/{channel}/moderation");
        if let Some(moderation) = self.fetch::<ChannelModeration>(&path, errors).await {
          self.data.borrow_mut().moderation.insert(channel, moderation);
        }
      }
    }
    if reads.has(Reads::CACHED_DEVICES) {
      for user in users {
        if self.is_quiet() {
          return;
        }
        let path = format!("/api/v1/admin/users/{user}/devices");
        if let Some(list) = self.fetch::<ListAdminDevicesResponse>(&path, errors).await {
          self.data.borrow_mut().devices.insert(user, list.devices);
        }
      }
    }
  }

  /// Fetches data the console shows on demand and stores it; one request per key at a time.
  fn load(&self, key: String, path: String, what: Loaded) {
    if !self.data.borrow_mut().loading.insert(key.clone()) {
      return;
    }
    self.run(AdminTask::Load { key, path, what });
  }

  async fn load_now(&self, key: String, path: String, what: Loaded) {
    let mut errors = Vec::new();
    let stored = match what {
      Loaded::Scheme(maybe_space) => self
        .fetch::<PermissionSchemeResponse>(&path, &mut errors)
        .await
        .map(|scheme| {
          Box::new(move |data: &mut AdminData| drop(data.schemes.insert(maybe_space, scheme))) as Store
        }),
      Loaded::Moderation(channel) => {
        self
          .fetch::<ChannelModeration>(&path, &mut errors)
          .await
          .map(|moderation| {
            Box::new(move |data: &mut AdminData| drop(data.moderation.insert(channel, moderation))) as Store
          })
      },
      Loaded::Members(channel) => self
        .fetch::<ListConversationMembersResponse>(&path, &mut errors)
        .await
        .map(|list| {
          let members = list.members.into_iter().map(|member| member.user.id).collect();
          Box::new(move |data: &mut AdminData| drop(data.members.insert(channel, members))) as Store
        }),
      Loaded::Devices(user) => self
        .fetch::<ListAdminDevicesResponse>(&path, &mut errors)
        .await
        .map(|list| {
          Box::new(move |data: &mut AdminData| drop(data.devices.insert(user, list.devices))) as Store
        }),
      Loaded::Notetaker => self
        .fetch::<teamcraft_proto::AdminNotetakerResponse>(&path, &mut errors)
        .await
        .map(|settings| Box::new(move |data: &mut AdminData| data.notetaker = Some(settings)) as Store),
      Loaded::Webhooks => self
        .fetch::<teamcraft_proto::AdminWebhooksResponse>(&path, &mut errors)
        .await
        .map(|webhooks| Box::new(move |data: &mut AdminData| data.webhooks = Some(webhooks)) as Store),
      Loaded::WebhookDeliveries(id) => self
        .fetch::<teamcraft_proto::WebhookDeliveriesResponse>(&path, &mut errors)
        .await
        .map(|log| {
          Box::new(move |data: &mut AdminData| drop(data.webhook_deliveries.insert(id, log))) as Store
        }),
    };
    {
      let mut data = self.data.borrow_mut();
      data.loading.remove(&key);
      match stored {
        Some(store) => store(&mut data),
        None => data.maybe_error = errors.into_iter().next(),
      }
    }
    self.changed();
  }

  async fn fetch<T: serde::de::DeserializeOwned>(&self, path: &str, errors: &mut Vec<String>) -> Option<T> {
    settle(self.fetch_result(path.to_owned()).await, errors)
  }

  /// Like [`Self::fetch_result`], and `Ok(None)` without a request when `is_wanted` is false.
  async fn fetch_if<T: serde::de::DeserializeOwned>(
    &self,
    is_wanted: bool,
    path: &str,
  ) -> Result<Option<T>, String> {
    if !is_wanted {
      return Ok(None);
    }
    self.fetch_result(path.to_owned()).await.map(Some)
  }

  /// The head of the channel list: as many pages as are loaded, at most [`MAX_REFRESH_PAGES`], from
  /// the first on. A page is [`PAGE_SIZE`] channels, whatever the number of channels.
  async fn read_conversations_if(
    &self,
    is_wanted: bool,
  ) -> Result<Option<Listed<AdminConversationSummary>>, String> {
    if !is_wanted {
      return Ok(None);
    }
    let (query, paging) = {
      let data = self.data.borrow();
      (data.channel_query.clone(), data.channel_paging)
    };
    let mut list = Listed::empty(paging.generation);
    let mut maybe_after: Option<ConversationId> = None;
    for _ in 0..paging.pages.clamp(1, MAX_REFRESH_PAGES) {
      let page: ListAdminConversationsResponse =
        self.fetch_result(conversations_path(&query, maybe_after)).await?;
      maybe_after = page.conversations.last().map(|summary| summary.id);
      list.has_more = page.has_more && maybe_after.is_some();
      list.pages += 1;
      list.items.extend(page.conversations);
      if !list.has_more {
        break;
      }
    }
    Ok(Some(list))
  }

  /// The head of the user list, like [`Self::read_conversations_if`].
  async fn read_users_if(&self, is_wanted: bool) -> Result<Option<Listed<AdminUserSummary>>, String> {
    if !is_wanted {
      return Ok(None);
    }
    let (query, paging) = {
      let data = self.data.borrow();
      (data.user_query.clone(), data.user_paging)
    };
    let mut list = Listed::empty(paging.generation);
    let mut maybe_after: Option<UserId> = None;
    for _ in 0..paging.pages.clamp(1, MAX_REFRESH_PAGES) {
      let page: ListAdminUsersResponse = self
        .fetch_result(users_path(&query.text, None, maybe_after))
        .await?;
      maybe_after = page.users.last().map(|summary| summary.id);
      list.has_more = page.has_more && maybe_after.is_some();
      list.pages += 1;
      list.items.extend(page.users);
      if !list.has_more {
        break;
      }
    }
    Ok(Some(list))
  }

  /// The active owners and admins, each role on its own (the server filters by role). Few
  /// accounts, but read to the end so the roster proposals leave nobody out.
  async fn read_role_holders(&self) -> Result<Vec<AdminUserSummary>, String> {
    let mut holders = Vec::new();
    for role in ["owner", "admin"] {
      let mut maybe_after: Option<UserId> = None;
      for _ in 0..MAX_ROLE_PAGES {
        let page: ListAdminUsersResponse = self.fetch_result(users_path("", Some(role), maybe_after)).await?;
        maybe_after = page.users.last().map(|summary| summary.id);
        holders.extend(page.users.into_iter().filter(|user| !user.is_deactivated));
        if !page.has_more || maybe_after.is_none() {
          break;
        }
      }
    }
    Ok(holders)
  }

  /// The page after the last channel loaded, appended once it arrives. An answer to a query that
  /// changed meanwhile is dropped.
  async fn more_channels_now(&self) {
    let (query, paging, maybe_after) = {
      let data = self.data.borrow();
      (
        data.channel_query.clone(),
        data.channel_paging,
        data.channels.last().map(|channel| channel.id),
      )
    };
    let result = self
      .fetch_result::<ListAdminConversationsResponse>(conversations_path(&query, maybe_after))
      .await;
    {
      let mut data = self.data.borrow_mut();
      data.loading.remove(MORE_CHANNELS_KEY);
      match result {
        Ok(page) if data.channel_paging.generation == paging.generation => {
          let space_names: HashMap<SpaceId, String> = data
            .spaces
            .iter()
            .map(|space| (space.id, space.name.clone()))
            .collect();
          let more = channels_of(
            &page.conversations,
            &space_names,
            &data.legacy_details,
            &data.policy.history_default,
            data.policy.retention.keep_pinned,
          );
          let paging = data.channel_paging;
          let (items, paging) = appended(
            std::mem::take(&mut data.channels),
            paging,
            more,
            page.has_more && !page.conversations.is_empty(),
            |channel| raw_id(channel.id),
          );
          data.channels = items;
          data.channel_paging = paging;
        },
        Ok(_) => {},
        Err(error) => data.maybe_error = Some(error),
      }
    }
    self.changed();
  }

  async fn more_users_now(&self) {
    let (query, paging, maybe_after) = {
      let data = self.data.borrow();
      (
        data.user_query.clone(),
        data.user_paging,
        data.users.last().map(|user| user.user.id),
      )
    };
    let result = self
      .fetch_result::<ListAdminUsersResponse>(users_path(&query.text, None, maybe_after))
      .await;
    {
      let mut data = self.data.borrow_mut();
      data.loading.remove(MORE_USERS_KEY);
      match result {
        Ok(page) if data.user_paging.generation == paging.generation => {
          let more: Vec<AdminUser> = page.users.iter().map(user_from_wire).collect();
          let paging = data.user_paging;
          let (items, paging) = appended(
            std::mem::take(&mut data.users),
            paging,
            more,
            page.has_more && !page.users.is_empty(),
            |user| raw_id(user.user.id),
          );
          data.users = items;
          data.user_paging = paging;
        },
        Ok(_) => {},
        Err(error) => data.maybe_error = Some(error),
      }
    }
    self.changed();
  }

  /// The channels and accounts `channels` and `users` name, [`MAX_ADMIN_IDS`] per request. An ID
  /// that can't be read is asked again with the next look at the notice.
  async fn references_now(&self, channels: Vec<ConversationId>, users: Vec<UserId>) {
    let mut found_channels = Vec::new();
    let mut found_users = Vec::new();
    let mut errors = Vec::new();
    for chunk in channels.chunks(MAX_ADMIN_IDS) {
      let path = conversations_by_id_path(chunk);
      match self.fetch_result::<ListAdminConversationsResponse>(path).await {
        Ok(page) => found_channels.extend(page.conversations),
        Err(error) => {
          errors.push(error);
          let mut data = self.data.borrow_mut();
          for id in chunk {
            data.asked_channels.remove(id);
          }
        },
      }
    }
    for chunk in users.chunks(MAX_ADMIN_IDS) {
      let path = users_by_id_path(chunk);
      match self.fetch_result::<ListAdminUsersResponse>(path).await {
        Ok(page) => found_users.extend(page.users),
        Err(error) => {
          errors.push(error);
          let mut data = self.data.borrow_mut();
          for id in chunk {
            data.asked_users.remove(id);
          }
        },
      }
    }
    {
      let mut data = self.data.borrow_mut();
      let space_names: HashMap<SpaceId, String> = data
        .spaces
        .iter()
        .map(|space| (space.id, space.name.clone()))
        .collect();
      let channels = channels_of(
        &found_channels,
        &space_names,
        &data.legacy_details,
        &data.policy.history_default,
        data.policy.retention.keep_pinned,
      );
      data
        .referenced_channels
        .extend(channels.into_iter().map(|channel| (channel.id, channel)));
      data
        .referenced_users
        .extend(found_users.iter().map(|user| (user.id, user_from_wire(user))));
      if let Some(error) = errors.into_iter().next() {
        data.maybe_error = Some(error);
      }
    }
    self.changed();
  }

  /// A GET of the console. A failure is a short code the console words itself (never the path or
  /// the status); a 429 also makes the console wait for the server's `Retry-After`.
  async fn fetch_result<T: serde::de::DeserializeOwned>(&self, path: String) -> Result<T, String> {
    let response = self.auth.api_get(&path).await.map_err(|error| {
      tracing::warn!(%error, %path, "an administration read did not get an answer");
      format!("{UNREACHABLE_CODE}:")
    })?;
    if response.status != 200 {
      tracing::warn!(status = response.status, %path, "an administration read was refused");
      return Err(self.read_failure(response.status, &response.body));
    }
    serde_json::from_str(&response.body).map_err(|error| {
      tracing::warn!(%error, %path, "an administration answer could not be read");
      format!("{READ_FAILED_CODE}:malformed")
    })
  }

  fn read_failure(&self, status: u16, body: &str) -> String {
    if status == 429 {
      let seconds = retry_after_of(body);
      self.back_off(seconds);
      return format!("{RATE_LIMITED_CODE}:{seconds}");
    }
    format!("{READ_FAILED_CODE}:{status}")
  }

  /// Like [`Self::send`], and an answer of type `T` replaces the cached copy at once, so the form
  /// shows the saved state without waiting for the refresh.
  fn send_stored(&self, method: ApiMethod, path: String, maybe_body: Option<String>, store: Stored) {
    self.run(AdminTask::SendStored {
      method,
      path,
      maybe_body,
      store,
    });
  }

  async fn send_stored_now(
    &self,
    method: ApiMethod,
    path: String,
    maybe_body: Option<String>,
    store: Stored,
  ) {
    let message = match self.auth.api_request(method, &path, maybe_body).await {
      Ok(response) if response.status < 300 => {
        if let Err(error) = store.apply(&mut self.data.borrow_mut(), &response.body) {
          tracing::debug!(%error, %path, "the answer could not be read");
        }
        None
      },
      Ok(response) => Some(format!(
        "{path}: {}",
        self.refusal_of(&response.body, response.status)
      )),
      Err(error) => Some(error.to_string()),
    };
    self.data.borrow_mut().maybe_error = message;
    self.changed();
    self.queue_refresh();
  }

  fn send_brands(
    &self,
    method: ApiMethod,
    path: String,
    request: &impl serde::Serialize,
    maybe_forgotten: Option<String>,
  ) {
    if let Ok(body) = serde_json::to_string(request) {
      self.send_stored(method, path, Some(body), Stored::Brands { maybe_forgotten });
    }
  }

  /// A connection test: its answer, or the refusal, is kept for the console to show.
  fn run_settings_test(&self, kind: SettingsTestKind, path: String, body: String) {
    self.data.borrow_mut().last_test = None;
    self.run(AdminTask::SettingsTest { kind, path, body });
  }

  async fn run_settings_test_now(&self, kind: SettingsTestKind, path: String, body: String) {
    let is_webhook = matches!(kind, SettingsTestKind::Webhook(_));
    let outcome = match self.auth.api_request(ApiMethod::Post, &path, Some(body)).await {
      Ok(response) if response.status < 300 => serde_json::from_str::<SettingsTestResponse>(&response.body)
        .unwrap_or_else(|error| SettingsTestResponse {
          is_ok: false,
          maybe_error: Some(error.to_string()),
        }),
      Ok(response) => SettingsTestResponse {
        is_ok: false,
        maybe_error: Some(self.refusal_of(&response.body, response.status)),
      },
      Err(error) => SettingsTestResponse {
        is_ok: false,
        maybe_error: Some(error.to_string()),
      },
    };
    {
      let mut data = self.data.borrow_mut();
      data.last_test = Some((kind, outcome));
      if is_webhook {
        // The test is in the delivery log now.
        data.webhook_deliveries.clear();
        data.asked_webhook_deliveries.clear();
      }
    }
    self.changed();
    // Mail tests change the delivery status shown next to the form.
    self.queue_refresh();
  }

  /// Fire and forget; a refusal is shown in the console, then the next refresh shows the truth.
  fn send(&self, method: ApiMethod, path: String, maybe_body: Option<String>) {
    self.run(AdminTask::Send {
      method,
      path,
      maybe_body,
    });
  }

  /// One after the other, so a large selection doesn't hit the rate limit at once; the rest still
  /// runs after a failure, and every failure is on show.
  async fn send_each_now(&self, requests: Vec<(ApiMethod, String, Option<String>)>) {
    let mut failures = Vec::new();
    for (method, path, maybe_body) in requests {
      match self.auth.api_request(method, &path, maybe_body).await {
        Ok(response) if response.status < 300 => {},
        Ok(response) => failures.push(format!(
          "{path}: {}",
          self.refusal_of(&response.body, response.status)
        )),
        Err(error) => failures.push(format!("{path}: {error}")),
      }
    }
    self.data.borrow_mut().maybe_error = (!failures.is_empty()).then(|| failures.join("; "));
    self.queue_refresh();
  }

  async fn send_now(&self, method: ApiMethod, path: String, maybe_body: Option<String>) {
    let message = match self.auth.api_request(method, &path, maybe_body).await {
      Ok(response) if response.status < 300 => None,
      Ok(response) => Some(format!(
        "{path}: {}",
        self.refusal_of(&response.body, response.status)
      )),
      Err(error) => Some(error.to_string()),
    };
    self.data.borrow_mut().maybe_error = message;
    self.queue_refresh();
  }

  /// Runs a request of the console in the background. Its code and the answers' wire types are in
  /// the console's chunk (`areas::admin_task`), like the refresh.
  fn run(&self, task: AdminTask) {
    let live = self.clone();
    spawn_local(async move {
      match crate::areas::admin_task(live.clone(), task).await {
        Ok(running) => running.await,
        Err(error) => {
          // Nothing is under way any more: a later look asks again.
          live.data.borrow_mut().loading.clear();
          live.chunk_failed(&error.to_string());
        },
      }
    });
  }

  /// Does what [`Self::run`] was given.
  pub(crate) async fn perform(&self, task: AdminTask) {
    match task {
      AdminTask::Send {
        method,
        path,
        maybe_body,
      } => self.send_now(method, path, maybe_body).await,
      AdminTask::SendEach { requests } => self.send_each_now(requests).await,
      AdminTask::SendStored {
        method,
        path,
        maybe_body,
        store,
      } => self.send_stored_now(method, path, maybe_body, store).await,
      AdminTask::Load { key, path, what } => self.load_now(key, path, what).await,
      AdminTask::MoreChannels => self.more_channels_now().await,
      AdminTask::MoreUsers => self.more_users_now().await,
      AdminTask::References { channels, users } => self.references_now(channels, users).await,
      AdminTask::SettingsTest { kind, path, body } => self.run_settings_test_now(kind, path, body).await,
      AdminTask::BrandPreview { key, path } => self.brand_preview_now(key, path).await,
      AdminTask::UploadBrandAsset {
        id,
        slot,
        bytes,
        content_type,
      } => self.upload_brand_asset_now(id, slot, bytes, content_type).await,
    }
  }

  async fn brand_preview_now(&self, key: String, path: String) {
    let maybe_url = match self.auth.api_request_bytes(ApiMethod::Get, &path, None).await {
      Ok(response) if response.status == 200 => {
        preview_data_url(response.maybe_content_type.as_deref(), &response.body)
      },
      _ => None,
    };
    {
      let mut data = self.data.borrow_mut();
      // The loading entry stays, which is what keeps a failed image from being requested again.
      if let Some(url) = maybe_url {
        data.loading.remove(&format!("preview:{key}"));
        data.brand_previews.insert(key, url);
      }
    }
    self.changed();
  }
}

/// A request of the console, run in the background by [`AdminLive::run`].
pub enum AdminTask {
  /// Fire and forget; a refusal is shown, then the next refresh shows the truth.
  Send {
    method: ApiMethod,
    path: String,
    maybe_body: Option<String>,
  },
  /// Several, one after the other; a refusal is shown and the rest still runs.
  SendEach {
    requests: Vec<(ApiMethod, String, Option<String>)>,
  },
  /// Like `Send`, and the answer replaces the cached copy at once.
  SendStored {
    method: ApiMethod,
    path: String,
    maybe_body: Option<String>,
    store: Stored,
  },
  /// Data the console shows on demand.
  Load { key: String, path: String, what: Loaded },
  /// The next page of the channel list (issue #1241).
  MoreChannels,
  /// The next page of the user list.
  MoreUsers,
  /// Channels and accounts by ID, for the names in notices (issue #1433).
  References {
    channels: Vec<ConversationId>,
    users: Vec<UserId>,
  },
  /// A connection test.
  SettingsTest {
    kind: SettingsTestKind,
    path: String,
    body: String,
  },
  /// A brand image for the editor.
  BrandPreview { key: String, path: String },
  UploadBrandAsset {
    id: String,
    slot: BrandAssetSlot,
    bytes: Vec<u8>,
    content_type: String,
  },
}

/// Which cached copy an answer replaces.
pub enum Stored {
  /// The brands; `maybe_forgotten` is a brand whose cached images are stale now.
  Brands {
    maybe_forgotten: Option<String>,
  },
  Mail,
  Calendar,
  Runtime,
  Notetaker,
  OidcSync,
  OidcSyncPreview,
  /// A webhook was changed: the list is read again; `has_secret` answers carry a new secret.
  Webhooks {
    has_secret: bool,
  },
}

/// What on-demand data a load is for.
pub enum Loaded {
  Scheme(Option<SpaceId>),
  Moderation(ConversationId),
  Members(ConversationId),
  Devices(UserId),
  Notetaker,
  Webhooks,
  WebhookDeliveries(teamcraft_proto::WebhookId),
}

/// Stores a loaded value in the cache.
type Store = Box<dyn FnOnce(&mut AdminData)>;

impl Stored {
  /// Puts the answer `body` into the cache.
  fn apply(self, data: &mut AdminData, body: &str) -> Result<(), serde_json::Error> {
    match self {
      Self::Brands { maybe_forgotten } => {
        data.brands = Some(serde_json::from_str(body)?);
        if let Some(id) = maybe_forgotten {
          forget_previews(data, &id);
        }
      },
      Self::Mail => data.mail = Some(serde_json::from_str(body)?),
      Self::Calendar => data.calendar = Some(serde_json::from_str(body)?),
      Self::Runtime => data.runtime = Some(serde_json::from_str(body)?),
      Self::Notetaker => data.notetaker = Some(serde_json::from_str(body)?),
      Self::OidcSync => data.oidc_sync = Some(serde_json::from_str(body)?),
      Self::OidcSyncPreview => data.oidc_sync_preview = Some(serde_json::from_str(body)?),
      Self::Webhooks { has_secret } => {
        data.webhooks = None;
        data.has_asked_webhooks = false;
        data.webhook_deliveries.clear();
        data.asked_webhook_deliveries.clear();
        if has_secret {
          let answer: teamcraft_proto::WebhookSecretResponse = serde_json::from_str(body)?;
          data.revealed_webhook_secret = Some((answer.webhook.id, answer.secret));
        }
      },
    }
    Ok(())
  }
}

/// The number of checks whose status is `alert`, from the health answer, without the wire types:
/// `None` for an answer that is not one.
fn alerting_checks(body: &str) -> Option<usize> {
  let value: serde_json::Value = serde_json::from_str(body).ok()?;
  let checks = value.get("checks")?.as_array()?;
  Some(
    checks
      .iter()
      .filter(|check| check.get("status").and_then(serde_json::Value::as_str) == Some("alert"))
      .count(),
  )
}

/// What a refresh reads, as a set. The console asks for everything when it opens and after the
/// admin's own change, and for the open section's data on its timer (issue #1217).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Reads(u32);

impl Reads {
  const NONE: Reads = Reads(0);
  const SPACES: Reads = Reads(1 << 0);
  const CONVERSATIONS: Reads = Reads(1 << 1);
  const USERS: Reads = Reads(1 << 2);
  const POLICY: Reads = Reads(1 << 3);
  const ANNOUNCEMENT: Reads = Reads(1 << 4);
  const INSTANCE: Reads = Reads(1 << 5);
  const BRANDS: Reads = Reads(1 << 6);
  const MAIL: Reads = Reads(1 << 7);
  const CALENDAR: Reads = Reads(1 << 8);
  const RUNTIME: Reads = Reads(1 << 9);
  const AUDIT: Reads = Reads(1 << 10);
  /// The active owners and admins by role: a request or two, whatever the number of accounts.
  const ROLE_HOLDERS: Reads = Reads(1 << 11);
  /// One request per channel, from servers that don't send owner and retention in the list.
  const LEGACY_DETAILS: Reads = Reads(1 << 12);
  const CACHED_SCHEMES: Reads = Reads(1 << 13);
  const CACHED_MODERATION: Reads = Reads(1 << 14);
  const CACHED_DEVICES: Reads = Reads(1 << 15);
  const HEALTH: Reads = Reads(1 << 16);
  const GUESTS: Reads = Reads(1 << 17);
  const ALL: Reads = Reads((1 << 18) - 1);

  fn has(self, part: Reads) -> bool {
    self.0 & part.0 == part.0
  }

  /// What the section shows. Sections that read no list of their own show the policy, which is one
  /// small request.
  fn of(section: AdminSection) -> Reads {
    match section {
      AdminSection::Channels => {
        Self::SPACES | Self::CONVERSATIONS | Self::LEGACY_DETAILS | Self::CACHED_MODERATION
      },
      AdminSection::Users => Self::USERS | Self::SPACES | Self::CACHED_DEVICES | Self::GUESTS,
      AdminSection::Permissions => Self::SPACES | Self::CACHED_SCHEMES,
      AdminSection::Retention => Self::SPACES | Self::CONVERSATIONS | Self::POLICY | Self::LEGACY_DETAILS,
      // Names of what the entries are about: the head of the lists, and the admins who act.
      AdminSection::Audit => {
        Self::AUDIT | Self::SPACES | Self::CONVERSATIONS | Self::USERS | Self::ROLE_HOLDERS
      },
      AdminSection::Announcement => Self::ANNOUNCEMENT,
      AdminSection::Brands => Self::BRANDS,
      AdminSection::Mail => Self::MAIL,
      AdminSection::Calendar => Self::CALENDAR,
      AdminSection::Settings => Self::RUNTIME,
      AdminSection::Instance => Self::INSTANCE,
      AdminSection::Health => Self::HEALTH,
      // The owners and admins to propose for the roster are read by role, not out of the user list.
      AdminSection::Security | AdminSection::People | AdminSection::Import => {
        Self::POLICY | Self::ROLE_HOLDERS
      },
      AdminSection::Backgrounds
      | AdminSection::Updates
      | AdminSection::Integrations
      | AdminSection::Webhooks
      | AdminSection::Notetaker => Self::POLICY,
    }
  }
}

impl std::ops::BitOr for Reads {
  type Output = Reads;

  fn bitor(self, other: Reads) -> Reads {
    Reads(self.0 | other.0)
  }
}

/// The head of a list as one refresh read it.
struct Listed<T> {
  items: Vec<T>,
  /// Pages requested.
  pages: usize,
  /// The server has entries after the last one read.
  has_more: bool,
  /// The list's [`Paging::generation`] when the read started.
  generation: u64,
}

impl<T> Listed<T> {
  fn empty(generation: u64) -> Self {
    Self {
      items: Vec::new(),
      pages: 0,
      has_more: false,
      generation,
    }
  }

  fn map<U>(self, convert: impl FnOnce(Vec<T>) -> Vec<U>) -> Listed<U> {
    Listed {
      items: convert(self.items),
      pages: self.pages,
      has_more: self.has_more,
      generation: self.generation,
    }
  }
}

/// How far a paged list is read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Paging {
  /// Pages loaded so far (0: none yet).
  pages: usize,
  /// The server has entries after the last one loaded.
  has_more: bool,
  /// Counts the queries: an answer read for an earlier query is dropped.
  generation: u64,
}

impl Paging {
  /// The list starts over for a new query; what was read for the old one no longer counts.
  fn restarted(self) -> Self {
    Self {
      pages: 0,
      has_more: false,
      generation: self.generation + 1,
    }
  }
}

/// Puts the head a refresh read in place of the pages loaded before. Pages the admin loaded beyond
/// what a refresh reads again stay as they were, from the first entry after the new head on (IDs
/// ascend), so a refresh costs at most [`MAX_REFRESH_PAGES`] requests however far the admin went.
/// `None` when the head belongs to an earlier query.
fn refreshed<T: Clone>(
  current: &[T],
  paging: Paging,
  head: Listed<T>,
  key: impl Fn(&T) -> u64,
) -> Option<(Vec<T>, Paging)> {
  if head.generation != paging.generation {
    return None;
  }
  let is_tail_kept = head.has_more && paging.pages > head.pages;
  let Some(last) = head.items.last().map(&key).filter(|_| is_tail_kept) else {
    let paging = Paging {
      pages: head.pages,
      has_more: head.has_more,
      ..paging
    };
    return Some((head.items, paging));
  };
  let mut items = head.items;
  items.extend(current.iter().filter(|item| key(item) > last).cloned());
  Some((items, paging))
}

/// Adds a page read after the last entry; an entry that is there already (it moved between pages
/// while the admin read) is not shown twice.
fn appended<T>(
  mut current: Vec<T>,
  paging: Paging,
  page: Vec<T>,
  has_more: bool,
  key: impl Fn(&T) -> u64,
) -> (Vec<T>, Paging) {
  let last = current.last().map(&key);
  current.extend(
    page
      .into_iter()
      .filter(|item| last.is_none_or(|last| key(item) > last)),
  );
  (
    current,
    Paging {
      pages: paging.pages + 1,
      has_more,
      ..paging
    },
  )
}

/// A channel's owner and effective retention: from the list, or read one by one from a server whose
/// list doesn't carry them.
#[derive(Clone, Default)]
struct ChannelDetails {
  maybe_owner: Option<UserId>,
  maybe_retention: Option<RetentionSetting>,
}

/// What the list says; for a server that sent neither field, what was read for the channel.
fn details_of(
  summary: &AdminConversationSummary,
  legacy: &HashMap<ConversationId, (i64, ChannelDetails)>,
) -> ChannelDetails {
  if summary.maybe_retention.is_some() {
    return ChannelDetails {
      maybe_owner: summary.maybe_owner_user_id,
      maybe_retention: summary.maybe_retention,
    };
  }
  legacy
    .get(&summary.id)
    .map(|(_, details)| details.clone())
    .unwrap_or_default()
}

fn channels_of(
  summaries: &[AdminConversationSummary],
  space_names: &HashMap<SpaceId, String>,
  legacy: &HashMap<ConversationId, (i64, ChannelDetails)>,
  history: &HistoryPolicy,
  keeps_pinned: bool,
) -> Vec<AdminChannel> {
  summaries
    .iter()
    .map(|summary| {
      channel_from_wire(
        summary,
        space_names,
        details_of(summary, legacy),
        history.clone(),
        keeps_pinned,
      )
    })
    .collect()
}

/// Seconds to wait after a 429, from the body's `retry_after_secs` (the header's value).
fn retry_after_of(body: &str) -> u64 {
  serde_json::from_str::<ErrorBody>(body)
    .ok()
    .and_then(|error| error.maybe_retry_after_secs)
    .unwrap_or(DEFAULT_BACKOFF_SECS)
    .clamp(MIN_BACKOFF_SECS, MAX_BACKOFF_SECS)
}

fn preview_key(id: &str, slot: &BrandAssetSlot) -> String {
  format!("{id}/{}", slot.as_str())
}

/// Drops the cached images of a brand after its images changed.
fn forget_previews(data: &mut AdminData, id: &str) {
  let prefix = format!("{id}/");
  data.brand_previews.retain(|key, _| !key.starts_with(&prefix));
  data
    .loading
    .retain(|key| !key.starts_with(&format!("preview:{prefix}")));
}

/// Media types a brand image may have; anything else is not shown.
const PREVIEW_TYPES: [&str; 6] = [
  "image/png",
  "image/jpeg",
  "image/webp",
  "image/svg+xml",
  "image/x-icon",
  "image/vnd.microsoft.icon",
];

/// The image as a `data:` URL; none for a media type that is not an allowed image type.
fn preview_data_url(maybe_content_type: Option<&str>, bytes: &[u8]) -> Option<String> {
  let content_type = maybe_content_type?
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase();
  if !PREVIEW_TYPES.contains(&content_type.as_str()) || bytes.is_empty() {
    return None;
  }
  Some(format!("data:{content_type};base64,{}", STANDARD.encode(bytes)))
}

/// Why a change was refused, for the banner: `rate_limited:{seconds}` for a 429 (the console words
/// it), else the server's message.
fn refusal_text(body: &str, status: u16) -> String {
  let parsed = serde_json::from_str::<ErrorBody>(body);
  let is_rate_limited = status == 429
    || parsed
      .as_ref()
      .is_ok_and(|e| e.error_code == ErrorCode::RateLimited);
  if is_rate_limited {
    return format!("{RATE_LIMITED_CODE}:{}", retry_after_of(body));
  }
  parsed.map_or_else(|_| status.to_string(), |error| error.message)
}

// ---- Mappings (pure, tested) ------------------------------------------------------------------------

/// Puts a failed request's reason on the list of errors and gives the value of a good one.
fn settle<T>(result: Result<T, String>, errors: &mut Vec<String>) -> Option<T> {
  match result {
    Ok(value) => Some(value),
    Err(error) => {
      errors.push(error);
      None
    },
  }
}

fn space_from_wire(space: &AdminSpaceSummary) -> AdminSpace {
  AdminSpace {
    id: space.id,
    name: space.name.clone(),
    join_policy: space.join_policy.clone(),
    is_archived: space.is_archived,
    member_count: space.member_count,
    guest_count: space.guest_count,
    channel_count: space.channel_count,
    created_at: space.created_at.0,
  }
}

fn channel_from_wire(
  summary: &AdminConversationSummary,
  space_names: &HashMap<SpaceId, String>,
  details: ChannelDetails,
  history: HistoryPolicy,
  keeps_pinned: bool,
) -> AdminChannel {
  let setting = details.maybe_retention.unwrap_or_default();
  let retention = Retention {
    maybe_days: setting.maybe_max_age_days,
    maybe_max_messages: setting
      .maybe_max_messages
      .and_then(|count| u32::try_from(count).ok()),
    keeps_pinned,
  };
  AdminChannel {
    id: summary.id,
    space_name: summary
      .maybe_space_id
      .and_then(|space| space_names.get(&space).cloned())
      .unwrap_or_default(),
    name: summary.maybe_name.clone().unwrap_or_default(),
    kind: match summary.visibility {
      ConversationVisibility::Public => ConversationKind::Public,
      ConversationVisibility::Private | ConversationVisibility::Unknown(_) => ConversationKind::Private,
    },
    member_count: summary.member_count,
    guest_count: summary.guest_count,
    maybe_owner: details.maybe_owner,
    last_activity: summary.maybe_last_activity_at.unwrap_or(summary.created_at).0,
    message_count: summary.message_count,
    // Ciphertext bytes of the channel's files and search index in storage.
    storage_bytes: summary.storage_bytes,
    retention,
    history,
    is_archived: summary.is_archived,
    maybe_inactivity: summary.maybe_inactivity.clone(),
  }
}

/// `GET /admin/conversations` for one page of what the query lists.
fn conversations_path(query: &AdminChannelQuery, maybe_after: Option<ConversationId>) -> String {
  let mut path = format!("/api/v1/admin/conversations?limit={PAGE_SIZE}");
  match query.filter {
    AdminChannelFilter::All => {},
    AdminChannelFilter::Private => path.push_str("&visibility=private"),
    AdminChannelFilter::WithGuests => path.push_str("&has_guests=true"),
    AdminChannelFilter::Inactive => path.push_str(&format!("&inactive_days={INACTIVE_AFTER_DAYS}")),
    AdminChannelFilter::Orphaned => path.push_str("&orphaned=true"),
  }
  let text = query.text.trim();
  if !text.is_empty() {
    path.push_str(&format!("&query={}", encode_component(text)));
  }
  if let Some(after) = maybe_after {
    path.push_str(&format!("&after={after}"));
  }
  path
}

/// `GET /admin/users` for one page: every account (or the active ones of `maybe_role`) whose name
/// contains `text`.
fn users_path(text: &str, maybe_role: Option<&str>, maybe_after: Option<UserId>) -> String {
  let status = if maybe_role.is_some() { "active" } else { "all" };
  let mut path = format!("/api/v1/admin/users?status={status}&limit={PAGE_SIZE}");
  let text = text.trim();
  if !text.is_empty() {
    path.push_str(&format!("&query={}", encode_component(text)));
  }
  if let Some(role) = maybe_role {
    path.push_str(&format!("&role={role}"));
  }
  if let Some(after) = maybe_after {
    path.push_str(&format!("&after={after}"));
  }
  path
}

/// `GET /admin/conversations` for these channels only (at most [`MAX_ADMIN_IDS`]).
fn conversations_by_id_path(ids: &[ConversationId]) -> String {
  format!(
    "/api/v1/admin/conversations?limit={MAX_ADMIN_IDS}&ids={}",
    id_list(ids)
  )
}

/// `GET /admin/users` for these accounts only, deactivated ones too.
fn users_by_id_path(ids: &[UserId]) -> String {
  format!(
    "/api/v1/admin/users?status=all&limit={MAX_ADMIN_IDS}&ids={}",
    id_list(ids)
  )
}

/// IDs separated by commas, as the `ids` filters take them.
fn id_list<T: std::fmt::Display>(ids: &[T]) -> String {
  ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

/// `text` as a URL query value: everything but unreserved characters as `%XX` of its UTF-8 bytes.
fn encode_component(text: &str) -> String {
  let mut encoded = String::with_capacity(text.len());
  for byte in text.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => encoded.push(char::from(byte)),
      _ => encoded.push_str(&format!("%{byte:02X}")),
    }
  }
  encoded
}

/// `GET /admin/guests` with what the console's filters ask for. IDs and filter names are plain.
fn guests_path(query: &ListAdminGuestsQuery) -> String {
  let mut path = format!("/api/v1/admin/guests?limit={GUEST_LIST_LIMIT}");
  if let Some(space) = query.maybe_space_id {
    path.push_str(&format!("&space_id={space}"));
  }
  if let Some(sponsor) = query.maybe_sponsor_id {
    path.push_str(&format!("&sponsor_id={sponsor}"));
  }
  if let Some(filter) = &query.maybe_filter {
    path.push_str(&format!("&filter={}", filter.as_str()));
  }
  if let Some(text) = query.maybe_sponsor_query.as_deref().map(str::trim)
    && !text.is_empty()
  {
    path.push_str(&format!("&sponsor_query={}", encode_component(text)));
  }
  path
}

fn user_from_wire(summary: &AdminUserSummary) -> AdminUser {
  let username = summary.maybe_username.clone().unwrap_or_default();
  let role = summary
    .maybe_oidc_role
    .as_deref()
    .and_then(|name| serde_json::from_value::<Role>(serde_json::Value::String(name.to_owned())).ok())
    .unwrap_or(match summary.kind {
      UserKind::Guest => Role::Guest,
      _ => Role::Member,
    });
  AdminUser {
    user: User {
      id: summary.id,
      username: username.clone(),
      display_name: summary.display_name.clone(),
      presence: PresenceStatus::Offline,
      maybe_custom_status: None::<CustomStatus>,
      avatar_hue: hue_from_seed(&format!("{}{username}", summary.display_name)),
      is_guest: summary.kind == UserKind::Guest,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: summary.is_deleted,
      device_setup: crate::model::DeviceSetup::Unknown,
      maybe_email: None,
    },
    role,
    device_count: summary.device_count,
    // Loaded when the user's details open (`AdminLive::load_devices`).
    devices: Vec::new(),
    last_seen: summary.maybe_last_seen_at.unwrap_or(summary.created_at).0,
    space_names: Vec::new(),
    space_count: summary.space_count,
    is_active: !summary.is_deactivated,
    is_in_chats: summary.conversation_count > 0 || summary.space_count > 0 || summary.device_count > 0,
    maybe_guest: None,
  }
}

/// The devices an admin sees: the server's list without the revoked ones.
fn devices_from_wire(devices: &[AdminDeviceResponse]) -> Vec<DeviceInfo> {
  devices
    .iter()
    .filter(|device| !device.is_revoked)
    .map(|device| DeviceInfo {
      id: device.id,
      name: device.name.clone(),
      platform: String::new(),
      last_seen: device.maybe_last_seen_at.unwrap_or(device.created_at).0,
      is_current: false,
      // Admins see the server's list; verifying is for the user's own devices.
      verification: DeviceVerification::Unknown,
      maybe_push: None,
    })
    .collect()
}

/// Maps the server's action names to the console's wording; unknown ones keep their name.
pub fn action_from_wire(action: &str) -> AuditAction {
  match action {
    "policy_appended" | "policy_changed" | "roster_appended" => AuditAction::PolicyChanged,
    "retention_changed" | "retention_run" => AuditAction::RetentionChanged,
    "device_revoked" => AuditAction::DeviceRemoved,
    "user_deactivated" => AuditAction::UserDeactivated,
    "user_reactivated" => AuditAction::UserReactivated,
    "user_removed_from_chats" => AuditAction::UserRemovedFromChats,
    "user_deleted" => AuditAction::UserDeleted,
    "invite_created" => AuditAction::GuestInvited,
    "guest_extended" => AuditAction::GuestExtended,
    "channel_archived" => AuditAction::ChannelArchived,
    "channel_restored" => AuditAction::ChannelRestored,
    "conversation_member_added" | "space_member_added" => AuditAction::MemberAdded,
    "conversation_member_removed" | "space_member_removed" => AuditAction::MemberRemoved,
    "conversation_member_role_changed" | "space_member_role_changed" | "oidc_role_changed" => {
      AuditAction::MemberRoleChanged
    },
    "conversation_updated" => AuditAction::ChannelUpdated,
    "channel_deleted" | "channel_deleted_last_member" => AuditAction::ChannelDeleted,
    "channel_inactivity_notice" | "channel_inactivity_delete_notice" => AuditAction::InactivityNotice,
    "channel_auto_archived" => AuditAction::ChannelAutoArchived,
    "channel_inactivity_deleted" => AuditAction::ChannelAutoDeleted,
    "channel_inactivity_snoozed" => AuditAction::InactivitySnoozed,
    "inactivity_run_limited" => AuditAction::InactivityLimited,
    "space_archived" => AuditAction::SpaceArchived,
    "space_restored" => AuditAction::SpaceRestored,
    "space_created" => AuditAction::SpaceCreated,
    "space_updated" => AuditAction::SpaceUpdated,
    "space_deleted" => AuditAction::SpaceDeleted,
    "oidc_backchannel_logout" => AuditAction::SessionEnded,
    "permission_scheme_changed" => AuditAction::PermissionsChanged,
    "channel_moderation_set" => AuditAction::ModerationChanged,
    "announcement_set" | "announcement_cleared" => AuditAction::AnnouncementChanged,
    "channel_owner_assigned" | "owner_assigned" => AuditAction::OwnerAssigned,
    "brand_created"
    | "brand_updated"
    | "brand_deleted"
    | "brand_taken_over"
    | "brand_asset_set"
    | "brand_asset_removed" => AuditAction::BrandChanged,
    "mail_settings_updated"
    | "mail_settings_reset"
    | "mail_test_sent"
    | "brand_mail_server_updated"
    | "brand_mail_server_removed"
    | "brand_mail_test_sent" => AuditAction::MailSettingsChanged,
    "calendar_settings_updated" | "calendar_settings_reset" | "calendar_test" => {
      AuditAction::CalendarSettingsChanged
    },
    "settings_updated" | "settings_reset" => AuditAction::SettingsChanged,
    _ => AuditAction::Other,
  }
}

fn audit_from_wire(
  entry: &AuditEntryResponse,
  spaces: &HashMap<SpaceId, String>,
  channels: &[AdminChannel],
  users: &[AdminUser],
) -> AuditEntry {
  let action = action_from_wire(&entry.action);
  let target = target_name(entry, spaces, channels, users);
  let detail = if action == AuditAction::Other {
    format!("{} {}", entry.action, entry.detail)
  } else {
    entry.detail.clone()
  };
  AuditEntry {
    id: raw_id(entry.id),
    at: entry.created_at.0,
    actor: entry.maybe_actor_user_id.unwrap_or_else(|| id_from_raw(0)),
    action,
    target,
    detail,
  }
}

fn target_name(
  entry: &AuditEntryResponse,
  spaces: &HashMap<SpaceId, String>,
  channels: &[AdminChannel],
  users: &[AdminUser],
) -> String {
  let (Some(kind), Some(raw)) = (
    entry.maybe_target_kind.as_deref(),
    entry.maybe_target_id.as_deref(),
  ) else {
    return String::new();
  };
  let Ok(number) = raw.parse::<u64>() else {
    return format!("{kind} {raw}");
  };
  let found = match kind {
    "conversation" | "channel" => channels
      .iter()
      .find(|channel| raw_id(channel.id) == number)
      .map(|channel| format!("#{}", channel.name)),
    "space" => spaces
      .iter()
      .find(|(id, _)| raw_id(**id) == number)
      .map(|(_, name)| name.clone()),
    "user" => users
      .iter()
      .find(|user| raw_id(user.user.id) == number)
      .map(|user| user.user.display_name.clone()),
    _ => None,
  };
  found.unwrap_or_else(|| format!("{kind} {number}"))
}

/// The audit request for the filter; the server filters by actor and time.
fn audit_path(filter: AuditFilter) -> String {
  let mut path = format!("/api/v1/admin/audit?limit={LIST_LIMIT}");
  if let Some(actor) = filter.maybe_actor {
    path.push_str(&format!("&actor={actor}"));
  }
  if let Some(since) = filter.maybe_since {
    path.push_str(&format!("&since={since}"));
  }
  if let Some(until) = filter.maybe_until {
    path.push_str(&format!("&until={until}"));
  }
  path
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{DeactivationReason, UnixMillis};

  #[test]
  fn the_audit_request_carries_the_filter() {
    assert_eq!(
      audit_path(AuditFilter::default()),
      "/api/v1/admin/audit?limit=200"
    );
    let filter = AuditFilter {
      maybe_actor: Some(id_from_raw(7)),
      maybe_since: Some(1000),
      maybe_until: Some(2000),
    };
    let path = audit_path(filter);
    assert!(path.contains("&actor=7") && path.contains("&since=1000") && path.contains("&until=2000"));
  }

  #[test]
  fn server_actions_get_the_consoles_wording() {
    assert_eq!(action_from_wire("user_deactivated"), AuditAction::UserDeactivated);
    assert_eq!(action_from_wire("device_revoked"), AuditAction::DeviceRemoved);
    assert_eq!(action_from_wire("space_member_added"), AuditAction::MemberAdded);
    assert_eq!(action_from_wire("something_new"), AuditAction::Other);
    assert_eq!(action_from_wire("brand_asset_set"), AuditAction::BrandChanged);
    assert_eq!(
      action_from_wire("mail_test_sent"),
      AuditAction::MailSettingsChanged
    );
    assert_eq!(
      action_from_wire("calendar_test"),
      AuditAction::CalendarSettingsChanged
    );
    assert_eq!(action_from_wire("settings_reset"), AuditAction::SettingsChanged);
  }

  #[test]
  fn users_carry_role_devices_and_state() {
    let summary = AdminUserSummary {
      id: id_from_raw(5),
      maybe_username: Some("carol".into()),
      display_name: "Carol Member".into(),
      kind: UserKind::Member,
      maybe_oidc_role: Some("admin".into()),
      is_deactivated: true,
      is_deleted: false,
      maybe_deactivation_reason: Some(DeactivationReason::Admin),
      device_count: 2,
      space_count: 1,
      conversation_count: 3,
      maybe_last_seen_at: None,
      created_at: UnixMillis(1000),
    };
    let devices = vec![
      AdminDeviceResponse {
        id: id_from_raw(8),
        name: "Laptop".into(),
        is_revoked: false,
        maybe_push_provider: None,
        maybe_last_seen_at: Some(UnixMillis(5000)),
        created_at: UnixMillis(1000),
      },
      AdminDeviceResponse {
        id: id_from_raw(9),
        name: "Old phone".into(),
        is_revoked: true,
        maybe_push_provider: None,
        maybe_last_seen_at: None,
        created_at: UnixMillis(1000),
      },
    ];
    let user = user_from_wire(&summary);
    assert_eq!(user.role, Role::Admin);
    assert!(!user.is_active);
    assert!(user.is_in_chats);
    // The list carries the count; the devices load with the user's details, revoked ones left out.
    assert_eq!(user.device_count, 2);
    assert!(user.devices.is_empty());
    let loaded = devices_from_wire(&devices);
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].last_seen, 5000);
    assert_eq!(user.last_seen, 1000);
  }

  #[test]
  fn audit_targets_are_named_when_known() {
    let channels = vec![AdminChannel {
      id: id_from_raw(21),
      space_name: "Space A".into(),
      name: "launch".into(),
      kind: ConversationKind::Private,
      member_count: 2,
      guest_count: 0,
      maybe_owner: None,
      last_activity: 0,
      message_count: 0,
      storage_bytes: 0,
      retention: Retention {
        maybe_days: None,
        maybe_max_messages: None,
        keeps_pinned: true,
      },
      history: HistoryPolicy::Full,
      is_archived: false,
      maybe_inactivity: None,
    }];
    let entry = AuditEntryResponse {
      id: id_from_raw(1),
      maybe_actor_user_id: None,
      action: "conversation_updated".into(),
      maybe_target_kind: Some("conversation".into()),
      maybe_target_id: Some("21".into()),
      detail: "{}".into(),
      created_at: UnixMillis(10),
    };
    let mapped = audit_from_wire(&entry, &HashMap::new(), &channels, &[]);
    assert_eq!(mapped.target, "#launch");
    assert_eq!(mapped.action, AuditAction::ChannelUpdated);
    let unknown = AuditEntryResponse {
      maybe_target_id: Some("999".into()),
      ..entry
    };
    assert_eq!(
      audit_from_wire(&unknown, &HashMap::new(), &channels, &[]).target,
      "conversation 999"
    );
  }

  #[test]
  fn previews_are_data_urls_of_image_types_only() {
    assert_eq!(
      preview_data_url(Some("image/png; charset=binary"), &[1, 2, 3]).as_deref(),
      Some("data:image/png;base64,AQID")
    );
    assert_eq!(preview_data_url(Some("text/html"), &[1]), None);
    assert_eq!(preview_data_url(None, &[1]), None);
    assert_eq!(preview_data_url(Some("image/png"), &[]), None);
  }

  #[test]
  fn changed_brands_forget_their_cached_images() {
    let mut data = AdminData::default();
    data.brand_previews.insert("brand-a/logo".into(), "data:x".into());
    data
      .brand_previews
      .insert("brand-ab/logo".into(), "data:y".into());
    data.loading.insert("preview:brand-a/icon".into());
    forget_previews(&mut data, "brand-a");
    assert!(!data.brand_previews.contains_key("brand-a/logo"));
    assert!(data.brand_previews.contains_key("brand-ab/logo"));
    assert!(data.loading.is_empty());
  }

  #[test]
  fn channels_use_the_effective_retention() {
    let summary = AdminConversationSummary {
      id: id_from_raw(21),
      maybe_space_id: Some(id_from_raw(4)),
      maybe_name: Some("launch".into()),
      visibility: ConversationVisibility::Public,
      is_archived: false,
      member_count: 3,
      guest_count: 1,
      message_count: 40,
      storage_bytes: 2048,
      created_at: UnixMillis(100),
      maybe_last_activity_at: None,
      maybe_owner_user_id: None,
      maybe_retention: None,
      maybe_inactivity: None,
    };
    let status = RetentionSetting {
      maybe_max_age_days: Some(30),
      maybe_max_messages: None,
    };
    let names = HashMap::from([(id_from_raw(4), "Space A".to_owned())]);
    let channel = channel_from_wire(
      &summary,
      &names,
      ChannelDetails {
        maybe_owner: Some(id_from_raw(1)),
        maybe_retention: Some(status),
      },
      HistoryPolicy::Full,
      false,
    );
    assert_eq!(channel.space_name, "Space A");
    assert_eq!(channel.retention.maybe_days, Some(30));
    assert!(!channel.retention.keeps_pinned);
    assert_eq!(channel.last_activity, 100);
    assert_eq!(
      channel.storage_bytes, 2048,
      "the storage column shows what the server counted"
    );
    assert_eq!(channel.maybe_owner, Some(id_from_raw(1)));
  }

  mod requests {
    //! What the console asks the server for (issue #1217): an open console on a large instance must
    //! stay far below the address's rate limit, whatever the number of channels and users.

    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use teamcraft_proto::{JoinPolicy, WorkspacePolicy};

    use super::*;
    use crate::auth::{ApiResponse, AuthError, LocalFuture, SignInLook};

    const CHANNELS: u64 = 450;
    const USERS: u64 = 300;
    const SPACES: u64 = 12;
    /// The server's rate limit per address and minute (`ip_requests_per_minute`).
    const IP_REQUESTS_PER_MINUTE: usize = 1200;
    /// The console's timer.
    const REFRESHES_PER_MINUTE: usize = 2;

    #[test]
    fn opening_the_console_on_a_large_instance_reads_the_lists_and_nothing_per_channel_or_user() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());

      let paths = server.paths();
      // Twelve lists and settings, the audit log, the guests and the owners and admins by role:
      // the same 17 requests however many channels, users and spaces there are (it used to be
      // about 650, then 28, with a page of 200 repeated up to 5 times for each list).
      assert!(
        paths.len() <= 12 + 1 + 1 + 2,
        "{} requests: {paths:?}",
        paths.len()
      );
      assert_no_detail_requests(&paths);
      assert!(
        !paths.iter().any(|path| path.ends_with("/members")),
        "no request per space: {paths:?}"
      );
      assert_eq!(
        live.channels().len(),
        PAGE_SIZE as usize,
        "the first page of channels, not all of them"
      );
      assert!(live.channels_have_more());
      assert_eq!(live.users().len(), PAGE_SIZE as usize);
      assert!(live.users_have_more());
      assert_eq!(live.channels()[7].maybe_owner, Some(id_from_raw(8)));
      assert_eq!(live.channels()[7].retention.maybe_days, Some(30));
    }

    #[test]
    fn the_timer_reads_only_what_the_open_section_shows() {
      for section in AdminSection::ALL {
        let server = FakeServer::new();
        let live = AdminLive::new(server.clone());
        block_on(live.refresh_once());
        server.clear();

        live.wanted.set(Reads::of(section));
        block_on(live.refresh_once());
        let paths = server.paths();
        assert_no_detail_requests(&paths);
        assert!(
          paths.len() <= 8,
          "{section:?}: {} requests: {paths:?}",
          paths.len()
        );
        if section != AdminSection::Audit {
          assert!(
            !paths.iter().any(|path| path.contains("/audit")),
            "{section:?}: {paths:?}"
          );
        }
        // The health is read by its own page, and nothing else is read for it (issue #213).
        let reads_health = paths.iter().any(|path| path == HEALTH_PATH);
        assert_eq!(
          reads_health,
          section == AdminSection::Health,
          "{section:?}: {paths:?}"
        );
        if section == AdminSection::Health {
          assert_eq!(paths, [HEALTH_PATH, OIDC_SYNC_PATH], "two small requests");
        }
      }
    }

    #[test]
    fn the_timer_the_tests_assume_is_the_one_the_console_runs() {
      assert_eq!(
        REFRESHES_PER_MINUTE as u64,
        60_000 / crate::components::admin_console::REFRESH_MS
      );
    }

    #[test]
    fn the_health_page_shows_what_it_read_and_counts_the_alerts_for_the_badges() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      let health = live.health().expect("read with the console");
      assert_eq!(health.checks.len(), 3);
      assert_eq!(live.open_alert_count(), 2);
    }

    #[test]
    fn an_alert_pushed_while_another_page_is_open_costs_one_small_read_and_no_console_reads() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      live.maybe_open_section.set(Some(AdminSection::Channels));
      live.open_alert_count.set(0);
      server.clear();

      block_on(live.read_health_count());

      assert_eq!(server.paths(), [HEALTH_PATH]);
      assert_eq!(live.open_alert_count(), 2);
    }

    #[test]
    fn the_count_for_the_badges_is_left_alone_when_the_answer_is_refused() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      live.open_alert_count.set(3);
      server.refuse_everything_with(
        429,
        r#"{"error_code":"rate_limited","message":"x","retry_after_secs":5}"#,
      );

      block_on(live.read_health_count());

      assert_eq!(
        live.open_alert_count(),
        3,
        "a refusal says nothing about the alerts"
      );
      assert!(live.is_quiet(), "and the console waits, as for any read");
    }

    #[test]
    fn the_alert_count_is_read_from_the_checks_and_nothing_else() {
      assert_eq!(
        alerting_checks(r#"{"checks":[{"status":"alert"},{"status":"ok"},{"status":"alert"}]}"#),
        Some(2)
      );
      assert_eq!(alerting_checks(r#"{"checks":[]}"#), Some(0));
      assert_eq!(alerting_checks("<html>login</html>"), None);
      assert_eq!(alerting_checks(r#"{"checks":"later"}"#), None);
    }

    #[test]
    fn an_open_console_stays_far_below_the_rate_limit_of_the_address() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      // The slowest section to refresh: the one with the most lists.
      let busiest = AdminSection::ALL
        .into_iter()
        .map(|section| {
          server.clear();
          live.wanted.set(Reads::of(section));
          block_on(live.refresh_once());
          server.paths().len()
        })
        .max()
        .unwrap_or(0);
      let per_minute = busiest * REFRESHES_PER_MINUTE;
      // Before: about 2 * min(channels, 100) + one per user + one per space per refresh.
      assert!(
        per_minute * 20 <= IP_REQUESTS_PER_MINUTE,
        "{per_minute} requests per minute of an open console"
      );
    }

    #[test]
    fn a_server_without_owner_and_retention_in_the_list_is_asked_per_channel_once() {
      let server = FakeServer::legacy();
      let live = AdminLive::new(server.clone());
      live.wanted.set(Reads::of(AdminSection::Retention));
      block_on(live.refresh_once());
      let first = server.paths();
      let per_channel = first
        .iter()
        .filter(|path| {
          (path.contains("/members") && !path.contains("/spaces/")) || path.ends_with("/retention")
        })
        .count();
      assert_eq!(
        per_channel,
        2 * LEGACY_DETAIL_LIMIT,
        "owner and retention of the first channels"
      );
      assert_eq!(live.channels()[3].retention.maybe_days, Some(30));
      assert_eq!(live.channels()[3].maybe_owner, Some(id_from_raw(4)));

      // The next look finds them cached; another section doesn't ask at all.
      server.clear();
      live.wanted.set(Reads::of(AdminSection::Retention));
      block_on(live.refresh_once());
      assert_no_detail_requests(&server.paths());
      server.clear();
      live.wanted.set(Reads::of(AdminSection::Announcement));
      block_on(live.refresh_once());
      assert_eq!(server.paths(), vec!["/api/v1/announcement".to_owned()]);
    }

    #[test]
    fn a_refusal_with_a_wait_stops_the_reading_and_is_worded_for_the_banner() {
      let server = FakeServer::new();
      server.refuse_everything_with(
        429,
        r#"{"error_code":"rate_limited","message":"too many","retry_after_secs":42}"#,
      );
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());

      // Only the first round of reads went out (the twelve lists); nothing followed once the server said to wait.
      assert!(server.paths().len() <= 12, "{:?}", server.paths());
      assert!(live.is_quiet());
      let detail = live.maybe_error().unwrap_or_default();
      assert_eq!(detail, "rate_limited:42");
      assert_eq!(
        crate::admin::admin_error_i18n_key(&detail),
        Some("admin-error-rate-limited")
      );
      // The same reads are wanted again once the server allows.
      assert!(live.wanted.get().has(Reads::ALL));
    }

    #[test]
    fn the_wait_comes_from_the_refusal_and_stays_within_bounds() {
      assert_eq!(
        retry_after_of(r#"{"error_code":"rate_limited","message":"m","retry_after_secs":7}"#),
        7
      );
      assert_eq!(
        retry_after_of(r#"{"error_code":"rate_limited","message":"m","retry_after_secs":0}"#),
        MIN_BACKOFF_SECS
      );
      assert_eq!(
        retry_after_of(r#"{"error_code":"rate_limited","message":"m","retry_after_secs":99999}"#),
        MAX_BACKOFF_SECS
      );
      assert_eq!(retry_after_of("<html>busy</html>"), DEFAULT_BACKOFF_SECS);
      assert_eq!(refusal_text("<html>", 429), "rate_limited:30");
      assert_eq!(refusal_text("<html>", 502), "502");
    }

    #[test]
    fn a_long_list_loads_a_page_at_a_time_and_more_on_request() {
      let server = FakeServer::with_channels(PAGE_SIZE as u64 * 4 + 50);
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      assert_eq!(live.channels().len(), PAGE_SIZE as usize);
      assert!(live.channels_have_more());
      for _ in 0..4 {
        more_channels(&live);
      }
      let channels = live.channels();
      assert_eq!(channels.len(), PAGE_SIZE as usize * 4 + 50);
      assert!(!live.channels_have_more());
      assert!(
        channels.windows(2).all(|pair| pair[0].id < pair[1].id),
        "in order, none twice"
      );
      let after: Vec<_> = server
        .paths()
        .into_iter()
        .filter(|path| path.contains("after="))
        .collect();
      assert_eq!(after.len(), 4, "one request per page: {after:?}");
    }

    #[test]
    fn the_timer_reads_the_head_of_what_was_loaded_and_keeps_the_rest() {
      let server = FakeServer::with_channels(PAGE_SIZE as u64 * 6 + 20);
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      for _ in 0..5 {
        more_channels(&live);
      }
      assert_eq!(live.channels().len(), PAGE_SIZE as usize * 6);
      server.clear();

      live.wanted.set(Reads::of(AdminSection::Channels));
      block_on(live.refresh_once());
      let pages = server
        .paths()
        .into_iter()
        .filter(|path| path.starts_with("/api/v1/admin/conversations?"))
        .count();
      assert_eq!(
        pages, MAX_REFRESH_PAGES,
        "the head, not the six pages the admin went through"
      );
      let channels = live.channels();
      assert_eq!(
        channels.len(),
        PAGE_SIZE as usize * 6,
        "what was loaded beyond stays"
      );
      assert!(channels.windows(2).all(|pair| pair[0].id < pair[1].id));
      assert!(live.channels_have_more());
    }

    #[test]
    fn the_server_searches_and_the_list_starts_over() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      more_channels(&live);
      server.clear();

      let query = AdminChannelQuery {
        text: " channel-12 ".into(),
        filter: AdminChannelFilter::Orphaned,
      };
      assert!(live.apply_channel_query(query.clone()));
      assert!(
        !live.apply_channel_query(query),
        "the same query is not asked again"
      );
      live.wanted.set(Reads::CONVERSATIONS);
      block_on(live.refresh_once());

      assert_eq!(
        server.paths(),
        ["/api/v1/admin/conversations?limit=100&orphaned=true&query=channel-12"],
        "one request: the first page of the search"
      );
      let names: Vec<String> = live.channels().into_iter().map(|channel| channel.name).collect();
      assert_eq!(
        names.len(),
        11,
        "channel-12 and channel-120 to channel-129: {names:?}"
      );
      assert!(names.iter().all(|name| name.starts_with("channel-12")));
      assert!(!live.channels_have_more());
    }

    #[test]
    fn users_are_searched_by_the_server_too() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      server.clear();
      assert!(live.apply_user_query(AdminUserQuery {
        text: "Person 29".into(),
      }));
      live.wanted.set(Reads::USERS);
      block_on(live.refresh_once());
      assert_eq!(
        server.paths(),
        ["/api/v1/admin/users?status=all&limit=100&query=Person%2029"]
      );
      let names: Vec<String> = live
        .users()
        .into_iter()
        .map(|user| user.user.display_name)
        .collect();
      assert_eq!(names.len(), 11, "Person 29 and Person 290 to 299: {names:?}");
      assert!(names.iter().all(|name| name.starts_with("Person 29")));
    }

    #[test]
    fn owners_and_admins_come_by_role_not_out_of_the_user_list() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      let paths = server.paths();
      assert!(paths.iter().any(|path| path.contains("role=owner")), "{paths:?}");
      assert!(paths.iter().any(|path| path.contains("role=admin")), "{paths:?}");
      // Person 250 is an admin and on none of the first page of 100.
      let (owners, admins) = live.roster_candidates();
      assert_eq!(owners, [id_from_raw::<UserId>(1)]);
      assert_eq!(admins, [id_from_raw::<UserId>(250)]);
      assert!(
        live
          .known_users()
          .iter()
          .any(|user| user.user.id == id_from_raw::<UserId>(250)),
        "known by name for the audit log and the security pages"
      );
      assert!(
        live
          .users()
          .iter()
          .all(|user| user.user.id != id_from_raw::<UserId>(250))
      );
    }

    #[test]
    fn a_failed_page_keeps_the_list_and_says_so() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      server.refuse_everything_with(502, "<html>");
      more_channels(&live);
      assert_eq!(live.channels().len(), PAGE_SIZE as usize);
      assert!(live.channels_have_more(), "asking again is possible");
      assert!(live.maybe_error().is_some());
      assert!(!live.is_loading_more());
    }

    #[test]
    fn the_head_of_a_list_replaces_the_pages_it_read_and_the_rest_stays() {
      let key = |number: &u64| *number;
      let paging = Paging {
        pages: 4,
        has_more: true,
        generation: 2,
      };
      let head = |items: Vec<u64>, has_more: bool| Listed {
        items,
        pages: 3,
        has_more,
        generation: 2,
      };
      let current: Vec<u64> = (1..=400).collect();

      // 3 and 4 are gone, 1000 is new; pages beyond the head are kept from its last entry on.
      let mut read: Vec<u64> = (1..=300).filter(|number| *number != 3 && *number != 4).collect();
      read.push(301);
      let (items, kept) = refreshed(&current, paging, head(read, true), key).expect("same query");
      assert_eq!(items.len(), 298 + 1 + 99);
      assert_eq!(items.last(), Some(&400));
      assert!(!items.contains(&3));
      assert_eq!(kept, paging);

      // The list ended inside the head (channels were deleted): the head is all there is.
      let (items, kept) =
        refreshed(&current, paging, head((1..=250).collect(), false), key).expect("same query");
      assert_eq!(items.len(), 250);
      assert_eq!(
        kept,
        Paging {
          pages: 3,
          has_more: false,
          generation: 2
        }
      );

      // The admin searched for something else meanwhile.
      let stale = Listed {
        generation: 1,
        ..head(vec![1], true)
      };
      assert!(refreshed(&current, paging, stale, key).is_none());
    }

    #[test]
    fn a_page_is_added_after_the_last_entry_and_never_twice() {
      let paging = Paging {
        pages: 1,
        has_more: true,
        generation: 0,
      };
      // 98 and 99 moved up while the admin read: the page starts before the end of the list.
      let (items, next) = appended(
        (1..=99).collect::<Vec<u64>>(),
        paging,
        (98..=200).collect(),
        true,
        |n| *n,
      );
      assert_eq!(items.len(), 200);
      assert_eq!(next.pages, 2);
      assert!(next.has_more);
      let (_, next) = appended(vec![1_u64], next, Vec::new(), false, |n| *n);
      assert!(!next.has_more);
    }

    #[test]
    fn names_on_pages_that_are_not_loaded_are_read_by_id() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      let late_channel = id_from_raw::<ConversationId>(CHANNELS);
      let late_user = id_from_raw::<UserId>(USERS - 1);
      assert!(!live.channels().iter().any(|channel| channel.id == late_channel));
      assert!(!live.known_users().iter().any(|user| user.user.id == late_user));
      server.clear();

      let (channels, users) = live.unasked_references(vec![late_channel, late_channel], vec![late_user]);
      assert_eq!((channels.len(), users.len()), (1, 1), "each once");
      block_on(live.references_now(channels, users));
      assert_eq!(
        server.paths(),
        [
          format!("/api/v1/admin/conversations?limit=100&ids={late_channel}"),
          format!("/api/v1/admin/users?status=all&limit=100&ids={late_user}"),
        ]
      );
      let named: Vec<String> = live
        .referenced_channels()
        .into_iter()
        .map(|channel| channel.name)
        .collect();
      assert_eq!(named, [format!("channel-{CHANNELS}")]);
      assert!(live.known_users().iter().any(|user| user.user.id == late_user));

      // Asked and answered: nothing more is read, and what the pages hold isn't asked for.
      let first_channel = live.channels()[0].id;
      assert_eq!(
        live.unasked_references(vec![late_channel, first_channel], vec![late_user]),
        (Vec::new(), Vec::new())
      );
    }

    #[test]
    fn many_references_are_read_in_requests_of_at_most_the_server_limit() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      server.clear();
      let users: Vec<UserId> = (1..=MAX_ADMIN_IDS as u64 + 1).map(id_from_raw).collect();
      block_on(live.references_now(Vec::new(), users));
      assert_eq!(server.paths().len(), 2);
    }

    #[test]
    fn a_reference_that_could_not_be_read_is_asked_again_later() {
      let server = FakeServer::new();
      let live = AdminLive::new(server.clone());
      let channel = id_from_raw::<ConversationId>(CHANNELS);
      let (channels, _) = live.unasked_references(vec![channel], Vec::new());
      server.refuse_everything_with(502, "bad gateway");
      block_on(live.references_now(channels, Vec::new()));
      assert!(live.maybe_error().is_some());
      assert!(live.referenced_channels().is_empty());
      let (again, _) = live.unasked_references(vec![channel], Vec::new());
      assert_eq!(again, [channel]);
    }

    #[test]
    fn the_guest_search_goes_to_the_server_as_a_url_value() {
      let path = guests_path(&ListAdminGuestsQuery {
        maybe_sponsor_query: Some(" Al&ice ".into()),
        ..ListAdminGuestsQuery::default()
      });
      assert_eq!(path, "/api/v1/admin/guests?limit=200&sponsor_query=Al%26ice");
      let blank = guests_path(&ListAdminGuestsQuery {
        maybe_sponsor_query: Some("  ".into()),
        ..ListAdminGuestsQuery::default()
      });
      assert_eq!(blank, "/api/v1/admin/guests?limit=200");
    }

    #[test]
    fn search_text_is_sent_as_a_url_value() {
      assert_eq!(encode_component("a b&c=d#é"), "a%20b%26c%3Dd%23%C3%A9");
      assert_eq!(encode_component("Ab-9_.~"), "Ab-9_.~");
      let path = conversations_path(
        &AdminChannelQuery {
          text: "x&y".into(),
          filter: AdminChannelFilter::Inactive,
        },
        Some(id_from_raw(7)),
      );
      assert_eq!(
        path,
        "/api/v1/admin/conversations?limit=100&inactive_days=30&query=x%26y&after=7"
      );
      assert_eq!(
        conversations_path(&AdminChannelQuery::default(), None),
        "/api/v1/admin/conversations?limit=100"
      );
      assert_eq!(
        users_path("", Some("admin"), None),
        "/api/v1/admin/users?status=active&limit=100&role=admin"
      );
    }

    fn more_channels(live: &AdminLive) {
      live.data.borrow_mut().loading.insert(MORE_CHANNELS_KEY.into());
      block_on(live.more_channels_now());
    }

    fn assert_no_detail_requests(paths: &[String]) {
      let per_object = paths.iter().filter(|path| {
        path.contains("/members") && !path.contains("/spaces/")
          || path.ends_with("/retention")
          || path.ends_with("/devices")
      });
      assert_eq!(per_object.count(), 0, "per channel or per user: {paths:?}");
    }

    /// A server with a large instance that answers the console's reads and records them.
    #[derive(Clone)]
    struct FakeServer {
      inner: Rc<FakeInner>,
    }

    struct FakeInner {
      requests: RefCell<Vec<String>>,
      channels: u64,
      is_legacy: bool,
      maybe_refusal: RefCell<Option<(u16, String)>>,
    }

    impl FakeServer {
      fn new() -> Rc<Self> {
        Self::build(CHANNELS, false)
      }

      fn legacy() -> Rc<Self> {
        Self::build(CHANNELS, true)
      }

      fn with_channels(channels: u64) -> Rc<Self> {
        Self::build(channels, false)
      }

      fn build(channels: u64, is_legacy: bool) -> Rc<Self> {
        Rc::new(Self {
          inner: Rc::new(FakeInner {
            requests: RefCell::new(Vec::new()),
            channels,
            is_legacy,
            maybe_refusal: RefCell::new(None),
          }),
        })
      }

      fn paths(&self) -> Vec<String> {
        self.inner.requests.borrow().clone()
      }

      fn clear(&self) {
        self.inner.requests.borrow_mut().clear();
      }

      fn refuse_everything_with(&self, status: u16, body: &str) {
        *self.inner.maybe_refusal.borrow_mut() = Some((status, body.to_owned()));
      }

      fn answer(&self, path: &str) -> (u16, String) {
        if let Some((status, body)) = self.inner.maybe_refusal.borrow().clone() {
          return (status, body);
        }
        let json = |value: &dyn erased::Json| (200, value.to_json());
        if path == "/api/v1/admin/spaces" {
          return json(&ListAdminSpacesResponse {
            spaces: (1..=SPACES).map(space).collect(),
          });
        }
        if path.starts_with("/api/v1/admin/conversations?") {
          let needle = text_param(path, "query");
          let ids = ids_param(path);
          let (items, has_more) = page(
            path,
            (1..=self.inner.channels).filter(|n| {
              needle
                .as_ref()
                .is_none_or(|needle| format!("channel-{n}").contains(needle.as_str()))
                && ids.as_ref().is_none_or(|ids| ids.contains(n))
            }),
            |n| conversation(n, self.inner.is_legacy),
          );
          return json(&ListAdminConversationsResponse {
            conversations: items,
            has_more,
          });
        }
        if path.starts_with("/api/v1/admin/users?") {
          let needle = text_param(path, "query");
          let role = text_param(path, "role");
          let ids = ids_param(path);
          let (items, has_more) = page(
            path,
            (1..=USERS).filter(|n| {
              ids.as_ref().is_none_or(|ids| ids.contains(n))
                && needle
                  .as_ref()
                  .is_none_or(|needle| format!("Person {n}").contains(needle.as_str()))
                && role
                  .as_ref()
                  .is_none_or(|role| role_of(*n) == Some(role.as_str()))
            }),
            user,
          );
          return json(&ListAdminUsersResponse {
            users: items,
            has_more,
          });
        }
        if path == "/api/v1/workspace/policy" {
          return json(&WorkspacePolicyResponse {
            version: 1,
            policy: WorkspacePolicy::default(),
            maybe_entry: None,
          });
        }
        if path == OIDC_SYNC_PATH {
          return (
            200,
            r#"{"state":"not_configured","mode":"apply","interval_secs":0,"stale_after_intervals":0,"brake":{"max_percent":10,"min_allowed":2},"is_stale":false,"held":[],"held_revision":0,"pending_removals":0,"unfinished_deactivations":0}"#
              .to_owned(),
          );
        }
        if path == HEALTH_PATH {
          return (
            200,
            r#"{"checks":[{"check":"database","status":"ok"},{"check":"backup","status":"alert","failing_since":1,"value":172800},{"check":"certificate","status":"alert","value":3}],"alerts":[],"is_alerting_enabled":true,"hold_secs":600,"started_at":1}"#
              .to_owned(),
          );
        }
        if path.starts_with("/api/v1/admin/audit") {
          return json(&ListAuditResponse {
            entries: Vec::new(),
            has_more: false,
          });
        }
        if path.starts_with("/api/v1/admin/conversations/") && path.ends_with("/members") {
          let number = id_in(path);
          return (
            200,
            format!(
              r#"{{"members":[{{"user":{},"role":"admin"}}]}}"#,
              serde_json::to_string(&user_summary_json(number)).unwrap_or_default()
            ),
          );
        }
        if path.starts_with("/api/v1/conversations/") && path.ends_with("/retention") {
          return (
            200,
            r#"{"own":{},"effective":{"max_age_days":30},"limit":{},"keep_pinned":true}"#.to_owned(),
          );
        }
        // Lists the test doesn't model: the console copes with a refusal.
        (404, "{}".to_owned())
      }
    }

    impl Authenticator for FakeServer {
      fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
        Box::pin(async { Ok(true) })
      }

      fn login(&self, _look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
        Box::pin(async { Ok(()) })
      }

      fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
        Box::pin(async { Ok(()) })
      }

      fn api_request(
        &self,
        _method: ApiMethod,
        path: &str,
        _maybe_body: Option<String>,
      ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
        self.inner.requests.borrow_mut().push(path.to_owned());
        let (status, body) = self.answer(path);
        Box::pin(async move { Ok(ApiResponse { status, body }) })
      }
    }

    /// Serialization behind one name, so `answer` can return any response type.
    mod erased {
      pub trait Json {
        fn to_json(&self) -> String;
      }

      impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> String {
          serde_json::to_string(self).unwrap_or_default()
        }
      }
    }

    /// The page of `total` numbered entries a list request asks for: `limit` entries after `after`.
    /// One page of the numbers that match, after the number the request names.
    fn page<T>(path: &str, matching: impl Iterator<Item = u64>, make: impl Fn(u64) -> T) -> (Vec<T>, bool) {
      let limit = text_param(path, "limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(50);
      // IDs are the numbers 1.. in order, so "after" is the last number of the previous page.
      let after = text_param(path, "after")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
      let mut rest: Vec<u64> = matching
        .filter(|number| *number > after)
        .take(limit + 1)
        .collect();
      let has_more = rest.len() > limit;
      rest.truncate(limit);
      (rest.into_iter().map(make).collect(), has_more)
    }

    /// The value of a query parameter, decoded.
    fn text_param(path: &str, name: &str) -> Option<String> {
      let (_, query) = path.split_once('?')?;
      let raw = query
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")))?;
      let mut bytes = Vec::new();
      let mut chars = raw.bytes();
      while let Some(byte) = chars.next() {
        if byte == b'%' {
          let hex: String = chars.by_ref().take(2).map(char::from).collect();
          bytes.push(u8::from_str_radix(&hex, 16).unwrap_or(b'?'));
        } else {
          bytes.push(byte);
        }
      }
      String::from_utf8(bytes).ok()
    }

    /// The numbers of an `ids` parameter.
    fn ids_param(path: &str) -> Option<Vec<u64>> {
      text_param(path, "ids").map(|ids| ids.split(',').filter_map(|id| id.parse().ok()).collect())
    }

    /// Person 1 owns the workspace, Person 250 is its only admin.
    fn role_of(number: u64) -> Option<&'static str> {
      match number {
        1 => Some("owner"),
        250 => Some("admin"),
        _ => None,
      }
    }

    fn id_in(path: &str) -> u64 {
      path
        .split('/')
        .find_map(|part| part.parse::<u64>().ok())
        .unwrap_or(0)
    }

    fn space(number: u64) -> AdminSpaceSummary {
      AdminSpaceSummary {
        id: id_from_raw(number),
        name: format!("Space {number}"),
        join_policy: JoinPolicy::InviteOnly,
        is_archived: false,
        member_count: 10,
        guest_count: 0,
        channel_count: 30,
        created_at: UnixMillis(1000),
      }
    }

    fn conversation(number: u64, is_legacy: bool) -> AdminConversationSummary {
      AdminConversationSummary {
        id: id_from_raw(number),
        maybe_space_id: Some(id_from_raw(number % SPACES + 1)),
        maybe_name: Some(format!("channel-{number}")),
        visibility: ConversationVisibility::Public,
        is_archived: false,
        member_count: 5,
        guest_count: 0,
        message_count: 100,
        storage_bytes: 0,
        created_at: UnixMillis(1000),
        maybe_last_activity_at: None,
        maybe_owner_user_id: (!is_legacy).then(|| id_from_raw(number)),
        maybe_retention: (!is_legacy).then_some(RetentionSetting {
          maybe_max_age_days: Some(30),
          maybe_max_messages: None,
        }),
        maybe_inactivity: None,
      }
    }

    fn user(number: u64) -> AdminUserSummary {
      AdminUserSummary {
        id: id_from_raw(number),
        maybe_username: Some(format!("person{number}")),
        display_name: format!("Person {number}"),
        kind: UserKind::Member,
        maybe_oidc_role: role_of(number).map(str::to_owned),
        is_deactivated: false,
        is_deleted: false,
        maybe_deactivation_reason: None,
        device_count: 2,
        space_count: 1,
        conversation_count: 3,
        maybe_last_seen_at: None,
        created_at: UnixMillis(1000),
      }
    }

    fn user_summary_json(number: u64) -> serde_json::Value {
      serde_json::json!({
        "id": serde_json::to_value(id_from_raw::<UserId>(number)).unwrap_or_default(),
        "display_name": format!("Person {number}"),
        "kind": "member",
      })
    }

    /// Runs a future whose steps are all ready (the fake server answers at once).
    fn block_on<F: Future>(future: F) -> F::Output {
      let mut future = pin!(future);
      let mut context = Context::from_waker(Waker::noop());
      loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
          return output;
        }
      }
    }
  }
}
