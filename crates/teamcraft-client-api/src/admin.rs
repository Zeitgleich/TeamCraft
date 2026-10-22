//! View models of the administration console. Admins see metadata only: nothing here carries message
//! content (admin.md §2.1).

use teamcraft_proto::{HistoryPolicy, Role};

use crate::model::{ConversationId, ConversationKind, DeviceId, TimestampMs, User, UserId};

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

#[derive(Clone, Debug, PartialEq, Eq)]
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

/// Who may start direct messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmScope {
  Anyone,
  SharedSpaces,
}

impl DmScope {
  pub const ALL: [DmScope; 2] = [Self::Anyone, Self::SharedSpaces];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Anyone => "anyone",
      Self::SharedSpaces => "shared_spaces",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// The signed workspace policy that member clients enforce themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityPolicy {
  pub version: u32,
  pub admin_self_join: AdminSelfJoin,
  pub org_recovery_for_members: bool,
  pub org_recovery_for_guests: bool,
  /// Admins that must approve a recovery (e2ee.md §6.3).
  pub recovery_approvals_required: u8,
  pub requires_mfa: bool,
  pub requires_mfa_for_guests: bool,
  pub guest_email_domains: String,
  pub session_days: u32,
  pub idle_timeout_minutes: u32,
  pub default_history: HistoryPolicy,
  pub dm_scope: DmScope,
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
  /// Anything the console has no wording for yet; the detail carries the raw action name.
  Other,
}

impl AuditAction {
  pub const ALL: [AuditAction; 18] = [
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
      Self::Other => "other",
    }
  }
}

/// The parts of the security policy that are stored in the signed workspace policy. The console
/// disables the others when the backend cannot store them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyField {
  AdminSelfJoin,
  OrgRecovery,
  DefaultHistory,
  /// Sign-in rules (MFA, session length, idle timeout).
  SignIn,
  GuestEmailDomains,
  DmScope,
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

/// Sections of the administration console.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdminSection {
  Channels,
  Users,
  Security,
  Retention,
  Backgrounds,
  Audit,
}

impl AdminSection {
  pub const ALL: [AdminSection; 6] = [
    Self::Channels,
    Self::Users,
    Self::Security,
    Self::Retention,
    Self::Backgrounds,
    Self::Audit,
  ];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Channels => "channels",
      Self::Users => "users",
      Self::Security => "security",
      Self::Retention => "retention",
      Self::Backgrounds => "backgrounds",
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
    for value in DmScope::ALL {
      assert_eq!(DmScope::from_slug(value.slug()), Some(value));
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
