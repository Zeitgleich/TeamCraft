//! The administration console on the real admin API (docs/admin.md): channels, users and devices,
//! the audit log and the workspace policy, all as metadata. Reads are cached after a refresh, so the
//! synchronous [`crate::backend::AdminBackend`] can answer from them; every change goes to the
//! server and then triggers a refresh. The signed policy is the exception: only a client with the
//! owner's keys (the desktop core) can sign it, so it is handed to the chat backend.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::set_timeout;
use leptos::task::spawn_local;
use teamcraft_proto::{
  AdminConversationSummary, AdminDeviceResponse, AdminSelfJoin as AdminSelfJoinWire, AdminSpaceSummary,
  AdminUserSummary, AuditEntryResponse, ConversationVisibility, HistoryPolicy,
  ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersResponse,
  ListAuditResponse, ListConversationMembersResponse, ListSpaceMembersResponse, RetentionSetting,
  RetentionStatusResponse, Role, SetRetentionRequest, SpaceId, UserKind, WorkspacePolicy,
  WorkspacePolicyResponse,
};

use crate::admin::{
  AdminChannel, AdminSelfJoin, AdminUser, AuditAction, AuditEntry, DeviceInfo, DmScope, Retention,
  SecurityPolicy,
};
use crate::auth::{ApiMethod, Authenticator};
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

  pub fn security_policy(&self) -> SecurityPolicy {
    let data = self.data.borrow();
    security_from_wire(data.policy_version, &data.policy)
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
    let audit = self
      .fetch::<ListAuditResponse>(&format!("/api/v1/admin/audit?limit={LIST_LIMIT}"), &mut errors)
      .await;
    let policy = self
      .fetch::<WorkspacePolicyResponse>("/api/v1/workspace/policy", &mut errors)
      .await;

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

pub fn security_from_wire(version: u64, policy: &WorkspacePolicy) -> SecurityPolicy {
  SecurityPolicy {
    version: u32::try_from(version).unwrap_or(u32::MAX),
    admin_self_join: match policy.admin_self_join {
      AdminSelfJoinWire::Visible => AdminSelfJoin::Visible,
      AdminSelfJoinWire::OwnerApproval => AdminSelfJoin::OwnerApproval,
      // Unknown values are treated as the strictest one, so they never grant more than intended.
      AdminSelfJoinWire::Disabled | AdminSelfJoinWire::Unknown(_) => AdminSelfJoin::Disabled,
    },
    org_recovery_for_members: policy.org_recovery.is_enabled_for_members,
    org_recovery_for_guests: policy.org_recovery.is_enabled_for_guests,
    recovery_approvals_required: policy.org_recovery.admins_required,
    // Not part of the signed policy: sign-in rules live in the identity provider.
    requires_mfa: false,
    requires_mfa_for_guests: false,
    guest_email_domains: String::new(),
    session_days: 30,
    idle_timeout_minutes: 120,
    default_history: policy.history_default.clone(),
    dm_scope: DmScope::Anyone,
  }
}

/// The edited fields on top of the policy that is in force; everything the form does not edit
/// (retention, link previews, compliance) stays as it is.
pub fn policy_from_security(base: &WorkspacePolicy, edited: &SecurityPolicy) -> WorkspacePolicy {
  let mut policy = base.clone();
  policy.admin_self_join = match edited.admin_self_join {
    AdminSelfJoin::Visible => AdminSelfJoinWire::Visible,
    AdminSelfJoin::OwnerApproval => AdminSelfJoinWire::OwnerApproval,
    AdminSelfJoin::Disabled => AdminSelfJoinWire::Disabled,
  };
  policy.org_recovery.is_enabled_for_members = edited.org_recovery_for_members;
  policy.org_recovery.is_enabled_for_guests = edited.org_recovery_for_guests;
  policy.org_recovery.admins_required = edited.recovery_approvals_required;
  policy.history_default = edited.default_history.clone();
  policy
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{DeactivationReason, UnixMillis};

  #[test]
  fn the_policy_maps_both_ways_and_keeps_what_the_form_cannot_edit() {
    let mut base = WorkspacePolicy::default();
    base.retention.keep_pinned = false;
    base.link_previews.domains = vec!["example.com".into()];
    let mut edited = security_from_wire(3, &base);
    assert_eq!(edited.version, 3);
    edited.admin_self_join = AdminSelfJoin::Disabled;
    edited.recovery_approvals_required = 3;
    edited.default_history = HistoryPolicy::FromJoin;
    let next = policy_from_security(&base, &edited);
    assert_eq!(next.admin_self_join, AdminSelfJoinWire::Disabled);
    assert_eq!(next.org_recovery.admins_required, 3);
    assert_eq!(next.history_default, HistoryPolicy::FromJoin);
    assert!(!next.retention.keep_pinned);
    assert_eq!(next.link_previews.domains, vec!["example.com".to_owned()]);
  }

  #[test]
  fn an_unknown_self_join_value_reads_as_the_strictest() {
    let policy = WorkspacePolicy {
      admin_self_join: AdminSelfJoinWire::Unknown("ask_twice".into()),
      ..WorkspacePolicy::default()
    };
    assert_eq!(
      security_from_wire(1, &policy).admin_self_join,
      AdminSelfJoin::Disabled
    );
  }

  #[test]
  fn server_actions_get_the_consoles_wording() {
    assert_eq!(action_from_wire("user_deactivated"), AuditAction::UserDeactivated);
    assert_eq!(action_from_wire("device_revoked"), AuditAction::DeviceRemoved);
    assert_eq!(action_from_wire("space_member_added"), AuditAction::MemberAdded);
    assert_eq!(action_from_wire("something_new"), AuditAction::Other);
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
