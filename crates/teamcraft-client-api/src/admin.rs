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

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DeviceInfo {
  pub id: DeviceId,
  pub name: String,
  pub platform: String,
  pub last_seen: TimestampMs,
  pub is_current: bool,
  /// For the user's own devices: what this device verified (issue #409). Admins see `Unknown`.
  #[serde(default)]
  pub verification: DeviceVerification,
  /// For the user's own devices: how the device gets pushes; `None` where that isn't known (the
  /// admin console, an older core).
  #[serde(rename = "push", default, skip_serializing_if = "Option::is_none")]
  pub maybe_push: Option<DevicePushStatus>,
}

/// Whether one of the user's devices is theirs by what their identity key signed (e2ee.md §3),
/// never by the server's word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceVerification {
  /// Its certificate opens with the user's identity key and the signed device list names it.
  Verified,
  /// The certificate is the user's, but the signed device list doesn't name it yet: it is still
  /// being set up, or the list is being updated.
  Pending,
  /// Nothing the user's identity key signed vouches for it: none of the user's devices set it up.
  Unverified,
  /// Not checked: no keys on this device, no list fetched yet, or a newer state this app doesn't
  /// know.
  #[default]
  #[serde(other)]
  Unknown,
}

/// How one of the user's devices gets push notifications, as the server reports it (diagnostics,
/// not a trust decision).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DevicePushStatus {
  /// The push service it registered with; `None`: it gets no pushes (a desktop, a browser without
  /// push).
  #[serde(rename = "provider", default, skip_serializing_if = "Option::is_none")]
  pub maybe_provider: Option<teamcraft_proto::PushProvider>,
  /// When the server last handed a push for it to the push service (since the server started).
  #[serde(rename = "last_sent_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_sent_at: Option<TimestampMs>,
  #[serde(rename = "last_failed_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_failed_at: Option<TimestampMs>,
  /// How the push service took the last test push the user sent it from this device.
  #[serde(rename = "test", default, skip_serializing_if = "Option::is_none")]
  pub maybe_test: Option<teamcraft_proto::TestPushOutcome>,
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
  /// Still in a conversation or space, or still with a device: a deactivated user like this can
  /// be removed from all chats.
  pub is_in_chats: bool,
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
  /// A deactivated user's devices left every group and the user left every chat.
  UserRemovedFromChats,
  /// An account was deleted; only its tombstone is left.
  UserDeleted,
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
  pub const ALL: [AuditAction; 27] = [
    Self::PolicyChanged,
    Self::RetentionChanged,
    Self::DeviceRemoved,
    Self::UserDeactivated,
    Self::UserReactivated,
    Self::UserRemovedFromChats,
    Self::UserDeleted,
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
      Self::UserRemovedFromChats => "user-removed-from-chats",
      Self::UserDeleted => "user-deleted",
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
  /// Owners of the verified roster whose identity key this device can't use: they can't sign and
  /// don't count as owners, but they stay in the roster until an owner removes them explicitly
  /// (an edit never drops them silently).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub unverifiable_owners: Vec<UserId>,
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

/// The entries of the signed roster in `status`, owners whose key can't be used included.
pub fn signed_roster(status: &OwnerRosterStatus) -> Vec<RosterEntry> {
  let owners = status
    .owners
    .iter()
    .map(|owner| owner.user)
    .chain(status.unverifiable_owners.iter().copied())
    .map(|user| RosterEntry {
      user,
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

/// What taking over the identity provider's roles proposes (issue #765): the signed roster plus
/// everyone the provider lists as owner or admin, with promotions to owner. It never removes or
/// demotes anybody: the server's role list can lag behind the provider (roles are read from
/// tokens) or miss people, and a list like that once cost signed entries. Removals and demotions
/// are the owner's explicit edit.
pub fn idp_roster_proposal(signed: &[RosterEntry], owners: &[UserId], admins: &[UserId]) -> Vec<RosterEntry> {
  let mut proposed: Vec<RosterEntry> = signed.to_vec();
  for entry in roster_from_roles(owners, admins) {
    match proposed.iter_mut().find(|existing| existing.user == entry.user) {
      Some(existing) => existing.role = existing.role.min(entry.role),
      None => proposed.push(entry),
    }
  }
  normalized_roster(&proposed)
}

/// Owners and admins as the server lists them, for the roster proposals: each account once, and
/// never a deactivated one (an old account of somebody who has a new one would be proposed twice).
pub fn role_holders(users: &[AdminUser]) -> (Vec<UserId>, Vec<UserId>) {
  let mut seen = std::collections::HashSet::new();
  let mut owners = Vec::new();
  let mut admins = Vec::new();
  for user in users
    .iter()
    .filter(|user| user.is_active && !user.user.is_deactivated)
  {
    if !seen.insert(user.user.id) {
      continue;
    }
    match user.role {
      Role::Owner => owners.push(user.user.id),
      Role::Admin => admins.push(user.user.id),
      _ => {},
    }
  }
  (owners, admins)
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

/// Where the signed-in user's admin role stands, as member clients see it (admin.md §2.3, issue
/// #765). Only the signed, fresh roster gives admin authority; the identity provider's role alone
/// gives none, and the UI says so wherever it matters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosterStanding {
  /// Nothing to point out: no admin role, or one the fresh signed roster holds.
  InForce,
  /// The identity provider gives the user `role`, but no owner signed it yet.
  NotSigned { role: Role, owners: Vec<UserId> },
  /// No owner signed the roster within the accepted age: admin rights are paused for everybody.
  /// `can_sign`: the user is an owner of the roster and can sign it again here.
  Expired { can_sign: bool, owners: Vec<UserId> },
  /// The workspace has no signed roster yet; the first policy save of an owner signs it.
  NoRoster,
  /// The user is a signed owner and the identity provider lists people the roster doesn't have:
  /// they get no admin rights until this owner signs them.
  AdditionsToSign { count: usize },
}

impl RosterStanding {
  /// Whether the user's own admin actions are refused by other members' apps right now.
  pub fn is_unsigned(&self) -> bool {
    matches!(
      self,
      Self::NotSigned { .. } | Self::Expired { .. } | Self::NoRoster
    )
  }
}

/// The standing of `me` with the server role `server_role` against the verified roster in
/// `maybe_status` at `now`. `idp_owners` and `idp_admins` are the provider's role holders (empty
/// where this client doesn't know them); `can_sign` whether this device holds signing keys.
pub fn roster_standing(
  me: UserId,
  server_role: &Role,
  maybe_status: Option<&OwnerRosterStatus>,
  (idp_owners, idp_admins): (&[UserId], &[UserId]),
  can_sign: bool,
  now: TimestampMs,
) -> RosterStanding {
  if !server_role.includes(&Role::Admin) {
    return RosterStanding::InForce;
  }
  let Some(status) = maybe_status else {
    return RosterStanding::NoRoster;
  };
  let owners: Vec<UserId> = status.owners.iter().map(|owner| owner.user).collect();
  let is_owner = owners.contains(&me);
  if !is_owner && !status.admins.contains(&me) {
    return RosterStanding::NotSigned {
      role: server_role.clone(),
      owners,
    };
  }
  if status.maybe_expires_at.is_some_and(|at| at <= now) {
    return RosterStanding::Expired {
      can_sign: is_owner && can_sign,
      owners,
    };
  }
  if is_owner && can_sign {
    let signed = signed_roster(status);
    let count = roster_changes(&signed, &idp_roster_proposal(&signed, idp_owners, idp_admins)).len();
    if count > 0 {
      return RosterStanding::AdditionsToSign { count };
    }
  }
  RosterStanding::InForce
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
  if detail.contains(&format!("{ROSTER_KEY_UNUSABLE_CODE}:")) {
    return Some("admin-error-roster-key-unusable");
  }
  detail
    .contains(&format!("{NOT_AN_OWNER_CODE}:"))
    .then_some("admin-error-not-an-owner")
}

/// Stable code that starts the core's refusal to sign a roster or policy for a non-owner.
pub const NOT_AN_OWNER_CODE: &str = "not_an_owner";

/// Stable code that starts the core's refusal to sign a roster that keeps an entry whose identity
/// key can't be used on this device.
pub const ROSTER_KEY_UNUSABLE_CODE: &str = "roster_key_unusable";

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
        unverifiable_owners: Vec::new(),
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

    mod standing {
      use super::*;

      const NOW: TimestampMs = 1_000_000;

      #[test]
      fn members_and_fresh_signed_admins_have_nothing_to_see() {
        let status = roster(&[1], &[2], Some(NOW + 1));
        assert_eq!(
          standing(9, Role::Member, Some(&status), &[], true),
          RosterStanding::InForce
        );
        assert_eq!(standing(9, Role::Guest, None, &[], true), RosterStanding::InForce);
        assert_eq!(
          standing(2, Role::Admin, Some(&status), &[], true),
          RosterStanding::InForce
        );
        assert_eq!(
          standing(1, Role::Owner, Some(&status), &[], true),
          RosterStanding::InForce
        );
        // A roster from before signing times doesn't expire.
        let unexpiring = roster(&[1], &[], None);
        assert_eq!(
          standing(1, Role::Owner, Some(&unexpiring), &[], true),
          RosterStanding::InForce
        );
      }

      #[test]
      fn a_provider_role_nobody_signed_is_not_in_force() {
        let status = roster(&[1, 2], &[], Some(NOW + 1));
        let standing = standing(3, Role::Owner, Some(&status), &[], true);
        assert_eq!(
          standing,
          RosterStanding::NotSigned {
            role: Role::Owner,
            owners: vec![user(1), user(2)]
          }
        );
        assert!(standing.is_unsigned());
      }

      #[test]
      fn an_expired_roster_pauses_everybody_and_owners_can_sign_again() {
        let status = roster(&[1], &[2], Some(NOW));
        assert_eq!(
          standing(1, Role::Owner, Some(&status), &[], true),
          RosterStanding::Expired {
            can_sign: true,
            owners: vec![user(1)]
          }
        );
        // Without signing keys on this device, or as an admin: ask an owner.
        assert!(matches!(
          standing(1, Role::Owner, Some(&status), &[], false),
          RosterStanding::Expired { can_sign: false, .. }
        ));
        assert!(matches!(
          standing(2, Role::Admin, Some(&status), &[], true),
          RosterStanding::Expired { can_sign: false, .. }
        ));
      }

      #[test]
      fn no_roster_yet_is_said_to_admins() {
        assert_eq!(
          standing(1, Role::Owner, None, &[], true),
          RosterStanding::NoRoster
        );
        assert_eq!(
          standing(1, Role::Admin, None, &[], true),
          RosterStanding::NoRoster
        );
      }

      #[test]
      fn a_signing_owner_is_told_about_provider_roles_to_sign() {
        let status = roster(&[1], &[2], Some(NOW + 1));
        assert_eq!(
          standing(1, Role::Owner, Some(&status), &[3, 4], true),
          RosterStanding::AdditionsToSign { count: 2 }
        );
        // Signed already, or no keys to sign with here: nothing to do on this device.
        assert_eq!(
          standing(1, Role::Owner, Some(&status), &[1], true),
          RosterStanding::InForce
        );
        assert_eq!(
          standing(1, Role::Owner, Some(&status), &[3], false),
          RosterStanding::InForce
        );
        // Admins can't sign; their own role is in force.
        assert_eq!(
          standing(2, Role::Admin, Some(&status), &[3], true),
          RosterStanding::InForce
        );
      }

      fn standing(
        me: i64,
        role: Role,
        maybe_status: Option<&OwnerRosterStatus>,
        idp_owners: &[i64],
        can_sign: bool,
      ) -> RosterStanding {
        let owners: Vec<UserId> = idp_owners.iter().map(|raw| user(*raw)).collect();
        roster_standing(user(me), &role, maybe_status, (&owners, &[]), can_sign, NOW)
      }

      fn roster(owners: &[i64], admins: &[i64], maybe_expires_at: Option<TimestampMs>) -> OwnerRosterStatus {
        OwnerRosterStatus {
          version: 3,
          owners: owners
            .iter()
            .map(|raw| OwnerSigning {
              user: user(*raw),
              maybe_last_signed_at: None,
            })
            .collect(),
          admins: admins.iter().map(|raw| user(*raw)).collect(),
          unverifiable_owners: Vec::new(),
          maybe_expires_at,
          recommended_minimum: 1,
          warning_lead_ms: 0,
        }
      }
    }

    /// Issue #765: taking over the provider's roles never drops a signed entry, whatever the
    /// server's (lagging or partial) role list says.
    #[test]
    fn the_identity_provider_proposal_adds_and_promotes_but_never_removes() {
      let signed = vec![owner(1), owner(2), admin(3), admin(4)];
      // The server lists only 1 as owner and 5 as new admin; 3 is owner there now; 2 and 4 are
      // missing (their stored role is stale).
      let proposed = idp_roster_proposal(&signed, &[user(1), user(3)], &[user(5)]);
      assert_eq!(proposed, vec![owner(1), owner(2), owner(3), admin(4), admin(5)]);
      let changes = roster_changes(&signed, &proposed);
      assert_eq!(
        changes,
        vec![
          RosterChange::ChangeRole {
            user: user(3),
            role: RosterRoleChoice::Owner
          },
          RosterChange::Add {
            user: user(5),
            role: RosterRoleChoice::Admin
          },
        ]
      );
      assert!(
        !changes
          .iter()
          .any(|change| matches!(change, RosterChange::Remove { .. }))
      );
      // An owner the provider lists only as admin stays owner.
      assert_eq!(idp_roster_proposal(&[owner(1)], &[], &[user(1)]), vec![owner(1)]);
      // Nothing listed: nothing changes.
      assert_eq!(idp_roster_proposal(&signed, &[], &[]), normalized_roster(&signed));
    }

    /// An owner whose key this device can't use is part of the starting list: any other edit keeps
    /// them, only an explicit removal takes them out.
    #[test]
    fn an_owner_with_an_unusable_key_is_never_dropped_by_an_edit() {
      let status = OwnerRosterStatus {
        version: 2,
        owners: vec![OwnerSigning {
          user: user(1),
          maybe_last_signed_at: None,
        }],
        admins: vec![user(3)],
        unverifiable_owners: vec![user(2)],
        maybe_expires_at: None,
        recommended_minimum: 2,
        warning_lead_ms: 0,
      };
      let signed = signed_roster(&status);
      assert_eq!(signed, vec![owner(1), owner(2), admin(3)]);
      // Adding somebody else changes nothing about the owner.
      let mut edited = signed.clone();
      edited.push(admin(4));
      assert_eq!(
        roster_changes(&signed, &normalized_roster(&edited)),
        vec![RosterChange::Add {
          user: user(4),
          role: RosterRoleChoice::Admin
        }]
      );
      // So does taking over provider roles that don't list them.
      let proposed = idp_roster_proposal(&signed, &[user(1)], &[]);
      assert!(proposed.contains(&owner(2)));
      // Removing them is a change the owner sees and confirms.
      let removed: Vec<RosterEntry> = signed
        .iter()
        .copied()
        .filter(|entry| entry.user != user(2))
        .collect();
      assert_eq!(
        roster_changes(&signed, &removed),
        vec![RosterChange::Remove { user: user(2) }]
      );
      assert_eq!(
        admin_error_i18n_key("roster_key_unusable: the key of user 2 can't be used"),
        Some("admin-error-roster-key-unusable")
      );
    }

    #[test]
    fn deactivated_and_repeated_accounts_are_not_role_holders() {
      let users = [
        admin_user(1, Role::Owner, true, false),
        admin_user(2, Role::Admin, true, false),
        // The old account of somebody who has a new one: deactivated, still with its role.
        admin_user(3, Role::Admin, false, true),
        admin_user(4, Role::Admin, false, false),
        admin_user(2, Role::Admin, true, false),
        admin_user(5, Role::Member, true, false),
      ];
      assert_eq!(role_holders(&users), (vec![user(1)], vec![user(2)]));
    }

    fn admin_user(raw: i64, role: Role, is_active: bool, is_deactivated: bool) -> AdminUser {
      AdminUser {
        user: User {
          id: user(raw),
          username: format!("person{raw}"),
          display_name: "Person A".into(),
          presence: crate::model::PresenceStatus::Offline,
          maybe_custom_status: None,
          avatar_hue: 0,
          is_guest: false,
          is_bot: false,
          maybe_organization: None,
          device_setup: crate::model::DeviceSetup::Unknown,
          is_deactivated,
          is_deleted: false,
          maybe_email: None,
        },
        role,
        devices: Vec::new(),
        last_seen: 0,
        space_names: Vec::new(),
        is_active,
        is_in_chats: false,
        maybe_guest: None,
      }
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
        unverifiable_owners: Vec::new(),
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
