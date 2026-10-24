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

/// The owners of the roster this device verified, for the console's owner warnings (admin.md §2.3).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerRosterStatus {
  pub owners: Vec<OwnerSigning>,
  /// When member clients stop accepting the admins unless an owner signs the roster again; `None`
  /// for a roster from before signing times.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<TimestampMs>,
  /// From the signed policy: fewer owners than this shows a warning (1 switches it off).
  pub recommended_minimum: u8,
  /// How long before [`Self::maybe_expires_at`] the console starts warning.
  pub warning_lead_ms: i64,
}

/// An owner and the newest roster or policy signature of theirs this device verified.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerSigning {
  pub user: UserId,
  #[serde(rename = "last_signed_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_signed_at: Option<TimestampMs>,
}

/// Something about the owners the console should point out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerWarning {
  /// Admin authority already ended on member clients.
  Expired,
  /// Admin authority ends at `at` unless an owner opens TeamCraft and signs again.
  ExpiresSoon { at: TimestampMs },
  /// Fewer owners than recommended: losing one could leave the workspace without any.
  TooFewOwners { count: usize, minimum: u8 },
}

/// The i18n message for an admin error the console words itself: refusals whose text starts with
/// a stable error code (the core puts it first). Other errors are shown as they are.
pub fn admin_error_i18n_key(detail: &str) -> Option<&'static str> {
  let last_owner = format!("{}:", teamcraft_proto::ErrorCode::LastOwner.as_str());
  detail.contains(&last_owner).then_some("admin-error-last-owner")
}

impl OwnerRosterStatus {
  /// The warnings at `now`, most urgent first.
  pub fn warnings(&self, now: TimestampMs) -> Vec<OwnerWarning> {
    let mut warnings = Vec::new();
    if let Some(at) = self.maybe_expires_at {
      if at <= now {
        warnings.push(OwnerWarning::Expired);
      } else if at.saturating_sub(now) <= self.warning_lead_ms {
        warnings.push(OwnerWarning::ExpiresSoon { at });
      }
    }
    let minimum = self.recommended_minimum.max(1);
    if self.owners.len() < usize::from(minimum) {
      warnings.push(OwnerWarning::TooFewOwners {
        count: self.owners.len(),
        minimum,
      });
    }
    warnings
  }
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

  #[test]
  fn the_last_owner_refusal_is_localized() {
    assert_eq!(
      admin_error_i18n_key("last_owner: the roster must keep at least one owner"),
      Some("admin-error-last-owner")
    );
    assert_eq!(admin_error_i18n_key("server answered 500: oops"), None);
  }

  mod owner_warnings {
    use super::*;
    use crate::model::Id;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    const NOW: TimestampMs = 100 * DAY_MS;

    #[test]
    fn two_owners_with_a_fresh_roster_need_no_warning() {
      assert!(status(2, Some(NOW + 20 * DAY_MS), 2).warnings(NOW).is_empty());
    }

    #[test]
    fn fewer_owners_than_recommended_are_pointed_out() {
      assert_eq!(
        status(1, Some(NOW + 20 * DAY_MS), 2).warnings(NOW),
        vec![OwnerWarning::TooFewOwners { count: 1, minimum: 2 }]
      );
      assert!(
        status(1, Some(NOW + 20 * DAY_MS), 1).warnings(NOW).is_empty(),
        "a minimum of 1 switches the warning off"
      );
      assert_eq!(
        status(1, None, 0).warnings(NOW),
        Vec::new(),
        "a minimum of 0 counts as 1"
      );
    }

    #[test]
    fn an_expiring_roster_is_announced_ahead_of_time() {
      let at = NOW + 7 * DAY_MS;
      assert_eq!(
        status(2, Some(at), 2).warnings(NOW),
        vec![OwnerWarning::ExpiresSoon { at }]
      );
      assert!(status(2, Some(NOW + 7 * DAY_MS + 1), 2).warnings(NOW).is_empty());
    }

    #[test]
    fn an_expired_roster_comes_first() {
      assert_eq!(
        status(1, Some(NOW), 2).warnings(NOW),
        vec![
          OwnerWarning::Expired,
          OwnerWarning::TooFewOwners { count: 1, minimum: 2 }
        ]
      );
      assert!(
        status(2, None, 2).warnings(NOW).is_empty(),
        "rosters from before signing times don't expire"
      );
    }

    fn status(owners: i64, maybe_expires_at: Option<TimestampMs>, minimum: u8) -> OwnerRosterStatus {
      OwnerRosterStatus {
        owners: (1..=owners)
          .map(|raw| OwnerSigning {
            user: UserId::from_snowflake(crate::model::Snowflake::new(raw).unwrap()),
            maybe_last_signed_at: None,
          })
          .collect(),
        maybe_expires_at,
        recommended_minimum: minimum,
        warning_lead_ms: 7 * DAY_MS,
      }
    }
  }
}
