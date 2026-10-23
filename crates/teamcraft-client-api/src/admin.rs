//! View models of the administration console. Admins see metadata only: nothing here carries message
//! content (admin.md §2.1).

use teamcraft_proto::{HistoryPolicy, Role};

use crate::model::{ConversationId, ConversationKind, DeviceId, SpaceId, TimestampMs, User, UserId};

/// Retention of one channel: whichever limit is stricter wins; `None` keeps everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Retention {
  pub maybe_days: Option<u32>,
  pub maybe_max_messages: Option<u32>,
  pub keeps_pinned: bool,
}

/// A channel as admins see it, including private ones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminChannel {
  pub id: ConversationId,
  pub space_name: String,
  pub name: String,
  pub kind: ConversationKind,
  pub member_count: u32,
  pub guest_count: u32,
  /// `None` marks an orphaned channel that needs a new owner (admin.md §2.5).
  pub maybe_owner: Option<UserId>,
  pub last_activity: TimestampMs,
  pub message_count: u64,
  pub storage_bytes: u64,
  pub retention: Retention,
  pub history: HistoryPolicy,
  pub is_archived: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
  pub id: DeviceId,
  pub name: String,
  pub platform: String,
  pub last_seen: TimestampMs,
  pub is_current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestInfo {
  pub sponsor: UserId,
  pub expires_at: TimestampMs,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdminUser {
  pub user: User,
  pub role: Role,
  pub devices: Vec<DeviceInfo>,
  pub last_seen: TimestampMs,
  pub space_names: Vec<String>,
  pub is_active: bool,
  pub maybe_guest: Option<GuestInfo>,
}

/// What happens when an admin adds themselves to a private channel (admin.md §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdminSelfJoin {
  Visible,
  OwnerApproval,
  Disabled,
}

impl AdminSelfJoin {
  pub const ALL: [AdminSelfJoin; 3] = [Self::Visible, Self::OwnerApproval, Self::Disabled];

  /// Wire name of the setting value.
  pub fn slug(self) -> &'static str {
    match self {
      Self::Visible => "visible",
      Self::OwnerApproval => "owner_approval",
      Self::Disabled => "disabled",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// Outcome of an admin adding themselves to a private channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelfJoinOutcome {
  /// Joined; members see a system message with an admin badge.
  Joined,
  /// A channel owner has to approve first.
  ApprovalRequested,
  Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuditAction {
  PolicyChanged,
  RetentionChanged,
  DeviceRemoved,
  UserDeactivated,
  UserReactivated,
  RecoveryStarted,
  AdminJoinedChannel,
  OwnerAssigned,
  GuestInvited,
  ChannelArchived,
  MemberAdded,
  MemberRemoved,
  MemberRoleChanged,
  ChannelUpdated,
  SpaceCreated,
  SpaceUpdated,
  SessionEnded,
  PermissionsChanged,
  ModerationChanged,
  AnnouncementChanged,
  /// A brand was created, changed, deleted, taken over or got a new image.
  BrandChanged,
  /// The mail server settings were changed, reset or tested.
  MailSettingsChanged,
  /// The calendar server settings were changed, reset or tested.
  CalendarSettingsChanged,
  /// Instance settings (limits, rules, mail texts, ...) were changed or reset.
  SettingsChanged,
  /// Anything the console has no wording for yet; the detail carries the raw action name.
  Other,
}

impl AuditAction {
  pub const ALL: [AuditAction; 25] = [
    Self::PolicyChanged,
    Self::RetentionChanged,
    Self::DeviceRemoved,
    Self::UserDeactivated,
    Self::UserReactivated,
    Self::RecoveryStarted,
    Self::AdminJoinedChannel,
    Self::OwnerAssigned,
    Self::GuestInvited,
    Self::ChannelArchived,
    Self::MemberAdded,
    Self::MemberRemoved,
    Self::MemberRoleChanged,
    Self::ChannelUpdated,
    Self::SpaceCreated,
    Self::SpaceUpdated,
    Self::SessionEnded,
    Self::PermissionsChanged,
    Self::ModerationChanged,
    Self::AnnouncementChanged,
    Self::BrandChanged,
    Self::MailSettingsChanged,
    Self::CalendarSettingsChanged,
    Self::SettingsChanged,
    Self::Other,
  ];

  pub fn slug(self) -> &'static str {
    match self {
      Self::PolicyChanged => "policy-changed",
      Self::RetentionChanged => "retention-changed",
      Self::DeviceRemoved => "device-removed",
      Self::UserDeactivated => "user-deactivated",
      Self::UserReactivated => "user-reactivated",
      Self::RecoveryStarted => "recovery-started",
      Self::AdminJoinedChannel => "admin-joined-channel",
      Self::OwnerAssigned => "owner-assigned",
      Self::GuestInvited => "guest-invited",
      Self::ChannelArchived => "channel-archived",
      Self::MemberAdded => "member-added",
      Self::MemberRemoved => "member-removed",
      Self::MemberRoleChanged => "member-role-changed",
      Self::ChannelUpdated => "channel-updated",
      Self::SpaceCreated => "space-created",
      Self::SpaceUpdated => "space-updated",
      Self::SessionEnded => "session-ended",
      Self::PermissionsChanged => "permissions-changed",
      Self::ModerationChanged => "moderation-changed",
      Self::AnnouncementChanged => "announcement-changed",
      Self::BrandChanged => "brand-changed",
      Self::MailSettingsChanged => "mail-settings-changed",
      Self::CalendarSettingsChanged => "calendar-settings-changed",
      Self::SettingsChanged => "settings-changed",
      Self::Other => "other",
    }
  }
}

/// Which connection a settings test checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTestKind {
  Mail,
  Calendar,
}

/// Why the policy form is read-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyBlocker {
  /// Only owners sign the policy.
  NotOwner,
  /// Signing needs the encryption keys, which only the desktop app holds.
  NoSigningKeys,
}

/// One audit log line: metadata only, never message content (admin.md §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEntry {
  pub id: u64,
  pub at: TimestampMs,
  pub actor: UserId,
  pub action: AuditAction,
  /// Name of the affected channel, user, device or setting.
  pub target: String,
  pub detail: String,
}

/// A space as admins see it, for scoping a permission scheme.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminSpace {
  pub id: SpaceId,
  pub name: String,
}

/// Narrows the audit log; the server filters, so the list stays short.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuditFilter {
  pub maybe_actor: Option<UserId>,
  pub maybe_since: Option<TimestampMs>,
  pub maybe_until: Option<TimestampMs>,
}

/// Sections of the administration console.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdminSection {
  Channels,
  Users,
  /// Roles and what they may do, per instance and space.
  Permissions,
  Security,
  /// Directory, statuses and notification defaults.
  People,
  Announcement,
  Backgrounds,
  /// Brands: host names, colors, images and texts per brand.
  Brands,
  /// The outgoing mail server.
  Mail,
  /// The calendar server meetings are written to.
  Calendar,
  /// Limits, rules and texts that used to need the configuration file.
  Settings,
  Retention,
  /// How the desktop apps update (docs/updates.md §4.4).
  Updates,
  /// Media server, push providers, storage and telephony.
  Instance,
  Audit,
}

impl AdminSection {
  pub const ALL: [AdminSection; 15] = [
    Self::Channels,
    Self::Users,
    Self::Permissions,
    Self::Security,
    Self::People,
    Self::Announcement,
    Self::Backgrounds,
    Self::Brands,
    Self::Mail,
    Self::Calendar,
    Self::Settings,
    Self::Retention,
    Self::Updates,
    Self::Instance,
    Self::Audit,
  ];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Channels => "channels",
      Self::Users => "users",
      Self::Permissions => "permissions",
      Self::Security => "security",
      Self::People => "people",
      Self::Announcement => "announcement",
      Self::Backgrounds => "backgrounds",
      Self::Brands => "brands",
      Self::Mail => "mail",
      Self::Calendar => "calendar",
      Self::Settings => "settings",
      Self::Retention => "retention",
      Self::Updates => "updates",
      Self::Instance => "instance",
      Self::Audit => "audit",
    }
  }
}

/// "12.4 MB" style size, base 1024 with one decimal.
pub fn format_bytes(bytes: u64) -> String {
  const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
  let mut value = bytes as f64;
  let mut unit = 0;
  while value >= 1024.0 && unit < UNITS.len() - 1 {
    value /= 1024.0;
    unit += 1;
  }
  if unit == 0 {
    format!("{bytes} B")
  } else {
    format!("{value:.1} {}", UNITS[unit])
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn policy_values_round_trip_through_slugs() {
    for value in AdminSelfJoin::ALL {
      assert_eq!(AdminSelfJoin::from_slug(value.slug()), Some(value));
    }
    assert_eq!(AdminSelfJoin::from_slug("nope"), None);
  }

  #[test]
  fn bytes_are_formatted() {
    assert_eq!(format_bytes(512), "512 B");
    assert_eq!(format_bytes(1536), "1.5 KB");
    assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
  }

  #[test]
  fn audit_slugs_are_unique() {
    let mut slugs: Vec<&str> = AuditAction::ALL.iter().map(|action| action.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), AuditAction::ALL.len());
  }
}
