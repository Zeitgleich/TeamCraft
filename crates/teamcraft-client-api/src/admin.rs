//! View models of the administration console. Admins see metadata only: nothing here carries message
//! content (admin.md §2.1).

use teamcraft_proto::{HistoryPolicy, JoinPolicy, Role};

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
  /// The verified roster's version, the base of an edit.
  #[serde(default)]
  pub version: u64,
  pub owners: Vec<OwnerSigning>,
  /// Admins of the verified roster (not owners).
  #[serde(default)]
  pub admins: Vec<UserId>,
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

/// A role in the signed admin roster (admin.md §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RosterRoleChoice {
  Owner,
  Admin,
}

/// One person of a roster an owner signs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RosterEntry {
  pub user: UserId,
  pub role: RosterRoleChoice,
}

/// How a proposed roster differs from the signed one, per person.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosterChange {
  Add {
    user: UserId,
    role: RosterRoleChoice,
  },
  /// The person stays in the roster with another role (promotion or demotion).
  ChangeRole {
    user: UserId,
    role: RosterRoleChoice,
  },
  Remove {
    user: UserId,
  },
}

/// Why a proposed roster can't be signed as it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RosterProblem {
  /// Nobody could sign the next roster (the core refuses it too, `last_owner`).
  NoOwner,
}

/// A roster as an ordered list of entries: owners first, each person once (the first role wins).
pub fn normalized_roster(entries: &[RosterEntry]) -> Vec<RosterEntry> {
  let mut seen = std::collections::HashSet::new();
  let mut unique: Vec<RosterEntry> = entries
    .iter()
    .filter(|entry| seen.insert(entry.user))
    .copied()
    .collect();
  unique.sort_by_key(|entry| entry.role);
  unique
}

/// The entries of the signed roster in `status`.
pub fn signed_roster(status: &OwnerRosterStatus) -> Vec<RosterEntry> {
  let owners = status.owners.iter().map(|owner| RosterEntry {
    user: owner.user,
    role: RosterRoleChoice::Owner,
  });
  let admins = status.admins.iter().map(|user| RosterEntry {
    user: *user,
    role: RosterRoleChoice::Admin,
  });
  normalized_roster(&owners.chain(admins).collect::<Vec<_>>())
}

/// The roster the identity provider's role holders describe; owners win over admins.
pub fn roster_from_roles(owners: &[UserId], admins: &[UserId]) -> Vec<RosterEntry> {
  let entries: Vec<RosterEntry> = owners
    .iter()
    .map(|user| RosterEntry {
      user: *user,
      role: RosterRoleChoice::Owner,
    })
    .chain(admins.iter().map(|user| RosterEntry {
      user: *user,
      role: RosterRoleChoice::Admin,
    }))
    .collect();
  normalized_roster(&entries)
}

/// The first roster, proposed for the owner's confirmation before the first policy is signed
/// (#265): the signer as owner (the root of trust), then the identity provider's role holders as
/// the server reports them. Nothing in it is trusted until the owner confirms each entry.
pub fn genesis_roster(me: UserId, owners: &[UserId], admins: &[UserId]) -> Vec<RosterEntry> {
  let signer = RosterEntry {
    user: me,
    role: RosterRoleChoice::Owner,
  };
  let mut entries = vec![signer];
  entries.extend(roster_from_roles(owners, admins));
  normalized_roster(&entries)
}

/// A roster split by role: (owners, admins).
pub fn roster_by_role(entries: &[RosterEntry]) -> (Vec<UserId>, Vec<UserId>) {
  let with_role = |wanted: RosterRoleChoice| {
    entries
      .iter()
      .filter(|entry| entry.role == wanted)
      .map(|entry| entry.user)
      .collect()
  };
  (
    with_role(RosterRoleChoice::Owner),
    with_role(RosterRoleChoice::Admin),
  )
}

/// What signing `proposed` would change compared with `signed`, in a stable order.
pub fn roster_changes(signed: &[RosterEntry], proposed: &[RosterEntry]) -> Vec<RosterChange> {
  let role_in = |roster: &[RosterEntry], user: UserId| {
    roster
      .iter()
      .find(|entry| entry.user == user)
      .map(|entry| entry.role)
  };
  let mut changes: Vec<RosterChange> = proposed
    .iter()
    .filter_map(|entry| match role_in(signed, entry.user) {
      None => Some(RosterChange::Add {
        user: entry.user,
        role: entry.role,
      }),
      Some(role) if role != entry.role => Some(RosterChange::ChangeRole {
        user: entry.user,
        role: entry.role,
      }),
      Some(_) => None,
    })
    .collect();
  changes.extend(
    signed
      .iter()
      .filter(|entry| role_in(proposed, entry.user).is_none())
      .map(|entry| RosterChange::Remove { user: entry.user }),
  );
  changes
}

/// Whether `proposed` may be signed.
pub fn roster_problem(proposed: &[RosterEntry]) -> Option<RosterProblem> {
  (!proposed.iter().any(|entry| entry.role == RosterRoleChoice::Owner)).then_some(RosterProblem::NoOwner)
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
  if detail.contains(&last_owner) {
    return Some("admin-error-last-owner");
  }
  if detail.contains(&format!("{ROSTER_BASE_CODE}:")) {
    return Some("admin-error-roster-changed");
  }
  detail
    .contains(&format!("{NOT_AN_OWNER_CODE}:"))
    .then_some("admin-error-not-an-owner")
}

/// Stable code that starts the core's refusal to sign a roster or policy for a non-owner.
pub const NOT_AN_OWNER_CODE: &str = "not_an_owner";

/// Stable code that starts the core's refusal to sign a roster edited from an older version.
pub const ROSTER_BASE_CODE: &str = "roster_changed";

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

/// A space as admins see it: metadata and counts only, never content (admin.md §2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminSpace {
  pub id: SpaceId,
  pub name: String,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  pub member_count: u32,
  pub guest_count: u32,
  pub channel_count: u32,
  pub created_at: TimestampMs,
}

/// What an admin changes on a space (admin.md §3.1); fields left `None` stay as they are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdminSpaceChanges {
  pub maybe_name: Option<String>,
  pub maybe_join_policy: Option<JoinPolicy>,
  pub maybe_is_archived: Option<bool>,
}

/// Whether the console's lists (spaces, channels) arrived: the first load of a large instance
/// takes a while, and a failure must not look like an empty instance.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AdminLoad {
  #[default]
  Loading,
  Loaded,
  /// Nothing could be loaded; the text says why. A reload retries.
  Failed(String),
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
  /// Bots for the integration bridge (docs/bridge.md).
  Integrations,
  Audit,
}

impl AdminSection {
  pub const ALL: [AdminSection; 16] = [
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
    Self::Integrations,
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
      Self::Integrations => "integrations",
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
    assert_eq!(
      admin_error_i18n_key("not_an_owner: only owners sign the roster"),
      Some("admin-error-not-an-owner")
    );
    assert_eq!(
      admin_error_i18n_key("roster_changed: another owner changed the roster meanwhile"),
      Some("admin-error-roster-changed")
    );
  }

  mod roster_edits {
    use super::*;
    use crate::model::Id;

    #[test]
    fn adding_an_owner_and_an_admin() {
      let signed = vec![owner(1)];
      let proposed = normalized_roster(&[owner(1), admin(3), owner(2)]);
      assert_eq!(proposed, vec![owner(1), owner(2), admin(3)], "owners first");
      assert_eq!(
        roster_changes(&signed, &proposed),
        vec![
          RosterChange::Add {
            user: user(2),
            role: RosterRoleChoice::Owner
          },
          RosterChange::Add {
            user: user(3),
            role: RosterRoleChoice::Admin
          },
        ]
      );
      assert_eq!(roster_problem(&proposed), None);
    }

    #[test]
    fn demoting_and_removing() {
      let signed = vec![owner(1), owner(2), admin(3)];
      let proposed = vec![owner(2), admin(1)];
      assert_eq!(
        roster_changes(&signed, &proposed),
        vec![
          RosterChange::ChangeRole {
            user: user(1),
            role: RosterRoleChoice::Admin
          },
          RosterChange::Remove { user: user(3) },
        ]
      );
    }

    #[test]
    fn a_roster_without_an_owner_is_a_problem() {
      assert_eq!(roster_problem(&[admin(1)]), Some(RosterProblem::NoOwner));
      assert_eq!(roster_problem(&[]), Some(RosterProblem::NoOwner));
    }

    #[test]
    fn the_genesis_roster_starts_with_the_signer_and_lists_every_role_holder() {
      // A hostile server reports a look-alike (5) as owner and demotes the signer to admin: the
      // signer stays owner, and the look-alike is listed for confirmation, not signed silently.
      let proposed = genesis_roster(user(1), &[user(5), user(2)], &[user(1), user(3)]);
      assert_eq!(proposed, vec![owner(1), owner(5), owner(2), admin(3)]);
      let to_confirm = roster_changes(&[], &proposed);
      assert_eq!(to_confirm.len(), 4, "every entry is shown for confirmation");
      assert!(to_confirm.contains(&RosterChange::Add {
        user: user(5),
        role: RosterRoleChoice::Owner
      }));
      assert_eq!(
        roster_by_role(&proposed),
        (vec![user(1), user(5), user(2)], vec![user(3)])
      );
      assert_eq!(
        genesis_roster(user(1), &[], &[]),
        vec![owner(1)],
        "the signer alone"
      );
    }

    #[test]
    fn identity_provider_roles_become_a_roster() {
      let proposed = roster_from_roles(&[user(2)], &[user(2), user(4)]);
      assert_eq!(
        proposed,
        vec![owner(2), admin(4)],
        "an owner who is also admin stays owner"
      );
      let status = OwnerRosterStatus {
        version: 1,
        owners: vec![OwnerSigning {
          user: user(1),
          maybe_last_signed_at: None,
        }],
        admins: vec![user(4)],
        maybe_expires_at: None,
        recommended_minimum: 2,
        warning_lead_ms: 0,
      };
      assert_eq!(
        roster_changes(&signed_roster(&status), &proposed),
        vec![
          RosterChange::Add {
            user: user(2),
            role: RosterRoleChoice::Owner
          },
          RosterChange::Remove { user: user(1) },
        ]
      );
      assert!(roster_changes(&proposed, &proposed).is_empty());
    }

    fn user(raw: i64) -> UserId {
      UserId::from_snowflake(crate::model::Snowflake::new(raw).unwrap())
    }

    fn owner(raw: i64) -> RosterEntry {
      RosterEntry {
        user: user(raw),
        role: RosterRoleChoice::Owner,
      }
    }

    fn admin(raw: i64) -> RosterEntry {
      RosterEntry {
        user: user(raw),
        role: RosterRoleChoice::Admin,
      }
    }
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
        version: 1,
        admins: Vec::new(),
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
