//! Signed per-channel roles (issue #380, e2ee.md §4.6.1, admin.md §2.3).
//!
//! The server stores each channel's role log but makes no trust decision. Every entry is a full
//! snapshot of the channel's admins and moderators, signed by a device whose user may assign them
//! under the rules of [`teamcraft_proto::role_rules`], checked against signed data only:
//!
//! - **Workspace admins and owners** of the verified roster, fresh at the entry's server time, may
//!   sign any snapshot built on this device's head. Only a list one of them looked at may name no
//!   head at all ([`REVIEWED_PREV_HASH`]): that checkpoint re-anchors a device whatever it had.
//! - **Somebody who has left the roster since**, for an entry on this device's head that both its
//!   signing time and its server time date inside their tenure, if this device didn't read the log
//!   after that tenure ended. It counts for roles, never for the anchor: the dates are claims
//!   (e2ee.md §4.6.1, "Reading the log late").
//! - **Channel admins** of the snapshot this device accepted last, verified against the identity
//!   key that snapshot names for them; their entry must build on exactly that snapshot.
//! - **The founder**: while nothing was accepted yet, an entry whose signer names themselves as
//!   admin, with the identity key this device pinned for them (trust on first use, like the
//!   workspace root of trust). A channel is founded by its creator; an older one by the first app
//!   the server lists as its admin that sees it without a log, naming who the server lists then, if
//!   they are in this device's encryption group. That is trust in the server at first sight: such a
//!   snapshot is marked, and never anchors (e2ee.md §4.6.1, "Founding at first sight"). In a
//!   channel with an anchor only the anchor's founder founds, under its key, and that founding
//!   anchors; nobody founds at first sight ([`RoleFounding`], e2ee.md §4.6.5).
//!
//! An entry that fails any check is skipped, not fatal: honest signers build on the last entry they
//! accepted, so a junk entry can't block the log. Only an entry from somebody entitled to sign that
//! this device can't place shows it missed something: from there on the device has an
//! [unverified stretch](ChannelRoleState::has_unverified_stretch), which counts nobody as a signed
//! admin until a workspace admin's checkpoint. Versions only move forward, so the server can't
//! roll a device back or replay an entry; heads travel inside encrypted messages so members notice
//! a server showing them different logs ([`ChannelRoleState::note_head`]).
//!
//! The verified role is capped by the server's: the server may take a role away (it can withhold
//! anything anyway), never grant one ([`effective_role`]).

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use teamcraft_crypto::identity::IdentityPublicKey;
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::IdentityDirectory;
use teamcraft_crypto::workspace::{WorkspaceLog, claimed_signer, log_entry_hash, verify_log_entry};
use teamcraft_proto::role_rules::{
  RoleActor, RoleChangeError, RoleTarget, SpaceStanding, check_channel_role_change,
};
use teamcraft_proto::{
  Base64Bytes, ChannelRole, ChannelRoleAssignment, ChannelRoleLogEntryResponse, ChannelRolesDocument,
  ConversationId, ConversationVisibility, MAX_CHANNEL_ROLE_ASSIGNMENTS, PermissionScheme, Role, UserId,
};

use crate::convert;
use crate::workspace::{AdminTenure, RosterRole, WorkspaceState};

/// Length of an entry hash and of the zero hash a first entry names.
const HASH_BYTES: usize = 32;
/// What a snapshot a workspace admin reviewed names as its previous entry: no hash of anything, so
/// on every device it is a checkpoint, which vouches for each role it lists and doesn't build on
/// whatever a device had (e2ee.md §4.6.1, "Reading the log late").
const REVIEWED_PREV_HASH: [u8; HASH_BYTES] = [0xff; HASH_BYTES];
/// A signer's device stands behind a person's identity key only for one of these reasons (review of
/// #1286): a first pin is whatever the server says, so a snapshot naming it would give the server a
/// channel admin of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoleKeyStanding {
  /// The key is the snapshot's, or one this device can check.
  Checked,
  /// As `Checked`, but this device has seen another key for the person than the snapshot names.
  Changed,
  /// Nothing here says this key is theirs: not in this device's group, not attested, not on the
  /// roster, and no snapshot to go by. Never signed.
  Uncheckable,
  #[serde(other)]
  Unknown,
}

/// The key to sign for somebody, and whether it differs from what this device saw before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CheckedRoleKey {
  pub key: [u8; 32],
  pub is_changed: bool,
}

/// What a device knows about one person's key when it is about to sign a role for them.
#[derive(Clone, Copy, Debug, Default)]
pub struct RoleKeyEvidence {
  /// The key the accepted snapshot names; `None` also where that snapshot can't be gone by.
  pub maybe_snapshot: Option<[u8; 32]>,
  /// The key this device pinned, whoever told it.
  pub maybe_pinned: Option<[u8; 32]>,
  /// A key that needs nobody's word: this user's own, or the verified roster's.
  pub maybe_signed: Option<[u8; 32]>,
  /// The identity provider attested the pin.
  pub is_attested: bool,
  /// The person is in this device's MLS group of the channel, whose credentials verify against
  /// the pin: a wrong pin would not have let them in.
  pub is_in_group: bool,
  /// The server now says another key than the pin.
  pub is_said_to_have_changed: bool,
}

impl RoleKeyEvidence {
  /// The key to sign, or `None` if this device can't stand behind any: a person the snapshot names
  /// keeps its key unless this device can check another one.
  pub fn checked(&self) -> Option<CheckedRoleKey> {
    let maybe_verified = self.maybe_signed.or(if self.is_attested || self.is_in_group {
      self.maybe_pinned
    } else {
      None
    });
    match (self.maybe_snapshot, maybe_verified) {
      (Some(snapshot), Some(verified)) => Some(CheckedRoleKey {
        key: verified,
        is_changed: snapshot != verified,
      }),
      (Some(snapshot), None) => Some(CheckedRoleKey {
        key: snapshot,
        is_changed: self.is_said_to_have_changed
          || self.maybe_pinned.is_some_and(|pinned| pinned != snapshot),
      }),
      (None, Some(verified)) => Some(CheckedRoleKey {
        key: verified,
        is_changed: self.is_said_to_have_changed,
      }),
      (None, None) => None,
    }
  }

  pub fn standing(&self) -> RoleKeyStanding {
    match self.checked() {
      None => RoleKeyStanding::Uncheckable,
      Some(checked) if checked.is_changed => RoleKeyStanding::Changed,
      Some(_) => RoleKeyStanding::Checked,
    }
  }
}

/// Hashes of refused entries a device remembers, to tell an entry built on one (it missed
/// something) from one built on a head nobody was shown (a fork or junk).
const MAX_SKIPPED_HASHES: usize = 64;
/// Entries from messages a device waits for the log to show; beyond this the oldest is dropped
/// unchecked. Honest signers send one per role change, and the next read confirms them.
const MAX_UNCONFIRMED: usize = 64;
/// Signers a device remembers for the purge's time limit, the most recent ones.
const MAX_RECENT_SIGNERS: usize = 16;
/// Members whose review requests a channel's state keeps (issue #1264, option 2A), the newest.
const MAX_REVIEW_REQUESTS: usize = 64;

/// What this device verified of one channel's role log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRoleState {
  /// Version of the last accepted entry; 0 while nothing is signed (a channel from before signed
  /// roles, whose roles are the server's word only).
  #[serde(default)]
  pub version: u64,
  #[serde(rename = "head_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_head_hash: Option<Base64Bytes>,
  /// The accepted snapshot: everybody above member.
  #[serde(default)]
  pub assignments: Vec<ChannelRoleAssignment>,
  /// The newest version looked at, accepted or skipped; nothing at or below it is read again.
  #[serde(default)]
  pub cursor: u64,
  /// Why the last skipped entry was skipped.
  #[serde(rename = "rejection", default, skip_serializing_if = "Option::is_none")]
  pub maybe_rejection: Option<String>,
  /// A member verified another entry for the same version, or entries this device isn't shown.
  #[serde(rename = "alarm", default, skip_serializing_if = "Option::is_none")]
  pub maybe_alarm: Option<String>,
  /// A member's head newer than ours, to check after the next sync.
  #[serde(rename = "ahead", default, skip_serializing_if = "Option::is_none")]
  pub maybe_ahead: Option<ChannelRolesHead>,
  /// Whether the accepted snapshot goes back to a workspace admin's entry or to the founding a
  /// channel's anchor proves (e2ee.md §4.6.5), not only to a founder's claim, which is trust on
  /// first use: a member and the server together can make one up. Only a decision that destroys
  /// history asks this (e2ee.md §4.6, "A signed deletion purges at once").
  #[serde(default)]
  pub is_workspace_anchored: bool,
  /// Who appointed each role of the accepted snapshot. Empty for a snapshot accepted before this
  /// was recorded: nothing is claimed then.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub appointments: Vec<RoleAppointment>,
  /// The roster version this device held when it last read the log to its end. An entry the log
  /// shows only now can't have been signed on an older roster than that, unless the server held it
  /// back. 0 for a device that had read nothing, which bounds nothing. `None`: a state from before
  /// this was recorded, whose last read nobody knows, so an entry by somebody who left isn't placed.
  #[serde(
    rename = "read_at_roster_version",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_read_at_roster_version: Option<u64>,
  /// The first entry of a stretch of the log this device can't verify, until a workspace admin's
  /// checkpoint ends it. See [`Self::has_unverified_stretch`].
  #[serde(rename = "unverified_from", default, skip_serializing_if = "Option::is_none")]
  pub maybe_unverified_from: Option<u64>,
  /// Whether the snapshot rests on an entry by somebody who has left the roster, taken on their and
  /// the server's dates. See [`Self::must_review_before_vouching`].
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub has_former_admin_entry: bool,
  /// Hashes of entries about this channel this device refused, newest last.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub skipped_hashes: Vec<Base64Bytes>,
  /// Whether the snapshot goes back to roles taken from the server's list at first sight
  /// ([`ChannelRolesDocument::has_server_listed_roles`]), until a workspace admin's reviewed list.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub has_server_listed_roles: bool,
  /// Whether `maybe_alarm` rests on signed entries, not on a member's word: an entry from a
  /// member's message the log doesn't show, or two valid entries for one version. Only such an
  /// alarm refuses approvals (e2ee.md §4.6.3, D3); a workspace admin's reviewed list ends it.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_alarm_proven: bool,
  /// Heads of entries accepted from members' messages that a read of the log hasn't shown yet: the
  /// next read compares them (e2ee.md §4.6.3, part 1). Oldest first.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub unconfirmed: Vec<ChannelRolesHead>,
  /// Who signed entries this device accepted, and since when each counts, for the time limit on a
  /// channel admin's purge (e2ee.md §4.6.3, part 3). Empty for a state from before this was
  /// recorded: such a device takes the retention.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub recent_signers: Vec<RecentSigner>,
  /// The state before the last accepted entry, to tell whether a second entry for the same version
  /// is valid (a fork) or junk. Without anything of its own nested.
  #[serde(rename = "before_head", default, skip_serializing_if = "Option::is_none")]
  pub maybe_before_head: Option<Box<ChannelRoleState>>,
  /// Who signed the last accepted entry, to name both signers of a fork.
  #[serde(rename = "head_signer", default, skip_serializing_if = "Option::is_none")]
  pub maybe_head_signer: Option<UserId>,
  /// Members whose messages said their device is stuck on this channel's roles (issue #1264,
  /// option 2A), one per sender, newest kept: a workspace admin's device offers the review for
  /// them and names who asked. Until a reviewed list is accepted.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub review_requests: Vec<ReviewRequest>,
  /// The server's time of the last reviewed list this device accepted. A request in a message the
  /// server dated no later was sent before that list and is answered by it.
  #[serde(rename = "reviewed_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_reviewed_at_ms: Option<i64>,
  /// Whether `recent_signers` may still hold entries recorded before a workspace admin's ordinary
  /// entry stopped counting (issue #1264, option 1B). True for a state stored before that, which
  /// lacks the field; [`Self::forget_legacy_recent_signers`] clears it once.
  #[serde(default = "is_legacy_state")]
  pub has_legacy_recent_signers: bool,
}

/// A member's message asked for a review of the channel's roles: who, and the server's time of
/// the newest such message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRequest {
  pub user_id: UserId,
  pub sent_at_ms: i64,
}

/// Somebody who signed an entry this device accepted. `from_ms` is the entry's signing time, capped
/// at this device's clock when it accepted it: never the server's time, which a withholding server
/// would set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentSigner {
  pub user_id: UserId,
  pub from_ms: i64,
}

/// What became of a signed entry a member sent in the channel (e2ee.md §4.6.3, part 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageEntry {
  /// The next version, valid: accepted, also if the log doesn't show it.
  Accepted,
  /// The entry this device accepted already.
  Known,
  /// Valid, and for a version this device holds another entry of: a proven alarm.
  Fork,
  /// Further on than this device can verify: a newer head, to read the log for.
  Ahead,
  /// Old, a founding, or not valid here: nothing changes, and no alarm.
  Ignored,
}

/// The accepted head of a channel's role log; sent along inside the channel's encrypted messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelRolesHead {
  pub version: u64,
  pub hash: Base64Bytes,
  /// The sender's device is stuck on this channel's roles ([`ChannelRoleState::needs_review`]) and
  /// asks a workspace admin to look (issue #1264, option 2A). A member's claim: it only offers the
  /// review a workspace admin's device already has, and changes nothing else. Absent from older
  /// clients and when false, so they read and send heads as before.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub needs_review: bool,
}

/// Who signed the entry that gave a user their role in the accepted snapshot, and on what authority
/// this device accepted it (issue #877). Recorded only from a verified entry, never from the
/// server's word; local state, not part of the signed document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAppointment {
  pub user_id: UserId,
  /// The signer of the entry that set the role.
  pub by: UserId,
  pub authority: AppointmentAuthority,
  /// The identity key the entry named for the user: another key is a new appointment.
  pub identity_key: Base64Bytes,
  /// Whether the snapshot this was accepted in goes back to a workspace admin by the rule that
  /// anchors the channel (`ChannelRoleState::is_workspace_anchored`), judged at this device's
  /// time. Only then is the appointment shown: otherwise it rests on a founder's claim, or on a
  /// roster the server could have backdated an entry against.
  #[serde(default)]
  pub is_anchored: bool,
}

/// What the stored appointments say about the signer's authority. Stored locally and so
/// forward compatible: a value a newer build wrote reads as `Unknown` and is never shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppointmentAuthority {
  WorkspaceOwner,
  WorkspaceAdmin,
  ChannelAdmin,
  /// A founder's claim: trust on first use, not an appointment.
  Founder,
  #[serde(other)]
  Unknown,
}

/// On what authority an entry was (or would be) signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignerAuthority {
  /// An owner or admin of the fresh, verified workspace roster.
  Workspace { is_owner: bool },
  /// An admin of the last accepted snapshot.
  ChannelAdmin,
  /// The first entry of a channel: its signer names themselves admin.
  Founder,
  /// An owner or admin who has left the roster, for an entry dated inside their tenure. Only ever
  /// found on reading the log, never offered for signing.
  FormerWorkspace { is_owner: bool },
}

/// Who may found a channel's roles, from its anchor (e2ee.md §4.6.5, "What the founding decides").
/// It decides only while this device accepted nothing yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleFounding {
  /// The channel carries no anchor and none is pinned: the first entry is its signer's claim, with
  /// the key this device pinned for them (trust on first use, e2ee.md §4.6.1), as before anchors.
  Claimed,
  /// The anchor verifies and the floor grants: only this founder's entry, under the anchor's
  /// identity key and naming them admin, founds the roles, and it anchors them (`may_anchor`). Any
  /// other first entry is junk.
  Anchored { founder: UserId, identity_key: [u8; 32] },
  /// The channel is anchored, but this device can't take a founder from it now: the anchor doesn't
  /// verify, the floor shows a fork, or it names no founder. Nothing founds and nothing anchors:
  /// the entries wait unread until the anchor decides (fail closed).
  Withheld,
}

/// What verifying a channel's entries needs besides the log itself.
pub struct RoleVerifier<'a, D: IdentityDirectory> {
  pub conversation: ConversationId,
  pub workspace: &'a WorkspaceState,
  /// How old the newest signed roster may be for its admins to count.
  pub roster_max_age_ms: i64,
  /// This device's time. Whether a roster anchors a snapshot is judged at it, not at the entry's
  /// time, which the server sets.
  pub now_ms: i64,
  /// How far an entry's dates may lie outside a tenure, for the clocks of owner, signer and server.
  pub tenure_clock_skew_ms: i64,
  /// Identity keys this device pinned: a founder must hold the key pinned for them.
  pub pinned: &'a D,
  /// Who may found the roles: the channel's anchor decides that, where it has one.
  pub founding: RoleFounding,
}

#[derive(Debug, thiserror::Error)]
pub enum ChannelRoleError {
  #[error("role entry {version} has an unreadable payload: {reason}")]
  Payload { version: u64, reason: String },
  #[error("role entry {version} is for another channel")]
  OtherChannel { version: u64 },
  #[error("role entry {version} has an invalid snapshot: {reason}")]
  Snapshot { version: u64, reason: &'static str },
  #[error("role entry {version} isn't signed by somebody allowed to assign roles here")]
  Unauthorized { version: u64 },
  #[error("role entry {version} doesn't build on the roles this device verified")]
  BrokenChain { version: u64 },
  #[error("role entry {version} breaks a role rule for {user}: {rule}")]
  RoleRule {
    version: u64,
    user: UserId,
    rule: RoleChangeError,
  },
}

/// What a signature by somebody who has left the roster comes to.
enum FormerAdmin {
  /// Not the signature of anybody whose tenure ended.
  NotOne,
  /// The server dates it outside their tenure, or the roster that ended it has no date: no standing
  /// whatever it claims. Nothing to act on, or a demoted admin could trouble every device with one.
  OutsideTenure,
  /// It holds: the entry counts for roles.
  Within { signer: UserId, is_owner: bool },
  /// The server dates it inside the tenure, but this device can't place it: a real entry it can't
  /// verify, or a backdated one. It can't tell which.
  Unplaceable,
}

impl RoleFounding {
  /// Whether `assignment`, an admin a first entry names, may be its founder.
  fn may_found<D: IdentityDirectory>(&self, assignment: &ChannelRoleAssignment, pinned: &D) -> bool {
    let key = assignment.identity_key.as_slice();
    match self {
      // Only keys this device pinned itself: the server can't make up a founder of its own.
      Self::Claimed => pinned
        .pinned_identity(convert::user(assignment.user_id))
        .is_some_and(|pinned| pinned.as_bytes().as_slice() == key),
      Self::Anchored {
        founder,
        identity_key,
      } => assignment.user_id == *founder && identity_key.as_slice() == key,
      Self::Withheld => false,
    }
  }
}

impl ChannelRoleState {
  /// The verified role of `user` (member when the snapshot doesn't name them, and for everybody
  /// during an unverified stretch: the snapshot may be out of date).
  pub fn role_of(&self, user: UserId) -> ChannelRole {
    if self.has_unverified_stretch() {
      return ChannelRole::Member;
    }
    self.snapshot_role_of(user)
  }

  /// Whether this device knows it missed part of the log: a signer entitled to sign wrote
  /// something it can't place. Then its snapshot says nothing about today: it isn't anchored and
  /// makes nobody an admin, and the channel still counts as signed, so this never falls back to the
  /// rule of an unsigned channel, where every member may act. Chat isn't affected. A workspace
  /// admin's checkpoint ends it (e2ee.md §4.6.1, "Reading the log late").
  pub fn has_unverified_stretch(&self) -> bool {
    self.maybe_unverified_from.is_some()
  }

  /// Whether a workspace admin's signature on this device's snapshot would say more than this device
  /// knows. Every member takes a workspace admin's entry as vouching for each role it lists, and it
  /// anchors them; so during a stretch, or on a snapshot that rests on somebody who left, such an
  /// admin signs only a list they looked at ([`Self::reviewed_document`]), never this snapshot with
  /// one change, or the signature would launder what it never saw into an anchor. The same holds
  /// while a member showed this device another or a newer entry than it holds (an alarm, or a head
  /// not read yet): it may have been shown an old log, a withheld demotion (e2ee.md §4.6.3), and its
  /// signature would carry the old snapshot's anchor on to every device that was shown the same
  /// (issue #1264).
  pub fn must_review_before_vouching(&self) -> bool {
    self.has_unverified_stretch()
      || self.has_former_admin_entry
      || self.maybe_alarm.is_some()
      || self.maybe_ahead.is_some()
  }

  /// What the accepted snapshot says of `user`, whether or not this device can vouch for it today.
  fn snapshot_role_of(&self, user: UserId) -> ChannelRole {
    self
      .assignments
      .iter()
      .find(|assignment| assignment.user_id == user)
      .map_or(ChannelRole::Member, |assignment| assignment.role.clone())
  }

  /// Who appointed `user` to the role the accepted snapshot gives them, as this device verified
  /// the signing entry. `None` for a member, for a snapshot accepted before this was recorded, for
  /// a founder's claim (trust on first use) and for anything that doesn't go back to a workspace
  /// admin by the rule that anchors the channel (`is_workspace_anchored`): the UI claims an
  /// authority only where a purge would accept it too. The signer and authority are verified;
  /// the signer's display name is not (it is the server's directory).
  pub fn appointment_of(&self, user: UserId) -> Option<&RoleAppointment> {
    let assignment = self
      .assignments
      .iter()
      .find(|assignment| assignment.user_id == user)?;
    if assignment.role == ChannelRole::Member || !self.is_workspace_anchored {
      return None;
    }
    self.appointments.iter().find(|appointment| {
      appointment.user_id == user
        // Somebody's own role (a workspace admin founding a channel) isn't an appointment.
        && appointment.by != user
        && appointment.is_anchored
        && appointment.identity_key == assignment.identity_key
        && matches!(
          appointment.authority,
          AppointmentAuthority::WorkspaceOwner
            | AppointmentAuthority::WorkspaceAdmin
            | AppointmentAuthority::ChannelAdmin
        )
    })
  }

  /// The users the accepted snapshot makes admins: nobody during an unverified stretch.
  pub fn admins(&self) -> Vec<UserId> {
    self
      .admin_assignments()
      .map(|assignment| assignment.user_id)
      .collect()
  }

  /// Whether anything was signed for the channel; before that its roles are the server's word. A
  /// device that accepted nothing yet but can't place an entitled signer's entry knows it was:
  /// it must not act by the rule of an unsigned channel, or take somebody's founding claim.
  pub fn is_signed(&self) -> bool {
    self.version > 0 || self.has_unverified_stretch()
  }

  /// Whether this device may take the channel for one without a log and found it from what the
  /// server lists: it read nothing but an empty log, and no member's message showed it a head. A
  /// log it refused entries of, or was told of, stays as it is: only a workspace admin's list ends
  /// that, so a server that hides a log can't get an honest signature on a second one from a device
  /// that knows better.
  pub fn is_unfounded(&self) -> bool {
    !self.is_signed() && self.cursor == 0 && self.maybe_alarm.is_none() && self.maybe_ahead.is_none()
  }

  /// Whether an admin the server lists may act by the rule of a channel nobody signed: this device
  /// accepted nothing, and nothing told it of a log it isn't shown. Entries it refused don't count, or
  /// a junk entry would lock the owner out; a head in a member's message does, as a real log exists.
  pub fn may_act_as_unsigned(&self) -> bool {
    !self.is_signed() && self.maybe_alarm.is_none() && self.maybe_ahead.is_none()
  }

  /// The key the accepted snapshot names for `user`.
  pub fn snapshot_key_of(&self, user: UserId) -> Option<[u8; 32]> {
    self
      .assignments
      .iter()
      .find(|assignment| assignment.user_id == user)
      .and_then(|assignment| <[u8; 32]>::try_from(assignment.identity_key.as_slice()).ok())
  }

  pub fn head(&self) -> Option<ChannelRolesHead> {
    let hash = self.maybe_head_hash.clone()?;
    Some(ChannelRolesHead {
      version: self.version,
      hash,
      needs_review: false,
    })
  }

  /// The head to send along in a message: [`Self::head`], marked when this device is stuck.
  /// `None` while nothing is accepted, so a device that never accepted an entry can't ask.
  pub fn head_to_send(&self) -> Option<ChannelRolesHead> {
    let mut head = self.head()?;
    head.needs_review = self.needs_review();
    Some(head)
  }

  /// Whether this device is stuck on the channel's roles until a workspace admin signs a list they
  /// looked at: an unverified stretch, a snapshot resting on somebody who left (the anchor lost on
  /// a late read), or an alarm. Nobody else knows (e2ee.md §4.6.1, "Nobody hears of a member's stuck
  /// device"), so its messages say so. A newer head not read yet isn't stuck: the next read settles
  /// it, and an honest concurrent change would raise it on every device for a moment.
  pub fn needs_review(&self) -> bool {
    self.has_unverified_stretch() || self.has_former_admin_entry || self.maybe_alarm.is_some()
  }

  /// Takes `sender`'s request for a review from a message the server dated `sent_at_ms` (issue
  /// #1264, option 2A). One request per sender, so a member can't fill the list; one in a message
  /// from before the last reviewed list this device accepted is answered by it. A stuck device's
  /// later messages ask again, also when the server keeps that list from it.
  pub fn note_review_request(&mut self, sender: UserId, sent_at_ms: i64) {
    if self
      .maybe_reviewed_at_ms
      .is_some_and(|reviewed_at| sent_at_ms <= reviewed_at)
    {
      return;
    }
    match self
      .review_requests
      .iter_mut()
      .find(|request| request.user_id == sender)
    {
      Some(request) => request.sent_at_ms = request.sent_at_ms.max(sent_at_ms),
      None => self.review_requests.push(ReviewRequest {
        user_id: sender,
        sent_at_ms,
      }),
    }
    if self.review_requests.len() > MAX_REVIEW_REQUESTS {
      self
        .review_requests
        .sort_by_key(|request| std::cmp::Reverse(request.sent_at_ms));
      self.review_requests.truncate(MAX_REVIEW_REQUESTS);
    }
  }

  /// Whether a member's device asked for a review that no reviewed list has answered yet.
  pub fn is_review_asked(&self) -> bool {
    !self.review_requests.is_empty()
  }

  /// Once, for a state stored before a workspace admin's ordinary entry stopped counting as a
  /// recent other signer (issue #1264, option 1B): drops every recorded signer the roster names as
  /// owner or admin, or who held such a tenure. That may drop a reviewed list's signer too, which
  /// costs only a purge's speed until the next entry; keeping them could keep a demoted admin's
  /// purge open for `channel_roles_max_age`.
  pub fn forget_legacy_recent_signers(&mut self, workspace: &WorkspaceState) {
    if !self.has_legacy_recent_signers {
      return;
    }
    self
      .recent_signers
      .retain(|recent| !is_or_was_workspace_admin(workspace, recent.user_id));
    self.has_legacy_recent_signers = false;
  }

  /// Verifies and applies new entries in version order. Each one is accepted or skipped; the
  /// reasons of the skipped ones are returned (and the last one kept).
  pub fn apply_entries<D: IdentityDirectory>(
    &mut self,
    mut entries: Vec<ChannelRoleLogEntryResponse>,
    verifier: &RoleVerifier<'_, D>,
  ) -> Vec<ChannelRoleError> {
    if self.waits_for_founding(verifier.founding) {
      return Vec::new();
    }
    entries.sort_by_key(|entry| entry.version);
    if self.maybe_read_at_roster_version.is_none() && self.version == 0 {
      self.maybe_read_at_roster_version = Some(0);
    }
    let mut skipped = Vec::new();
    for entry in entries {
      if entry.version <= self.cursor {
        continue;
      }
      self.cursor = entry.version;
      if let Err(error) = self.apply_entry(&entry, verifier) {
        self.maybe_rejection = Some(error.to_string());
        skipped.push(error);
      }
    }
    skipped
  }

  /// Whether the entries must wait unread: nothing is accepted yet, and the channel's anchor can't
  /// say who founded it right now (e2ee.md §4.6.5). Read and refused, the founding would be lost for
  /// good once the anchor verifies; read as a claim, it would be the trust on first use the anchor
  /// closes.
  pub fn waits_for_founding(&self, founding: RoleFounding) -> bool {
    founding == RoleFounding::Withheld && !self.is_signed()
  }

  /// Before a read: a state from before reads were recorded may have skipped entries it would mark
  /// now, so it looks at them once more. Nothing older than its head counts again.
  pub fn prepare_read(&mut self) {
    if self.maybe_read_at_roster_version.is_none() {
      self.cursor = self.cursor.min(self.version);
    }
  }

  /// After this device read the log to its end, holding `roster_version` before the read.
  pub fn note_read(&mut self, roster_version: u64) {
    let read_at = self.maybe_read_at_roster_version.unwrap_or(0).max(roster_version);
    self.maybe_read_at_roster_version = Some(read_at);
  }

  /// Compares a member's head with ours: the same version with another hash means the server
  /// showed us different entries; a newer one is checked after the next sync.
  pub fn note_head(&mut self, other: &ChannelRolesHead) {
    if other.version == self.version && self.maybe_head_hash.as_ref() != Some(&other.hash) {
      self.maybe_alarm = Some(format!(
        "another member verified a different entry {} of this channel's roles: the server shows members different logs",
        other.version
      ));
    } else if other.version > self.version {
      self.maybe_ahead = Some(other.clone());
    }
  }

  /// After a sync: a member's newer head that this device still doesn't have means entries are
  /// held back from it, or it skipped an entry the member accepted.
  pub fn check_ahead(&mut self) {
    let Some(ahead) = self.maybe_ahead.take() else {
      return;
    };
    if ahead.version > self.cursor {
      self.maybe_alarm = Some(format!(
        "another member has entry {} of this channel's roles, which this device isn't shown",
        ahead.version
      ));
    } else if ahead.version > self.version {
      self.maybe_alarm = Some(format!(
        "another member accepted entry {} of this channel's roles, which this device refused",
        ahead.version
      ));
    } else {
      self.note_head(&ahead);
    }
  }

  /// Takes a signed entry a member sent in the channel, `entry.created_at` being the message's
  /// server time (e2ee.md §4.6.3, part 1). It is checked exactly as from the log; nothing that
  /// fails changes anything, so a member can't push this device past a real entry with junk.
  /// Foundings stay out: a server can get a second one signed on its own, so a fork of them would
  /// be no proof.
  pub fn take_message_entry<D: IdentityDirectory>(
    &mut self,
    entry: &ChannelRoleLogEntryResponse,
    verifier: &RoleVerifier<'_, D>,
  ) -> MessageEntry {
    let Some(head) = head_of(entry) else {
      return MessageEntry::Ignored;
    };
    if entry.version <= self.cursor {
      if entry.version != self.version || !self.is_signed() {
        return MessageEntry::Ignored;
      }
      if self.maybe_head_hash.as_ref() == Some(&head.hash) {
        return MessageEntry::Known;
      }
      let Some(other_signer) = self.signer_beside_head(entry, verifier) else {
        return MessageEntry::Ignored;
      };
      // Named, so whoever reviews the roles sees who signed both: a demoted admin can sign an
      // entry on the head they were still an admin at, and that looks like a real fork.
      let head_signer = self
        .maybe_head_signer
        .map_or_else(|| "unknown".to_owned(), |user| user.to_string());
      self.prove_alarm(format!(
        "two valid entries {} of this channel's roles, signed by users {head_signer} and {other_signer}: the server shows members different logs",
        entry.version
      ));
      return MessageEntry::Fork;
    }
    if entry.version > self.cursor.saturating_add(1) {
      self.note_head(&head);
      return MessageEntry::Ahead;
    }
    if !self.is_signed() {
      return MessageEntry::Ignored;
    }
    let mut next = self.clone();
    next.cursor = entry.version;
    if next.apply_entry(entry, verifier).is_err() {
      return MessageEntry::Ignored;
    }
    next.unconfirmed.push(head);
    if next.unconfirmed.len() > MAX_UNCONFIRMED {
      next.unconfirmed.remove(0);
    }
    *self = next;
    MessageEntry::Accepted
  }

  /// Where the next read of the log starts: after the cursor, or before the oldest entry taken from
  /// a message, so the read shows what the log has at its version.
  pub fn log_read_from(&self) -> u64 {
    self
      .unconfirmed
      .iter()
      .map(|head| head.version.saturating_sub(1))
      .fold(self.cursor, u64::min)
  }

  /// Compares the entries taken from messages with what a read of the log from
  /// [`Self::log_read_from`] shows. Another entry at that version, or none in a log read to its
  /// end, is a proven alarm: somebody allowed to sign signed it, and the server leaves it out.
  pub fn compare_log(&mut self, entries: &[ChannelRoleLogEntryResponse], is_read_to_end: bool) {
    for head in std::mem::take(&mut self.unconfirmed) {
      let maybe_logged = entries
        .iter()
        .find(|entry| entry.version == head.version)
        .and_then(head_of);
      match maybe_logged {
        Some(logged) if logged.hash == head.hash => {},
        Some(_) => {
          let claimed = entries
            .iter()
            .find(|entry| entry.version == head.version)
            .and_then(|entry| claimed_signer(entry.signature.as_slice()).ok())
            .map_or_else(|| "unknown".to_owned(), |user| user.to_string());
          self.prove_alarm(format!(
            "the log shows another entry {} of this channel's roles (claimed by user {claimed}) than a member sent in the channel",
            head.version
          ));
        },
        None if is_read_to_end => self.prove_alarm(format!(
          "the log leaves out entry {} of this channel's roles, which a member sent in the channel",
          head.version
        )),
        None => self.unconfirmed.push(head),
      }
    }
  }

  /// Whether somebody other than `signer` signed an entry this device accepted, counting from no
  /// earlier than `since_ms` (e2ee.md §4.6.3, part 3). Nobody whose role goes back to `signer`
  /// counts: a demoted admin on a withheld branch could otherwise keep the limit open through an
  /// account they promoted (review of #1416).
  pub fn has_other_signer_since(&self, signer: UserId, since_ms: i64) -> bool {
    self.recent_signers.iter().any(|recent| {
      recent.user_id != signer && recent.from_ms >= since_ms && !self.is_appointed_by(recent.user_id, signer)
    })
  }

  /// Whether `user`'s role in the accepted snapshot goes back to `appointer`, directly or through
  /// people they appointed, as this device recorded the appointments.
  fn is_appointed_by(&self, user: UserId, appointer: UserId) -> bool {
    let mut current = user;
    // Each step names another person of the snapshot; more steps than that is a cycle.
    for _ in 0..=self.appointments.len() {
      let Some(appointment) = self
        .appointments
        .iter()
        .find(|appointment| appointment.user_id == current)
      else {
        return false;
      };
      if appointment.by == appointer {
        return true;
      }
      if appointment.by == current {
        return false;
      }
      current = appointment.by;
    }
    false
  }

  /// On what authorities `signer` could sign the next entry now (`now_ms`: the time the roster's
  /// freshness is judged at), in the order members try them. Empty: an entry of theirs would be
  /// refused by every member.
  pub fn signing_authorities(
    &self,
    signer: UserId,
    workspace: &WorkspaceState,
    now_ms: i64,
    roster_max_age_ms: i64,
  ) -> Vec<SignerAuthority> {
    let mut authorities: Vec<SignerAuthority> =
      workspace_authority(workspace, signer, now_ms, roster_max_age_ms)
        .into_iter()
        .collect();
    if self.role_of(signer) == ChannelRole::Admin {
      authorities.push(SignerAuthority::ChannelAdmin);
    }
    if !self.is_signed() {
      authorities.push(SignerAuthority::Founder);
    }
    authorities
  }

  /// The next document: this device's snapshot with `changes` applied (`Member` drops the
  /// person), built on its head. `keys`: identity keys for people the snapshot doesn't name yet.
  pub fn next_document(
    &self,
    conversation: ConversationId,
    changes: &[(UserId, ChannelRole)],
    keys: &BTreeMap<UserId, [u8; 32]>,
    signed_at_ms: i64,
  ) -> Result<ChannelRolesDocument, ChannelRoleError> {
    let version = self.cursor + 1;
    let mut assignments = self.assignments.clone();
    for (user, role) in changes {
      assignments.retain(|assignment| assignment.user_id != *user);
      if *role == ChannelRole::Member {
        continue;
      }
      let key = keys.get(user).ok_or(ChannelRoleError::Snapshot {
        version,
        reason: "no identity key for a person given a role",
      })?;
      assignments.push(ChannelRoleAssignment {
        user_id: *user,
        role: role.clone(),
        identity_key: Base64Bytes::new(key.to_vec()),
      });
    }
    assignments.sort_by_key(|assignment| assignment.user_id);
    Ok(ChannelRolesDocument {
      version,
      conversation_id: conversation,
      assignments,
      prev_hash: self.head_or_zero(),
      signed_at_ms,
      has_server_listed_roles: self.has_server_listed_roles,
    })
  }

  /// The document for a list of roles a workspace admin reviewed: exactly `roles` (`Member` names
  /// nobody), with a [checkpoint's](REVIEWED_PREV_HASH) place on every device.
  pub fn reviewed_document(
    &self,
    conversation: ConversationId,
    roles: &[(UserId, ChannelRole)],
    keys: &BTreeMap<UserId, [u8; 32]>,
    signed_at_ms: i64,
  ) -> Result<ChannelRolesDocument, ChannelRoleError> {
    let version = self.cursor + 1;
    let mut assignments = Vec::new();
    for (user, role) in roles {
      if *role == ChannelRole::Member {
        continue;
      }
      let key = keys.get(user).ok_or(ChannelRoleError::Snapshot {
        version,
        reason: "no identity key for a person given a role",
      })?;
      assignments.push(ChannelRoleAssignment {
        user_id: *user,
        role: role.clone(),
        identity_key: Base64Bytes::new(key.to_vec()),
      });
    }
    assignments.sort_by_key(|assignment| assignment.user_id);
    Ok(ChannelRolesDocument {
      version,
      conversation_id: conversation,
      assignments,
      prev_hash: Base64Bytes::new(REVIEWED_PREV_HASH.to_vec()),
      signed_at_ms,
      has_server_listed_roles: false,
    })
  }

  /// The checks every member makes on a document built on this device's head, before the
  /// signature: what [`Self::apply_entries`] would refuse is never signed.
  pub fn check_document(
    &self,
    document: &ChannelRolesDocument,
    conversation: ConversationId,
    signer: UserId,
    authority: SignerAuthority,
  ) -> Result<(), ChannelRoleError> {
    let version = document.version;
    if document.conversation_id != conversation {
      return Err(ChannelRoleError::OtherChannel { version });
    }
    check_snapshot(version, &document.assignments)?;
    let continues = document.prev_hash == self.head_or_zero();
    match authority {
      // A checkpoint by the workspace's admins replaces whatever this device had. Only a list one of
      // them looked at: anything else names a head, and only builds on it (review of #1286).
      SignerAuthority::Workspace { .. } if is_reviewed(document) && !document.has_server_listed_roles => {
        return Ok(());
      },
      SignerAuthority::Workspace { .. }
      | SignerAuthority::ChannelAdmin
      | SignerAuthority::FormerWorkspace { .. }
        if !continues =>
      {
        return Err(ChannelRoleError::BrokenChain { version });
      },
      SignerAuthority::Workspace { .. } => {},
      SignerAuthority::FormerWorkspace { .. } => {},
      SignerAuthority::ChannelAdmin => {
        if self.role_of(signer) != ChannelRole::Admin {
          return Err(ChannelRoleError::Unauthorized { version });
        }
      },
      SignerAuthority::Founder => {
        let is_admin_in_it = document
          .assignments
          .iter()
          .any(|assignment| assignment.user_id == signer && assignment.role == ChannelRole::Admin);
        if self.is_signed() || !continues || !is_admin_in_it {
          return Err(ChannelRoleError::Unauthorized { version });
        }
      },
    }
    self.check_changes(document, signer, authority)
  }

  fn apply_entry<D: IdentityDirectory>(
    &mut self,
    entry: &ChannelRoleLogEntryResponse,
    verifier: &RoleVerifier<'_, D>,
  ) -> Result<(), ChannelRoleError> {
    let version = entry.version;
    let document: ChannelRolesDocument =
      serde_json::from_slice(entry.payload.as_slice()).map_err(|error| ChannelRoleError::Payload {
        version,
        reason: error.to_string(),
      })?;
    if document.version != version {
      return Err(ChannelRoleError::Payload {
        version,
        reason: "the document names another version than its entry".into(),
      });
    }
    // The first authority the signature holds under and the document passes with decides; a
    // workspace admin founding a channel signs as its founder, for example.
    let mut outcome = Err(ChannelRoleError::Unauthorized { version });
    let mut maybe_authority = None;
    let mut maybe_signer = None;
    let former = self.judge_former_admin(entry, &document, verifier);
    let mut signers = self.verify_signer(entry, &document, verifier);
    if let FormerAdmin::Within { signer, is_owner } = former {
      signers.push((signer, SignerAuthority::FormerWorkspace { is_owner }));
    }
    // The workspace admin behind the signature, if the roster vouches for it: they appointed what
    // the entry changes even where it passes as a founder's.
    let maybe_workspace_signer = signers
      .iter()
      .copied()
      .find(|(_, authority)| matches!(authority, SignerAuthority::Workspace { .. }));
    // A workspace admin founding a channel passes as its founder (nobody changes their own role
    // otherwise), and it is a workspace admin's word all the same. It anchors only on what a purge
    // itself would take from that admin (`signed_deletion`): the server sets the entry's time.
    // Roles nobody looked at aren't anything to anchor on, whoever signed. That goes on through an
    // older client's entry too, which doesn't carry the mark on: only a reviewed list ends it.
    // An anchor takes a workspace admin who knows what they vouch for. Their ordinary entry changes one
    // role, and every key in it may check while the roles are a colluding founder's: so it only carries
    // on an anchor. It takes a list they looked at, or their own founding, which names what they chose.
    let may_anchor = !document.has_server_listed_roles
      && (!self.has_server_listed_roles || is_reviewed(&document))
      && (self.is_workspace_anchored || is_reviewed(&document) || !self.is_signed())
      && !names_another_key_than_pinned(&document, verifier.pinned);
    let is_by_workspace_admin = may_anchor
      && verifier.workspace.heads.maybe_roster_signed_at_ms.is_some()
      && verifier
        .workspace
        .is_roster_fresh(verifier.now_ms, verifier.roster_max_age_ms)
      && signers
        .iter()
        .any(|(_, authority)| matches!(authority, SignerAuthority::Workspace { .. }));
    // e2ee.md §4.6.5: in an anchored channel only the anchor's founder founds; any other first entry
    // is junk, whoever signed it, and shows no gap.
    let is_anchored_unsigned = !self.is_signed() && verifier.founding != RoleFounding::Claimed;
    for (signer, authority) in signers.iter().copied() {
      if is_anchored_unsigned && authority != SignerAuthority::Founder {
        continue;
      }
      outcome = self.check_document(&document, verifier.conversation, signer, authority);
      if outcome.is_ok() {
        maybe_authority = Some(authority);
        maybe_signer = Some((signer, authority));
        break;
      }
    }
    let hash = log_entry_hash(
      WorkspaceLog::ChannelRoles,
      version,
      entry.payload.as_slice(),
      entry.signature.as_slice(),
    )
    .map_err(|error| ChannelRoleError::Payload {
      version,
      reason: error.to_string(),
    })?;
    if outcome.is_err() {
      if !is_anchored_unsigned && self.shows_a_gap(entry, &document, &signers, &former, verifier) {
        self.maybe_unverified_from.get_or_insert(version);
        self.is_workspace_anchored = false;
      }
      if document.conversation_id == verifier.conversation {
        self.remember_skipped(&hash);
      }
    }
    outcome?;
    // The anchor proves the founding (e2ee.md §4.6.5): a snapshot going back to it is anchored, as
    // a workspace admin's own founding is, on the same conditions otherwise.
    let is_anchored_founding = may_anchor
      && matches!(verifier.founding, RoleFounding::Anchored { .. })
      && maybe_authority == Some(SignerAuthority::Founder);
    self.keep_before_head();
    self.maybe_head_signer = maybe_signer.map(|(signer, _)| signer);
    if let Some((signer, authority)) = maybe_signer
      && counts_as_recent_signer(&document, signer, authority, &signers, verifier.workspace)
    {
      self.note_signer(signer, document.signed_at_ms.min(verifier.now_ms));
    }
    let maybe_appointer = match (maybe_signer, maybe_workspace_signer) {
      (Some((_, SignerAuthority::Founder)), Some(workspace)) => Some(workspace),
      (accepted, _) => accepted,
    };
    // A workspace admin's entry built on another head than ours re-anchors this device: it vouches
    // for every role it lists, including the ones this device already had from an older entry.
    let is_checkpoint = matches!(maybe_appointer, Some((_, SignerAuthority::Workspace { .. })))
      && document.prev_hash != self.head_or_zero();
    let previous = std::mem::take(&mut self.appointments);
    self.version = version;
    self.maybe_head_hash = Some(Base64Bytes::new(hash.to_vec()));
    let previous_assignments = std::mem::replace(&mut self.assignments, document.assignments);
    let has_server_listed_roles = document.has_server_listed_roles;
    let was_reviewed = is_reviewed_hash(&document.prev_hash);
    // Any reviewed list answers what stuck devices asked for: only a workspace admin's passes, and
    // they looked, whether or not it anchors here.
    if was_reviewed {
      self.review_requests.clear();
      // The server's date, never later than this device's now: a list dated ahead would silence
      // every request until then.
      let reviewed_at = entry.created_at.0.min(verifier.now_ms);
      self.maybe_reviewed_at_ms = Some(
        self
          .maybe_reviewed_at_ms
          .map_or(reviewed_at, |at| at.max(reviewed_at)),
      );
    }
    match maybe_authority {
      _ if is_by_workspace_admin || is_anchored_founding => self.is_workspace_anchored = true,
      // A claim, a roster admin on a roster too old to anchor anything, or somebody who left and
      // whose dates this device can only take their word for: their snapshot replaces what was
      // anchored before. A purge is the one thing that can't be repaired.
      Some(
        SignerAuthority::Founder
        | SignerAuthority::Workspace { .. }
        | SignerAuthority::FormerWorkspace { .. },
      ) => self.is_workspace_anchored = false,
      // A channel admin carries on what the snapshot they build on was.
      Some(SignerAuthority::ChannelAdmin) | None => {},
    }
    if !is_by_workspace_admin && (has_server_listed_roles || self.has_server_listed_roles) {
      self.has_server_listed_roles = true;
      self.is_workspace_anchored = false;
    }
    // Only a checkpoint this device could anchor on says what holds today.
    if is_by_workspace_admin {
      self.maybe_unverified_from = None;
      self.has_former_admin_entry = false;
      self.has_server_listed_roles = false;
      // Somebody who can look looked: what the members disagreed about is settled by this list.
      if was_reviewed {
        self.maybe_alarm = None;
        self.is_alarm_proven = false;
      }
    } else if matches!(maybe_authority, Some(SignerAuthority::FormerWorkspace { .. })) {
      self.has_former_admin_entry = true;
    }
    self.appointments =
      self.next_appointments(&previous, &previous_assignments, maybe_appointer, is_checkpoint);
    Ok(())
  }

  /// The appointments after an entry (already applied to `self.assignments`): a role or identity
  /// key the entry changed or added was appointed by its signer, and so is every role a
  /// checkpoint lists; one it left as it was keeps what this device recorded (nothing, for a
  /// snapshot from before appointments were recorded).
  fn next_appointments(
    &self,
    previous: &[RoleAppointment],
    previous_assignments: &[ChannelRoleAssignment],
    maybe_appointer: Option<(UserId, SignerAuthority)>,
    is_checkpoint: bool,
  ) -> Vec<RoleAppointment> {
    self
      .assignments
      .iter()
      .filter_map(|assignment| {
        let user_id = assignment.user_id;
        let maybe_before = previous_assignments
          .iter()
          .find(|before| before.user_id == user_id);
        let is_unchanged = maybe_before.is_some_and(|before| {
          before.role == assignment.role && before.identity_key == assignment.identity_key
        });
        if is_unchanged && !is_checkpoint {
          return previous
            .iter()
            .find(|appointment| appointment.user_id == user_id)
            .cloned();
        }
        let (by, authority) = maybe_appointer?;
        Some(RoleAppointment {
          user_id,
          by,
          authority: match authority {
            SignerAuthority::Workspace { is_owner: true }
            | SignerAuthority::FormerWorkspace { is_owner: true } => AppointmentAuthority::WorkspaceOwner,
            SignerAuthority::Workspace { is_owner: false }
            | SignerAuthority::FormerWorkspace { is_owner: false } => AppointmentAuthority::WorkspaceAdmin,
            SignerAuthority::ChannelAdmin => AppointmentAuthority::ChannelAdmin,
            SignerAuthority::Founder => AppointmentAuthority::Founder,
          },
          identity_key: assignment.identity_key.clone(),
          is_anchored: self.is_workspace_anchored,
        })
      })
      .collect()
  }

  /// Who signed the entry and on what authorities the signature holds: the workspace roster, this
  /// device's snapshot and (before the first accepted entry) the founder's claim, in that order.
  fn verify_signer<D: IdentityDirectory>(
    &self,
    entry: &ChannelRoleLogEntryResponse,
    document: &ChannelRolesDocument,
    verifier: &RoleVerifier<'_, D>,
  ) -> Vec<(UserId, SignerAuthority)> {
    let version = entry.version;
    let mut found = Vec::new();
    let verify_with = |directory: &KeyDirectory| {
      verify_log_entry(
        entry.signature.as_slice(),
        WorkspaceLog::ChannelRoles,
        version,
        entry.payload.as_slice(),
        directory,
      )
      .ok()
      .and_then(|device| convert::user_from_crypto(device.user_id()).ok())
    };
    // The server sets the date, and a date in the future must not make a fresh roster look old: an
    // entry can't be newer than this device's now.
    let at_ms = entry.created_at.0.min(verifier.now_ms);
    if verifier
      .workspace
      .is_roster_fresh(at_ms, verifier.roster_max_age_ms)
    {
      let admins = KeyDirectory::from_keys(roster_admin_keys(verifier.workspace));
      if let Some(signer) = verify_with(&admins) {
        let is_owner = verifier
          .workspace
          .roster
          .members
          .iter()
          .any(|member| member.user_id == signer && member.role == RosterRole::Owner);
        found.push((signer, SignerAuthority::Workspace { is_owner }));
      }
    }
    let channel_admins = KeyDirectory::from_assignments(self.admin_assignments());
    if let Some(signer) = verify_with(&channel_admins) {
      found.push((signer, SignerAuthority::ChannelAdmin));
    }
    if !self.is_signed() {
      let claimed = document.assignments.iter().filter(|assignment| {
        assignment.role == ChannelRole::Admin && verifier.founding.may_found(assignment, verifier.pinned)
      });
      if let Some(signer) = verify_with(&KeyDirectory::from_assignments(claimed)) {
        found.push((signer, SignerAuthority::Founder));
      }
    }
    found
  }

  /// Whether the entry's signer left the roster, and what that comes to. Neither date proves
  /// anything alone: the signer sets one and the server the other. So an entry counts only if both
  /// fall inside the tenure, and the server's date can only take that away. It gets no leeway at
  /// the end of a tenure: every device judges by the same signed end, so an entry after it is junk
  /// on all of them alike. What the roster's versions add needs no clock: an entry this device
  /// didn't see when it read the log holding roster R isn't older than R, so a tenure over by then
  /// doesn't cover it.
  fn judge_former_admin<D: IdentityDirectory>(
    &self,
    entry: &ChannelRoleLogEntryResponse,
    document: &ChannelRolesDocument,
    verifier: &RoleVerifier<'_, D>,
  ) -> FormerAdmin {
    let Some(signer) = claimed_signer(entry.signature.as_slice())
      .ok()
      .and_then(|user| convert::user_from_crypto(user).ok())
    else {
      return FormerAdmin::NotOne;
    };
    let may_place = !self.has_unverified_stretch() && document.prev_hash == self.head_or_zero();
    let skew = verifier.tenure_clock_skew_ms;
    let mut verdict = FormerAdmin::NotOne;
    for tenure in verifier.workspace.admin_history.ended_tenures_of(signer) {
      if !is_signed_with(entry, signer, tenure) {
        continue;
      }
      if !may_be_appended_at(tenure, entry.created_at.0, skew) {
        if matches!(verdict, FormerAdmin::NotOne) {
          verdict = FormerAdmin::OutsideTenure;
        }
        continue;
      }
      if may_place
        && may_be_signed_at(tenure, document.signed_at_ms, skew)
        && self
          .maybe_read_at_roster_version
          .is_some_and(|read_at| outlasts(tenure, read_at))
      {
        return FormerAdmin::Within {
          signer,
          is_owner: tenure.is_owner,
        };
      }
      verdict = FormerAdmin::Unplaceable;
    }
    verdict
  }

  /// Whether a refused entry shows this device missed part of the log: it comes from somebody
  /// entitled to sign, about this channel, in a valid shape. A member, a stranger or junk never
  /// counts, or anybody could take away a channel's anchor. Neither does a fork: a channel admin on
  /// a head nobody showed this device. What can still start one is listed in e2ee.md §4.6.1,
  /// "Reading the log late".
  fn shows_a_gap<D: IdentityDirectory>(
    &self,
    entry: &ChannelRoleLogEntryResponse,
    document: &ChannelRolesDocument,
    signers: &[(UserId, SignerAuthority)],
    former: &FormerAdmin,
    verifier: &RoleVerifier<'_, D>,
  ) -> bool {
    if document.conversation_id != verifier.conversation
      || check_snapshot(entry.version, &document.assignments).is_err()
    {
      return false;
    }
    // A channel admin of this snapshot built on an entry this device refused.
    let is_on_a_refused_entry = self.skipped_hashes.contains(&document.prev_hash)
      && signers
        .iter()
        .any(|(_, authority)| *authority == SignerAuthority::ChannelAdmin);
    // An admin the roster names, whose authority lapsed only because this roster is too old.
    let is_on_a_stale_roster = !verifier.workspace.is_roster_fresh(
      entry.created_at.0.min(verifier.now_ms),
      verifier.roster_max_age_ms,
    ) && is_signature_valid(
      entry,
      &KeyDirectory::from_keys(roster_admin_keys(verifier.workspace)),
    );
    // Without the roster's history a departed admin can't be told from any other pinned key.
    let history = &verifier.workspace.admin_history;
    let is_unknown_past = !history.is_complete
      && verifier.workspace.roster.version > 0
      && is_signature_valid(entry, verifier.pinned);
    matches!(former, FormerAdmin::Unplaceable)
      || is_on_a_refused_entry
      || is_on_a_stale_roster
      || is_unknown_past
  }

  /// Every change against this device's snapshot follows the role rules for the signer: no own
  /// role changed (the founder's own claim aside), no last admin dropped, only known roles. The
  /// scheme is the built-in one; a space's own scheme is the server's word.
  fn check_changes(
    &self,
    document: &ChannelRolesDocument,
    signer: UserId,
    authority: SignerAuthority,
  ) -> Result<(), ChannelRoleError> {
    let (instance_role, maybe_own_role) = match authority {
      SignerAuthority::Workspace { is_owner } | SignerAuthority::FormerWorkspace { is_owner } => (
        if is_owner { Role::Owner } else { Role::Admin },
        Some(self.snapshot_role_of(signer)),
      ),
      SignerAuthority::ChannelAdmin | SignerAuthority::Founder => (Role::Member, Some(ChannelRole::Admin)),
    };
    let actor = RoleActor {
      instance_role,
      is_space_admin: false,
      maybe_own_role,
      visibility: ConversationVisibility::Private,
      is_archived: false,
      maybe_granted: None,
    };
    let admin_count = self
      .assignments
      .iter()
      .filter(|assignment| assignment.role == ChannelRole::Admin)
      .count();
    let next = ChannelRoleState {
      assignments: document.assignments.clone(),
      ..Self::default()
    };
    let users: BTreeSet<UserId> = self
      .assignments
      .iter()
      .chain(&document.assignments)
      .map(|assignment| assignment.user_id)
      .collect();
    for user in users {
      let (current, new_role) = (self.snapshot_role_of(user), next.snapshot_role_of(user));
      if current == new_role || (authority == SignerAuthority::Founder && user == signer) {
        continue;
      }
      let target = RoleTarget {
        is_guest: false,
        space: SpaceStanding::Unknown,
        is_self: user == signer,
        current,
      };
      check_channel_role_change(
        &PermissionScheme::default(),
        &actor,
        &target,
        &new_role,
        admin_count,
      )
      .map_err(|rule| ChannelRoleError::RoleRule {
        version: document.version,
        user,
        rule,
      })?;
    }
    Ok(())
  }

  /// Who signed `entry`, for the version this device holds another entry of, if it would have been
  /// accepted in its place. Not for a founding: the server can get a second one signed by itself.
  fn signer_beside_head<D: IdentityDirectory>(
    &self,
    entry: &ChannelRoleLogEntryResponse,
    verifier: &RoleVerifier<'_, D>,
  ) -> Option<UserId> {
    let before = self.maybe_before_head.as_ref()?;
    if !before.is_signed() {
      return None;
    }
    let mut other = (**before).clone();
    other.cursor = entry.version;
    other.apply_entry(entry, verifier).ok()?;
    other.maybe_head_signer
  }

  fn prove_alarm(&mut self, reason: String) {
    self.maybe_alarm = Some(reason);
    self.is_alarm_proven = true;
  }

  /// Keeps the state as it was before the entry being accepted, for [`Self::signer_beside_head`].
  /// The lists the check doesn't read are set aside rather than cloned: this runs for every
  /// accepted entry of a long log.
  fn keep_before_head(&mut self) {
    self.maybe_before_head = None;
    let unconfirmed = std::mem::take(&mut self.unconfirmed);
    let recent_signers = std::mem::take(&mut self.recent_signers);
    let skipped_hashes = std::mem::take(&mut self.skipped_hashes);
    let before = self.clone();
    self.unconfirmed = unconfirmed;
    self.recent_signers = recent_signers;
    self.skipped_hashes = skipped_hashes;
    self.maybe_before_head = Some(Box::new(before));
  }

  fn note_signer(&mut self, signer: UserId, from_ms: i64) {
    match self
      .recent_signers
      .iter_mut()
      .find(|recent| recent.user_id == signer)
    {
      Some(recent) => recent.from_ms = recent.from_ms.max(from_ms),
      None => self.recent_signers.push(RecentSigner {
        user_id: signer,
        from_ms,
      }),
    }
    if self.recent_signers.len() > MAX_RECENT_SIGNERS {
      self
        .recent_signers
        .sort_by_key(|recent| std::cmp::Reverse(recent.from_ms));
      self.recent_signers.truncate(MAX_RECENT_SIGNERS);
    }
  }

  fn remember_skipped(&mut self, hash: &[u8; HASH_BYTES]) {
    self.skipped_hashes.push(Base64Bytes::new(hash.to_vec()));
    if self.skipped_hashes.len() > MAX_SKIPPED_HASHES {
      self.skipped_hashes.remove(0);
    }
  }

  /// The admins of the accepted snapshot this device vouches for: none during an unverified
  /// stretch.
  fn admin_assignments(&self) -> impl Iterator<Item = &ChannelRoleAssignment> + '_ {
    self
      .assignments
      .iter()
      .filter(|assignment| !self.has_unverified_stretch() && assignment.role == ChannelRole::Admin)
  }

  fn head_or_zero(&self) -> Base64Bytes {
    self
      .maybe_head_hash
      .clone()
      .unwrap_or_else(|| Base64Bytes::new(vec![0; HASH_BYTES]))
  }
}

/// Whether the document names a key for somebody other than the one this device pinned for them.
/// An anchor lets its admins purge at once, so a device doesn't take one on a key it knows of another
/// for: an older client's signature on a first pin from the server, or a real new key this device
/// hasn't taken yet. It counts for the role either way; the anchor can wait.
fn names_another_key_than_pinned<D: IdentityDirectory>(document: &ChannelRolesDocument, pinned: &D) -> bool {
  document.assignments.iter().any(|assignment| {
    pinned
      .pinned_identity(convert::user(assignment.user_id))
      .is_some_and(|key| key.as_bytes().as_slice() != assignment.identity_key.as_slice())
  })
}

/// The head an entry would make: its version and hash.
fn head_of(entry: &ChannelRoleLogEntryResponse) -> Option<ChannelRolesHead> {
  let hash = log_entry_hash(
    WorkspaceLog::ChannelRoles,
    entry.version,
    entry.payload.as_slice(),
    entry.signature.as_slice(),
  )
  .ok()?;
  Some(ChannelRolesHead {
    version: entry.version,
    hash: Base64Bytes::new(hash.to_vec()),
    needs_review: false,
  })
}

/// Whether an accepted entry keeps the time limit on a channel admin's purge open (e2ee.md §4.6.3,
/// part 3). A workspace admin's ordinary entry doesn't (issue #1264, option 1B): their device may
/// sit outside the channel's group, where a server can withhold a demotion and every message that
/// would tell of it, and their signature on the old snapshot would then let the demoted admin
/// purge at once. It still carries the anchor as before. A list they looked at and their own
/// founding count: those name what they chose, not a snapshot handed to them. Decided on who
/// signed, whatever authority the entry passed under, so a workspace admin who is also a channel
/// admin doesn't count through that role either, nor on a roster too old to vouch for them.
fn counts_as_recent_signer(
  document: &ChannelRolesDocument,
  signer: UserId,
  accepted: SignerAuthority,
  signers: &[(UserId, SignerAuthority)],
  workspace: &WorkspaceState,
) -> bool {
  let is_workspace_signature = signers.iter().any(|(_, authority)| {
    matches!(
      authority,
      SignerAuthority::Workspace { .. } | SignerAuthority::FormerWorkspace { .. }
    )
  }) || workspace
    .roster
    .members
    .iter()
    .any(|member| member.user_id == signer && matches!(member.role, RosterRole::Owner | RosterRole::Admin));
  !is_workspace_signature || is_reviewed(document) || accepted == SignerAuthority::Founder
}

/// Whether the document is a list a workspace admin looked at.
fn is_reviewed(document: &ChannelRolesDocument) -> bool {
  is_reviewed_hash(&document.prev_hash)
}

fn is_reviewed_hash(prev_hash: &Base64Bytes) -> bool {
  prev_hash.as_slice() == REVIEWED_PREV_HASH.as_slice()
}

/// Whether the roster names `user` as owner or admin, however old it is, or the admin history
/// holds a tenure of theirs.
fn is_or_was_workspace_admin(workspace: &WorkspaceState, user: UserId) -> bool {
  workspace
    .roster
    .members
    .iter()
    .any(|member| member.user_id == user && matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    || workspace
      .admin_history
      .tenures
      .iter()
      .any(|tenure| tenure.user_id == user)
}

/// The serde default of [`ChannelRoleState::has_legacy_recent_signers`]: a stored state without the
/// field is from before it.
fn is_legacy_state() -> bool {
  true
}

/// The role to act on: the verified one, but never more than the server lists (the server can
/// take a role away, as it can withhold anything, but never grant one). A guest of the instance is
/// a member whatever is signed (role_rules: guests stay members).
pub fn effective_role(verified: &ChannelRole, server: &ChannelRole, is_guest: bool) -> ChannelRole {
  if is_guest {
    return ChannelRole::Member;
  }
  if rank(verified) <= rank(server) {
    verified.clone()
  } else {
    server.clone()
  }
}

fn rank(role: &ChannelRole) -> u8 {
  match role {
    ChannelRole::Admin => 2,
    ChannelRole::Moderator => 1,
    ChannelRole::Member | ChannelRole::Unknown(_) => 0,
  }
}

/// Whether the entry is signed with the identity key of this tenure.
fn is_signed_with(entry: &ChannelRoleLogEntryResponse, signer: UserId, tenure: &AdminTenure) -> bool {
  let Ok(key) = <[u8; 32]>::try_from(tenure.identity_key.as_slice()) else {
    return false;
  };
  let directory = KeyDirectory::from_keys(BTreeMap::from([(
    convert::user(signer),
    IdentityPublicKey::from_bytes(key),
  )]));
  is_signature_valid(entry, &directory)
}

fn is_signature_valid(entry: &ChannelRoleLogEntryResponse, directory: &impl IdentityDirectory) -> bool {
  verify_log_entry(
    entry.signature.as_slice(),
    WorkspaceLog::ChannelRoles,
    entry.version,
    entry.payload.as_slice(),
    directory,
  )
  .is_ok()
}

/// Whether the signer may have dated the entry so: inside the tenure, by the signing times of the
/// rosters that began and ended it, with leeway for their clocks. A roster without a signing time
/// bounds nothing at the start, and at the end it dates nothing inside: it proves no tenure over.
fn may_be_signed_at(tenure: &AdminTenure, at_ms: i64, skew_ms: i64) -> bool {
  is_after_start(tenure, at_ms, skew_ms)
    && tenure_end_ms(tenure).is_some_and(|until| at_ms <= until.saturating_add(skew_ms))
}

/// Whether the server may have dated the entry so: like [`may_be_signed_at`], without leeway at the
/// end. A demoted admin's entry after the roster that removed them is junk on every device alike.
fn may_be_appended_at(tenure: &AdminTenure, at_ms: i64, skew_ms: i64) -> bool {
  is_after_start(tenure, at_ms, skew_ms) && tenure_end_ms(tenure).is_some_and(|until| at_ms <= until)
}

fn is_after_start(tenure: &AdminTenure, at_ms: i64, skew_ms: i64) -> bool {
  tenure
    .maybe_from_ms
    .is_none_or(|from| at_ms >= from.saturating_sub(skew_ms))
}

fn tenure_end_ms(tenure: &AdminTenure) -> Option<i64> {
  tenure.maybe_until.as_ref()?.maybe_at_ms
}

/// Whether the signer was still on the roster this device held when it last read the log, or later.
fn outlasts(tenure: &AdminTenure, read_at_roster_version: u64) -> bool {
  tenure
    .maybe_until
    .as_ref()
    .is_some_and(|until| until.version > read_at_roster_version)
}

/// A snapshot names everybody once, with a known role above member and a usable key, and keeps
/// at least one admin (a channel never loses its last admin, role_rules).
fn check_snapshot(version: u64, assignments: &[ChannelRoleAssignment]) -> Result<(), ChannelRoleError> {
  let invalid = |reason| Err(ChannelRoleError::Snapshot { version, reason });
  if assignments.len() > MAX_CHANNEL_ROLE_ASSIGNMENTS {
    return invalid("too many assignments");
  }
  let mut users = BTreeSet::new();
  for assignment in assignments {
    if !users.insert(assignment.user_id) {
      return invalid("a person is named twice");
    }
    if !matches!(assignment.role, ChannelRole::Admin | ChannelRole::Moderator) {
      return invalid("only admins and moderators are listed");
    }
    if assignment.identity_key.len() != 32 {
      return invalid("an identity key has the wrong length");
    }
  }
  if !assignments
    .iter()
    .any(|assignment| assignment.role == ChannelRole::Admin)
  {
    return invalid("no admin");
  }
  Ok(())
}

/// Identity keys of the roster's owners and admins: they may sign any channel's roles.
pub(crate) fn roster_admin_keys(workspace: &WorkspaceState) -> BTreeMap<crypto::UserId, IdentityPublicKey> {
  workspace
    .roster
    .members
    .iter()
    .filter(|member| matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .filter_map(|member| {
      let key = <[u8; 32]>::try_from(member.identity_key.as_slice()).ok()?;
      Some((convert::user(member.user_id), IdentityPublicKey::from_bytes(key)))
    })
    .collect()
}

/// [`SignerAuthority::Workspace`] if `user` is an owner or admin of the roster, fresh at `at_ms`.
fn workspace_authority(
  workspace: &WorkspaceState,
  user: UserId,
  at_ms: i64,
  roster_max_age_ms: i64,
) -> Option<SignerAuthority> {
  if !workspace.is_roster_fresh(at_ms, roster_max_age_ms) {
    return None;
  }
  let member = workspace
    .roster
    .members
    .iter()
    .find(|member| member.user_id == user)?;
  match member.role {
    RosterRole::Owner => Some(SignerAuthority::Workspace { is_owner: true }),
    RosterRole::Admin => Some(SignerAuthority::Workspace { is_owner: false }),
    RosterRole::Unknown => None,
  }
}

/// Identity keys as an [`IdentityDirectory`], for one verification attempt.
pub(crate) struct KeyDirectory(BTreeMap<crypto::UserId, IdentityPublicKey>);

impl KeyDirectory {
  pub(crate) fn from_keys(keys: BTreeMap<crypto::UserId, IdentityPublicKey>) -> Self {
    Self(keys)
  }

  pub(crate) fn from_assignments<'a>(assignments: impl Iterator<Item = &'a ChannelRoleAssignment>) -> Self {
    Self(
      assignments
        .filter_map(|assignment| {
          let key = <[u8; 32]>::try_from(assignment.identity_key.as_slice()).ok()?;
          Some((
            convert::user(assignment.user_id),
            IdentityPublicKey::from_bytes(key),
          ))
        })
        .collect(),
    )
  }
}

impl IdentityDirectory for KeyDirectory {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    self.0.get(&user_id).copied()
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;

  use rand_core::OsRng;
  use teamcraft_crypto::identity::{DeviceKeyPair, IdentityKeyPair, SignedDeviceCertificate};
  use teamcraft_crypto::workspace::sign_log_entry;
  use teamcraft_proto::{Id, Snowflake, UnixMillis, WorkspaceLogEntryResponse, WorkspaceLogKind};

  use super::*;
  use crate::workspace::{AdminHistory, LogHeads, RosterDocument, RosterMember};

  const CHANNEL: i64 = 900;
  const NOW_MS: i64 = 1_000_000;
  const DAY_MS: i64 = 24 * 60 * 60 * 1000;
  const MAX_AGE_MS: i64 = 30 * DAY_MS;
  const SKEW_MS: i64 = 10 * 60 * 1000;

  /// e2ee.md §4.6.5, "What the founding decides": in an anchored channel only the anchor's founder
  /// founds, and that founding anchors the roles (`may_anchor`).
  mod anchored_founding {
    use super::*;

    /// Alice, a plain member, founded the channel's anchor: her founding anchors, and so does what
    /// she appoints on it.
    #[test]
    fn an_anchored_founders_entry_anchors() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = anchored_by(&alice, &[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert!(state.is_workspace_anchored, "the anchor proves her founding");

      let appoint = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, appoint).is_empty());
      assert!(state.is_workspace_anchored);
      let appointment = state.appointment_of(bob.user_id).unwrap();
      assert_eq!(
        (appointment.by, appointment.authority),
        (alice.user_id, AppointmentAuthority::ChannelAdmin)
      );
    }

    /// The same entry on a channel without an anchor is a claim, as before.
    #[test]
    fn an_unanchored_founders_entry_stays_a_claim() {
      let alice = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let state = founded(&world, &alice, &[]);
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert!(!state.is_workspace_anchored);
    }

    /// A member, a workspace admin and the founder under another key all sign a first entry naming
    /// themselves admin: junk, which starts no stretch. The founder's own entry after them founds.
    #[test]
    fn another_first_entry_is_skipped() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = anchored_by(&alice, &[&alice, &bob], &[&admin]);
      let mut state = ChannelRoleState::default();
      let by_bob = bob.entry(&state, &[(&bob, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, by_bob).len(), 1);
      let by_admin = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, by_admin).len(), 1);
      // A workspace admin's ordinary entry naming somebody else is no founding either.
      let appointing = admin.entry(&state, &[(&bob, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, appointing).len(), 1);
      let other_key = Person::new(1);
      let by_other_key = other_key.entry(&state, &[(&other_key, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, by_other_key).len(), 1);
      assert!(!state.is_signed(), "nothing founded, no stretch");
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);

      let founding = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert!(state.is_workspace_anchored);
    }

    /// A founding that names the anchor's founder only as a moderator, or as admin under another
    /// signer, founds nothing.
    #[test]
    fn a_founding_signed_by_another_or_without_the_founder_as_admin_is_skipped() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = anchored_by(&alice, &[&alice, &bob], &[]);
      let mut state = ChannelRoleState::default();
      let as_moderator = alice.entry(
        &state,
        &[(&alice, ChannelRole::Moderator), (&bob, ChannelRole::Admin)],
      );
      assert_eq!(world.apply(&mut state, as_moderator).len(), 1);
      let by_bob = bob.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert_eq!(world.apply(&mut state, by_bob).len(), 1);
      assert!(!state.is_signed());
    }

    /// The floor doesn't grant (a fork), or the anchor doesn't verify here: nobody founds and
    /// nothing anchors. The entries wait unread, and once the anchor decides they are read.
    #[test]
    fn without_a_founder_from_the_anchor_the_entries_wait() {
      let alice = Person::new(1);
      let mut world = anchored_by(&alice, &[&alice], &[]);
      let anchored = world.founding;
      world.founding = RoleFounding::Withheld;
      let mut state = ChannelRoleState::default();
      let founding = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding.clone()).is_empty());
      assert_eq!((state.version, state.cursor), (0, 0), "read nothing");
      assert!(!state.is_workspace_anchored);
      assert!(state.waits_for_founding(RoleFounding::Withheld));

      world.founding = anchored;
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert!(state.is_workspace_anchored);
    }

    /// Once something is accepted, the anchor decides nothing more: entries are read as ever.
    #[test]
    fn a_withheld_anchor_doesnt_stop_a_signed_log() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let mut world = anchored_by(&alice, &[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      world.founding = RoleFounding::Withheld;
      let appoint = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, appoint).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
    }

    /// A founder's founding that names somebody under another key than this device pinned counts
    /// for the roles, but doesn't anchor, as for a workspace admin's.
    #[test]
    fn a_founding_naming_another_key_than_pinned_doesnt_anchor() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = anchored_by(&alice, &[&alice, &Person::new(2)], &[]);
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
      assert!(!state.is_workspace_anchored);
    }

    /// A device whose anchor names `founder`, pinning `people` and with `roster_admins`.
    fn anchored_by(founder: &Person, people: &[&Person], roster_admins: &[&Person]) -> World {
      let mut world = World::new(people, roster_admins);
      world.founding = RoleFounding::Anchored {
        founder: founder.user_id,
        identity_key: key_bytes(founder),
      };
      world
    }
  }

  mod accepted {
    use super::*;

    #[test]
    fn the_founder_and_then_a_channel_admin_build_the_roles() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let mut state = ChannelRoleState::default();
      let founding = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);

      let promote = alice.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Moderator),
          (&carol, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
      // Carol, made admin, signs the next entry herself.
      let demote = carol.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&carol, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, demote).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      assert_eq!((state.version, state.cursor), (3, 3));
    }

    #[test]
    fn a_list_a_workspace_admin_looked_at_replaces_what_a_device_has() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      // A checkpoint: it names no head, and re-anchors the device.
      let mut stale = state.clone();
      stale.maybe_head_hash = Some(Base64Bytes::new(REVIEWED_PREV_HASH.to_vec()));
      let checkpoint = admin.entry(&stale, &[(&bob, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Member);
    }

    #[test]
    fn a_workspace_admin_founding_a_channel_signs_as_its_founder() {
      let admin = Person::new(9);
      let world = World::new(&[&admin], &[&admin]);
      let mut state = ChannelRoleState::default();
      let founding = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert_eq!(state.role_of(admin.user_id), ChannelRole::Admin);
      assert!(state.is_workspace_anchored, "a roster admin's word, not a claim");
    }

    #[test]
    fn a_founders_claim_is_anchored_only_by_a_workspace_admins_entry_and_stays_so() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      assert!(!state.is_workspace_anchored);
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert!(!state.is_workspace_anchored, "a founded admin adds no trust");

      let checkpoint = admin.reviewed(&state, &[(&bob, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert!(state.is_workspace_anchored);
      let next = bob.entry(
        &state,
        &[(&bob, ChannelRole::Admin), (&alice, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, next).is_empty());
      assert!(state.is_workspace_anchored, "a channel admin carries it on");
    }

    /// e2ee.md §4.6.1, "Founding at first sight": a workspace admin's app that founds a channel from
    /// the server's list saw none of it, so its signature anchors nothing, and nor does its next
    /// role change on that snapshot. A list they looked at does.
    #[test]
    fn roles_from_the_servers_list_anchor_nothing_until_a_workspace_admin_looked() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = ChannelRoleState::default();
      let mut founding = admin.document(
        &state,
        &[(&admin, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      founding.has_server_listed_roles = true;
      assert!(world.apply(&mut state, admin.signed(founding)).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
      assert!(!state.is_workspace_anchored);
      assert!(state.has_server_listed_roles);

      let next = state
        .next_document(
          conversation(CHANNEL),
          &[(alice.user_id, ChannelRole::Moderator)],
          &BTreeMap::from([(alice.user_id, [1; 32])]),
          NOW_MS,
        )
        .unwrap();
      assert!(next.has_server_listed_roles, "the mark carries on");
      assert!(world.apply(&mut state, admin.signed(next)).is_empty());
      assert!(!state.is_workspace_anchored);
      assert_eq!(
        state.appointment_of(alice.user_id),
        None,
        "nothing claimed either"
      );

      let mut reviewed = admin.document(&state, &[(&bob, ChannelRole::Admin)]);
      reviewed.prev_hash = Base64Bytes::new(REVIEWED_PREV_HASH.to_vec());
      assert!(world.apply(&mut state, admin.signed(reviewed)).is_empty());
      assert!(state.is_workspace_anchored);
      assert!(!state.has_server_listed_roles);
    }

    /// A signature on the server's list is never a checkpoint: replayed against a device with a log,
    /// it can't put the server's list in place of the roles there, so it strips nobody.
    #[test]
    fn roles_from_the_servers_list_replace_nothing_a_device_has() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      let before = state.assignments.clone();
      let mut replayed = admin.document(&ChannelRoleState::default(), &[(&bob, ChannelRole::Admin)]);
      replayed.version = state.cursor + 1;
      replayed.has_server_listed_roles = true;
      assert_eq!(world.apply(&mut state, admin.signed(replayed)).len(), 1);
      assert_eq!(state.assignments, before);
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert!(!state.has_server_listed_roles);
    }

    /// Review of #1286, finding 2: only a list somebody looked at replaces what a device has.
    #[test]
    fn an_ordinary_workspace_entry_on_another_head_is_no_checkpoint() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      let before = state.assignments.clone();
      let elsewhere = admin.document(&ChannelRoleState::default(), &[(&bob, ChannelRole::Admin)]);
      let mut entry = elsewhere;
      entry.version = state.cursor + 1;
      assert_eq!(world.apply(&mut state, admin.signed(entry)).len(), 1);
      assert_eq!(state.assignments, before);
      assert!(!state.is_workspace_anchored);

      let mut reviewed = admin.document(&state, &[(&bob, ChannelRole::Admin)]);
      reviewed.prev_hash = Base64Bytes::new(REVIEWED_PREV_HASH.to_vec());
      assert!(world.apply(&mut state, admin.signed(reviewed)).is_empty());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Member, "this one does");
      assert!(state.is_workspace_anchored);
    }

    /// Review of #1286, finding 3: an older client doesn't carry the mark on, and that ends nothing.
    #[test]
    fn an_older_clients_entry_on_marked_roles_neither_anchors_nor_ends_the_mark() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = ChannelRoleState::default();
      let mut founding = alice.document(&state, &[(&alice, ChannelRole::Admin)]);
      founding.has_server_listed_roles = true;
      assert!(world.apply(&mut state, alice.signed(founding)).is_empty());

      let older = admin.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, older).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
      assert!(state.has_server_listed_roles);
      assert!(!state.is_workspace_anchored);
      assert_eq!(state.appointment_of(bob.user_id), None);
    }

    /// Review of #1286, finding 4: the server dating a workspace admin's entry in the future makes
    /// no fresh roster look old, so it strips nobody on this device.
    #[test]
    fn an_entry_dated_in_the_future_starts_no_stretch_on_a_fresh_roster() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      let mut entry = admin.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      entry.created_at = UnixMillis(NOW_MS + 365 * DAY_MS);
      assert!(world.apply(&mut state, entry).is_empty());
      assert!(!state.has_unverified_stretch());
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
    }

    /// Review of #1286, finding 1: every way a device could come by a key, and what it signs.
    mod whose_key_is_signed {
      use super::*;

      const SNAPSHOT: [u8; 32] = [1; 32];
      const PINNED: [u8; 32] = [2; 32];
      const ROSTER: [u8; 32] = [3; 32];

      #[test]
      fn a_first_pin_is_nobodys_word() {
        let evidence = RoleKeyEvidence {
          maybe_pinned: Some(PINNED),
          ..RoleKeyEvidence::default()
        };
        assert_eq!(evidence.checked(), None);
        assert_eq!(evidence.standing(), RoleKeyStanding::Uncheckable);
        assert_eq!(RoleKeyEvidence::default().checked(), None);
      }

      #[test]
      fn the_group_an_attestation_or_the_roster_check_a_key() {
        for evidence in [
          RoleKeyEvidence {
            maybe_pinned: Some(PINNED),
            is_in_group: true,
            ..RoleKeyEvidence::default()
          },
          RoleKeyEvidence {
            maybe_pinned: Some(PINNED),
            is_attested: true,
            ..RoleKeyEvidence::default()
          },
        ] {
          assert_eq!(
            evidence.checked(),
            Some(CheckedRoleKey {
              key: PINNED,
              is_changed: false
            })
          );
        }
        let roster = RoleKeyEvidence {
          maybe_pinned: Some(PINNED),
          maybe_signed: Some(ROSTER),
          ..RoleKeyEvidence::default()
        };
        assert_eq!(roster.checked().map(|found| found.key), Some(ROSTER));
      }

      #[test]
      fn a_person_the_snapshot_names_keeps_its_key_unless_another_is_checked() {
        let unchecked = RoleKeyEvidence {
          maybe_snapshot: Some(SNAPSHOT),
          maybe_pinned: Some(PINNED),
          ..RoleKeyEvidence::default()
        };
        assert_eq!(
          unchecked.checked(),
          Some(CheckedRoleKey {
            key: SNAPSHOT,
            is_changed: true
          }),
          "the server's swap is marked and not signed"
        );
        let checked = RoleKeyEvidence {
          is_in_group: true,
          ..unchecked
        };
        assert_eq!(
          checked.checked(),
          Some(CheckedRoleKey {
            key: PINNED,
            is_changed: true
          }),
          "a real new key, which the group holds, marked for whoever looks"
        );
        let same = RoleKeyEvidence {
          maybe_snapshot: Some(SNAPSHOT),
          maybe_pinned: Some(SNAPSHOT),
          ..RoleKeyEvidence::default()
        };
        assert_eq!(same.standing(), RoleKeyStanding::Checked);
      }
    }

    /// Third review of #1286: a founder and the server made the roles up; a workspace admin changes
    /// one, and every key in it checks. Only a list they looked at anchors that.
    #[test]
    fn a_workspace_admins_ordinary_entry_anchors_nothing_new() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &bob, &[]);
      let ordinary = admin.entry(
        &state,
        &[(&bob, ChannelRole::Admin), (&alice, ChannelRole::Moderator)],
      );
      assert!(world.apply(&mut state, ordinary).is_empty());
      assert_eq!(
        state.role_of(alice.user_id),
        ChannelRole::Moderator,
        "it counts for roles"
      );
      assert!(!state.is_workspace_anchored);

      let mut reviewed = admin.document(&state, &[(&alice, ChannelRole::Admin)]);
      reviewed.prev_hash = Base64Bytes::new(REVIEWED_PREV_HASH.to_vec());
      assert!(world.apply(&mut state, admin.signed(reviewed)).is_empty());
      assert!(state.is_workspace_anchored);
      let next = admin.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert!(world.apply(&mut state, next).is_empty());
      assert!(state.is_workspace_anchored, "on an anchor it carries on");
    }

    /// A workspace admin's own founding of a channel with nothing signed anchors, as it names what
    /// they chose.
    #[test]
    fn a_workspace_admins_own_founding_anchors() {
      let admin = Person::new(9);
      let world = World::new(&[&admin], &[&admin]);
      let mut state = ChannelRoleState::default();
      let founding = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, founding).is_empty());
      assert!(state.is_workspace_anchored);
    }

    /// Third review of #1286: a device told of a log it isn't shown knows there is one.
    #[test]
    fn only_a_device_told_of_no_log_acts_as_on_an_unsigned_channel() {
      assert!(ChannelRoleState::default().may_act_as_unsigned());
      let junk_only = ChannelRoleState {
        cursor: 1,
        ..ChannelRoleState::default()
      };
      assert!(
        junk_only.may_act_as_unsigned(),
        "the repair for a junk entry stays"
      );
      let head = ChannelRolesHead {
        version: 3,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      };
      assert!(
        !ChannelRoleState {
          maybe_ahead: Some(head),
          ..ChannelRoleState::default()
        }
        .may_act_as_unsigned()
      );
      assert!(
        !ChannelRoleState {
          maybe_alarm: Some("held back".into()),
          ..ChannelRoleState::default()
        }
        .may_act_as_unsigned()
      );
    }

    #[test]
    fn only_a_device_that_saw_no_log_and_no_head_takes_a_channel_for_unfounded() {
      assert!(ChannelRoleState::default().is_unfounded());
      let head = ChannelRolesHead {
        version: 3,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      };
      for state in [
        ChannelRoleState {
          version: 1,
          cursor: 1,
          ..ChannelRoleState::default()
        },
        // It looked at entries and took none: there is a log, whoever wrote it.
        ChannelRoleState {
          cursor: 1,
          ..ChannelRoleState::default()
        },
        ChannelRoleState {
          cursor: 1,
          maybe_unverified_from: Some(1),
          ..ChannelRoleState::default()
        },
        // A member's message named a head the server doesn't show.
        ChannelRoleState {
          maybe_ahead: Some(head.clone()),
          ..ChannelRoleState::default()
        },
        ChannelRoleState {
          maybe_alarm: Some("held back".into()),
          ..ChannelRoleState::default()
        },
      ] {
        assert!(!state.is_unfounded(), "{state:?}");
      }
    }
  }

  mod anchoring {
    use super::*;

    /// A demotion withheld past the roster's age, and an entry the server dates back to when the
    /// roster was fresh: accepted for roles as before, but it anchors nothing, and takes away what
    /// was anchored, since it replaces the snapshot.
    #[test]
    fn a_backdated_entry_on_a_roster_too_old_now_anchors_nothing() {
      let (alice, mallory, eve) = (Person::new(1), Person::new(2), Person::new(9));
      let mut world = World::new(&[&alice, &mallory], &[&eve]);
      let mut state = founded(&world, &eve, &[(&alice, ChannelRole::Admin)]);
      assert!(state.is_workspace_anchored);

      world.now_ms = NOW_MS + MAX_AGE_MS + 1;
      let mut checkpoint = eve.entry(
        &state,
        &[
          (&eve, ChannelRole::Admin),
          (&alice, ChannelRole::Admin),
          (&mallory, ChannelRole::Admin),
        ],
      );
      checkpoint.created_at = UnixMillis(NOW_MS);
      assert!(world.apply(&mut state, checkpoint).is_empty());

      assert_eq!(state.role_of(mallory.user_id), ChannelRole::Admin);
      assert!(!state.is_workspace_anchored);
    }

    /// Other uses take an unchained roster with no signing time as fresh. An anchor doesn't.
    #[test]
    fn a_roster_without_a_signing_time_anchors_nothing() {
      let (alice, admin) = (Person::new(1), Person::new(9));
      let mut world = World::new(&[&alice], &[&admin]);
      world.workspace.heads.is_roster_chained = false;
      world.workspace.heads.maybe_roster_signed_at_ms = None;
      let mut state = founded(&world, &alice, &[]);
      let checkpoint = admin.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(state.version, 2);
      assert!(!state.is_workspace_anchored);
    }
  }

  mod refused {
    use super::*;

    #[test]
    fn moderators_members_and_strangers_sign_nothing() {
      let (alice, bob, carol, stranger) = (Person::new(1), Person::new(2), Person::new(3), Person::new(4));
      let world = World::new(&[&alice, &bob, &carol, &stranger], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      for signer in [&bob, &carol, &stranger] {
        let entry = signer.entry(
          &state,
          &[(&alice, ChannelRole::Admin), (&carol, ChannelRole::Admin)],
        );
        let skipped = world.apply(&mut state, entry);
        assert!(
          matches!(skipped.as_slice(), [ChannelRoleError::Unauthorized { .. }]),
          "{skipped:?}"
        );
      }
      assert_eq!(state.role_of(carol.user_id), ChannelRole::Member);
      assert_eq!(state.version, 1);
    }

    #[test]
    fn unsigned_tampered_and_misplaced_entries_are_skipped() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let state = founded(&world, &alice, &[]);
      let changes = [(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)];

      let mut unsigned = alice.entry(&state, &changes);
      unsigned.signature = Base64Bytes::new(Vec::new());
      let mut tampered = alice.entry(&state, &changes);
      let mut document: ChannelRolesDocument = serde_json::from_slice(tampered.payload.as_slice()).unwrap();
      document
        .assignments
        .retain(|assignment| assignment.user_id != alice.user_id);
      tampered.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
      // Signed for the roster log, not for a channel's role log.
      let mut other_log = alice.entry(&state, &changes);
      other_log.signature = Base64Bytes::new(
        sign_log_entry(
          &alice.device,
          &alice.certificate,
          WorkspaceLog::Roster,
          other_log.version,
          other_log.payload.as_slice(),
        )
        .unwrap(),
      );
      for entry in [unsigned, tampered, other_log] {
        let mut probe = state.clone();
        assert_eq!(world.apply(&mut probe, entry).len(), 1);
        assert_eq!(probe.role_of(bob.user_id), ChannelRole::Member);
        assert_eq!(probe.version, 1);
      }
      // A valid entry of another channel is refused here.
      let entry = alice.entry_in(CHANNEL + 1, &state, &changes);
      let mut probe = state.clone();
      assert!(matches!(
        world.apply(&mut probe, entry).as_slice(),
        [ChannelRoleError::OtherChannel { .. }]
      ));
    }

    #[test]
    fn a_founder_claim_with_a_key_this_device_did_not_pin_is_refused() {
      // The server made up a key for Alice and signed with it: not the key pinned for her.
      let alice = Person::new(1);
      let impostor = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let mut state = ChannelRoleState::default();
      let forged = impostor.entry(&state, &[(&impostor, ChannelRole::Admin)]);
      assert!(matches!(
        world.apply(&mut state, forged).as_slice(),
        [ChannelRoleError::Unauthorized { .. }]
      ));
      assert!(!state.is_signed());
    }

    #[test]
    fn nobody_founds_a_channel_twice() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      // Bob claims the channel as if nothing was signed.
      let unsigned = ChannelRoleState {
        cursor: state.cursor,
        ..ChannelRoleState::default()
      };
      let claim = bob.entry(&unsigned, &[(&bob, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, claim).len(), 1);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
    }

    #[test]
    fn role_rules_hold_for_channel_admins() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      // Bob changes his own role.
      let own = bob.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert!(matches!(
        world.apply(&mut state.clone(), own).as_slice(),
        [ChannelRoleError::RoleRule {
          rule: RoleChangeError::OwnRole,
          ..
        }]
      ));
      // Nobody left as admin.
      let mut none = bob.document(&state, &[(&bob, ChannelRole::Admin)]);
      none.assignments.clear();
      assert!(matches!(
        world.apply(&mut state.clone(), bob.signed(none)).as_slice(),
        [ChannelRoleError::Snapshot { .. }]
      ));
      // A role this version doesn't know.
      let mut unknown = bob.document(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      unknown.assignments.push(ChannelRoleAssignment {
        user_id: carol.user_id,
        role: ChannelRole::Unknown("owner".into()),
        identity_key: carol.key(),
      });
      assert!(matches!(
        world.apply(&mut state.clone(), bob.signed(unknown)).as_slice(),
        [ChannelRoleError::Snapshot { .. }]
      ));
    }

    #[test]
    fn a_stale_roster_gives_workspace_admins_no_authority() {
      let (alice, admin) = (Person::new(1), Person::new(9));
      let mut world = World::new(&[&alice], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      world.workspace.heads.maybe_roster_signed_at_ms = Some(NOW_MS - MAX_AGE_MS - 1);
      let entry = admin.entry(&state, &[(&admin, ChannelRole::Admin)]);
      assert_eq!(world.apply(&mut state, entry).len(), 1);
      assert_eq!(state.role_of(admin.user_id), ChannelRole::Member);
    }
  }

  mod replay_and_rollback {
    use super::*;

    #[test]
    fn an_old_entry_again_or_at_another_version_changes_nothing() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, promote.clone()).is_empty());
      let demote = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, demote).is_empty());
      // The server serves the promotion again: at or below the cursor, ignored.
      assert!(world.apply(&mut state, promote.clone()).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      // At a new position: the version is signed, so it doesn't verify there.
      let mut moved = promote;
      moved.version = state.cursor + 1;
      assert_eq!(world.apply(&mut state, moved).len(), 1);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      assert_eq!(state.version, 3);
    }

    #[test]
    fn a_junk_entry_does_not_block_the_next_valid_one() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let mut junk = bob.entry(&state, &[(&bob, ChannelRole::Admin)]);
      junk.payload = Base64Bytes::new(b"not json".to_vec());
      assert_eq!(world.apply(&mut state, junk).len(), 1);
      // Alice built on the last entry she accepted, the founding one.
      let valid = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert_eq!(valid.version, 3);
      assert!(world.apply(&mut state, valid).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
    }

    #[test]
    fn a_channel_admin_entry_built_on_another_head_is_refused() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let mut forked = state.clone();
      forked.maybe_head_hash = Some(Base64Bytes::new(vec![5; 32]));
      let entry = bob.entry(
        &forked,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(matches!(
        world.apply(&mut state, entry).as_slice(),
        [ChannelRoleError::BrokenChain { .. }]
      ));
    }

    #[test]
    fn diverging_and_withheld_heads_raise_an_alarm() {
      let alice = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let mut state = founded(&world, &alice, &[]);
      let own = state.head().unwrap();
      state.note_head(&own);
      assert_eq!(state.maybe_alarm, None);
      state.note_head(&ChannelRolesHead {
        version: own.version,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      });
      assert!(state.maybe_alarm.take().is_some(), "a fork");
      state.note_head(&ChannelRolesHead {
        version: own.version + 1,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      });
      assert!(state.maybe_alarm.is_none());
      state.check_ahead();
      assert!(state.maybe_alarm.is_some(), "entries this device isn't shown");
    }
  }

  /// Entries members send in the channel (e2ee.md §4.6.3, part 1): a demotion the log leaves out
  /// still reaches the members, and the log leaving it out is proof.
  mod entries_in_messages {
    use super::*;

    #[test]
    fn a_withheld_demotion_applies_and_the_log_leaving_it_out_is_a_proven_alarm() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let demotion = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert_eq!(world.take(&mut state, &demotion), MessageEntry::Accepted);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
      assert_eq!(state.maybe_alarm, None, "nothing is wrong yet");
      assert_eq!(state.log_read_from(), 1, "the next read shows version 2 again");
      // The server's log, read to its end, stops at the founding.
      state.compare_log(&[], true);
      assert!(state.is_alarm_proven);
      assert!(state.maybe_alarm.is_some());
      assert_eq!(
        state.role_of(bob.user_id),
        ChannelRole::Member,
        "the demotion holds"
      );
    }

    #[test]
    fn a_log_showing_the_same_entry_confirms_it() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let demotion = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert_eq!(world.take(&mut state, &demotion), MessageEntry::Accepted);
      // A log not read to its end yet says nothing.
      state.compare_log(&[], false);
      assert_eq!(state.unconfirmed.len(), 1);
      state.compare_log(std::slice::from_ref(&demotion), true);
      assert!(state.unconfirmed.is_empty());
      assert_eq!(
        (state.maybe_alarm.as_deref(), state.is_alarm_proven),
        (None, false)
      );
      // The read applies nothing twice.
      assert!(world.apply(&mut state, demotion).is_empty());
      assert_eq!(state.version, 2);
    }

    #[test]
    fn a_log_showing_another_entry_at_that_version_is_a_proven_alarm() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let demotion = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      let bobs = bob.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Admin),
          (&carol, ChannelRole::Admin),
        ],
      );
      assert_eq!(world.take(&mut state, &demotion), MessageEntry::Accepted);
      state.compare_log(&[bobs], true);
      assert!(state.is_alarm_proven);
      assert_eq!(
        state.role_of(carol.user_id),
        ChannelRole::Member,
        "the first one seen stays"
      );
    }

    /// The server shows this device Bob's entry and the members' messages carry Alice's demotion of
    /// him, both valid for version 2.
    #[test]
    fn two_valid_entries_for_one_version_are_a_proven_alarm_and_the_first_stays() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let demotion = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      let bobs = bob.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Admin),
          (&carol, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, bobs.clone()).is_empty());
      assert_eq!(world.take(&mut state, &demotion), MessageEntry::Fork);
      assert!(state.is_alarm_proven);
      let alarm = state.maybe_alarm.clone().unwrap();
      assert!(
        alarm.contains(&alice.user_id.to_string()) && alarm.contains(&bob.user_id.to_string()),
        "the reviewer sees who signed both: {alarm}"
      );
      assert_eq!(state.head().unwrap().hash, entry_hash(&bobs));
      assert_eq!(state.role_of(carol.user_id), ChannelRole::Admin);
      assert_eq!(world.take(&mut state, &bobs), MessageEntry::Known);
    }

    /// Anything a member could make up changes nothing and raises nothing, so the real entry at that
    /// version still applies from the log.
    #[test]
    fn forged_unsigned_and_unentitled_entries_change_nothing() {
      let (alice, bob, mallory) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &mallory], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let before = state.clone();
      let by_a_member = mallory.entry(&state, &[(&mallory, ChannelRole::Admin)]);
      let mut tampered = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      let mut document: ChannelRolesDocument = serde_json::from_slice(tampered.payload.as_slice()).unwrap();
      document.assignments[0].user_id = mallory.user_id;
      tampered.payload = Base64Bytes::new(serde_json::to_vec(&document).unwrap());
      let mut unsigned = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      unsigned.signature = Base64Bytes::new(vec![0; 64]);
      let elsewhere = alice.entry_in(CHANNEL + 1, &state, &[(&alice, ChannelRole::Admin)]);
      for entry in [&by_a_member, &tampered, &unsigned, &elsewhere] {
        assert_eq!(world.take(&mut state, entry), MessageEntry::Ignored);
        assert_eq!(state, before);
      }
      // A forged entry for the version this device holds isn't a fork either.
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      let accepted = state.clone();
      let mut forged = mallory.entry(&before, &[(&mallory, ChannelRole::Admin)]);
      forged.version = state.version;
      assert_eq!(world.take(&mut state, &forged), MessageEntry::Ignored);
      assert_eq!(state, accepted);
    }

    #[test]
    fn an_older_entry_again_is_ignored() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, promote.clone()).is_empty());
      let demote = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, demote).is_empty());
      let before = state.clone();
      assert_eq!(world.take(&mut state, &promote), MessageEntry::Ignored);
      assert_eq!(state, before);
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Member);
    }

    /// A device offline for weeks reads a later entry first: it reads the log, without an alarm.
    #[test]
    fn a_device_behind_reads_the_log_and_catches_up_quietly() {
      let (alice, bob, carol) = (Person::new(1), Person::new(2), Person::new(3));
      let world = World::new(&[&alice, &bob, &carol], &[]);
      let behind = founded(&world, &alice, &[]);
      let mut ahead = behind.clone();
      let promote = alice.entry(
        &ahead,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut ahead, promote.clone()).is_empty());
      let carol_too = alice.entry(
        &ahead,
        &[
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Admin),
          (&carol, ChannelRole::Moderator),
        ],
      );
      let mut state = behind;
      assert_eq!(world.take(&mut state, &carol_too), MessageEntry::Ahead);
      assert!(state.maybe_ahead.is_some());
      assert_eq!(state.maybe_alarm, None);
      let mut read = state.clone();
      assert!(world.apply_all(&mut read, vec![promote, carol_too]).is_empty());
      read.check_ahead();
      assert_eq!((read.maybe_alarm.as_deref(), read.version), (None, 3));
      assert_eq!(read.role_of(carol.user_id), ChannelRole::Moderator);
    }

    /// A server can get a second founding signed by itself: foundings never travel in messages.
    #[test]
    fn foundings_are_skipped_without_an_alarm() {
      let (alice, dave) = (Person::new(1), Person::new(4));
      let world = World::new(&[&alice, &dave], &[]);
      let mut unsigned = ChannelRoleState::default();
      let founding = alice.entry(&unsigned, &[(&alice, ChannelRole::Admin)]);
      assert_eq!(world.take(&mut unsigned, &founding), MessageEntry::Ignored);
      assert!(unsigned.is_unfounded());
      let mut state = founded(&world, &alice, &[]);
      let mut other = dave.document(&ChannelRoleState::default(), &[(&dave, ChannelRole::Admin)]);
      other.has_server_listed_roles = true;
      let second = dave.signed(other);
      assert_eq!(world.take(&mut state, &second), MessageEntry::Ignored);
      assert_eq!(
        (state.maybe_alarm.as_deref(), state.is_alarm_proven),
        (None, false)
      );
    }

    /// A member's head is their word: it raises the line, never a proven alarm.
    #[test]
    fn a_claimed_head_is_no_proof() {
      let alice = Person::new(1);
      let world = World::new(&[&alice], &[]);
      let mut state = founded(&world, &alice, &[]);
      state.note_head(&ChannelRolesHead {
        version: state.version,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      });
      assert!(state.maybe_alarm.is_some());
      assert!(!state.is_alarm_proven);
    }

    #[test]
    fn only_a_workspace_admins_reviewed_list_ends_a_proven_alarm() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      let demotion = alice.entry(&state, &[(&alice, ChannelRole::Admin)]);
      assert_eq!(world.take(&mut state, &demotion), MessageEntry::Accepted);
      state.compare_log(&[], true);
      assert!(state.is_alarm_proven);
      let ordinary = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      assert!(world.apply(&mut state, ordinary).is_empty());
      assert!(state.is_alarm_proven, "an ordinary entry doesn't end it");
      let reviewed = admin.reviewed(&state, &[(&alice, ChannelRole::Admin)]);
      assert!(world.apply(&mut state, reviewed).is_empty());
      assert_eq!(
        (state.maybe_alarm.as_deref(), state.is_alarm_proven),
        (None, false)
      );
    }

    /// A demoted admin on a withheld branch can't keep the purge's limit open through somebody
    /// they appointed, directly or through others (review of #1416).
    #[test]
    fn signers_whose_role_goes_back_to_the_deleter_dont_count() {
      let (alice, mallory, eve, frank, admin) = (
        Person::new(1),
        Person::new(2),
        Person::new(5),
        Person::new(6),
        Person::new(9),
      );
      let world = World::new(&[&alice, &mallory, &eve, &frank], &[&admin]);
      let mut state = ChannelRoleState::default();
      let founding = admin.reviewed(
        &state,
        &[(&alice, ChannelRole::Admin), (&mallory, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, founding).is_empty());
      let eve_by_mallory = mallory.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&mallory, ChannelRole::Admin),
          (&eve, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, eve_by_mallory).is_empty());
      let frank_by_eve = eve.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&mallory, ChannelRole::Admin),
          (&eve, ChannelRole::Admin),
          (&frank, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, frank_by_eve).is_empty());
      state.recent_signers = [&eve, &frank]
        .iter()
        .map(|person| RecentSigner {
          user_id: person.user_id,
          from_ms: NOW_MS,
        })
        .collect();
      assert!(!state.has_other_signer_since(mallory.user_id, NOW_MS));
      assert!(
        state.has_other_signer_since(alice.user_id, NOW_MS),
        "not Alice's appointees"
      );
      state.recent_signers.push(RecentSigner {
        user_id: admin.user_id,
        from_ms: NOW_MS,
      });
      assert!(state.has_other_signer_since(mallory.user_id, NOW_MS));
    }

    /// The start of the purge's limit: the signer's time, never later than this device's.
    #[test]
    fn signers_are_recorded_from_their_signing_time_capped_at_this_devices() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let mut world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[(&bob, ChannelRole::Admin)]);
      world.now_ms = NOW_MS - DAY_MS;
      let mut document = bob.document(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      document.signed_at_ms = NOW_MS + 5 * DAY_MS;
      assert!(world.apply(&mut state, bob.signed(document)).is_empty());
      assert!(
        state.has_other_signer_since(bob.user_id, NOW_MS),
        "Alice founded it"
      );
      assert!(!state.has_other_signer_since(bob.user_id, NOW_MS + 1));
      // Somebody else deleting: Bob's entry counts from this device's clock, not his date.
      let other = user(7);
      state
        .recent_signers
        .retain(|recent| recent.user_id == bob.user_id);
      assert!(state.has_other_signer_since(other, NOW_MS - DAY_MS));
      assert!(!state.has_other_signer_since(other, NOW_MS - DAY_MS + 1));
    }

    /// Issue #1264, option 1B: a workspace admin's device may have been shown an old log with a
    /// demotion withheld. Its ordinary entry still carries the anchor, but doesn't let the demoted
    /// admin purge at once. A list they looked at does.
    #[test]
    fn a_workspace_admins_ordinary_entry_keeps_the_anchor_but_not_the_purge_open() {
      let (alice, bob, eve) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&eve]);
      let mut state = founded(&world, &eve, &[(&alice, ChannelRole::Admin)]);
      assert!(state.is_workspace_anchored);
      assert!(
        state.has_other_signer_since(alice.user_id, NOW_MS),
        "her own founding counts"
      );
      state.recent_signers.clear();

      let promote = eve.entry(
        &state,
        &[
          (&eve, ChannelRole::Admin),
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Moderator),
        ],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert!(state.is_workspace_anchored, "the anchor carries on");
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
      assert!(state.recent_signers.is_empty(), "{:?}", state.recent_signers);
      assert!(!state.has_other_signer_since(alice.user_id, NOW_MS));

      let reviewed = eve.reviewed(
        &state,
        &[(&eve, ChannelRole::Admin), (&alice, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, reviewed).is_empty());
      assert!(state.has_other_signer_since(alice.user_id, NOW_MS));
    }

    /// The same for a workspace admin who is also a channel admin: decided by who signed, not by
    /// the authority the entry passed under.
    #[test]
    fn a_workspace_admin_who_is_a_channel_admin_counts_only_for_a_reviewed_list() {
      let (alice, bob, eve) = (Person::new(1), Person::new(2), Person::new(9));
      let mut world = World::new(&[&alice, &bob, &eve], &[&eve]);
      let mut state = founded(&world, &alice, &[(&eve, ChannelRole::Admin)]);
      state.recent_signers.clear();
      // Dated where the roster is too old to vouch for Eve: the entry passes as a channel admin's.
      world.now_ms = NOW_MS + MAX_AGE_MS + 1;
      let mut promote = eve.entry(
        &state,
        &[
          (&alice, ChannelRole::Admin),
          (&eve, ChannelRole::Admin),
          (&bob, ChannelRole::Moderator),
        ],
      );
      promote.created_at = UnixMillis(world.now_ms);
      assert!(world.apply(&mut state, promote).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Moderator);
      assert!(!state.has_other_signer_since(alice.user_id, NOW_MS));
    }
  }

  mod review_requests {
    use super::*;

    #[test]
    fn the_flag_is_left_out_when_false_and_absent_reads_as_false() {
      let head = ChannelRolesHead {
        version: 3,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: false,
      };
      let json = serde_json::to_value(&head).unwrap();
      assert_eq!(json.get("needs_review"), None, "older clients see what they saw");
      let read: ChannelRolesHead = serde_json::from_value(json).unwrap();
      assert_eq!(read, head);

      let flagged = ChannelRolesHead {
        needs_review: true,
        ..head.clone()
      };
      let json = serde_json::to_value(&flagged).unwrap();
      assert_eq!(json["needs_review"], serde_json::Value::Bool(true));
      assert_eq!(serde_json::from_value::<ChannelRolesHead>(json).unwrap(), flagged);
    }

    /// A newer client may add more: an older one reads the head as before.
    #[test]
    fn unknown_fields_in_a_head_are_ignored() {
      let mut json = serde_json::to_value(ChannelRolesHead {
        version: 3,
        hash: Base64Bytes::new(vec![1; 32]),
        needs_review: true,
      })
      .unwrap();
      json["some_later_field"] = serde_json::json!({ "x": 1 });
      let read: ChannelRolesHead = serde_json::from_value(json).unwrap();
      assert_eq!((read.version, read.needs_review), (3, true));
    }

    #[test]
    fn a_stuck_device_marks_the_head_it_sends_and_a_newer_head_alone_does_not() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      assert!(!state.head_to_send().unwrap().needs_review);
      assert!(!state.head().unwrap().needs_review);

      state.note_head(&ChannelRolesHead {
        version: state.version + 1,
        hash: Base64Bytes::new(vec![7; 32]),
        needs_review: false,
      });
      assert!(state.maybe_ahead.is_some());
      assert!(
        !state.head_to_send().unwrap().needs_review,
        "the next read settles it"
      );

      state.check_ahead();
      assert!(state.maybe_alarm.is_some());
      assert!(state.head_to_send().unwrap().needs_review);
      assert!(!state.head().unwrap().needs_review, "only the sent head asks");

      let mut stretched = founded(&world, &alice, &[]);
      stretched.maybe_unverified_from = Some(2);
      assert!(stretched.needs_review());
      let mut late = founded(&world, &alice, &[]);
      late.has_former_admin_entry = true;
      assert!(late.needs_review());
    }

    /// A workspace admin's device takes a request until a reviewed list; a request in a message the
    /// server dated before that list stays answered, one after it asks again (also when the server
    /// keeps the list from the stuck device, whose head then stays below it).
    #[test]
    fn a_request_stands_until_a_reviewed_list_and_a_later_message_asks_again() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &admin, &[(&alice, ChannelRole::Admin)]);
      state.note_review_request(bob.user_id, NOW_MS - 10);
      assert!(state.is_review_asked());

      let reviewed = admin.reviewed(
        &state,
        &[(&admin, ChannelRole::Admin), (&alice, ChannelRole::Admin)],
      );
      let reviewed_at = reviewed.created_at.0;
      assert!(world.apply(&mut state, reviewed).is_empty());
      assert!(!state.is_review_asked());
      assert_eq!(state.maybe_reviewed_at_ms, Some(reviewed_at));

      state.note_review_request(bob.user_id, reviewed_at - 5);
      state.note_review_request(bob.user_id, reviewed_at);
      assert!(!state.is_review_asked(), "sent before the list");
      state.note_review_request(bob.user_id, reviewed_at + 1);
      assert!(state.is_review_asked(), "still stuck after it");

      let stored: ChannelRoleState = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
      assert_eq!(stored.review_requests, state.review_requests);
      assert_eq!(stored.maybe_reviewed_at_ms, Some(reviewed_at));
    }

    /// A server dating a reviewed list ahead can't silence the requests after it: the date is
    /// capped at this device's clock.
    #[test]
    fn a_reviewed_list_dated_ahead_counts_from_this_devices_now() {
      let (alice, admin) = (Person::new(1), Person::new(9));
      let world = World::new(&[&alice], &[&admin]);
      let mut state = founded(&world, &admin, &[(&alice, ChannelRole::Admin)]);
      let mut reviewed = admin.reviewed(
        &state,
        &[(&admin, ChannelRole::Admin), (&alice, ChannelRole::Admin)],
      );
      reviewed.created_at = UnixMillis(world.now_ms + 365 * DAY_MS);
      assert!(world.apply(&mut state, reviewed).is_empty());
      assert_eq!(state.maybe_reviewed_at_ms, Some(world.now_ms));
      state.note_review_request(alice.user_id, world.now_ms + 1);
      assert!(state.is_review_asked());
    }

    /// One standing request per sender, the newest; a bounded list however many ask.
    #[test]
    fn requests_are_one_per_sender_and_bounded() {
      let mut state = ChannelRoleState::default();
      state.note_review_request(user(2), NOW_MS);
      state.note_review_request(user(2), NOW_MS + 5);
      state.note_review_request(user(2), NOW_MS + 1);
      assert_eq!(
        state.review_requests,
        vec![ReviewRequest {
          user_id: user(2),
          sent_at_ms: NOW_MS + 5,
        }]
      );
      for raw in 10..(10 + MAX_REVIEW_REQUESTS as i64 + 6) {
        state.note_review_request(user(raw), NOW_MS + raw);
      }
      assert_eq!(state.review_requests.len(), MAX_REVIEW_REQUESTS);
      assert!(
        !state
          .review_requests
          .iter()
          .any(|request| request.user_id == user(2)),
        "the oldest go"
      );
    }

    /// A head's flag alone changes nothing in the state: the client takes it with the message's
    /// sender and server time.
    #[test]
    fn a_flagged_head_is_compared_as_any_head() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let before = state.clone();
      let mut head = state.head().unwrap();
      head.needs_review = true;
      state.note_head(&head);
      assert_eq!(state, before);
    }
  }

  mod legacy_recent_signers {
    use super::*;

    /// Issue #1264, option 1B, for a state stored before it: workspace admins it recorded, current
    /// or former, are dropped once; others stay, and nothing is dropped again.
    #[test]
    fn a_state_from_before_drops_workspace_admins_once() {
      let story = Story::new();
      let mut world = story.world();
      story.eve_leaves(&mut world);
      let state = ChannelRoleState {
        recent_signers: [&story.frank, &story.eve, &story.bob]
          .iter()
          .map(|person| RecentSigner {
            user_id: person.user_id,
            from_ms: NOW_MS,
          })
          .collect(),
        ..ChannelRoleState::default()
      };
      let mut stored = serde_json::to_value(&state).unwrap();
      stored
        .as_object_mut()
        .unwrap()
        .remove("has_legacy_recent_signers");
      let mut legacy: ChannelRoleState = serde_json::from_value(stored).unwrap();
      assert!(legacy.has_legacy_recent_signers);

      legacy.forget_legacy_recent_signers(&world.workspace);
      let kept: Vec<UserId> = legacy
        .recent_signers
        .iter()
        .map(|recent| recent.user_id)
        .collect();
      assert_eq!(kept, vec![story.bob.user_id], "Frank is an admin, Eve was one");
      assert!(!legacy.has_legacy_recent_signers);

      legacy.recent_signers = state.recent_signers.clone();
      legacy.forget_legacy_recent_signers(&world.workspace);
      assert_eq!(legacy.recent_signers.len(), 3, "once only");
      let reread: ChannelRoleState = serde_json::from_value(serde_json::to_value(&legacy).unwrap()).unwrap();
      assert!(!reread.has_legacy_recent_signers);
    }

    #[test]
    fn a_new_state_is_no_legacy_state() {
      let state = ChannelRoleState::default();
      assert!(!state.has_legacy_recent_signers);
      let reread: ChannelRoleState = serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
      assert!(!reread.has_legacy_recent_signers);
    }
  }

  mod legacy_and_effective_roles {
    use super::*;

    #[test]
    fn the_server_takes_roles_away_but_never_grants_them() {
      use ChannelRole::{Admin, Member, Moderator};
      for (verified, server, is_guest, effective) in [
        (Admin, Admin, false, Admin),
        (Admin, Member, false, Member),
        (Member, Admin, false, Member),
        (Moderator, Admin, false, Moderator),
        (Admin, Admin, true, Member),
      ] {
        assert_eq!(
          effective_role(&verified, &server, is_guest),
          effective,
          "{verified:?} / {server:?} / guest {is_guest}"
        );
      }
    }

    #[test]
    fn a_channel_without_a_signed_entry_grants_nobody_a_role() {
      let state = ChannelRoleState::default();
      assert!(!state.is_signed());
      assert_eq!(state.role_of(user(1)), ChannelRole::Member);
      assert_eq!(
        effective_role(&state.role_of(user(1)), &ChannelRole::Admin, false),
        ChannelRole::Member
      );
    }

    #[test]
    fn who_may_sign_follows_the_verified_roles() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let unsigned = ChannelRoleState::default();
      assert_eq!(
        unsigned.signing_authorities(bob.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::Founder]
      );
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      assert!(
        state
          .signing_authorities(bob.user_id, &world.workspace, NOW_MS, MAX_AGE_MS)
          .is_empty()
      );
      assert_eq!(
        state.signing_authorities(alice.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::ChannelAdmin]
      );
      assert_eq!(
        state.signing_authorities(admin.user_id, &world.workspace, NOW_MS, MAX_AGE_MS),
        vec![SignerAuthority::Workspace { is_owner: false }]
      );
    }
  }

  mod appointers {
    use super::*;

    #[test]
    fn a_founders_own_claim_is_no_appointment() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let state = founded(&world, &alice, &[(&bob, ChannelRole::Moderator)]);
      assert_eq!(state.appointment_of(alice.user_id), None);
      // The founding entry also named Bob, but a founder's entry is trust on first use too.
      assert_eq!(state.appointment_of(bob.user_id), None);
    }

    #[test]
    fn a_channel_admin_is_not_named_where_nothing_goes_back_to_a_workspace_admin() {
      let (alice, bob) = (Person::new(1), Person::new(2));
      let world = World::new(&[&alice, &bob], &[]);
      let mut state = founded(&world, &alice, &[]);
      let promote = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert!(!state.is_workspace_anchored);
      assert_eq!(
        state.appointment_of(bob.user_id),
        None,
        "a founder's claim carried on"
      );
    }

    #[test]
    fn a_channel_admin_is_named_in_an_anchored_channel_for_the_roles_they_changed_only() {
      let (alice, bob, carol, admin) = (Person::new(1), Person::new(2), Person::new(3), Person::new(9));
      let world = World::new(&[&alice, &bob, &carol], &[&admin]);
      let mut state = founded(
        &world,
        &admin,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Moderator)],
      );
      let promote = alice.entry(
        &state,
        &[
          (&admin, ChannelRole::Admin),
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Moderator),
          (&carol, ChannelRole::Admin),
        ],
      );
      assert!(world.apply(&mut state, promote).is_empty());
      assert!(state.is_workspace_anchored);
      let found = state.appointment_of(carol.user_id).expect("appointed");
      assert_eq!(
        (found.by, found.authority),
        (alice.user_id, AppointmentAuthority::ChannelAdmin)
      );
      let kept = state.appointment_of(bob.user_id).expect("not re-appointed");
      assert_eq!(
        (kept.by, kept.authority),
        (admin.user_id, AppointmentAuthority::WorkspaceAdmin)
      );
    }

    #[test]
    fn a_workspace_admin_is_named_and_the_appointment_outlives_later_entries() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(&world, &alice, &[]);
      let appoint = admin.reviewed(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, appoint).is_empty());
      let found = state.appointment_of(bob.user_id).expect("appointed").clone();
      assert_eq!(
        (found.by, found.authority),
        (admin.user_id, AppointmentAuthority::WorkspaceAdmin)
      );

      // Alice, a channel admin, signs again without changing Bob: nothing changes for Bob.
      let more = alice.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, more).is_empty());
      assert_eq!(state.appointment_of(bob.user_id), Some(&found));
    }

    #[test]
    fn a_workspace_owner_is_named_as_an_owner() {
      let (alice, bob, owner) = (Person::new(1), Person::new(2), Person::new(9));
      let mut world = World::new(&[&alice, &bob], &[&owner]);
      world.workspace.roster.members[0].role = RosterRole::Owner;
      let mut state = founded(&world, &alice, &[]);
      let appoint = owner.reviewed(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, appoint).is_empty());
      assert_eq!(
        state.appointment_of(bob.user_id).map(|found| found.authority),
        Some(AppointmentAuthority::WorkspaceOwner)
      );
    }

    #[test]
    fn a_workspace_admin_founding_a_channel_names_the_roles_they_gave_others_not_their_own() {
      let (alice, admin) = (Person::new(1), Person::new(9));
      let world = World::new(&[&alice, &admin], &[&admin]);
      let state = founded(&world, &admin, &[(&alice, ChannelRole::Admin)]);
      assert_eq!(
        state.appointment_of(alice.user_id).map(|found| found.by),
        Some(admin.user_id)
      );
      assert_eq!(state.appointment_of(admin.user_id), None, "their own role");
    }

    #[test]
    fn a_skipped_entry_appoints_nobody() {
      let (alice, bob, stranger, admin) = (Person::new(1), Person::new(2), Person::new(5), Person::new(9));
      let world = World::new(&[&alice, &bob, &stranger], &[&admin]);
      let mut state = founded(&world, &admin, &[(&alice, ChannelRole::Admin)]);
      let before = state.clone();
      let forged = stranger.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&stranger, ChannelRole::Admin)],
      );
      assert!(!world.apply(&mut state, forged).is_empty());
      assert_eq!(state.appointment_of(stranger.user_id), None);
      assert_eq!(state.appointments, before.appointments);
    }

    /// The server dates an entry back to when the roster was fresh, with a demotion withheld: the
    /// role counts as before, but the label "workspace admin" would claim what a purge wouldn't take.
    #[test]
    fn a_backdated_workspace_entry_is_not_labelled_a_workspace_admins() {
      let (alice, mallory, eve) = (Person::new(1), Person::new(2), Person::new(9));
      let mut world = World::new(&[&alice, &mallory], &[&eve]);
      let mut state = founded(&world, &eve, &[(&alice, ChannelRole::Admin)]);
      assert!(state.appointment_of(alice.user_id).is_some());

      world.now_ms = NOW_MS + MAX_AGE_MS + 1;
      let mut checkpoint = eve.entry(
        &state,
        &[
          (&eve, ChannelRole::Admin),
          (&alice, ChannelRole::Admin),
          (&mallory, ChannelRole::Admin),
        ],
      );
      checkpoint.created_at = UnixMillis(NOW_MS);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(state.role_of(mallory.user_id), ChannelRole::Admin);
      assert_eq!(state.appointment_of(mallory.user_id), None);
      assert_eq!(
        state.appointment_of(alice.user_id),
        None,
        "the snapshot is no longer anchored"
      );
    }

    #[test]
    fn a_roster_without_a_signing_time_labels_nobody_a_workspace_admin() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let mut world = World::new(&[&alice, &bob], &[&admin]);
      world.workspace.heads.is_roster_chained = false;
      world.workspace.heads.maybe_roster_signed_at_ms = None;
      let mut state = founded(&world, &alice, &[]);
      let checkpoint = admin.entry(
        &state,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(state.role_of(bob.user_id), ChannelRole::Admin);
      assert_eq!(state.appointment_of(bob.user_id), None);
    }

    #[test]
    fn a_replaced_identity_key_is_a_new_appointment() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let mut state = founded(
        &world,
        &admin,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      let before = state.appointment_of(bob.user_id).expect("appointed").clone();
      assert_eq!(before.authority, AppointmentAuthority::WorkspaceAdmin);

      // Alice swaps the key Bob's later entries are verified against, keeping his role.
      let mut document = alice.document(
        &state,
        &[
          (&admin, ChannelRole::Admin),
          (&alice, ChannelRole::Admin),
          (&bob, ChannelRole::Admin),
        ],
      );
      let swapped = Person::new(2).key();
      for assignment in &mut document.assignments {
        if assignment.user_id == bob.user_id {
          assignment.identity_key = swapped.clone();
        }
      }
      assert!(world.apply(&mut state, alice.signed(document)).is_empty());
      let after = state.appointment_of(bob.user_id).expect("appointed again");
      assert_eq!(
        (after.by, after.authority),
        (alice.user_id, AppointmentAuthority::ChannelAdmin)
      );
      assert_eq!(after.identity_key, swapped);
    }

    #[test]
    fn a_checkpoint_names_its_signer_for_roles_the_device_already_had() {
      let (alice, bob, first, second) = (Person::new(1), Person::new(2), Person::new(8), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&first, &second]);
      let roles = [
        (&first, ChannelRole::Admin),
        (&alice, ChannelRole::Admin),
        (&bob, ChannelRole::Admin),
      ];
      let mut state = founded(
        &world,
        &first,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      assert_eq!(
        state.appointment_of(bob.user_id).map(|found| found.by),
        Some(first.user_id)
      );
      // This device skipped a demotion and re-appointment in between: the checkpoint names no head,
      // and lists Bob as the device already has him.
      let mut stale = state.clone();
      stale.maybe_head_hash = Some(Base64Bytes::new(REVIEWED_PREV_HASH.to_vec()));
      let checkpoint = second.entry(&stale, &roles);
      assert!(world.apply(&mut state, checkpoint).is_empty());
      assert_eq!(
        state.appointment_of(bob.user_id).map(|found| found.by),
        Some(second.user_id)
      );
    }

    #[test]
    fn a_snapshot_from_before_appointments_were_recorded_claims_nothing() {
      let alice = Person::new(1);
      let json = format!(
        r#"{{"version":1,"is_workspace_anchored":true,"assignments":[{{"user_id":"{}","role":"admin","identity_key":"AAAA"}}],"cursor":1}}"#,
        alice.user_id
      );
      let state: ChannelRoleState = serde_json::from_str(&json).expect("an older stored state");
      assert_eq!(state.role_of(alice.user_id), ChannelRole::Admin);
      assert_eq!(state.appointment_of(alice.user_id), None);
    }

    #[test]
    fn an_authority_a_newer_build_wrote_reads_as_unknown_and_is_not_shown() {
      let (alice, bob, admin) = (Person::new(1), Person::new(2), Person::new(9));
      let world = World::new(&[&alice, &bob], &[&admin]);
      let state = founded(
        &world,
        &admin,
        &[(&alice, ChannelRole::Admin), (&bob, ChannelRole::Admin)],
      );
      let json = serde_json::to_string(&state)
        .expect("serializes")
        .replace("WorkspaceAdmin", "SomethingFromTheFuture");
      let read: ChannelRoleState = serde_json::from_str(&json).expect("the rest of the state still reads");
      assert_eq!(read.role_of(bob.user_id), ChannelRole::Admin);
      assert!(
        read
          .appointments
          .iter()
          .any(|found| found.authority == AppointmentAuthority::Unknown)
      );
      assert_eq!(read.appointment_of(bob.user_id), None);
    }
  }

  /// Issue #1260: a device that reads the log late, after a workspace admin who signed in it left
  /// the roster.
  mod late_reader {
    use super::*;

    /// With nobody hostile: Eve, a second workspace admin, demotes Alice and makes Bob an admin, Bob
    /// carries on, Eve leaves the roster, and only then this device reads the log.
    #[test]
    fn a_departed_signer_leaves_no_demoted_admin_anchored() {
      let story = Story::new();
      let mut world = story.world();
      let mut reader = story.first_read(&world);
      assert!(reader.is_workspace_anchored);

      let (demotion, carried_on, on_time) = story.demotion(&world, &reader);
      story.eve_leaves(&mut world);
      world.apply(&mut reader, demotion);
      world.apply(&mut reader, carried_on);

      assert_eq!(
        reader.role_of(story.alice.user_id),
        ChannelRole::Member,
        "the demotion counts"
      );
      assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Admin);
      assert_eq!(
        reader.head(),
        on_time.head(),
        "the same log as a device that was there"
      );
      assert!(!reader.is_workspace_anchored, "nothing it can justify today");
    }

    /// Issue #1264, option 1B: an ordinary entry by a workspace admin who has left, taken on their
    /// tenure, keeps no channel admin's purge open either; the channel admin carrying on does.
    #[test]
    fn a_departed_workspace_admins_entry_is_no_recent_signer() {
      let story = Story::new();
      let mut world = story.world();
      let mut reader = story.first_read(&world);
      reader.recent_signers.clear();
      let (demotion, carried_on, _) = story.demotion(&world, &reader);
      story.eve_leaves(&mut world);
      assert!(world.apply(&mut reader, demotion).is_empty());
      assert!(reader.has_former_admin_entry, "taken as a former admin's");
      assert!(reader.recent_signers.is_empty(), "{:?}", reader.recent_signers);
      assert!(world.apply(&mut reader, carried_on).is_empty());
      let signers: Vec<UserId> = reader
        .recent_signers
        .iter()
        .map(|recent| recent.user_id)
        .collect();
      assert_eq!(signers, vec![story.bob.user_id]);
    }

    #[test]
    fn a_device_reading_the_log_from_scratch_ends_where_the_others_are() {
      let story = Story::new();
      let mut world = story.world();
      let founding = story.frank.entry(
        &ChannelRoleState::default(),
        &[
          (&story.frank, ChannelRole::Admin),
          (&story.alice, ChannelRole::Admin),
        ],
      );
      let mut first = ChannelRoleState::default();
      assert!(world.apply(&mut first, founding.clone()).is_empty());
      let (demotion, carried_on, on_time) = story.demotion(&world, &first);
      story.eve_leaves(&mut world);

      let mut fresh = ChannelRoleState::default();
      for entry in [founding, demotion, carried_on] {
        assert!(world.apply(&mut fresh, entry).is_empty());
      }
      assert_eq!(fresh.head(), on_time.head());
      assert_eq!(fresh.admins(), on_time.admins());
      assert!(!fresh.is_workspace_anchored);
      assert!(!fresh.has_unverified_stretch());
    }

    /// Weeks away, with owners re-signing the roster in between: an ordinary case. Nothing is
    /// flagged, roles catch up, and a workspace admin's next entry anchors the channel again.
    #[test]
    fn an_honest_device_away_for_weeks_catches_up_without_a_word() {
      let story = Story::new();
      let mut world = story.world();
      let mut reader = story.first_read(&world);
      reader.note_read(world.workspace.roster.version);
      let (demotion, carried_on, on_time) = story.demotion(&world, &reader);
      story.eve_leaves(&mut world);
      for week in 1..=6 {
        story
          .olivia
          .borrow_mut()
          .publish(&mut world, &[&story.frank], NOW_MS + week * 7 * DAY_MS);
      }
      world.now_ms = NOW_MS + 6 * 7 * DAY_MS + DAY_MS;

      assert!(world.apply(&mut reader, demotion).is_empty());
      assert!(world.apply(&mut reader, carried_on).is_empty());
      assert_eq!(reader.head(), on_time.head());
      assert_eq!(reader.admins(), on_time.admins());
      assert!(!reader.has_unverified_stretch());
      assert!(!reader.is_workspace_anchored);
      assert_eq!(reader.appointment_of(story.bob.user_id), None, "not claimed");

      let checkpoint = story.frank.reviewed(
        &reader,
        &[
          (&story.frank, ChannelRole::Admin),
          (&story.bob, ChannelRole::Admin),
          (&story.alice, ChannelRole::Moderator),
        ],
      );
      assert!(world.apply(&mut reader, checkpoint).is_empty());
      assert!(reader.is_workspace_anchored);
      assert_eq!(
        reader.appointment_of(story.alice.user_id).map(|found| found.by),
        Some(story.frank.user_id)
      );
    }

    /// What a workspace admin's signature would launder (e2ee.md §4.6.1, "Reading the log late").
    mod vouching {
      use super::*;

      /// Variant A: Mallory and a hostile server date an entry back, appointing Zed. The workspace
      /// admin's device takes it on first read without a word. Her ordinary signature would make Zed
      /// an anchored admin on every device, so it must not be offered.
      #[test]
      fn a_snapshot_resting_on_somebody_who_left_must_be_reviewed() {
        let story = Story::new();
        let mut world = story.world();
        let mut device = story.first_read(&world);
        story.eve_leaves(&mut world);
        let entry = story.eve.entry(
          &device,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert!(world.apply(&mut device, entry).is_empty());
        assert!(device.must_review_before_vouching());
        // A channel admin carrying on doesn't end it.
        let next = story.bob.entry(
          &device,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert!(world.apply(&mut device, next).is_empty());
        assert!(device.must_review_before_vouching());
        // A workspace admin's entry does: the reader then rests on that.
        let reviewed = story.frank.signed(
          device
            .reviewed_document(
              conversation(CHANNEL),
              &[(story.frank.user_id, ChannelRole::Admin)],
              &BTreeMap::from([(story.frank.user_id, key_bytes(&story.frank))]),
              NOW_MS,
            )
            .unwrap(),
        );
        assert!(
          world
            .apply(&mut device, at_server_time(reviewed, world.now_ms))
            .is_empty()
        );
        assert!(!device.must_review_before_vouching());
        assert!(device.is_workspace_anchored);
        assert_eq!(device.admins(), vec![story.frank.user_id]);
      }

      /// Issue #1264: the workspace admin's device is anchored and was shown an old log, a withheld
      /// demotion. A member's head says so: another entry for its version, or a newer one it isn't
      /// shown. Its ordinary signature would carry the old snapshot's anchor on, so it reviews first.
      #[test]
      fn a_device_told_of_another_or_a_newer_entry_must_review() {
        let story = Story::new();
        let world = story.world();
        let device = story.first_read(&world);
        assert!(device.is_workspace_anchored);
        assert!(!device.must_review_before_vouching());

        let mut forked = device.clone();
        forked.note_head(&ChannelRolesHead {
          version: device.version,
          hash: Base64Bytes::new(vec![7; 32]),
          needs_review: false,
        });
        assert!(forked.maybe_alarm.is_some());
        assert!(forked.must_review_before_vouching());

        let mut behind = device.clone();
        behind.note_head(&ChannelRolesHead {
          version: device.version + 1,
          hash: Base64Bytes::new(vec![7; 32]),
          needs_review: false,
        });
        assert!(behind.must_review_before_vouching(), "until the log is read");
        // The log still doesn't show it: the alarm stays, and so does the review.
        behind.check_ahead();
        assert!(behind.maybe_alarm.is_some());
        assert!(behind.must_review_before_vouching());

        // A list a workspace admin looked at ends it.
        let reviewed = story.frank.signed(
          forked
            .reviewed_document(
              conversation(CHANNEL),
              &[(story.frank.user_id, ChannelRole::Admin)],
              &BTreeMap::from([(story.frank.user_id, key_bytes(&story.frank))]),
              NOW_MS,
            )
            .unwrap(),
        );
        assert!(
          world
            .apply(&mut forked, at_server_time(reviewed, world.now_ms))
            .is_empty()
        );
        assert!(!forked.must_review_before_vouching());
      }

      /// Variant B, honest: the device is in a stretch and its snapshot names Alice, demoted. What was
      /// reviewed is signed exactly, with no role from the old snapshot, wherever a device stands.
      #[test]
      fn a_reviewed_list_is_signed_exactly_and_is_a_checkpoint_on_every_device() {
        let story = Story::new();
        let mut world = story.world();
        let mut stretched = story.first_read(&world);
        let (demotion, _, on_time) = story.demotion(&world, &stretched);
        story.eve_leaves(&mut world);
        stretched.note_read(world.workspace.roster.version);
        assert_eq!(world.apply(&mut stretched, demotion).len(), 1);
        assert!(stretched.must_review_before_vouching());

        let roles = [
          (story.frank.user_id, ChannelRole::Admin),
          (story.bob.user_id, ChannelRole::Moderator),
          (story.alice.user_id, ChannelRole::Member),
        ];
        let keys = BTreeMap::from([
          (story.frank.user_id, key_bytes(&story.frank)),
          (story.bob.user_id, key_bytes(&story.bob)),
        ]);
        for mut device in [stretched, on_time] {
          // The log's next version for this device, as the server would number it for both.
          let document = device
            .reviewed_document(conversation(CHANNEL), &roles, &keys, world.now_ms)
            .unwrap();
          assert_eq!(document.assignments.len(), 2, "nobody the list doesn't name");
          let entry = at_server_time(story.frank.signed(document), world.now_ms);
          assert!(world.apply(&mut device, entry).is_empty());
          assert_eq!(device.role_of(story.alice.user_id), ChannelRole::Member);
          assert_eq!(device.role_of(story.bob.user_id), ChannelRole::Moderator);
          assert!(device.is_workspace_anchored);
          assert!(!device.has_unverified_stretch());
          assert_eq!(
            device.appointment_of(story.bob.user_id).map(|found| found.by),
            Some(story.frank.user_id),
            "the reviewer vouches for every role"
          );
        }
      }
    }

    mod a_demoted_admin {
      use super::*;

      /// Eve signs after leaving, with an old date of her choice, and the server still lets her
      /// append (she is a channel admin it lists). It dates the entry when it came, even a moment
      /// after the roster that removed her: junk on every device, read since or not.
      #[test]
      fn after_the_roster_that_removed_her_is_junk_on_every_device() {
        let story = Story::new();
        let mut world = story.world();
        let first = story.first_read(&world);
        story.eve_leaves(&mut world);
        let mut read_since = first.clone();
        read_since.note_read(world.workspace.roster.version);

        let mut document = story.eve.document(
          &first,
          &[
            (&story.eve, ChannelRole::Admin),
            (&story.frank, ChannelRole::Admin),
          ],
        );
        document.signed_at_ms = NOW_MS;
        let entry = at_server_time(story.eve.signed(document), NOW_MS + DAY_MS + 1);
        for mut reader in [first.clone(), read_since] {
          assert_eq!(world.apply(&mut reader, entry.clone()).len(), 1);
          assert_eq!(reader.head(), first.head());
          assert_eq!(reader.admins(), first.admins());
          assert!(reader.is_workspace_anchored);
          assert!(!reader.has_unverified_stretch());
        }
      }

      /// The owner's clock runs ahead, so the roster that removes Eve says later than it was signed.
      /// In that gap her entry passes as dated inside: a device that read the log since can't place
      /// it, one that didn't takes it. They disagree (residual risk, e2ee.md §4.6.1).
      #[test]
      fn in_the_gap_of_an_owners_clock_running_ahead_splits_devices() {
        let story = Story::new();
        let mut world = story.world();
        let first = story.first_read(&world);
        story.eve_leaves(&mut world);
        let mut read_since = first.clone();
        read_since.note_read(world.workspace.roster.version);
        let mut late = first.clone();
        late.note_read(1);

        let entry = story.eve.entry(
          &first,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert_eq!(world.apply(&mut read_since, entry.clone()).len(), 1);
        assert!(world.apply(&mut late, entry).is_empty());

        assert!(read_since.has_unverified_stretch());
        assert_eq!(late.role_of(story.bob.user_id), ChannelRole::Admin);
      }

      /// With a server that dates her backdated entry back too. This device read the log on the
      /// roster without her, and the entry wasn't there: it can't be what it says.
      #[test]
      fn with_a_hostile_server_gains_nothing_where_the_device_read_the_log_since() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        story.eve_leaves(&mut world);
        reader.note_read(world.workspace.roster.version);

        let entry = story.eve.entry(
          &reader,
          &[(&story.eve, ChannelRole::Admin), (&story.bob, ChannelRole::Admin)],
        );
        assert_eq!(world.apply(&mut reader, entry).len(), 1);

        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Member);
        assert!(reader.has_unverified_stretch());
        assert!(!reader.is_workspace_anchored);
      }

      /// The same on a device that last read the log before she left: it can't tell, as it couldn't
      /// if the server had kept the new roster from it. Roles change, the anchor doesn't come along:
      /// her Mallory purges nothing (residual risk, e2ee.md §4.6.1).
      #[test]
      fn with_a_hostile_server_never_gets_an_anchor_on_a_device_that_read_the_log_before() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        reader.note_read(world.workspace.roster.version);
        story.eve_leaves(&mut world);

        let entry = story.eve.entry(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert!(world.apply(&mut reader, entry).is_empty());

        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Admin);
        assert!(!reader.is_workspace_anchored);
        assert_eq!(reader.appointment_of(story.bob.user_id), None);
      }

      /// Alice, demoted in an entry this device can't place, still stands in its snapshot. She counts
      /// for nothing, and neither does "unsigned": that would let every member act.
      #[test]
      fn in_a_stale_snapshot_gains_nothing_from_an_unverified_stretch() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        reader.note_read(world.workspace.roster.version);
        assert_eq!(world.apply(&mut reader, demotion).len(), 1);
        assert!(reader.has_unverified_stretch());

        let alice = story.alice.user_id;
        assert!(reader.is_signed(), "never the rule of an unsigned channel");
        assert_eq!(reader.admins(), Vec::new(), "the merge rule's input: nobody");
        assert_eq!(reader.role_of(alice), ChannelRole::Member);
        assert!(!reader.is_workspace_anchored);
        assert_eq!(reader.appointment_of(alice), None);
        assert_eq!(
          reader.signing_authorities(alice, &world.workspace, world.now_ms, MAX_AGE_MS),
          Vec::new()
        );
        let own = story.alice.entry(
          &reader,
          &[
            (&story.alice, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        let skipped = world.apply(&mut reader, at_server_time(own, world.now_ms));
        assert!(matches!(
          skipped.as_slice(),
          [ChannelRoleError::Unauthorized { .. }]
        ));
        assert_eq!(reader.version, 1);
      }
    }

    /// A device with nothing accepted yet: an entry it can't place still shows the channel is signed.
    #[test]
    fn a_stretch_before_anything_was_accepted_is_not_an_unsigned_channel() {
      let story = Story::new();
      let mut world = story.world();
      let roles = [
        (&story.frank, ChannelRole::Admin),
        (&story.bob, ChannelRole::Admin),
      ];
      let mut reader = ChannelRoleState::default();
      let mut document = story.eve.document(&reader, &roles);
      document.signed_at_ms = NOW_MS + 3 * DAY_MS;
      let entry = story.eve.signed(document);
      story.eve_leaves(&mut world);
      assert_eq!(world.apply(&mut reader, entry).len(), 1);

      assert_eq!(reader.version, 0);
      assert!(reader.is_signed());
      assert_eq!(reader.admins(), Vec::new());
      let claim = story.alice.entry(&reader, &[(&story.alice, ChannelRole::Admin)]);
      let skipped = world.apply(&mut reader, at_server_time(claim, world.now_ms));
      assert_eq!(skipped.len(), 1, "no founding on top of it");
    }

    mod a_stretch_ends {
      use super::*;

      fn stretched(story: &Story, world: &mut World) -> ChannelRoleState {
        let mut reader = story.first_read(world);
        let (demotion, ..) = story.demotion(world, &reader);
        story.eve_leaves(world);
        reader.note_read(world.workspace.roster.version);
        assert_eq!(world.apply(&mut reader, demotion).len(), 1);
        assert!(reader.has_unverified_stretch());
        reader
      }

      #[test]
      fn with_a_checkpoint_by_a_workspace_admin_on_a_fresh_roster() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = stretched(&story, &mut world);
        assert_eq!(
          reader.signing_authorities(story.frank.user_id, &world.workspace, world.now_ms, MAX_AGE_MS),
          vec![SignerAuthority::Workspace { is_owner: false }],
          "the one who can end it can sign"
        );
        let mut stale = reader.clone();
        stale.maybe_head_hash = Some(Base64Bytes::new(REVIEWED_PREV_HASH.to_vec()));
        let checkpoint = story.frank.entry(
          &stale,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert!(
          world
            .apply(&mut reader, at_server_time(checkpoint, world.now_ms))
            .is_empty()
        );
        assert!(!reader.has_unverified_stretch());
        assert!(reader.is_workspace_anchored);
        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Admin);
        assert_eq!(reader.role_of(story.alice.user_id), ChannelRole::Member);
      }

      /// A checkpoint that lists the signer as the snapshot already has them isn't their own role
      /// changed, though nobody counts as an admin during the stretch.
      #[test]
      fn with_a_checkpoint_that_keeps_the_signers_own_role() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = stretched(&story, &mut world);
        let checkpoint = story.frank.reviewed(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.alice, ChannelRole::Admin),
          ],
        );
        assert!(
          world
            .apply(&mut reader, at_server_time(checkpoint, world.now_ms))
            .is_empty()
        );
        assert!(!reader.has_unverified_stretch());
      }

      /// The server dates the entry back to a fresh roster: it passes for roles as ever, but it says
      /// nothing about today.
      #[test]
      fn not_with_a_checkpoint_on_a_roster_too_old_now() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = stretched(&story, &mut world);
        world.now_ms += MAX_AGE_MS + 2 * DAY_MS;
        let checkpoint = story.frank.entry(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        assert!(world.apply(&mut reader, checkpoint).is_empty());
        assert!(reader.has_unverified_stretch());
        assert!(!reader.is_workspace_anchored);
      }
    }

    mod the_server_clock {
      use super::*;

      /// The server's date alone makes nothing count: Eve's clock says after she left.
      #[test]
      fn dated_back_does_not_make_an_entry_count() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        story.eve_leaves(&mut world);

        let mut document = story.eve.document(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        document.signed_at_ms = world.now_ms;
        let entry = at_server_time(story.eve.signed(document), NOW_MS);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);

        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Member);
        assert!(
          reader.has_unverified_stretch(),
          "the server says one thing, Eve another"
        );
      }

      /// A real entry the server dates after she left: as good as withholding it, which it can do
      /// anyway. The server's date can only take an entry away.
      #[test]
      fn dated_forward_takes_an_honest_entry_away_like_withholding_it() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        let before = reader.clone();

        assert_eq!(
          world
            .apply(&mut reader, at_server_time(demotion, world.now_ms))
            .len(),
          1
        );

        assert_eq!(reader.head(), before.head());
        assert!(reader.is_workspace_anchored);
      }

      /// Before she was on the roster at all.
      #[test]
      fn dated_before_the_tenure_does_not_count_either() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);

        let entry = at_server_time(demotion, NOW_MS - 2 * DAY_MS);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);
        assert_eq!(reader.version, 1);
      }
    }

    mod clocks {
      use super::*;

      /// This device's clock isn't read for somebody who left: the same log gives the same roles,
      /// however wrong it is.
      #[test]
      fn a_wrong_clock_on_this_device_changes_no_role() {
        for now_ms in [NOW_MS - 400 * DAY_MS, NOW_MS + 4000 * DAY_MS] {
          let story = Story::new();
          let mut world = story.world();
          let mut reader = story.first_read(&world);
          let (demotion, carried_on, on_time) = story.demotion(&world, &reader);
          story.eve_leaves(&mut world);
          world.now_ms = now_ms;
          assert!(world.apply(&mut reader, demotion).is_empty());
          assert!(world.apply(&mut reader, carried_on).is_empty());
          assert_eq!(reader.head(), on_time.head());
          assert!(!reader.is_workspace_anchored);
        }
      }

      /// Eve's own clock runs a day ahead or behind: she can't be placed, and it shows.
      #[test]
      fn a_signer_clock_far_off_fails_closed() {
        for signed_at_ms in [NOW_MS + 3 * DAY_MS, NOW_MS - 3 * DAY_MS] {
          let story = Story::new();
          let mut world = story.world();
          let mut reader = story.first_read(&world);
          let mut document = story.eve.document(
            &reader,
            &[
              (&story.frank, ChannelRole::Admin),
              (&story.bob, ChannelRole::Admin),
            ],
          );
          document.signed_at_ms = signed_at_ms;
          let entry = story.eve.signed(document);
          story.eve_leaves(&mut world);
          assert_eq!(world.apply(&mut reader, entry).len(), 1);
          assert_eq!(reader.version, 1);
          assert!(reader.has_unverified_stretch());
        }
      }

      /// Minutes off between the owner's, her and the server's clocks are nothing to fail over.
      #[test]
      fn a_little_skew_between_clocks_is_tolerated() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let mut document = story.eve.document(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        let end = NOW_MS + DAY_MS;
        document.signed_at_ms = end + SKEW_MS;
        let entry = at_server_time(story.eve.signed(document), end);
        story.eve_leaves(&mut world);
        assert!(world.apply(&mut reader, entry).is_empty());
        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Admin);
      }
    }

    mod came_back {
      use super::*;

      /// On the roster again today: an admin like any other, whatever happened in between.
      #[test]
      fn a_signer_on_the_roster_again_counts_as_one_and_anchors() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        story
          .olivia
          .borrow_mut()
          .publish(&mut world, &[&story.frank, &story.eve], NOW_MS + 2 * DAY_MS);
        assert!(world.apply(&mut reader, demotion).is_empty());
        assert!(reader.is_workspace_anchored);
      }

      /// Left, came back, left: each tenure counts for itself, the time between them for nothing.
      #[test]
      fn the_time_between_two_tenures_counts_for_nothing() {
        let story = Story::new();
        let mut world = story.world();
        let reader = story.first_read(&world);
        story.eve_leaves(&mut world);
        story
          .olivia
          .borrow_mut()
          .publish(&mut world, &[&story.frank, &story.eve], NOW_MS + 5 * DAY_MS);
        story
          .olivia
          .borrow_mut()
          .publish(&mut world, &[&story.frank], NOW_MS + 6 * DAY_MS);
        let roles = [
          (&story.frank, ChannelRole::Admin),
          (&story.bob, ChannelRole::Admin),
        ];
        let entry_at = |signed_at_ms, created_at_ms| {
          let mut document = story.eve.document(&reader, &roles);
          document.signed_at_ms = signed_at_ms;
          at_server_time(story.eve.signed(document), created_at_ms)
        };

        for inside in [NOW_MS, NOW_MS + 5 * DAY_MS + DAY_MS / 2] {
          let mut probe = reader.clone();
          assert!(world.apply(&mut probe, entry_at(inside, inside)).is_empty());
          assert_eq!(probe.role_of(story.bob.user_id), ChannelRole::Admin);
        }

        let gap = NOW_MS + 3 * DAY_MS;
        let mut probe = reader.clone();
        assert_eq!(world.apply(&mut probe, entry_at(gap, gap)).len(), 1);
        assert!(
          !probe.has_unverified_stretch(),
          "the server dates it when she had left too"
        );
        let mut probe = reader.clone();
        assert_eq!(world.apply(&mut probe, entry_at(gap, NOW_MS)).len(), 1);
        assert!(probe.has_unverified_stretch());
      }

      /// Back with a new identity key: the old key's entries are judged by the old tenure, and the
      /// new key says nothing about them.
      #[test]
      fn an_entry_under_the_key_of_an_earlier_tenure_is_judged_by_that_tenure() {
        let story = Story::new();
        let new_eve = Person::new(9);
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        story
          .olivia
          .borrow_mut()
          .publish(&mut world, &[&story.frank, &new_eve], NOW_MS + 2 * DAY_MS);
        assert!(world.apply(&mut reader, demotion).is_empty());
        assert_eq!(reader.role_of(story.bob.user_id), ChannelRole::Admin);
        assert!(
          !reader.is_workspace_anchored,
          "the old key's word, not today's admin's"
        );
      }
    }

    mod what_is_no_gap {
      use super::*;

      /// A random head, and an older head of this very log (a stale device's fork): nothing shows
      /// that this device missed anything, and a channel admin can't start a stretch this way.
      #[test]
      fn a_channel_admin_on_a_head_nobody_showed_this_device() {
        let story = Story::new();
        let world = story.world();
        let mut reader = story.first_read(&world);
        let older = reader.clone();
        let roles = [
          (&story.frank, ChannelRole::Admin),
          (&story.alice, ChannelRole::Admin),
        ];
        let next = story.alice.entry(&reader, &roles);
        assert!(world.apply(&mut reader, next).is_empty());
        let mut random = reader.clone();
        random.maybe_head_hash = Some(Base64Bytes::new(vec![5; HASH_BYTES]));

        for stale in [random, older] {
          let mut base = stale;
          base.cursor = reader.cursor;
          let entry = story.alice.entry(&base, &roles);
          let mut probe = reader.clone();
          assert!(matches!(
            world.apply(&mut probe, entry).as_slice(),
            [ChannelRoleError::BrokenChain { .. }]
          ));
          assert!(!probe.has_unverified_stretch());
          assert!(probe.is_workspace_anchored);
        }
      }

      /// A roster that dates no end proves no tenure over: an entry against it has no standing, and
      /// a demoted admin can't use the missing date to trouble a device for ever.
      #[test]
      fn a_departure_on_a_roster_without_a_signing_time() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        for tenure in &mut world.workspace.admin_history.tenures {
          if let Some(until) = &mut tenure.maybe_until {
            until.maybe_at_ms = None;
          }
        }
        assert_eq!(world.apply(&mut reader, demotion).len(), 1);
        assert_eq!(reader.version, 1);
        assert!(!reader.has_unverified_stretch());
      }

      /// None of these can take away a channel's anchor: otherwise anybody could.
      #[test]
      fn junk_members_strangers_and_other_channels_change_nothing() {
        let story = Story::new();
        let stranger = Person::new(5);
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let roles = [
          (&story.frank, ChannelRole::Admin),
          (&story.bob, ChannelRole::Admin),
        ];
        let mut junk = story.eve.entry(&reader, &roles);
        junk.payload = Base64Bytes::new(b"not json".to_vec());
        let by_a_member = story.bob.entry(&reader, &roles);
        let by_a_stranger = stranger.entry(&reader, &roles);
        let other_channel = story.eve.entry_in(CHANNEL + 1, &reader, &roles);
        let mut no_admin = story.eve.document(&reader, &roles);
        no_admin.assignments.clear();
        let invalid = story.eve.signed(no_admin);
        story.eve_leaves(&mut world);
        let before = reader.clone();

        for entry in [junk, by_a_member, by_a_stranger, other_channel, invalid] {
          let mut probe = before.clone();
          assert_eq!(world.apply(&mut probe, at_server_time(entry, NOW_MS)).len(), 1);
          assert!(!probe.has_unverified_stretch());
          assert!(probe.is_workspace_anchored);
        }
        // Reading on, no version counts twice.
        let entry = story.bob.entry(&reader, &roles);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);
        assert_eq!(reader.head(), before.head());
      }
    }

    mod what_is_a_gap {
      use super::*;

      /// A member accepted an entry this device refused, and built on it: it did miss something.
      #[test]
      fn a_channel_admin_built_on_an_entry_this_device_refused() {
        let story = Story::new();
        let world = story.world();
        let mut reader = story.first_read(&world);
        let roles = [
          (&story.frank, ChannelRole::Admin),
          (&story.alice, ChannelRole::Admin),
        ];
        let refused = story.bob.entry(&reader, &roles);
        let refused_hash = entry_hash(&refused);
        assert_eq!(world.apply(&mut reader, refused).len(), 1);
        assert!(!reader.has_unverified_stretch());

        let mut other = reader.clone();
        other.maybe_head_hash = Some(refused_hash);
        let entry = story.alice.entry(&other, &roles);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);
        assert!(reader.has_unverified_stretch());
      }

      #[test]
      fn an_admin_of_this_roster_who_is_too_old_to_judge_them() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        // Time really passed: a date only the server claims wouldn't make this roster old.
        world.now_ms = NOW_MS + MAX_AGE_MS;
        let entry = story.eve.entry(
          &reader,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        let entry = at_server_time(entry, NOW_MS + MAX_AGE_MS);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);
        assert!(reader.has_unverified_stretch());
      }

      /// She left, but the entry names another head, which only a current admin may: not placed.
      #[test]
      fn a_departed_signer_on_another_head() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let mut other = reader.clone();
        other.maybe_head_hash = Some(Base64Bytes::new(vec![5; HASH_BYTES]));
        let entry = story.eve.entry(
          &other,
          &[
            (&story.frank, ChannelRole::Admin),
            (&story.bob, ChannelRole::Admin),
          ],
        );
        story.eve_leaves(&mut world);
        assert_eq!(world.apply(&mut reader, entry).len(), 1);
        assert!(reader.has_unverified_stretch());
        assert_eq!(reader.version, 1);
      }

      /// Without the roster's history a departed admin looks like any member, so anybody's key
      /// counts: it fails closed. Junk still doesn't.
      #[test]
      fn any_pinned_signer_while_the_roster_history_is_unknown() {
        let story = Story::new();
        let mut world = story.world();
        let mut reader = story.first_read(&world);
        let (demotion, ..) = story.demotion(&world, &reader);
        story.eve_leaves(&mut world);
        world.workspace.admin_history = AdminHistory::default();

        let mut probe = reader.clone();
        let mut junk = demotion.clone();
        junk.signature = Base64Bytes::new(vec![1, 2, 3]);
        assert_eq!(world.apply(&mut probe, junk).len(), 1);
        assert!(!probe.has_unverified_stretch());

        assert_eq!(world.apply(&mut reader, demotion).len(), 1);
        assert!(reader.has_unverified_stretch());
        assert!(!reader.is_workspace_anchored);
      }
    }

    mod read_marks {
      use super::*;

      #[test]
      fn the_roster_version_of_the_last_read_only_moves_forward() {
        let mut state = ChannelRoleState::default();
        state.note_read(4);
        state.note_read(2);
        assert_eq!(state.maybe_read_at_roster_version, Some(4));
      }

      /// A device from before this build doesn't know when it last read the log, so it can't place
      /// an entry by somebody who left: it looks at what it skipped once more and fails closed, and
      /// the stale anchor it kept is gone.
      #[test]
      fn a_state_from_before_looks_at_what_it_skipped_once_and_fails_closed() {
        let story = Story::new();
        let mut world = story.world();
        let first = story.first_read(&world);
        let (demotion, carried_on, _) = story.demotion(&world, &first);
        story.eve_leaves(&mut world);
        let mut reader = first;
        reader.maybe_read_at_roster_version = None;
        reader.cursor = 3;

        reader.prepare_read();
        assert_eq!(reader.cursor, 1);
        assert_eq!(world.apply(&mut reader, demotion).len(), 1);
        assert_eq!(world.apply(&mut reader, carried_on).len(), 1);
        reader.note_read(world.workspace.roster.version);
        assert!(reader.has_unverified_stretch());
        assert!(!reader.is_workspace_anchored);

        reader.cursor = 9;
        reader.prepare_read();
        assert_eq!(reader.cursor, 9, "once");
      }

      #[test]
      fn the_new_state_round_trips_and_is_absent_while_unset() {
        let state = ChannelRoleState {
          maybe_read_at_roster_version: Some(3),
          maybe_unverified_from: Some(4),
          ..ChannelRoleState::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""read_at_roster_version":3"#));
        assert!(json.contains(r#""unverified_from":4"#));
        assert_eq!(serde_json::from_str::<ChannelRoleState>(&json).unwrap(), state);
        let unset = serde_json::to_string(&ChannelRoleState::default()).unwrap();
        assert!(
          !unset.contains("unverified_from"),
          "nothing new for a channel without any of it"
        );
      }
    }
  }

  struct Person {
    user_id: UserId,
    identity: IdentityKeyPair,
    device: DeviceKeyPair,
    certificate: SignedDeviceCertificate,
  }

  impl Person {
    fn new(raw: i64) -> Self {
      let user_id = user(raw);
      let identity = IdentityKeyPair::generate(&mut OsRng);
      let device = DeviceKeyPair::generate(
        convert::user(user_id),
        crypto::DeviceId::new(raw.unsigned_abs() * 100),
        &mut OsRng,
      );
      let certificate = identity
        .certify_device(convert::user(user_id), &device.public_keys())
        .unwrap();
      Self {
        user_id,
        identity,
        device,
        certificate,
      }
    }

    fn key(&self) -> Base64Bytes {
      Base64Bytes::new(self.identity.public_key().as_bytes().to_vec())
    }

    /// The next document after `state`, holding exactly `roles`.
    fn document(&self, state: &ChannelRoleState, roles: &[(&Person, ChannelRole)]) -> ChannelRolesDocument {
      self.document_in(CHANNEL, state, roles)
    }

    fn document_in(
      &self,
      channel: i64,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRolesDocument {
      let mut assignments: Vec<ChannelRoleAssignment> = roles
        .iter()
        .map(|(person, role)| ChannelRoleAssignment {
          user_id: person.user_id,
          role: role.clone(),
          identity_key: person.key(),
        })
        .collect();
      assignments.sort_by_key(|assignment| assignment.user_id);
      ChannelRolesDocument {
        version: state.cursor + 1,
        conversation_id: conversation(channel),
        assignments,
        prev_hash: state.head_or_zero(),
        signed_at_ms: NOW_MS,
        has_server_listed_roles: false,
      }
    }

    /// A list this person looked at, holding exactly `roles`: a checkpoint, if they may sign one.
    fn reviewed(
      &self,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRoleLogEntryResponse {
      let mut document = self.document(state, roles);
      document.prev_hash = Base64Bytes::new(REVIEWED_PREV_HASH.to_vec());
      self.signed(document)
    }

    fn entry(
      &self,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRoleLogEntryResponse {
      self.signed(self.document(state, roles))
    }

    fn entry_in(
      &self,
      channel: i64,
      state: &ChannelRoleState,
      roles: &[(&Person, ChannelRole)],
    ) -> ChannelRoleLogEntryResponse {
      self.signed(self.document_in(channel, state, roles))
    }

    fn signed(&self, document: ChannelRolesDocument) -> ChannelRoleLogEntryResponse {
      let payload = serde_json::to_vec(&document).unwrap();
      let signature = sign_log_entry(
        &self.device,
        &self.certificate,
        WorkspaceLog::ChannelRoles,
        document.version,
        &payload,
      )
      .unwrap();
      ChannelRoleLogEntryResponse {
        version: document.version,
        payload: Base64Bytes::new(payload),
        signature: Base64Bytes::new(signature),
        maybe_signer_device_id: None,
        created_at: UnixMillis(NOW_MS),
      }
    }
  }

  /// What a verifying device knows: the keys it pinned and the verified, fresh roster.
  struct World {
    pinned: KeyDirectory,
    workspace: WorkspaceState,
    now_ms: i64,
    founding: RoleFounding,
  }

  impl World {
    fn new(people: &[&Person], roster_admins: &[&Person]) -> Self {
      let pinned = KeyDirectory::from_keys(
        people
          .iter()
          .chain(roster_admins)
          .map(|person| (convert::user(person.user_id), person.identity.public_key()))
          .collect(),
      );
      let workspace = WorkspaceState {
        roster: RosterDocument {
          version: 1,
          members: roster_admins
            .iter()
            .map(|person| RosterMember {
              user_id: person.user_id,
              role: RosterRole::Admin,
              identity_key: person.key(),
            })
            .collect(),
          ..RosterDocument::default()
        },
        heads: LogHeads {
          is_roster_chained: true,
          maybe_roster_signed_at_ms: Some(NOW_MS),
          ..LogHeads::default()
        },
        admin_history: AdminHistory {
          is_complete: true,
          ..AdminHistory::default()
        },
        ..WorkspaceState::default()
      };
      Self {
        pinned,
        workspace,
        now_ms: NOW_MS,
        founding: RoleFounding::Claimed,
      }
    }

    fn apply(
      &self,
      state: &mut ChannelRoleState,
      entry: ChannelRoleLogEntryResponse,
    ) -> Vec<ChannelRoleError> {
      self.apply_all(state, vec![entry])
    }

    fn apply_all(
      &self,
      state: &mut ChannelRoleState,
      entries: Vec<ChannelRoleLogEntryResponse>,
    ) -> Vec<ChannelRoleError> {
      state.apply_entries(entries, &self.verifier())
    }

    /// `entry` as a member's message carries it.
    fn take(&self, state: &mut ChannelRoleState, entry: &ChannelRoleLogEntryResponse) -> MessageEntry {
      state.take_message_entry(entry, &self.verifier())
    }

    fn verifier(&self) -> RoleVerifier<'_, KeyDirectory> {
      RoleVerifier {
        conversation: conversation(CHANNEL),
        workspace: &self.workspace,
        roster_max_age_ms: MAX_AGE_MS,
        now_ms: self.now_ms,
        tenure_clock_skew_ms: SKEW_MS,
        pinned: &self.pinned,
        founding: self.founding,
      }
    }
  }

  /// The roster log as a real device verifies it: an owner signs every version, chained.
  struct RosterLog {
    owner: Person,
    version: u64,
    maybe_head: Option<Base64Bytes>,
  }

  impl RosterLog {
    fn new(owner: Person) -> Self {
      Self {
        owner,
        version: 0,
        maybe_head: None,
      }
    }

    /// The next roster, holding the owner and `admins`, verified by `world`'s device.
    fn publish(&mut self, world: &mut World, admins: &[&Person], signed_at_ms: i64) {
      if self.version == 0 {
        world.workspace = WorkspaceState::default();
      }
      let entry = self.next_entry(admins, signed_at_ms);
      world.workspace.apply_entries(vec![entry]).unwrap();
    }

    fn next_entry(&mut self, admins: &[&Person], signed_at_ms: i64) -> WorkspaceLogEntryResponse {
      self.version += 1;
      let mut members = vec![RosterMember {
        user_id: self.owner.user_id,
        role: RosterRole::Owner,
        identity_key: self.owner.key(),
      }];
      members.extend(admins.iter().map(|person| RosterMember {
        user_id: person.user_id,
        role: RosterRole::Admin,
        identity_key: person.key(),
      }));
      let document = RosterDocument {
        version: self.version,
        members,
        maybe_org_recovery_key: None,
        maybe_prev_hash: Some(
          self
            .maybe_head
            .clone()
            .unwrap_or_else(|| Base64Bytes::new(vec![0; HASH_BYTES])),
        ),
        maybe_signed_at_ms: Some(signed_at_ms),
        ..RosterDocument::default()
      };
      let payload = serde_json::to_vec(&document).unwrap();
      let signature = sign_log_entry(
        &self.owner.device,
        &self.owner.certificate,
        WorkspaceLog::Roster,
        self.version,
        &payload,
      )
      .unwrap();
      let hash = log_entry_hash(WorkspaceLog::Roster, self.version, &payload, &signature).unwrap();
      self.maybe_head = Some(Base64Bytes::new(hash.to_vec()));
      WorkspaceLogEntryResponse {
        kind: WorkspaceLogKind::Roster,
        version: self.version,
        payload: Base64Bytes::new(payload),
        signature: Base64Bytes::new(signature),
        maybe_signer_device_id: None,
        created_at: UnixMillis(signed_at_ms),
      }
    }
  }

  /// The people of the late reader's tests: Olivia owns the workspace, Frank and Eve are workspace
  /// admins, Alice a channel admin Frank appointed, Bob a member until Eve appoints him.
  struct Story {
    olivia: RefCell<RosterLog>,
    frank: Person,
    eve: Person,
    alice: Person,
    bob: Person,
  }

  impl Story {
    fn new() -> Self {
      Self {
        olivia: RefCell::new(RosterLog::new(Person::new(7))),
        frank: Person::new(8),
        eve: Person::new(9),
        alice: Person::new(1),
        bob: Person::new(2),
      }
    }

    /// A device that verified the roster with both admins, a day before the entries.
    fn world(&self) -> World {
      let mut world = World::new(&[&self.alice, &self.bob, &self.frank, &self.eve], &[]);
      self
        .olivia
        .borrow_mut()
        .publish(&mut world, &[&self.frank, &self.eve], NOW_MS - DAY_MS);
      world
    }

    /// The channel as Frank founded it, read on time: anchored, with Alice an admin.
    fn first_read(&self, world: &World) -> ChannelRoleState {
      founded(world, &self.frank, &[(&self.alice, ChannelRole::Admin)])
    }

    /// Eve demotes Alice and appoints Bob, who signs the next entry: both entries and the state of
    /// a device that was there.
    fn demotion(
      &self,
      world: &World,
      state: &ChannelRoleState,
    ) -> (
      ChannelRoleLogEntryResponse,
      ChannelRoleLogEntryResponse,
      ChannelRoleState,
    ) {
      let mut on_time = state.clone();
      let roles = [(&self.frank, ChannelRole::Admin), (&self.bob, ChannelRole::Admin)];
      let demotion = self.eve.entry(&on_time, &roles);
      assert!(world.apply(&mut on_time, demotion.clone()).is_empty());
      assert_eq!(on_time.role_of(self.alice.user_id), ChannelRole::Member);
      let carried_on = self.bob.entry(&on_time, &roles);
      assert!(world.apply(&mut on_time, carried_on.clone()).is_empty());
      (demotion, carried_on, on_time)
    }

    /// The next roster leaves Eve out, a day after the entries, and a day later this device reads.
    fn eve_leaves(&self, world: &mut World) {
      self
        .olivia
        .borrow_mut()
        .publish(world, &[&self.frank], NOW_MS + DAY_MS);
      world.now_ms = NOW_MS + 2 * DAY_MS;
    }
  }

  /// A channel founded by `founder`, with `others` given their roles in the founding entry.
  fn founded(world: &World, founder: &Person, others: &[(&Person, ChannelRole)]) -> ChannelRoleState {
    let mut state = ChannelRoleState::default();
    let mut roles = vec![(founder, ChannelRole::Admin)];
    roles.extend(others.iter().map(|(person, role)| (*person, role.clone())));
    let entry = founder.entry(&state, &roles);
    assert!(world.apply(&mut state, entry).is_empty());
    state
  }

  fn key_bytes(person: &Person) -> [u8; 32] {
    *person.identity.public_key().as_bytes()
  }

  fn entry_hash(entry: &ChannelRoleLogEntryResponse) -> Base64Bytes {
    let hash = log_entry_hash(
      WorkspaceLog::ChannelRoles,
      entry.version,
      entry.payload.as_slice(),
      entry.signature.as_slice(),
    )
    .unwrap();
    Base64Bytes::new(hash.to_vec())
  }

  /// The entry as the server dates it.
  fn at_server_time(
    mut entry: ChannelRoleLogEntryResponse,
    created_at_ms: i64,
  ) -> ChannelRoleLogEntryResponse {
    entry.created_at = UnixMillis(created_at_ms);
    entry
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
