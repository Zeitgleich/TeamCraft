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
  AdminInstanceResponse, AdminMailSettingsResponse, AdminSpaceSummary, AdminUserSummary,
  AnnouncementResponse, AssignChannelOwnerRequest, AuditEntryResponse, BrandAssetSlot, ChannelModeration,
  ConversationVisibility, CreateBrandRequest, ErrorBody, ErrorCode, GetAnnouncementResponse, HistoryPolicy,
  ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersResponse,
  ListAuditResponse, ListConversationMembersResponse, ListSpaceMembersResponse, PermissionSchemeResponse,
  RetentionSetting, RetentionStatusResponse, Role, RuntimeSettingsKey, RuntimeSettingsResponse,
  RuntimeSettingsSection, SchemeChange, SendTestMailRequest, SetAnnouncementRequest,
  SetChannelModerationRequest, SetRetentionRequest, SettingsTestResponse, SpaceId, UpdateBrandRequest,
  UpdateCalendarSettingsRequest, UpdateMailSettingsRequest, UpdatePermissionSchemeRequest,
  UpdateSpaceRequest, UserKind, WorkspacePolicy, WorkspacePolicyResponse,
};

use crate::admin::{
  AdminChannel, AdminLoad, AdminSection, AdminSpace, AdminSpaceChanges, AdminUser, AuditAction, AuditEntry,
  AuditFilter, DeviceInfo, DeviceVerification, RATE_LIMITED_CODE, READ_FAILED_CODE, Retention,
  SettingsTestKind, UNREACHABLE_CODE,
};
use crate::auth::{ApiMethod, Authenticator, BinaryBody};
use crate::clock::now_ms;
use crate::model::{
  ConversationId, ConversationKind, CustomStatus, PresenceStatus, User, UserId, id_from_raw, raw_id,
};
use crate::server_backend::hue_from_seed;

const REFRESH_DELAY: Duration = Duration::from_millis(500);
const LIST_LIMIT: u32 = 200;
/// Pages of channels (and of users) one refresh reads: 1000 entries. Beyond that the console says
/// the list is cut (issue #1217) instead of ending silently.
const MAX_LIST_PAGES: usize = 5;
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
}

/// What the last refresh returned.
#[derive(Default)]
pub struct AdminData {
  load: AdminLoad,
  channels: Vec<AdminChannel>,
  /// The lists end before the last entry (more pages than [`MAX_LIST_PAGES`]).
  is_channel_list_truncated: bool,
  users: Vec<AdminUser>,
  is_user_list_truncated: bool,
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
  brands: Option<AdminBrandsResponse>,
  mail: Option<AdminMailSettingsResponse>,
  /// Loaded when the console shows the notetaker page.
  notetaker: Option<teamcraft_proto::AdminNotetakerResponse>,
  has_asked_notetaker: bool,
  calendar: Option<AdminCalendarSettingsResponse>,
  runtime: Option<RuntimeSettingsResponse>,
  last_test: Option<(SettingsTestKind, SettingsTestResponse)>,
  /// `data:` URLs of brand images by `{brand}/{slot}`; loaded when the editor shows them.
  brand_previews: HashMap<String, String>,
  audit_filter: AuditFilter,
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

  pub fn is_channel_list_truncated(&self) -> bool {
    self.data.borrow().is_channel_list_truncated
  }

  pub fn is_user_list_truncated(&self) -> bool {
    self.data.borrow().is_user_list_truncated
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
      self.run_settings_test(SettingsTestKind::Mail, "/api/v1/admin/settings/mail/test", body);
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
      "/api/v1/admin/settings/calendar/test",
      "{}".to_owned(),
    );
  }

  pub fn last_settings_test(&self) -> Option<(SettingsTestKind, SettingsTestResponse)> {
    self.data.borrow().last_test.clone()
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
    teamcraft_client_api::admin::role_holders(&self.data.borrow().users)
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
    let ((spaces, conversations, users, policy, announcement), (instance, brands, mail, calendar, runtime)) =
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
      )
      .await;
    let spaces = settle(spaces, &mut errors).flatten();
    let conversations = settle(conversations, &mut errors).flatten();
    let users = settle(users, &mut errors).flatten();
    let policy = settle(policy, &mut errors).flatten();
    let announcement = settle(announcement, &mut errors).flatten();
    let instance = settle(instance, &mut errors).flatten();
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
        }
        if let Some(spaces) = &spaces {
          data.spaces = spaces.spaces.iter().map(space_from_wire).collect();
        }
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
    let space_memberships = match &spaces {
      Some(spaces) if reads.has(Reads::MEMBERSHIPS) && !self.is_quiet() => {
        Some(self.space_memberships(&spaces.spaces).await)
      },
      _ => None,
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

    {
      let mut data = self.data.borrow_mut();
      if let Some(list) = &conversations {
        let channels = channels_of(
          &list.items,
          &space_names,
          &data.legacy_details,
          &policy_default_history,
          keeps_pinned,
        );
        data.channels = channels;
        data.is_channel_list_truncated = list.is_truncated;
      }
      if let Some(list) = &users {
        let previous: HashMap<UserId, Vec<String>> = data
          .users
          .iter()
          .map(|user| (user.user.id, user.space_names.clone()))
          .collect();
        data.users = list
          .items
          .iter()
          .map(|summary| {
            let names = match &space_memberships {
              Some(memberships) => memberships.get(&summary.id).cloned().unwrap_or_default(),
              None => previous.get(&summary.id).cloned().unwrap_or_default(),
            };
            user_from_wire(summary, names)
          })
          .collect();
        data.is_user_list_truncated = list.is_truncated;
      } else if let Some(memberships) = &space_memberships {
        for user in &mut data.users {
          user.space_names = memberships.get(&user.user.id).cloned().unwrap_or_default();
        }
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
        let (channels, users) = (data.channels.clone(), data.users.clone());
        data.audit = audit
          .entries
          .iter()
          .map(|entry| audit_from_wire(entry, &space_names, &channels, &users))
          .collect();
      }
      if data.load != AdminLoad::Loaded && spaces.is_some() && conversations.is_some() {
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
    self.queue_reads(Reads::of(section), REFRESH_DELAY);
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

  async fn space_memberships(&self, spaces: &[AdminSpaceSummary]) -> HashMap<UserId, Vec<String>> {
    let mut memberships: HashMap<UserId, Vec<String>> = HashMap::new();
    let mut ignored = Vec::new();
    for space in spaces {
      if self.is_quiet() {
        break;
      }
      let path = format!("/api/v1/admin/spaces/{}/members", space.id);
      if let Some(list) = self.fetch::<ListSpaceMembersResponse>(&path, &mut ignored).await {
        for member in list.members {
          memberships
            .entry(member.user.id)
            .or_default()
            .push(space.name.clone());
        }
      }
    }
    memberships
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

  /// The channel list, page after page up to [`MAX_LIST_PAGES`].
  async fn read_conversations_if(
    &self,
    is_wanted: bool,
  ) -> Result<Option<Listed<AdminConversationSummary>>, String> {
    if !is_wanted {
      return Ok(None);
    }
    let mut items = Vec::new();
    let mut maybe_after: Option<ConversationId> = None;
    for _ in 0..MAX_LIST_PAGES {
      let mut path = format!("/api/v1/admin/conversations?limit={LIST_LIMIT}");
      if let Some(after) = maybe_after {
        path.push_str(&format!("&after={after}"));
      }
      let page: ListAdminConversationsResponse = self.fetch_result(path).await?;
      maybe_after = page.conversations.last().map(|summary| summary.id);
      items.extend(page.conversations);
      if !page.has_more || maybe_after.is_none() {
        return Ok(Some(Listed {
          items,
          is_truncated: false,
        }));
      }
    }
    Ok(Some(Listed {
      items,
      is_truncated: true,
    }))
  }

  /// The user list, page after page up to [`MAX_LIST_PAGES`].
  async fn read_users_if(&self, is_wanted: bool) -> Result<Option<Listed<AdminUserSummary>>, String> {
    if !is_wanted {
      return Ok(None);
    }
    let mut items = Vec::new();
    let mut maybe_after: Option<UserId> = None;
    for _ in 0..MAX_LIST_PAGES {
      let mut path = format!("/api/v1/admin/users?status=all&limit={LIST_LIMIT}");
      if let Some(after) = maybe_after {
        path.push_str(&format!("&after={after}"));
      }
      let page: ListAdminUsersResponse = self.fetch_result(path).await?;
      maybe_after = page.users.last().map(|summary| summary.id);
      items.extend(page.users);
      if !page.has_more || maybe_after.is_none() {
        return Ok(Some(Listed {
          items,
          is_truncated: false,
        }));
      }
    }
    Ok(Some(Listed {
      items,
      is_truncated: true,
    }))
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
  fn run_settings_test(&self, kind: SettingsTestKind, path: &'static str, body: String) {
    self.data.borrow_mut().last_test = None;
    self.run(AdminTask::SettingsTest { kind, path, body });
  }

  async fn run_settings_test_now(&self, kind: SettingsTestKind, path: &'static str, body: String) {
    let outcome = match self.auth.api_request(ApiMethod::Post, path, Some(body)).await {
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
    self.data.borrow_mut().last_test = Some((kind, outcome));
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
      AdminTask::SendStored {
        method,
        path,
        maybe_body,
        store,
      } => self.send_stored_now(method, path, maybe_body, store).await,
      AdminTask::Load { key, path, what } => self.load_now(key, path, what).await,
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
  /// Like `Send`, and the answer replaces the cached copy at once.
  SendStored {
    method: ApiMethod,
    path: String,
    maybe_body: Option<String>,
    store: Stored,
  },
  /// Data the console shows on demand.
  Load { key: String, path: String, what: Loaded },
  /// A connection test.
  SettingsTest {
    kind: SettingsTestKind,
    path: &'static str,
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
}

/// What on-demand data a load is for.
pub enum Loaded {
  Scheme(Option<SpaceId>),
  Moderation(ConversationId),
  Members(ConversationId),
  Devices(UserId),
  Notetaker,
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
    }
    Ok(())
  }
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
  /// One request per space: which spaces each user is in.
  const MEMBERSHIPS: Reads = Reads(1 << 11);
  /// One request per channel, from servers that don't send owner and retention in the list.
  const LEGACY_DETAILS: Reads = Reads(1 << 12);
  const CACHED_SCHEMES: Reads = Reads(1 << 13);
  const CACHED_MODERATION: Reads = Reads(1 << 14);
  const CACHED_DEVICES: Reads = Reads(1 << 15);
  const ALL: Reads = Reads((1 << 16) - 1);

  fn has(self, part: Reads) -> bool {
    self.0 & part.0 == part.0
  }

  /// What the section shows. Sections that read no list of their own show the policy, which is one
  /// small request.
  fn of(section: AdminSection) -> Reads {
    match section {
      AdminSection::Channels => {
        Self::SPACES | Self::CONVERSATIONS | Self::USERS | Self::LEGACY_DETAILS | Self::CACHED_MODERATION
      },
      AdminSection::Users => Self::USERS | Self::SPACES | Self::CACHED_DEVICES,
      AdminSection::Permissions => Self::SPACES | Self::CACHED_SCHEMES,
      AdminSection::Retention => Self::SPACES | Self::CONVERSATIONS | Self::POLICY | Self::LEGACY_DETAILS,
      AdminSection::Audit => Self::AUDIT | Self::SPACES | Self::CONVERSATIONS | Self::USERS,
      AdminSection::Announcement => Self::ANNOUNCEMENT,
      AdminSection::Brands => Self::BRANDS,
      AdminSection::Mail => Self::MAIL,
      AdminSection::Calendar => Self::CALENDAR,
      AdminSection::Settings => Self::RUNTIME,
      AdminSection::Instance => Self::INSTANCE,
      // The owners and admins to propose for the roster come from the user list.
      AdminSection::Security | AdminSection::People => Self::POLICY | Self::USERS,
      AdminSection::Backgrounds
      | AdminSection::Updates
      | AdminSection::Integrations
      | AdminSection::Notetaker
      | AdminSection::Import => Self::POLICY,
    }
  }
}

impl std::ops::BitOr for Reads {
  type Output = Reads;

  fn bitor(self, other: Reads) -> Reads {
    Reads(self.0 | other.0)
  }
}

/// A list read page by page; `is_truncated` when more pages were left than one refresh reads.
struct Listed<T> {
  items: Vec<T>,
  is_truncated: bool,
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
  }
}

fn user_from_wire(summary: &AdminUserSummary, space_names: Vec<String>) -> AdminUser {
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
    space_names,
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
    "conversation_member_added" | "space_member_added" => AuditAction::MemberAdded,
    "conversation_member_removed" | "space_member_removed" => AuditAction::MemberRemoved,
    "conversation_member_role_changed" | "space_member_role_changed" | "oidc_role_changed" => {
      AuditAction::MemberRoleChanged
    },
    "conversation_updated" => AuditAction::ChannelUpdated,
    "channel_deleted" | "channel_deleted_last_member" => AuditAction::ChannelDeleted,
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
    "mail_settings_updated" | "mail_settings_reset" | "mail_test_sent" => AuditAction::MailSettingsChanged,
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
    let user = user_from_wire(&summary, vec!["Space A".into()]);
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
      // Ten lists, the further pages of channels (2) and users (1), the audit log, and one request
      // per space: 26 requests in all, once, where the console used to make about 650.
      assert!(
        paths.len() <= 10 + 3 + 1 + SPACES as usize,
        "{} requests: {paths:?}",
        paths.len()
      );
      assert_no_detail_requests(&paths);
      assert_eq!(
        live.channels().len(),
        CHANNELS as usize,
        "every page of channels is read"
      );
      assert_eq!(live.users().len(), USERS as usize);
      assert!(!live.is_channel_list_truncated());
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
      }
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

      // Only the first round of lists went out; nothing followed once the server said to wait.
      assert!(server.paths().len() <= 10, "{:?}", server.paths());
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
    fn a_list_longer_than_the_pages_read_says_so() {
      let server = FakeServer::with_channels(MAX_LIST_PAGES as u64 * u64::from(LIST_LIMIT) + 50);
      let live = AdminLive::new(server.clone());
      block_on(live.refresh_once());
      assert_eq!(live.channels().len(), MAX_LIST_PAGES * LIST_LIMIT as usize);
      assert!(live.is_channel_list_truncated());
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
          let (items, has_more) = page(path, self.inner.channels, |n| {
            conversation(n, self.inner.is_legacy)
          });
          return json(&ListAdminConversationsResponse {
            conversations: items,
            has_more,
          });
        }
        if path.starts_with("/api/v1/admin/users?") {
          let (items, has_more) = page(path, USERS, user);
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
        if path.starts_with("/api/v1/admin/audit") {
          return json(&ListAuditResponse {
            entries: Vec::new(),
            has_more: false,
          });
        }
        if path.starts_with("/api/v1/admin/spaces/") && path.ends_with("/members") {
          return json(&ListSpaceMembersResponse { members: Vec::new() });
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
    fn page<T>(path: &str, total: u64, make: impl Fn(u64) -> T) -> (Vec<T>, bool) {
      let query = |name: &str| {
        path
          .split(['?', '&'])
          .find_map(|pair| pair.strip_prefix(&format!("{name}=")))
          .and_then(|value| value.parse::<u64>().ok())
      };
      let limit = query("limit").unwrap_or(50);
      // IDs are the numbers 1.. in order, so "after" is the last number of the previous page.
      let first = query("after").unwrap_or(0) + 1;
      let last = (first + limit - 1).min(total);
      let items = (first..=last).map(&make).collect();
      (items, last < total)
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
      }
    }

    fn user(number: u64) -> AdminUserSummary {
      AdminUserSummary {
        id: id_from_raw(number),
        maybe_username: Some(format!("person{number}")),
        display_name: format!("Person {number}"),
        kind: UserKind::Member,
        maybe_oidc_role: None,
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
