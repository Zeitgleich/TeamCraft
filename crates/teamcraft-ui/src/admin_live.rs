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
use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use teamcraft_proto::{
  AdminBrandsResponse, AdminCalendarSettingsResponse, AdminConversationSummary, AdminDeviceResponse,
  AdminInstanceResponse, AdminMailSettingsResponse, AdminSpaceSummary, AdminUserSummary,
  AnnouncementResponse, AssignChannelOwnerRequest, AuditEntryResponse, BrandAssetSlot, ChannelModeration,
  ConversationVisibility, CreateBrandRequest, GetAnnouncementResponse, HistoryPolicy,
  ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersResponse,
  ListAuditResponse, ListConversationMembersResponse, ListSpaceMembersResponse, PermissionSchemeResponse,
  RetentionSetting, RetentionStatusResponse, Role, RuntimeSettingsKey, RuntimeSettingsResponse,
  RuntimeSettingsSection, SchemeChange, SendTestMailRequest, SetAnnouncementRequest,
  SetChannelModerationRequest, SetRetentionRequest, SettingsTestResponse, SpaceId, UpdateBrandRequest,
  UpdateCalendarSettingsRequest, UpdateMailSettingsRequest, UpdatePermissionSchemeRequest, UserKind,
  WorkspacePolicy, WorkspacePolicyResponse,
};

use crate::admin::{
  AdminChannel, AdminSpace, AdminUser, AuditAction, AuditEntry, AuditFilter, DeviceInfo, Retention,
  SettingsTestKind,
};
use crate::auth::{ApiMethod, Authenticator, BinaryBody};
use crate::model::{
  ConversationId, ConversationKind, CustomStatus, PresenceStatus, User, UserId, id_from_raw, raw_id,
};
use crate::server_backend::hue_from_seed;

const REFRESH_DELAY: Duration = Duration::from_millis(500);
const LIST_LIMIT: u32 = 200;
/// Channels beyond this many get no per-channel details (owner, retention) until the list shrinks.
const DETAIL_LIMIT: usize = 100;
/// Signing and publishing happens in the core; the server shows the result after this long.
const PUBLISH_SETTLE: Duration = Duration::from_millis(1500);

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct AdminLive {
  auth: Rc<dyn Authenticator>,
  data: Rc<RefCell<AdminData>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  refresh_queued: Rc<Cell<bool>>,
}

/// What the last refresh returned.
#[derive(Default)]
pub struct AdminData {
  is_loaded: bool,
  channels: Vec<AdminChannel>,
  users: Vec<AdminUser>,
  audit: Vec<AuditEntry>,
  policy: WorkspacePolicy,
  policy_version: u64,
  spaces: Vec<AdminSpace>,
  announcement: Option<AnnouncementResponse>,
  instance: Option<AdminInstanceResponse>,
  brands: Option<AdminBrandsResponse>,
  mail: Option<AdminMailSettingsResponse>,
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
    }
  }

  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  pub fn channels(&self) -> Vec<AdminChannel> {
    self.data.borrow().channels.clone()
  }

  pub fn users(&self) -> Vec<AdminUser> {
    self.data.borrow().users.clone()
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
    let forgotten = id.to_owned();
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/brands/{id}"),
      None,
      move |data, brands: AdminBrandsResponse| {
        data.brands = Some(brands);
        forget_previews(data, &forgotten);
      },
    );
  }

  pub fn take_over_brand(&self, id: &str) {
    self.send_stored(
      ApiMethod::Post,
      format!("/api/v1/admin/brands/{id}/take-over"),
      Some("{}".to_owned()),
      |data, brands: AdminBrandsResponse| data.brands = Some(brands),
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
    let live = self.clone();
    let path = format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str());
    spawn_local(async move {
      let maybe_url = match live.auth.api_request_bytes(ApiMethod::Get, &path, None).await {
        Ok(response) if response.status == 200 => {
          preview_data_url(response.maybe_content_type.as_deref(), &response.body)
        },
        _ => None,
      };
      {
        let mut data = live.data.borrow_mut();
        // The loading entry stays, which is what keeps a failed image from being requested again.
        if let Some(url) = maybe_url {
          data.loading.remove(&format!("preview:{key}"));
          data.brand_previews.insert(key, url);
        }
      }
      live.changed();
    });
    None
  }

  pub fn upload_brand_asset(&self, id: &str, slot: &BrandAssetSlot, bytes: Vec<u8>, content_type: String) {
    let live = self.clone();
    let (id, slot) = (id.to_owned(), slot.clone());
    spawn_local(async move {
      let path = format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str());
      let body = BinaryBody { content_type, bytes };
      let message = match live
        .auth
        .api_request_bytes(ApiMethod::Put, &path, Some(body))
        .await
      {
        Ok(response) if response.status < 300 => {
          let text = String::from_utf8_lossy(&response.body);
          if let Ok(brands) = serde_json::from_str::<AdminBrandsResponse>(&text) {
            live.data.borrow_mut().brands = Some(brands);
          }
          None
        },
        Ok(response) => Some(format!(
          "{path}: {}",
          refusal_of(&String::from_utf8_lossy(&response.body), response.status)
        )),
        Err(error) => Some(error.to_string()),
      };
      {
        let mut data = live.data.borrow_mut();
        data.maybe_error = message;
        forget_previews(&mut data, &id);
      }
      live.changed();
      live.queue_refresh();
    });
  }

  pub fn delete_brand_asset(&self, id: &str, slot: &BrandAssetSlot) {
    let forgotten = id.to_owned();
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/brands/{id}/assets/{}", slot.as_str()),
      None,
      move |data, brands: AdminBrandsResponse| {
        data.brands = Some(brands);
        forget_previews(data, &forgotten);
      },
    );
  }

  pub fn mail_settings(&self) -> Option<AdminMailSettingsResponse> {
    self.data.borrow().mail.clone()
  }

  pub fn save_mail_settings(&self, request: UpdateMailSettingsRequest) {
    if let Ok(body) = serde_json::to_string(&request) {
      self.send_stored(
        ApiMethod::Put,
        "/api/v1/admin/settings/mail".to_owned(),
        Some(body),
        |data, mail: AdminMailSettingsResponse| data.mail = Some(mail),
      );
    }
  }

  pub fn reset_mail_settings(&self) {
    self.send_stored(
      ApiMethod::Delete,
      "/api/v1/admin/settings/mail".to_owned(),
      None,
      |data, mail: AdminMailSettingsResponse| data.mail = Some(mail),
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
        |data, calendar: AdminCalendarSettingsResponse| data.calendar = Some(calendar),
      );
    }
  }

  pub fn reset_calendar_settings(&self) {
    self.send_stored(
      ApiMethod::Delete,
      "/api/v1/admin/settings/calendar".to_owned(),
      None,
      |data, calendar: AdminCalendarSettingsResponse| data.calendar = Some(calendar),
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
        |data, runtime: RuntimeSettingsResponse| data.runtime = Some(runtime),
      );
    }
  }

  pub fn reset_runtime_settings(&self, key: RuntimeSettingsKey) {
    self.send_stored(
      ApiMethod::Delete,
      format!("/api/v1/admin/settings/{}", key.as_str()),
      None,
      |data, runtime: RuntimeSettingsResponse| data.runtime = Some(runtime),
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
    self.load(format!("scheme:{maybe_space:?}"), path, move |data, scheme| {
      data.schemes.insert(maybe_space, scheme);
    });
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
      move |data, moderation| {
        data.moderation.insert(channel, moderation);
      },
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
      move |data, list: ListConversationMembersResponse| {
        data.members.insert(
          channel,
          list.members.into_iter().map(|member| member.user.id).collect(),
        );
      },
    );
    Vec::new()
  }

  pub fn assign_channel_owner(&self, channel: ConversationId, user: UserId) {
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

  /// Owners and admins by role: the first signed roster, if the workspace has none.
  pub fn roster_candidates(&self) -> (Vec<UserId>, Vec<UserId>) {
    let data = self.data.borrow();
    let by_role = |wanted: Role| {
      data
        .users
        .iter()
        .filter(|user| user.role == wanted)
        .map(|user| user.user.id)
        .collect()
    };
    (by_role(Role::Owner), by_role(Role::Admin))
  }

  /// Fetches everything now; a part that fails keeps its previous value and shows its error.
  pub async fn refresh(&self) {
    let mut errors: Vec<String> = Vec::new();
    let spaces = self
      .fetch::<ListAdminSpacesResponse>("/api/v1/admin/spaces", &mut errors)
      .await;
    let conversations = self
      .fetch::<ListAdminConversationsResponse>(
        &format!("/api/v1/admin/conversations?limit={LIST_LIMIT}"),
        &mut errors,
      )
      .await;
    let users = self
      .fetch::<ListAdminUsersResponse>(
        &format!("/api/v1/admin/users?status=all&limit={LIST_LIMIT}"),
        &mut errors,
      )
      .await;
    let audit_path = audit_path(self.data.borrow().audit_filter);
    let audit = self.fetch::<ListAuditResponse>(&audit_path, &mut errors).await;
    let policy = self
      .fetch::<WorkspacePolicyResponse>("/api/v1/workspace/policy", &mut errors)
      .await;
    let announcement = self
      .fetch::<GetAnnouncementResponse>("/api/v1/announcement", &mut errors)
      .await;
    let instance = self
      .fetch::<AdminInstanceResponse>("/api/v1/admin/instance", &mut errors)
      .await;
    let brands = self
      .fetch::<AdminBrandsResponse>("/api/v1/admin/brands", &mut errors)
      .await;
    let mail = self
      .fetch::<AdminMailSettingsResponse>("/api/v1/admin/settings/mail", &mut errors)
      .await;
    let calendar = self
      .fetch::<AdminCalendarSettingsResponse>("/api/v1/admin/settings/calendar", &mut errors)
      .await;
    let runtime = self
      .fetch::<RuntimeSettingsResponse>("/api/v1/admin/settings", &mut errors)
      .await;

    self.reload_cached(&mut errors).await;

    let space_names: HashMap<SpaceId, String> = spaces
      .as_ref()
      .map(|spaces| {
        spaces
          .spaces
          .iter()
          .map(|space| (space.id, space.name.clone()))
          .collect()
      })
      .unwrap_or_default();
    let policy_default_history = policy.as_ref().map_or(HistoryPolicy::Full, |policy| {
      policy.policy.history_default.clone()
    });

    let mut channels = Vec::new();
    if let Some(list) = &conversations {
      for (index, summary) in list.conversations.iter().enumerate() {
        let details = if index < DETAIL_LIMIT {
          self.channel_details(summary).await
        } else {
          ChannelDetails::default()
        };
        channels.push(channel_from_wire(
          summary,
          &space_names,
          details,
          policy_default_history.clone(),
        ));
      }
    }
    let space_memberships = match &spaces {
      Some(spaces) => self.space_memberships(&spaces.spaces).await,
      None => HashMap::new(),
    };
    let mut admin_users = Vec::new();
    if let Some(list) = &users {
      for summary in &list.users {
        let devices = self.devices_of(summary.id).await;
        let names = space_memberships.get(&summary.id).cloned().unwrap_or_default();
        admin_users.push(user_from_wire(summary, devices, names));
      }
    }

    {
      let mut data = self.data.borrow_mut();
      data.is_loaded = true;
      if conversations.is_some() {
        data.channels = channels;
      }
      if users.is_some() {
        data.users = admin_users;
      }
      if let Some(policy) = policy {
        data.policy = policy.policy;
        data.policy_version = policy.version;
      }
      if let Some(spaces) = &spaces {
        data.spaces = spaces
          .spaces
          .iter()
          .map(|space| AdminSpace {
            id: space.id,
            name: space.name.clone(),
          })
          .collect();
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
      data.maybe_error = errors.into_iter().next();
    }
    self.changed();
  }

  /// Schedules a refresh soon; calls while one is scheduled collapse into it.
  pub fn queue_refresh(&self) {
    self.queue_refresh_after(REFRESH_DELAY);
  }

  /// A refresh after the core had time to sign and publish a policy.
  pub fn queue_refresh_after_publish(&self) {
    self.queue_refresh_after(PUBLISH_SETTLE);
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

  fn changed(&self) {
    let notify = self.notify.borrow().clone();
    if let Some(notify) = notify {
      notify();
    }
  }

  async fn channel_details(&self, summary: &AdminConversationSummary) -> ChannelDetails {
    let mut ignored = Vec::new();
    let members = self
      .fetch::<ListConversationMembersResponse>(
        &format!("/api/v1/admin/conversations/{}/members", summary.id),
        &mut ignored,
      )
      .await;
    let retention = self
      .fetch::<RetentionStatusResponse>(
        &format!("/api/v1/conversations/{}/retention", summary.id),
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
      maybe_retention: retention,
    }
  }

  async fn space_memberships(&self, spaces: &[AdminSpaceSummary]) -> HashMap<UserId, Vec<String>> {
    let mut memberships: HashMap<UserId, Vec<String>> = HashMap::new();
    let mut ignored = Vec::new();
    for space in spaces {
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

  async fn devices_of(&self, user: UserId) -> Vec<AdminDeviceResponse> {
    let mut ignored = Vec::new();
    self
      .fetch::<ListAdminDevicesResponse>(&format!("/api/v1/admin/users/{user}/devices"), &mut ignored)
      .await
      .map(|list| list.devices)
      .unwrap_or_default()
  }

  /// Re-reads the on-demand data that is cached, so changes of other admins show up.
  async fn reload_cached(&self, errors: &mut Vec<String>) {
    let (scopes, channels): (Vec<Option<SpaceId>>, Vec<ConversationId>) = {
      let data = self.data.borrow();
      (
        data.schemes.keys().copied().collect(),
        data.moderation.keys().copied().collect(),
      )
    };
    for scope in scopes {
      let path = match scope {
        Some(space) => format!("/api/v1/admin/spaces/{space}/permission-scheme"),
        None => "/api/v1/admin/permission-scheme".to_owned(),
      };
      if let Some(scheme) = self.fetch::<PermissionSchemeResponse>(&path, errors).await {
        self.data.borrow_mut().schemes.insert(scope, scheme);
      }
    }
    for channel in channels {
      let path = format!("/api/v1/admin/conversations/{channel}/moderation");
      if let Some(moderation) = self.fetch::<ChannelModeration>(&path, errors).await {
        self.data.borrow_mut().moderation.insert(channel, moderation);
      }
    }
  }

  /// Fetches data the console shows on demand and stores it; one request per key at a time.
  fn load<T: serde::de::DeserializeOwned + 'static>(
    &self,
    key: String,
    path: String,
    store: impl FnOnce(&mut AdminData, T) + 'static,
  ) {
    if !self.data.borrow_mut().loading.insert(key.clone()) {
      return;
    }
    let live = self.clone();
    spawn_local(async move {
      let mut errors = Vec::new();
      let value = live.fetch::<T>(&path, &mut errors).await;
      {
        let mut data = live.data.borrow_mut();
        data.loading.remove(&key);
        match value {
          Some(value) => store(&mut data, value),
          None => data.maybe_error = errors.into_iter().next(),
        }
      }
      live.changed();
    });
  }

  async fn fetch<T: serde::de::DeserializeOwned>(&self, path: &str, errors: &mut Vec<String>) -> Option<T> {
    let response = match self.auth.api_get(path).await {
      Ok(response) => response,
      Err(error) => {
        errors.push(error.to_string());
        return None;
      },
    };
    if response.status != 200 {
      errors.push(format!("{path}: {}", response.status));
      return None;
    }
    match serde_json::from_str(&response.body) {
      Ok(value) => Some(value),
      Err(error) => {
        errors.push(format!("{path}: {error}"));
        None
      },
    }
  }

  /// Like [`Self::send`], and an answer of type `T` replaces the cached copy at once, so the form
  /// shows the saved state without waiting for the refresh.
  fn send_stored<T: serde::de::DeserializeOwned + 'static>(
    &self,
    method: ApiMethod,
    path: String,
    maybe_body: Option<String>,
    store: impl FnOnce(&mut AdminData, T) + 'static,
  ) {
    let live = self.clone();
    spawn_local(async move {
      let message = match live.auth.api_request(method, &path, maybe_body).await {
        Ok(response) if response.status < 300 => {
          match serde_json::from_str::<T>(&response.body) {
            Ok(value) => store(&mut live.data.borrow_mut(), value),
            Err(error) => tracing::debug!(%error, %path, "the answer could not be read"),
          }
          None
        },
        Ok(response) => Some(format!("{path}: {}", refusal_of(&response.body, response.status))),
        Err(error) => Some(error.to_string()),
      };
      live.data.borrow_mut().maybe_error = message;
      live.changed();
      live.queue_refresh();
    });
  }

  fn send_brands(
    &self,
    method: ApiMethod,
    path: String,
    request: &impl serde::Serialize,
    maybe_forgotten: Option<String>,
  ) {
    if let Ok(body) = serde_json::to_string(request) {
      self.send_stored(
        method,
        path,
        Some(body),
        move |data, brands: AdminBrandsResponse| {
          data.brands = Some(brands);
          if let Some(id) = maybe_forgotten {
            forget_previews(data, &id);
          }
        },
      );
    }
  }

  /// A connection test: its answer, or the refusal, is kept for the console to show.
  fn run_settings_test(&self, kind: SettingsTestKind, path: &'static str, body: String) {
    self.data.borrow_mut().last_test = None;
    let live = self.clone();
    spawn_local(async move {
      let outcome = match live.auth.api_request(ApiMethod::Post, path, Some(body)).await {
        Ok(response) if response.status < 300 => serde_json::from_str::<SettingsTestResponse>(&response.body)
          .unwrap_or_else(|error| SettingsTestResponse {
            is_ok: false,
            maybe_error: Some(error.to_string()),
          }),
        Ok(response) => SettingsTestResponse {
          is_ok: false,
          maybe_error: Some(refusal_of(&response.body, response.status)),
        },
        Err(error) => SettingsTestResponse {
          is_ok: false,
          maybe_error: Some(error.to_string()),
        },
      };
      live.data.borrow_mut().last_test = Some((kind, outcome));
      live.changed();
      // Mail tests change the delivery status shown next to the form.
      live.queue_refresh();
    });
  }

  /// Fire and forget; a refusal is shown in the console, then the next refresh shows the truth.
  fn send(&self, method: ApiMethod, path: String, maybe_body: Option<String>) {
    let live = self.clone();
    spawn_local(async move {
      let message = match live.auth.api_request(method, &path, maybe_body).await {
        Ok(response) if response.status < 300 => None,
        Ok(response) => Some(format!("{path}: {}", refusal_of(&response.body, response.status))),
        Err(error) => Some(error.to_string()),
      };
      live.data.borrow_mut().maybe_error = message;
      live.queue_refresh();
    });
  }
}

/// Per-channel details that need their own requests.
#[derive(Default)]
struct ChannelDetails {
  maybe_owner: Option<UserId>,
  maybe_retention: Option<RetentionStatusResponse>,
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

fn refusal_of(body: &str, status: u16) -> String {
  serde_json::from_str::<teamcraft_proto::ErrorBody>(body)
    .map(|error| error.message)
    .unwrap_or_else(|_| status.to_string())
}

// ---- Mappings (pure, tested) ------------------------------------------------------------------------

fn channel_from_wire(
  summary: &AdminConversationSummary,
  space_names: &HashMap<SpaceId, String>,
  details: ChannelDetails,
  history: HistoryPolicy,
) -> AdminChannel {
  let retention = details.maybe_retention.map_or(
    Retention {
      maybe_days: None,
      maybe_max_messages: None,
      keeps_pinned: true,
    },
    |status| Retention {
      maybe_days: status.effective.maybe_max_age_days,
      maybe_max_messages: status
        .effective
        .maybe_max_messages
        .and_then(|count| u32::try_from(count).ok()),
      keeps_pinned: status.keep_pinned,
    },
  );
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
    // The server cannot see attachments' sizes in ciphertext it does not index; shown as unknown (0).
    storage_bytes: 0,
    retention,
    history,
    is_archived: summary.is_archived,
  }
}

fn user_from_wire(
  summary: &AdminUserSummary,
  devices: Vec<AdminDeviceResponse>,
  space_names: Vec<String>,
) -> AdminUser {
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
      device_setup: crate::model::DeviceSetup::Unknown,
    },
    role,
    devices: devices
      .into_iter()
      .filter(|device| !device.is_revoked)
      .map(|device| DeviceInfo {
        id: device.id,
        name: device.name,
        platform: String::new(),
        last_seen: device.maybe_last_seen_at.unwrap_or(device.created_at).0,
        is_current: false,
      })
      .collect(),
    last_seen: summary.maybe_last_seen_at.unwrap_or(summary.created_at).0,
    space_names,
    is_active: !summary.is_deactivated,
    maybe_guest: None,
  }
}

/// Maps the server's action names to the console's wording; unknown ones keep their name.
pub fn action_from_wire(action: &str) -> AuditAction {
  match action {
    "policy_appended" | "policy_changed" | "roster_appended" => AuditAction::PolicyChanged,
    "retention_changed" | "retention_run" => AuditAction::RetentionChanged,
    "device_revoked" => AuditAction::DeviceRemoved,
    "user_deactivated" => AuditAction::UserDeactivated,
    "user_reactivated" => AuditAction::UserReactivated,
    "invite_created" => AuditAction::GuestInvited,
    "conversation_member_added" | "space_member_added" => AuditAction::MemberAdded,
    "conversation_member_removed" | "space_member_removed" => AuditAction::MemberRemoved,
    "conversation_member_role_changed" | "space_member_role_changed" | "oidc_role_changed" => {
      AuditAction::MemberRoleChanged
    },
    "conversation_updated" => AuditAction::ChannelUpdated,
    "space_created" => AuditAction::SpaceCreated,
    "space_updated" => AuditAction::SpaceUpdated,
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
      maybe_deactivation_reason: Some(DeactivationReason::Admin),
      device_count: 2,
      space_count: 1,
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
    let user = user_from_wire(&summary, devices, vec!["Space A".into()]);
    assert_eq!(user.role, Role::Admin);
    assert!(!user.is_active);
    assert_eq!(user.devices.len(), 1);
    assert_eq!(user.devices[0].last_seen, 5000);
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
      created_at: UnixMillis(100),
      maybe_last_activity_at: None,
    };
    let status = RetentionStatusResponse {
      own: RetentionSetting::default(),
      effective: RetentionSetting {
        maybe_max_age_days: Some(30),
        maybe_max_messages: None,
      },
      limit: RetentionSetting::default(),
      keep_pinned: false,
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
    );
    assert_eq!(channel.space_name, "Space A");
    assert_eq!(channel.retention.maybe_days, Some(30));
    assert!(!channel.retention.keeps_pinned);
    assert_eq!(channel.last_activity, 100);
    assert_eq!(channel.maybe_owner, Some(id_from_raw(1)));
  }
}
