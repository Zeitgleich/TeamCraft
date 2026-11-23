//! Strict channels in the UI (e2ee.md §4.6.2, issue #1897): what a channel's settings and its
//! notices show about a channel whose membership changes only signed channel admins may make, and
//! which actions they offer.
//!
//! The rule and the verdicts are the core's (`teamcraft-crypto`); this module only words them:
//!
//! - [`StrictChannel`] is what this device knows of one private channel, from the group it holds.
//!   The backend returns none while the app doesn't implement strict channels (the core's
//!   `strict-channels` feature), so nothing here is shown and no protection is claimed then.
//! - [`StrictGates`] decides which actions are offered: both the signed state (the roles in the
//!   group context, the fresh workspace roster) and the server's permissions have to allow them
//!   (admin.md §1.1), so nobody gets an error after the click.
//! - [`strict_losses`] lists who loses what when a channel turns strict (D1, D2): space admins and
//!   custom scheme holders who change its members today without a signed channel role, and
//!   moderators, who then only remove plain members.

use serde::{Deserialize, Serialize};
use teamcraft_proto::role_rules::{RoleActor, can_in_channel};
use teamcraft_proto::{ChannelRole, ConversationVisibility, Permission, PermissionScheme, Role, SpaceRole};

use crate::backend::ChatBackend;
use crate::model::{ConversationId, DeviceId, SpaceRoster, UserId};

/// What this device knows of a private channel's strict mode (e2ee.md §4.6.2).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictChannel {
  /// This device is in the channel's group: the verdicts below are its own.
  #[serde(default)]
  pub has_group: bool,
  /// The group context carries the roles: only signed channel admins (and moderators, for plain
  /// members) change members.
  #[serde(default)]
  pub is_strict: bool,
  /// The roles in the group context while strict; otherwise the roles turning it on would sign:
  /// the channel's signed admins and moderators who are in the group.
  #[serde(default)]
  pub roles: Vec<StrictRoleHolder>,
  /// The workspace owners and admins of the fresh signed roster: they keep changing members of a
  /// strict channel, and only they turn it on (D3). Empty while the roster is stale.
  #[serde(default)]
  pub workspace_signers: Vec<UserId>,
  /// How far the channel is on its way to strict (steps 2 and 3); `None` before anybody asked.
  #[serde(default, rename = "readiness", skip_serializing_if = "Option::is_none")]
  pub maybe_readiness: Option<StrictReadiness>,
  /// This device stopped at a refused membership change (D4).
  #[serde(default, rename = "stop", skip_serializing_if = "Option::is_none")]
  pub maybe_stop: Option<StrictStop>,
  /// A welcome back into the channel that wasn't opened: it came from somebody who isn't an admin
  /// of the roles this device knew.
  #[serde(default, rename = "rejoin", skip_serializing_if = "Option::is_none")]
  pub maybe_rejoin: Option<StrictRejoin>,
}

/// A role in a strict channel's group context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictRoleHolder {
  pub user: UserId,
  pub role: StrictRole,
}

/// The roles a strict channel's context holds (D1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrictRole {
  /// Adds and removes anybody.
  Admin,
  /// Removes plain members and adds nobody.
  Moderator,
}

/// Steps 2 and 3 of turning strict on, as this device sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictReadiness {
  /// Who asked the devices, `None` if no ask reached this device.
  #[serde(default, rename = "asked_by", skip_serializing_if = "Option::is_none")]
  pub maybe_asked_by: Option<UserId>,
  /// Devices on an older version or not online since the ask: each blocks turning strict on until
  /// it answers or is removed (D6).
  #[serde(default)]
  pub waiting: Vec<StrictDevice>,
  /// Devices that answered and will refresh their membership on their own.
  #[serde(default)]
  pub refreshing: Vec<StrictDevice>,
  /// Every device is ready: the channel can be turned strict now.
  #[serde(default)]
  pub is_ready: bool,
}

/// A device in a channel's group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictDevice {
  pub user: UserId,
  pub device: DeviceId,
}

/// Where this device stopped (D4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictStop {
  /// The server's time of the refused change.
  pub at_ms: i64,
  /// Whom the refused change claims as its signer, unverified: whom "Remove {name} and continue"
  /// names. `None` for a change that names nobody.
  #[serde(default, rename = "signer", skip_serializing_if = "Option::is_none")]
  pub maybe_signer: Option<UserId>,
}

/// A held welcome back into a strict channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictRejoin {
  /// Who made the welcome that wasn't opened.
  pub sender: UserId,
  /// Who is in that welcome's group and wasn't in this device's last one.
  #[serde(default)]
  pub joined: Vec<UserId>,
  /// Who was in this device's last group and isn't in the welcome's.
  #[serde(default)]
  pub gone: Vec<UserId>,
  /// The user may decide now: the wait for an admin is over, or nobody else could answer.
  #[serde(default)]
  pub is_due: bool,
  /// The user said to take the next welcome.
  #[serde(default)]
  pub is_accepted: bool,
}

/// What somebody loses when the channel turns strict (D1, D2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrictLossKind {
  /// Changes the channel's members today and no longer will: a space admin or custom scheme holder
  /// without a signed channel role.
  ChangingMembers,
  /// A moderator: removes plain members only, adds nobody.
  AddingMembers,
}

/// Somebody who loses something when the channel turns strict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrictLoss {
  pub user: UserId,
  pub loss: StrictLossKind,
}

/// A person who may change the channel's members today, as far as this device knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrictCandidate {
  pub user: UserId,
  pub is_space_admin: bool,
  /// The person's channel role, `None` outside the channel.
  pub maybe_channel_role: Option<ChannelRole>,
  pub is_guest: bool,
}

/// Which strict channel actions the user is offered: the signed state and the server's roles both
/// have to allow each (admin.md §1.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StrictGates {
  /// Ask the devices whether they are ready, and turn strict on once they are (D3).
  pub can_turn_on: bool,
  /// Turn strict off: an admin of the roles or a workspace owner or admin.
  pub can_turn_off: bool,
  /// Remove a device that blocks turning strict on (D6).
  pub can_remove_blocker: bool,
  /// "Remove {name} and continue" / "Continue" after a stop (D4).
  pub can_continue: bool,
}

/// What the user holds, for [`StrictGates::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StrictActor {
  pub user: UserId,
  /// The server grants `manage_members` in the channel.
  pub can_manage_members: bool,
  /// The server grants `manage_channel_roles` in the channel (the rewind asks for it).
  pub can_manage_channel_roles: bool,
}

impl StrictChannel {
  /// The user's role in the group context, if the channel is strict and they hold one.
  pub fn role_of(&self, user: UserId) -> Option<StrictRole> {
    if !self.is_strict {
      return None;
    }
    self
      .roles
      .iter()
      .find(|holder| holder.user == user)
      .map(|holder| holder.role)
  }

  /// Whether the user is an owner or admin of the fresh signed workspace roster.
  pub fn is_workspace_signer(&self, user: UserId) -> bool {
    self.workspace_signers.contains(&user)
  }

  /// Whether the user's changes to the members of this strict channel count: an admin of the
  /// roles or a workspace signer.
  pub fn may_change_members(&self, user: UserId) -> bool {
    self.role_of(user) == Some(StrictRole::Admin) || self.is_workspace_signer(user)
  }
}

impl StrictGates {
  pub fn new(channel: &StrictChannel, actor: StrictActor) -> Self {
    let is_signer = channel.is_workspace_signer(actor.user);
    let is_stopped = channel.maybe_stop.is_some();
    let has_group = channel.has_group;
    Self {
      // Turning on needs this device in the group (steps 2 to 4 run from it) and the server's
      // `manage_channel_roles`, which the roles commit's hint is checked against.
      can_turn_on: has_group
        && !channel.is_strict
        && !is_stopped
        && is_signer
        && actor.can_manage_channel_roles,
      can_turn_off: has_group
        && channel.is_strict
        && !is_stopped
        && channel.may_change_members(actor.user)
        && actor.can_manage_channel_roles,
      can_remove_blocker: has_group
        && !channel.is_strict
        && !is_stopped
        && is_signer
        && actor.can_manage_members
        && channel
          .maybe_readiness
          .as_ref()
          .is_some_and(|readiness| !readiness.waiting.is_empty()),
      can_continue: has_group
        && is_stopped
        && channel.may_change_members(actor.user)
        && actor.can_manage_channel_roles,
    }
  }
}

/// Who loses what when the channel turns strict with `channel.roles` (D1, D2): everybody among
/// `candidates` who may change the channel's members today and keeps no signed way to, sorted by
/// user. Workspace signers keep theirs. `scheme` is the space's permission scheme.
pub fn strict_losses(
  scheme: &PermissionScheme,
  channel: &StrictChannel,
  candidates: &[StrictCandidate],
) -> Vec<StrictLoss> {
  let mut losses: Vec<StrictLoss> = candidates
    .iter()
    .filter(|candidate| !candidate.is_guest && !channel.is_workspace_signer(candidate.user))
    .filter(|candidate| may_manage_members_today(scheme, candidate))
    .filter_map(|candidate| {
      let maybe_role = channel
        .roles
        .iter()
        .find(|holder| holder.user == candidate.user)
        .map(|holder| holder.role);
      match maybe_role {
        Some(StrictRole::Admin) => None,
        Some(StrictRole::Moderator) => Some(StrictLossKind::AddingMembers),
        None => Some(StrictLossKind::ChangingMembers),
      }
      .map(|loss| StrictLoss {
        user: candidate.user,
        loss,
      })
    })
    .collect();
  losses.sort_by_key(|loss| loss.user);
  losses.dedup_by_key(|loss| loss.user);
  losses
}

/// [`strict_losses`] for a channel as the backend knows it (its members with their roles, the
/// space's admins), under the space's permission `scheme`.
pub fn strict_losses_in<B: ChatBackend + ?Sized>(
  backend: &B,
  conversation: ConversationId,
  channel: &StrictChannel,
  scheme: &PermissionScheme,
) -> Vec<StrictLoss> {
  strict_losses(scheme, channel, &strict_candidates(backend, conversation))
}

/// Who may change the channel's members today as far as the backend knows: its members with their
/// roles, and the space's admins outside it.
pub fn strict_candidates<B: ChatBackend + ?Sized>(
  backend: &B,
  conversation: ConversationId,
) -> Vec<StrictCandidate> {
  let roles = backend.channel_roles(conversation);
  let maybe_space = backend
    .conversation(conversation)
    .and_then(|info| info.maybe_space);
  let space_members = match maybe_space.map(|space| backend.space_members(space)) {
    Some(SpaceRoster::Loaded(members)) => members,
    _ => Vec::new(),
  };
  let is_space_admin = |user: UserId| {
    space_members
      .iter()
      .any(|member| member.user == user && member.role == SpaceRole::SpaceAdmin)
  };
  let mut candidates: Vec<StrictCandidate> = roles
    .iter()
    .map(|member| StrictCandidate {
      user: member.user,
      is_space_admin: is_space_admin(member.user),
      maybe_channel_role: Some(member.role.clone()),
      is_guest: backend.user(member.user).is_some_and(|found| found.is_guest),
    })
    .collect();
  for member in &space_members {
    let is_listed = candidates.iter().any(|candidate| candidate.user == member.user);
    if member.role == SpaceRole::SpaceAdmin && !member.is_bot && !is_listed {
      candidates.push(StrictCandidate {
        user: member.user,
        is_space_admin: true,
        maybe_channel_role: None,
        is_guest: member.is_guest,
      });
    }
  }
  candidates
}

/// Whether the person may change the channel's members today under the server's rules.
fn may_manage_members_today(scheme: &PermissionScheme, candidate: &StrictCandidate) -> bool {
  let actor = RoleActor {
    instance_role: Role::Member,
    is_space_admin: candidate.is_space_admin,
    maybe_own_role: candidate.maybe_channel_role.clone(),
    visibility: ConversationVisibility::Private,
    is_archived: false,
    maybe_granted: None,
  };
  can_in_channel(scheme, &actor, &Permission::ManageMembers)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  mod gates {
    use super::*;

    #[test]
    fn only_a_workspace_signer_with_the_servers_right_turns_it_on() {
      let channel = channel(false, &[]);
      assert!(StrictGates::new(&channel, actor(id(SIGNER), true, true)).can_turn_on);
      // The server refuses the roles commit's hint without `manage_channel_roles`.
      assert!(!StrictGates::new(&channel, actor(id(SIGNER), true, false)).can_turn_on);
      // A channel admin who is no workspace signer asks one (D3).
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, true)).can_turn_on);
      let outside = StrictChannel {
        has_group: false,
        ..channel.clone()
      };
      assert!(!StrictGates::new(&outside, actor(id(SIGNER), true, true)).can_turn_on);
    }

    #[test]
    fn a_strict_channel_is_turned_off_by_its_admins_and_signers_only() {
      let channel = channel(
        true,
        &[
          (id(ADMIN), StrictRole::Admin),
          (id(MODERATOR), StrictRole::Moderator),
        ],
      );
      assert!(StrictGates::new(&channel, actor(id(ADMIN), true, true)).can_turn_off);
      assert!(StrictGates::new(&channel, actor(id(SIGNER), true, true)).can_turn_off);
      assert!(!StrictGates::new(&channel, actor(id(MODERATOR), true, true)).can_turn_off);
      assert!(!StrictGates::new(&channel, actor(id(MEMBER), true, true)).can_turn_off);
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, false)).can_turn_off);
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, true)).can_turn_on);
    }

    #[test]
    fn after_a_stop_only_admins_with_the_rewind_right_continue() {
      let mut channel = channel(
        true,
        &[
          (id(ADMIN), StrictRole::Admin),
          (id(MODERATOR), StrictRole::Moderator),
        ],
      );
      channel.maybe_stop = Some(StrictStop {
        at_ms: 1,
        maybe_signer: Some(id(MEMBER)),
      });
      assert!(StrictGates::new(&channel, actor(id(ADMIN), false, true)).can_continue);
      assert!(StrictGates::new(&channel, actor(id(SIGNER), false, true)).can_continue);
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, false)).can_continue);
      assert!(!StrictGates::new(&channel, actor(id(MODERATOR), true, true)).can_continue);
      assert!(!StrictGates::new(&channel, actor(id(MEMBER), true, true)).can_continue);
      // Nothing else is offered while stopped: no commit of this device goes out.
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, true)).can_turn_off);
    }

    #[test]
    fn a_blocker_is_removable_only_while_one_waits() {
      let mut channel = channel(false, &[]);
      assert!(!StrictGates::new(&channel, actor(id(SIGNER), true, true)).can_remove_blocker);
      channel.maybe_readiness = Some(StrictReadiness {
        maybe_asked_by: Some(id(SIGNER)),
        waiting: vec![StrictDevice {
          user: id(MEMBER),
          device: id(9),
        }],
        refreshing: Vec::new(),
        is_ready: false,
      });
      assert!(StrictGates::new(&channel, actor(id(SIGNER), true, true)).can_remove_blocker);
      assert!(!StrictGates::new(&channel, actor(id(SIGNER), false, true)).can_remove_blocker);
      assert!(!StrictGates::new(&channel, actor(id(ADMIN), true, true)).can_remove_blocker);
    }

    #[test]
    fn nothing_is_offered_outside_a_known_strict_role_set() {
      let gates = StrictGates::new(&StrictChannel::default(), actor(id(MEMBER), true, true));
      assert_eq!(gates, StrictGates::default());
    }
  }

  mod losses {
    use super::*;

    #[test]
    fn space_admins_without_a_signed_role_and_moderators_lose_their_part() {
      let channel = channel(
        false,
        &[
          (id(ADMIN), StrictRole::Admin),
          (id(MODERATOR), StrictRole::Moderator),
        ],
      );
      let candidates = [
        candidate(id(ADMIN), false, Some(ChannelRole::Admin)),
        candidate(id(MODERATOR), false, Some(ChannelRole::Moderator)),
        candidate(id(SPACE_ADMIN), true, Some(ChannelRole::Member)),
        candidate(id(MEMBER), false, Some(ChannelRole::Member)),
        // A workspace signer keeps changing members (§4.6.2, "Who may do what").
        candidate(id(SIGNER), true, Some(ChannelRole::Member)),
      ];
      let losses = strict_losses(&PermissionScheme::default(), &channel, &candidates);
      assert_eq!(
        losses,
        vec![
          StrictLoss {
            user: id(MODERATOR),
            loss: StrictLossKind::AddingMembers,
          },
          StrictLoss {
            user: id(SPACE_ADMIN),
            loss: StrictLossKind::ChangingMembers,
          },
        ]
      );
    }

    #[test]
    fn an_admin_only_the_server_lists_loses_changing_members() {
      // The server lists them as admin, but no signed role carries them into the context.
      let channel = channel(false, &[(id(ADMIN), StrictRole::Admin)]);
      let candidates = [candidate(id(MEMBER), false, Some(ChannelRole::Admin))];
      let losses = strict_losses(&PermissionScheme::default(), &channel, &candidates);
      assert_eq!(
        losses,
        vec![StrictLoss {
          user: id(MEMBER),
          loss: StrictLossKind::ChangingMembers,
        }]
      );
    }

    #[test]
    fn guests_and_plain_members_lose_nothing() {
      let channel = channel(false, &[]);
      let mut guest = candidate(id(MEMBER), false, Some(ChannelRole::Member));
      guest.is_guest = true;
      let candidates = [guest, candidate(id(MODERATOR), false, Some(ChannelRole::Member))];
      assert!(strict_losses(&PermissionScheme::default(), &channel, &candidates).is_empty());
    }
  }

  const ADMIN: i64 = 1;
  const MODERATOR: i64 = 2;
  const MEMBER: i64 = 3;
  const SPACE_ADMIN: i64 = 4;
  const SIGNER: i64 = 5;

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn channel(is_strict: bool, roles: &[(UserId, StrictRole)]) -> StrictChannel {
    StrictChannel {
      has_group: true,
      is_strict,
      roles: roles
        .iter()
        .map(|(user, role)| StrictRoleHolder {
          user: *user,
          role: *role,
        })
        .collect(),
      workspace_signers: vec![id(SIGNER)],
      ..StrictChannel::default()
    }
  }

  fn actor(user: UserId, can_manage_members: bool, can_manage_channel_roles: bool) -> StrictActor {
    StrictActor {
      user,
      can_manage_members,
      can_manage_channel_roles,
    }
  }

  fn candidate(
    user: UserId,
    is_space_admin: bool,
    maybe_channel_role: Option<ChannelRole>,
  ) -> StrictCandidate {
    StrictCandidate {
      user,
      is_space_admin,
      maybe_channel_role,
      is_guest: false,
    }
  }
}
