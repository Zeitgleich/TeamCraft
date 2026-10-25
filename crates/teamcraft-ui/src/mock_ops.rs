//! Mock data for calls, administration and device security. Child module of `mock` so it can read the
//! demo users and conversations the chat part owns.

use std::collections::HashMap;

use teamcraft_proto::{
  ALL_PERMISSIONS, AdminBrandsResponse, AdminCalendarSettingsResponse, AdminInstanceResponse,
  AdminMailSettingsResponse, AdminSelfJoin as AdminSelfJoinWire, AnnouncementResponse, BrandAssetSlot,
  ChannelModeration, CreateBrandRequest, GuestPolicy, HistoryPolicy, OverrideEffect, Permission,
  PermissionScheme, PermissionSchemeResponse, PushProvider, PushProviderState, PushProviderStatus, Role,
  RuntimeSettingsKey, RuntimeSettingsResponse, RuntimeSettingsSection, SchemeChange, SchemeOverride,
  SchemeRole, SchemeRoleResponse, SendTestMailRequest, SessionPolicy, SetAnnouncementRequest,
  SettingsTestResponse, SfuStatus, StorageStatus, UnixMillis, UpdateBrandRequest,
  UpdateCalendarSettingsRequest, UpdateMailSettingsRequest, WorkspacePolicy,
};

use super::{MockBackend, dm_first, id_from_raw};
use crate::admin::{
  AdminChannel, AdminSpace, AdminUser, AuditAction, AuditEntry, AuditFilter, DeviceInfo, GuestInfo,
  OwnerRosterStatus, OwnerSigning, Retention, SelfJoinOutcome, SettingsTestKind,
};
use crate::backend::{AdminBackend, CallBackend, SecurityBackend};
use crate::calls::{HuddleInfo, MediaDevice, MediaDevices, Participant};
use crate::model::{
  ConversationId, ConversationKind, DeviceId, PresenceStatus, SpaceId, TimestampMs, UserId, raw_id,
};
use crate::security::{ContactVerification, KeyChange, demo_fingerprint, demo_safety_number};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const MIB: u64 = 1024 * 1024;

/// Admin-only state: policy, audit log and per-user devices. Reset on reload like the rest of the mock.
pub(super) struct OpsState {
  policy_version: u64,
  policy: WorkspacePolicy,
  scheme_changes: HashMap<Option<SpaceId>, Vec<SchemeChange>>,
  moderation: HashMap<ConversationId, ChannelModeration>,
  announcement: Option<AnnouncementResponse>,
  audit_filter: AuditFilter,
  channels: Vec<AdminChannel>,
  users: Vec<AdminUser>,
  audit: Vec<AuditEntry>,
  next_audit_id: u64,
  acknowledged_key_changes: Vec<ConversationId>,
  verified_contacts: Vec<UserId>,
  settings: super::settings_data::SettingsMock,
}

impl OpsState {
  pub(super) fn new(now: TimestampMs, users: &[crate::model::User]) -> Self {
    Self {
      policy_version: 12,
      policy: default_policy(),
      scheme_changes: HashMap::new(),
      moderation: HashMap::new(),
      announcement: None,
      audit_filter: AuditFilter::default(),
      channels: demo_channels(now),
      users: demo_admin_users(now, users),
      audit: demo_audit(now),
      next_audit_id: 1000,
      acknowledged_key_changes: Vec::new(),
      // Sam was compared in person already.
      verified_contacts: vec![id_from_raw(2)],
      settings: super::settings_data::SettingsMock::new(now),
    }
  }

  fn record(&mut self, now: TimestampMs, action: AuditAction, target: &str, detail: &str) {
    self.next_audit_id += 1;
    self.audit.push(AuditEntry {
      id: self.next_audit_id,
      at: now,
      actor: id_from_raw(1),
      action,
      target: target.to_owned(),
      detail: detail.to_owned(),
    });
  }
}

impl CallBackend for MockBackend {
  fn media_devices(&self) -> MediaDevices {
    let device = |id: &str, label: &str| MediaDevice {
      id: id.into(),
      label: label.into(),
    };
    MediaDevices::listed(
      vec![
        device("cam-0", "Integrated Camera"),
        device("cam-1", "External USB Camera"),
      ],
      vec![
        device("mic-0", "Built-in Microphone"),
        device("mic-1", "Headset Microphone"),
        device("mic-2", "Conference Speakerphone"),
      ],
      vec![
        device("spk-0", "Built-in Speakers"),
        device("spk-1", "Headset Earphones"),
        device("spk-2", "Conference Speakerphone"),
      ],
    )
  }

  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant> {
    let state = self.state.borrow();
    let members: Vec<UserId> = state
      .conversations
      .iter()
      .find(|conv| conv.id == conversation)
      .map(|conv| conv.member_ids.clone())
      .unwrap_or_default();
    // Group calls in big channels show a handful of people, enough to exercise the grid.
    members
      .into_iter()
      .filter(|member| *member != id_from_raw::<UserId>(1))
      .take(7)
      .enumerate()
      .map(|(index, user)| Participant {
        user,
        is_muted: index % 3 == 1,
        is_camera_on: index % 4 != 2,
        has_hand_raised: index == 3,
        is_sharing_screen: index == 0 && conversation == id_from_raw::<ConversationId>(13),
      })
      .collect()
  }

  fn has_calls(&self) -> bool {
    true
  }

  fn has_simulated_calls(&self) -> bool {
    true
  }

  fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo> {
    (conversation == id_from_raw::<ConversationId>(12)).then(|| HuddleInfo {
      participants: vec![id_from_raw(2), id_from_raw(3)],
    })
  }
}

impl AdminBackend for MockBackend {
  fn my_role(&self) -> Role {
    Role::Admin
  }

  fn admin_channels(&self) -> Vec<AdminChannel> {
    self.ops.borrow().channels.clone()
  }

  fn admin_users(&self) -> Vec<AdminUser> {
    self.ops.borrow().users.clone()
  }

  fn workspace_policy(&self) -> (u64, WorkspacePolicy) {
    let ops = self.ops.borrow();
    (ops.policy_version, ops.policy.clone())
  }

  /// Demo: the signed-in user is the only owner, so the console shows the "too few owners" warning.
  fn admin_owner_roster(&self) -> Option<OwnerRosterStatus> {
    Some(OwnerRosterStatus {
      owners: vec![OwnerSigning {
        user: crate::backend::ChatBackend::me(self).id,
        maybe_last_signed_at: Some(self.now - 3 * DAY),
      }],
      version: 1,
      admins: Vec::new(),
      maybe_expires_at: Some(self.now + 27 * DAY),
      recommended_minimum: self.ops.borrow().policy.owners.recommended_minimum,
      warning_lead_ms: 7 * DAY,
    })
  }

  fn set_workspace_policy(&self, policy: WorkspacePolicy, _genesis: Vec<crate::admin::RosterEntry>) {
    let mut ops = self.ops.borrow_mut();
    ops.policy_version += 1;
    let detail = format!("policy version {}", ops.policy_version);
    ops.policy = policy;
    ops.record(self.now, AuditAction::PolicyChanged, "security policy", &detail);
  }

  fn admin_spaces(&self) -> Vec<AdminSpace> {
    let mut names: Vec<String> = self
      .ops
      .borrow()
      .channels
      .iter()
      .map(|c| c.space_name.clone())
      .collect();
    names.sort();
    names.dedup();
    names
      .into_iter()
      .enumerate()
      .filter(|(_, name)| !name.is_empty())
      .map(|(index, name)| AdminSpace {
        id: id_from_raw(900 + index as u64),
        name,
        join_policy: teamcraft_proto::JoinPolicy::Open,
        is_archived: false,
        member_count: 0,
        guest_count: 0,
        channel_count: 0,
        created_at: 0,
      })
      .collect()
  }

  fn permission_scheme(&self, maybe_space: Option<SpaceId>) -> PermissionSchemeResponse {
    let ops = self.ops.borrow();
    let at = |scope: Option<SpaceId>| ops.scheme_changes.get(&scope).cloned().unwrap_or_default();
    let edits = |changes: &[SchemeChange]| -> Vec<(SchemeRole, Permission, bool)> {
      changes
        .iter()
        .filter_map(|change| match change.effect {
          OverrideEffect::Grant => Some((change.role.clone(), change.permission.clone(), true)),
          OverrideEffect::Revoke => Some((change.role.clone(), change.permission.clone(), false)),
          _ => None,
        })
        .collect()
    };
    let instance = PermissionScheme::default().with_changes(&edits(&at(None)));
    let own = maybe_space.map(|space| at(Some(space))).unwrap_or_default();
    let scheme = if maybe_space.is_some() {
      instance.with_changes(&edits(&own))
    } else {
      instance
    };
    let own = if maybe_space.is_some() { own } else { at(None) };
    PermissionSchemeResponse {
      roles: [
        SchemeRole::Guest,
        SchemeRole::Member,
        SchemeRole::SpaceAdmin,
        SchemeRole::ChannelAdmin,
        SchemeRole::Admin,
        SchemeRole::Owner,
      ]
      .into_iter()
      .map(|role| SchemeRoleResponse {
        granted: ALL_PERMISSIONS
          .iter()
          .filter(|permission| scheme.grants(&role, permission))
          .cloned()
          .collect(),
        overrides: own
          .iter()
          .filter(|change| change.role == role)
          .map(|change| SchemeOverride {
            permission: change.permission.clone(),
            effect: change.effect.clone(),
          })
          .collect(),
        role,
      })
      .collect(),
    }
  }

  fn change_permissions(&self, maybe_space: Option<SpaceId>, changes: Vec<SchemeChange>) {
    let mut ops = self.ops.borrow_mut();
    let list = ops.scheme_changes.entry(maybe_space).or_default();
    for change in changes {
      list.retain(|existing| !(existing.role == change.role && existing.permission == change.permission));
      if change.effect != OverrideEffect::Inherit {
        list.push(change);
      }
    }
    let target = if maybe_space.is_some() {
      "space scheme"
    } else {
      "instance scheme"
    };
    ops.record(
      self.now,
      AuditAction::PermissionsChanged,
      target,
      "permissions changed",
    );
  }

  fn channel_moderation(&self, channel: ConversationId) -> ChannelModeration {
    self
      .ops
      .borrow()
      .moderation
      .get(&channel)
      .cloned()
      .unwrap_or_default()
  }

  fn set_channel_moderation(&self, channel: ConversationId, moderation: ChannelModeration) {
    let mut ops = self.ops.borrow_mut();
    ops.moderation.insert(channel, moderation);
    let name = ops
      .channels
      .iter()
      .find(|c| c.id == channel)
      .map(|c| format!("#{}", c.name));
    ops.record(
      self.now,
      AuditAction::ModerationChanged,
      &name.unwrap_or_default(),
      "moderation changed",
    );
  }

  fn channel_member_candidates(&self, channel: ConversationId) -> Vec<UserId> {
    let _ = channel;
    self
      .ops
      .borrow()
      .users
      .iter()
      .filter(|u| u.is_active)
      .take(4)
      .map(|u| u.user.id)
      .collect()
  }

  fn assign_channel_owner(&self, channel: ConversationId, user: UserId) {
    let mut ops = self.ops.borrow_mut();
    let Some(entry) = ops.channels.iter_mut().find(|c| c.id == channel) else {
      return;
    };
    entry.maybe_owner = Some(user);
    let name = format!("#{}", entry.name);
    ops.record(self.now, AuditAction::OwnerAssigned, &name, "");
  }

  fn announcement(&self) -> Option<AnnouncementResponse> {
    self.ops.borrow().announcement.clone()
  }

  fn set_announcement(&self, announcement: SetAnnouncementRequest) {
    let mut ops = self.ops.borrow_mut();
    ops.announcement = Some(AnnouncementResponse {
      id: id_from_raw(1),
      text: announcement.text,
      level: announcement.level,
      is_dismissible: announcement.is_dismissible,
      created_at: UnixMillis(self.now),
      maybe_expires_at: announcement.maybe_expires_at,
      is_dismissed: false,
    });
    ops.record(self.now, AuditAction::AnnouncementChanged, "announcement", "set");
  }

  fn clear_announcement(&self) {
    let mut ops = self.ops.borrow_mut();
    ops.announcement = None;
    ops.record(
      self.now,
      AuditAction::AnnouncementChanged,
      "announcement",
      "cleared",
    );
  }

  fn admin_brands(&self) -> Option<AdminBrandsResponse> {
    Some(self.ops.borrow().settings.brands.clone())
  }

  fn create_brand(&self, request: CreateBrandRequest) {
    let mut ops = self.ops.borrow_mut();
    let id = request.id.clone();
    ops.settings.create_brand(request);
    ops.record(self.now, AuditAction::BrandChanged, &id, "created");
  }

  fn update_brand(&self, id: String, request: UpdateBrandRequest) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.update_brand(&id, request);
    ops.record(self.now, AuditAction::BrandChanged, &id, "updated");
  }

  fn delete_brand(&self, id: String) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.delete_brand(&id);
    ops.record(self.now, AuditAction::BrandChanged, &id, "deleted");
  }

  fn take_over_brand(&self, id: String) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.take_over_brand(&id);
    ops.record(self.now, AuditAction::BrandChanged, &id, "taken over");
  }

  fn upload_brand_asset(&self, id: String, slot: BrandAssetSlot, _bytes: Vec<u8>, _content_type: String) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.set_asset(&id, slot, true);
    ops.record(self.now, AuditAction::BrandChanged, &id, "image set");
  }

  fn delete_brand_asset(&self, id: String, slot: BrandAssetSlot) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.set_asset(&id, slot, false);
    ops.record(self.now, AuditAction::BrandChanged, &id, "image removed");
  }

  fn mail_settings(&self) -> Option<AdminMailSettingsResponse> {
    Some(self.ops.borrow().settings.mail.clone())
  }

  fn save_mail_settings(&self, request: UpdateMailSettingsRequest) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.save_mail(request);
    ops.record(self.now, AuditAction::MailSettingsChanged, "mail", "updated");
  }

  fn reset_mail_settings(&self) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.reset_mail(self.now);
    ops.record(self.now, AuditAction::MailSettingsChanged, "mail", "reset");
  }

  fn send_test_mail(&self, _request: SendTestMailRequest) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.last_test = Some((
      SettingsTestKind::Mail,
      SettingsTestResponse {
        is_ok: true,
        maybe_error: None,
      },
    ));
    ops.record(self.now, AuditAction::MailSettingsChanged, "mail", "test sent");
  }

  fn calendar_settings(&self) -> Option<AdminCalendarSettingsResponse> {
    Some(self.ops.borrow().settings.calendar.clone())
  }

  fn save_calendar_settings(&self, request: UpdateCalendarSettingsRequest) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.save_calendar(request);
    ops.record(
      self.now,
      AuditAction::CalendarSettingsChanged,
      "calendar",
      "updated",
    );
  }

  fn reset_calendar_settings(&self) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.reset_calendar(self.now);
    ops.record(
      self.now,
      AuditAction::CalendarSettingsChanged,
      "calendar",
      "reset",
    );
  }

  fn test_calendar(&self) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.last_test = Some((
      SettingsTestKind::Calendar,
      SettingsTestResponse {
        is_ok: true,
        maybe_error: None,
      },
    ));
    ops.record(
      self.now,
      AuditAction::CalendarSettingsChanged,
      "calendar",
      "tested",
    );
  }

  fn last_settings_test(&self) -> Option<(SettingsTestKind, SettingsTestResponse)> {
    self.ops.borrow().settings.last_test.clone()
  }

  fn runtime_settings(&self) -> Option<RuntimeSettingsResponse> {
    Some(self.ops.borrow().settings.runtime.clone())
  }

  fn save_runtime_settings(&self, section: RuntimeSettingsSection) {
    let mut ops = self.ops.borrow_mut();
    let name = section.key().as_str().to_owned();
    ops.settings.save_runtime(section);
    ops.record(self.now, AuditAction::SettingsChanged, &name, "updated");
  }

  fn reset_runtime_settings(&self, key: RuntimeSettingsKey) {
    let mut ops = self.ops.borrow_mut();
    ops.settings.reset_runtime(key.clone());
    ops.record(self.now, AuditAction::SettingsChanged, key.as_str(), "reset");
  }

  fn instance_status(&self) -> Option<AdminInstanceResponse> {
    Some(AdminInstanceResponse {
      sfu: SfuStatus {
        is_configured: true,
        maybe_url: Some("https://sfu.example.com:4433/call".to_owned()),
        is_reporting_enabled: true,
        has_host_controls: true,
        maybe_last_contact_at: Some(UnixMillis(self.now - 4_000)),
        active_calls: 2,
      },
      push: vec![
        push_status(PushProvider::WebPush, PushProviderState::Active, 14, self.now),
        push_status(PushProvider::Fcm, PushProviderState::Active, 9, self.now),
        push_status(PushProvider::UnifiedPush, PushProviderState::Active, 1, self.now),
        push_status(PushProvider::Apns, PushProviderState::NotConfigured, 0, self.now),
      ],
      storage: StorageStatus {
        is_configured: true,
        attachment_count: 1_842,
        attachment_bytes: 3 * 1024 * MIB + 412 * MIB,
        index_segment_bytes: 96 * MIB,
      },
      is_telephony_available: false,
    })
  }

  fn audit_filter(&self) -> AuditFilter {
    self.ops.borrow().audit_filter
  }

  fn set_audit_filter(&self, filter: AuditFilter) {
    self.ops.borrow_mut().audit_filter = filter;
  }

  fn set_channel_retention(&self, channel: ConversationId, retention: Retention) {
    let mut ops = self.ops.borrow_mut();
    let Some(entry) = ops.channels.iter_mut().find(|candidate| candidate.id == channel) else {
      return;
    };
    entry.retention = retention;
    let name = entry.name.clone();
    ops.record(
      self.now,
      AuditAction::RetentionChanged,
      &format!("#{name}"),
      "retention updated",
    );
  }

  fn remove_device(&self, user: UserId, device: DeviceId) {
    let mut ops = self.ops.borrow_mut();
    let Some(entry) = ops.users.iter_mut().find(|candidate| candidate.user.id == user) else {
      return;
    };
    let Some(position) = entry.devices.iter().position(|candidate| candidate.id == device) else {
      return;
    };
    let removed = entry.devices.remove(position);
    let target = format!("{} / {}", entry.user.display_name, removed.name);
    ops.record(self.now, AuditAction::DeviceRemoved, &target, "sessions revoked");
  }

  fn set_user_active(&self, user: UserId, is_active: bool) {
    let mut ops = self.ops.borrow_mut();
    let Some(entry) = ops.users.iter_mut().find(|candidate| candidate.user.id == user) else {
      return;
    };
    entry.is_active = is_active;
    let target = entry.user.display_name.clone();
    let action = if is_active {
      AuditAction::UserReactivated
    } else {
      AuditAction::UserDeactivated
    };
    ops.record(self.now, action, &target, "");
  }

  fn can_start_org_recovery(&self) -> bool {
    true
  }

  fn start_org_recovery(&self, user: UserId) {
    let mut ops = self.ops.borrow_mut();
    let Some(target) = ops
      .users
      .iter()
      .find(|candidate| candidate.user.id == user)
      .map(|u| u.user.display_name.clone())
    else {
      return;
    };
    ops.record(
      self.now,
      AuditAction::RecoveryStarted,
      &target,
      "waiting for a second admin",
    );
  }

  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome {
    let mut ops = self.ops.borrow_mut();
    let outcome = match ops.policy.admin_self_join {
      AdminSelfJoinWire::Visible => SelfJoinOutcome::Joined,
      AdminSelfJoinWire::OwnerApproval => SelfJoinOutcome::ApprovalRequested,
      AdminSelfJoinWire::Disabled | AdminSelfJoinWire::Unknown(_) => SelfJoinOutcome::Disabled,
    };
    if outcome == SelfJoinOutcome::Joined
      && let Some(name) = ops
        .channels
        .iter()
        .find(|c| c.id == channel)
        .map(|c| c.name.clone())
    {
      ops.record(
        self.now,
        AuditAction::AdminJoinedChannel,
        &format!("#{name}"),
        "members were notified",
      );
    }
    outcome
  }

  fn audit_log(&self) -> Vec<AuditEntry> {
    let mut log = self.ops.borrow().audit.clone();
    log.sort_by_key(|entry| std::cmp::Reverse((entry.at, entry.id)));
    log
  }
}

impl SecurityBackend for MockBackend {
  fn my_devices(&self) -> Vec<DeviceInfo> {
    let ops = self.ops.borrow();
    ops
      .users
      .iter()
      .find(|entry| entry.user.id == id_from_raw::<UserId>(1))
      .map(|entry| entry.devices.clone())
      .unwrap_or_default()
  }

  fn remove_my_device(&self, device: DeviceId) {
    self.remove_device(id_from_raw(1), device);
  }

  fn key_change(&self, conversation: ConversationId) -> Option<KeyChange> {
    let ops = self.ops.borrow();
    // Robin's identity key changed after a full reset (the third demo DM).
    (conversation == id_from_raw::<ConversationId>(dm_first() + 2)
      && !ops.acknowledged_key_changes.contains(&conversation))
    .then(|| KeyChange {
      user: id_from_raw(4),
      conversation,
      old_fingerprint: "3fa9c1d27b84e05a916c2d48f07be3a5".into(),
      new_fingerprint: "b71e04c98d2a5f63e0187a4c5d9b2f16".into(),
      changed_at: self.now - 3 * HOUR,
    })
  }

  fn acknowledge_key_change(&self, conversation: ConversationId) {
    self.ops.borrow_mut().acknowledged_key_changes.push(conversation);
  }

  fn my_identity_fingerprint(&self) -> String {
    demo_fingerprint(raw_id(self.state.borrow().users[0].id))
  }

  fn contact_verification(&self, user: UserId) -> ContactVerification {
    let me = raw_id(self.state.borrow().users[0].id);
    ContactVerification {
      safety_number: demo_safety_number(me, raw_id(user)),
      is_verified: self.ops.borrow().verified_contacts.contains(&user),
    }
  }

  fn set_contact_verified(&self, user: UserId, is_verified: bool) {
    let mut ops = self.ops.borrow_mut();
    ops.verified_contacts.retain(|known| *known != user);
    if is_verified {
      ops.verified_contacts.push(user);
    }
  }
}

fn default_policy() -> WorkspacePolicy {
  let defaults = WorkspacePolicy::default();
  WorkspacePolicy {
    history_default: HistoryPolicy::FromJoin,
    session: SessionPolicy {
      requires_mfa: true,
      maybe_max_age_hours: Some(30 * 24),
      maybe_idle_timeout_minutes: Some(120),
    },
    guests: GuestPolicy {
      allowed_email_domains: vec!["example.com".to_owned(), "partner.example".to_owned()],
      ..GuestPolicy::default()
    },
    ..defaults
  }
}

/// Demo status: configured providers delivered a minute ago, the others never.
fn push_status(
  provider: PushProvider,
  state: PushProviderState,
  device_count: u32,
  now: i64,
) -> PushProviderStatus {
  let is_active = state == PushProviderState::Active;
  PushProviderStatus {
    provider,
    state,
    device_count,
    success_count: if is_active {
      u64::from(device_count) * 40
    } else {
      0
    },
    failure_count: 0,
    endpoint_failure_count: 0,
    maybe_last_success_at: is_active.then_some(UnixMillis(now - 60_000)),
    maybe_last_failure_at: None,
    maybe_last_error: None,
  }
}

fn demo_channels(now: TimestampMs) -> Vec<AdminChannel> {
  let retention = |days: Option<u32>, messages: Option<u32>| Retention {
    maybe_days: days,
    maybe_max_messages: messages,
    keeps_pinned: true,
  };
  #[allow(clippy::too_many_arguments)]
  let channel = |id: u64,
                 space: &str,
                 name: &str,
                 kind: ConversationKind,
                 members: u32,
                 guests: u32,
                 owner: Option<u64>,
                 idle: i64,
                 messages: u64,
                 storage_mib: u64,
                 retention: Retention,
                 history: HistoryPolicy| AdminChannel {
    id: id_from_raw(id),
    space_name: space.into(),
    name: name.into(),
    kind,
    member_count: members,
    guest_count: guests,
    maybe_owner: owner.map(id_from_raw),
    last_activity: now - idle,
    message_count: messages,
    storage_bytes: storage_mib * MIB,
    retention,
    history,
    is_archived: false,
  };
  use ConversationKind::{Integration, Private, Public};
  let from_join = HistoryPolicy::FromJoin;
  let full = HistoryPolicy::Full;
  vec![
    channel(
      10,
      "Project A",
      "general",
      Public,
      8,
      1,
      Some(1),
      4 * MINUTE,
      6_240,
      412,
      retention(None, None),
      full.clone(),
    ),
    channel(
      11,
      "Project A",
      "announcements",
      Public,
      8,
      1,
      Some(1),
      3 * DAY,
      48,
      12,
      retention(None, None),
      full.clone(),
    ),
    channel(
      12,
      "Project A",
      "engineering",
      Public,
      7,
      0,
      Some(2),
      9 * MINUTE,
      18_730,
      1_840,
      retention(Some(365), None),
      full.clone(),
    ),
    channel(
      13,
      "Project A",
      "design",
      Public,
      5,
      1,
      Some(3),
      2 * HOUR,
      3_120,
      2_210,
      retention(Some(180), Some(20_000)),
      full.clone(),
    ),
    channel(
      14,
      "Project A",
      "leadership",
      Private,
      3,
      0,
      Some(1),
      5 * HOUR,
      1_064,
      96,
      retention(None, None),
      from_join.clone(),
    ),
    channel(
      15,
      "Project A",
      "ci-builds",
      Integration,
      8,
      0,
      Some(2),
      2 * MINUTE,
      2_400,
      38,
      retention(Some(30), None),
      from_join.clone(),
    ),
    channel(
      16,
      "Project A",
      "app-logs",
      Integration,
      8,
      0,
      Some(2),
      MINUTE,
      50_000,
      4_610,
      retention(Some(30), Some(100_000)),
      from_join.clone(),
    ),
    channel(
      40,
      "Project A",
      "hr-confidential",
      Private,
      2,
      0,
      None,
      11 * DAY,
      312,
      41,
      retention(Some(90), None),
      from_join.clone(),
    ),
    channel(
      41,
      "Project A",
      "customer-escalations",
      Private,
      6,
      2,
      Some(6),
      6 * HOUR,
      2_906,
      530,
      retention(Some(365), None),
      from_join.clone(),
    ),
    channel(
      42,
      "Project A",
      "incident-2025-03",
      Private,
      4,
      0,
      Some(2),
      140 * DAY,
      877,
      64,
      retention(None, None),
      full.clone(),
    ),
    channel(
      20,
      "Project B",
      "general",
      Public,
      6,
      0,
      Some(1),
      40 * MINUTE,
      1_422,
      150,
      retention(None, None),
      full.clone(),
    ),
    channel(
      21,
      "Project B",
      "planning",
      Public,
      5,
      0,
      Some(5),
      7 * HOUR,
      960,
      88,
      retention(Some(365), None),
      from_join.clone(),
    ),
    channel(
      43,
      "Project B",
      "budget",
      Private,
      3,
      0,
      Some(1),
      DAY,
      205,
      19,
      retention(Some(730), None),
      from_join,
    ),
  ]
}

fn demo_admin_users(now: TimestampMs, users: &[crate::model::User]) -> Vec<AdminUser> {
  let device = |id: u64, name: &str, platform: &str, seen: i64, current: bool| DeviceInfo {
    id: id_from_raw(id),
    name: name.into(),
    platform: platform.into(),
    last_seen: now - seen,
    is_current: current,
  };
  users
    .iter()
    .filter(|user| !user.is_bot)
    .map(|user| {
      let n = raw_id(user.id);
      let role = match n {
        1 => Role::Admin,
        2 => Role::Owner,
        9 => Role::Guest,
        _ => Role::Member,
      };
      let devices = match n {
        1 => vec![
          device(101, "Laptop (Linux)", "Linux desktop", 0, true),
          device(102, "Phone (iOS)", "iOS", 3 * HOUR, false),
        ],
        2 => vec![
          device(201, "Laptop (macOS)", "macOS desktop", 12 * MINUTE, false),
          device(202, "Phone (Android)", "Android", 40 * MINUTE, false),
          device(203, "Office desktop", "Linux desktop", 9 * DAY, false),
        ],
        5 => vec![],
        9 => vec![device(901, "Firefox on Windows", "Web", 2 * DAY, false)],
        _ => vec![
          device(
            n * 100 + 1,
            "Laptop",
            "macOS desktop",
            (n as i64) * 17 * MINUTE,
            false,
          ),
          device(n * 100 + 2, "Phone", "iOS", (n as i64) * 3 * HOUR, false),
        ],
      };
      let last_seen = devices
        .iter()
        .map(|d| d.last_seen)
        .max()
        .unwrap_or(now - 30 * DAY);
      let mut user = user.clone();
      if n == 5 {
        user.presence = PresenceStatus::Offline;
      }
      AdminUser {
        space_names: if n == 9 {
          vec!["Project A".into()]
        } else {
          vec!["Project A".into(), "Project B".into()]
        },
        is_active: n != 8,
        maybe_guest: (n == 9).then(|| GuestInfo {
          sponsor: id_from_raw(3),
          expires_at: now + 12 * DAY,
        }),
        role,
        devices,
        last_seen,
        user,
      }
    })
    .collect()
}

fn demo_audit(now: TimestampMs) -> Vec<AuditEntry> {
  let rows: [(u64, AuditAction, &str, &str); 14] = [
    (
      2,
      AuditAction::PolicyChanged,
      "security policy",
      "policy version 12: admin_self_join set to visible",
    ),
    (
      1,
      AuditAction::AdminJoinedChannel,
      "#customer-escalations",
      "members were notified",
    ),
    (
      2,
      AuditAction::GuestInvited,
      "Riley Guest",
      "expires in 14 days, sponsor Jordan Lee",
    ),
    (
      1,
      AuditAction::RetentionChanged,
      "#app-logs",
      "30 days or 100,000 messages",
    ),
    (
      2,
      AuditAction::DeviceRemoved,
      "Casey Diaz / Phone",
      "reported lost",
    ),
    (
      1,
      AuditAction::OwnerAssigned,
      "#incident-2025-03",
      "new owner Sam Rivera",
    ),
    (
      2,
      AuditAction::UserDeactivated,
      "Casey Diaz",
      "disabled in the identity provider",
    ),
    (1, AuditAction::ChannelArchived, "#old-launch", ""),
    (
      2,
      AuditAction::RecoveryStarted,
      "Kim Novak",
      "approved by 2 admins",
    ),
    (
      1,
      AuditAction::PolicyChanged,
      "security policy",
      "policy version 11: session length 30 days",
    ),
    (
      2,
      AuditAction::RetentionChanged,
      "#design",
      "180 days or 20,000 messages",
    ),
    (1, AuditAction::GuestInvited, "Riley Guest", "pending first login"),
    (
      2,
      AuditAction::DeviceRemoved,
      "Kim Novak / Old laptop",
      "recovery cleanup",
    ),
    (
      1,
      AuditAction::PolicyChanged,
      "security policy",
      "policy version 10: org recovery enabled for members",
    ),
  ];
  rows
    .into_iter()
    .enumerate()
    .map(|(index, (actor, action, target, detail))| AuditEntry {
      id: index as u64 + 1,
      at: now - (index as i64 + 1) * (5 * HOUR + 17 * MINUTE),
      actor: id_from_raw(actor),
      action,
      target: target.to_owned(),
      detail: detail.to_owned(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::{AdminBackend, CallBackend, SecurityBackend};

  const NOW: TimestampMs = 1_792_584_000_000;

  #[test]
  fn admin_sees_private_channels_without_membership() {
    let backend = MockBackend::new(NOW);
    let channels = backend.admin_channels();
    let private: Vec<_> = channels
      .iter()
      .filter(|c| c.kind == ConversationKind::Private)
      .collect();
    assert!(private.len() >= 4);
    // Metadata of a channel the demo user is not a member of.
    assert!(crate::backend::ChatBackend::conversation(&backend, id_from_raw(40)).is_none());
    assert!(private.iter().any(|c| c.maybe_owner.is_none()));
  }

  #[test]
  fn self_join_follows_the_policy() {
    let backend = MockBackend::new(NOW);
    let channel = id_from_raw(41);
    assert_eq!(backend.admin_join_channel(channel), SelfJoinOutcome::Joined);
    let (_, mut policy) = backend.workspace_policy();
    policy.admin_self_join = AdminSelfJoinWire::Disabled;
    backend.set_workspace_policy(policy, Vec::new());
    assert_eq!(backend.admin_join_channel(channel), SelfJoinOutcome::Disabled);
    assert_eq!(backend.workspace_policy().0, 13);
  }

  #[test]
  fn changes_are_audited_newest_first() {
    let backend = MockBackend::new(NOW);
    let user = id_from_raw(2);
    let device = backend
      .admin_users()
      .into_iter()
      .find(|u| u.user.id == user)
      .map(|u| u.devices[0].id);
    backend.remove_device(user, device.expect("demo owner has devices"));
    let log = backend.audit_log();
    assert_eq!(log[0].action, AuditAction::DeviceRemoved);
    assert!(log.windows(2).all(|pair| pair[0].at >= pair[1].at));
  }

  #[test]
  fn key_change_can_be_acknowledged() {
    let backend = MockBackend::new(NOW);
    let conversation = id_from_raw(dm_first() + 2);
    assert!(backend.key_change(conversation).is_some());
    backend.acknowledge_key_change(conversation);
    assert!(backend.key_change(conversation).is_none());
  }

  #[test]
  fn huddle_and_roster_exist() {
    let backend = MockBackend::new(NOW);
    assert_eq!(
      backend.huddle(id_from_raw(12)).map(|h| h.participants.len()),
      Some(2)
    );
    assert!(backend.huddle(id_from_raw(13)).is_none());
    assert!(!backend.call_participants(id_from_raw(12)).is_empty());
  }
}
