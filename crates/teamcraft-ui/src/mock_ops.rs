//! Mock data for calls, administration and device security. Child module of `mock` so it can read the
//! demo users and conversations the chat part owns.

use teamcraft_proto::{HistoryPolicy, Role};

use super::{MockBackend, dm_first, id_from_raw};
use crate::admin::{
  AdminChannel, AdminSelfJoin, AdminUser, AuditAction, AuditEntry, DeviceInfo, DmScope, GuestInfo, Retention,
  SecurityPolicy, SelfJoinOutcome,
};
use crate::backend::{AdminBackend, CallBackend, SecurityBackend};
use crate::calls::{HuddleInfo, MediaDevice, MediaDevices, Participant};
use crate::model::{ConversationId, ConversationKind, DeviceId, PresenceStatus, TimestampMs, UserId, raw_id};
use crate::security::KeyChange;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const MIB: u64 = 1024 * 1024;

/// Admin-only state: policy, audit log and per-user devices. Reset on reload like the rest of the mock.
pub(super) struct OpsState {
  policy: SecurityPolicy,
  channels: Vec<AdminChannel>,
  users: Vec<AdminUser>,
  audit: Vec<AuditEntry>,
  next_audit_id: u64,
  acknowledged_key_changes: Vec<ConversationId>,
}

impl OpsState {
  pub(super) fn new(now: TimestampMs, users: &[crate::model::User]) -> Self {
    Self {
      policy: default_policy(),
      channels: demo_channels(now),
      users: demo_admin_users(now, users),
      audit: demo_audit(now),
      next_audit_id: 1000,
      acknowledged_key_changes: Vec::new(),
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
    MediaDevices {
      cameras: vec![
        device("cam-0", "Integrated Camera"),
        device("cam-1", "External USB Camera"),
      ],
      microphones: vec![
        device("mic-0", "Built-in Microphone"),
        device("mic-1", "Headset Microphone"),
        device("mic-2", "Conference Speakerphone"),
      ],
      speakers: vec![
        device("spk-0", "Built-in Speakers"),
        device("spk-1", "Headset Earphones"),
        device("spk-2", "Conference Speakerphone"),
      ],
    }
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

  fn security_policy(&self) -> SecurityPolicy {
    self.ops.borrow().policy.clone()
  }

  fn set_security_policy(&self, mut policy: SecurityPolicy) {
    let mut ops = self.ops.borrow_mut();
    policy.version = ops.policy.version + 1;
    let detail = format!("policy version {}", policy.version);
    ops.policy = policy;
    ops.record(self.now, AuditAction::PolicyChanged, "security policy", &detail);
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
      AdminSelfJoin::Visible => SelfJoinOutcome::Joined,
      AdminSelfJoin::OwnerApproval => SelfJoinOutcome::ApprovalRequested,
      AdminSelfJoin::Disabled => SelfJoinOutcome::Disabled,
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
}

fn default_policy() -> SecurityPolicy {
  SecurityPolicy {
    version: 12,
    admin_self_join: AdminSelfJoin::Visible,
    org_recovery_for_members: true,
    org_recovery_for_guests: false,
    recovery_approvals_required: 2,
    requires_mfa: true,
    requires_mfa_for_guests: true,
    guest_email_domains: "example.com, partner.example".into(),
    session_days: 30,
    idle_timeout_minutes: 120,
    default_history: HistoryPolicy::FromJoin,
    dm_scope: DmScope::Anyone,
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
    let mut policy = backend.security_policy();
    policy.admin_self_join = AdminSelfJoin::Disabled;
    backend.set_security_policy(policy);
    assert_eq!(backend.admin_join_channel(channel), SelfJoinOutcome::Disabled);
    assert_eq!(backend.security_policy().version, 13);
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
