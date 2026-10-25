//! Who may give a channel member which role (admin.md §1, §2.3). The UI uses it to offer only what
//! will work and the core to refuse early; the server enforces the same rules with the same
//! permission scheme ([`teamcraft_proto::is_allowed`]) and stays the last word on what is stored.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ChannelContext, ChannelModeration, ChannelRole, ConversationVisibility, Permission, PermissionScheme, Role,
  SpaceContext, SpaceRole, Target, is_allowed,
};

/// The roles a channel member can be given, most privileged first.
pub const ASSIGNABLE_CHANNEL_ROLES: [ChannelRole; 3] =
  [ChannelRole::Admin, ChannelRole::Moderator, ChannelRole::Member];

/// The signed-in user's standing, from which the permission to manage roles follows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleActor {
  /// Instance role: owners and admins come from the verified roster.
  pub instance_role: Role,
  pub is_space_admin: bool,
  /// `None`: not a member of the channel.
  pub maybe_own_role: Option<ChannelRole>,
}

/// The member whose role would change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleTarget {
  pub is_guest: bool,
  pub is_self: bool,
  pub current: ChannelRole,
}

/// Why a role change is not offered or is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RoleChangeError {
  #[error("missing the permission to manage channel roles")]
  NotAllowed,
  #[error("people can't change their own role")]
  OwnRole,
  #[error("guests can't be channel admins or moderators")]
  GuestRole,
  #[error("a channel needs at least one admin")]
  LastAdmin,
  #[error("unknown channel role")]
  UnknownRole,
}

/// Whether the actor holds `manage_channel_roles` in this channel (channel owners, space admins who
/// are in the channel, workspace admins).
pub fn can_manage_channel_roles(actor: &RoleActor) -> bool {
  let space_role = if actor.is_space_admin {
    SpaceRole::SpaceAdmin
  } else {
    SpaceRole::Member
  };
  let target = Target::Channel(ChannelContext {
    maybe_space: Some(SpaceContext {
      maybe_role: Some(space_role),
      maybe_space_id: None,
    }),
    // Private is the strict reading: a space admin outside the channel can't reach it.
    visibility: ConversationVisibility::Private,
    is_archived: false,
    maybe_role: actor.maybe_own_role.clone(),
    moderation: ChannelModeration::default(),
  });
  is_allowed(
    &PermissionScheme::default(),
    &actor.instance_role,
    &Permission::ManageChannelRoles,
    &target,
  )
}

/// Checks one role change; `admin_count` is the number of channel admins now.
pub fn check_channel_role_change(
  actor: &RoleActor,
  target: &RoleTarget,
  new_role: &ChannelRole,
  admin_count: usize,
) -> Result<(), RoleChangeError> {
  if matches!(new_role, ChannelRole::Unknown(_)) {
    return Err(RoleChangeError::UnknownRole);
  }
  if !can_manage_channel_roles(actor) {
    return Err(RoleChangeError::NotAllowed);
  }
  if target.is_self {
    return Err(RoleChangeError::OwnRole);
  }
  if target.is_guest && *new_role != ChannelRole::Member {
    return Err(RoleChangeError::GuestRole);
  }
  if target.current == ChannelRole::Admin && *new_role != ChannelRole::Admin && admin_count <= 1 {
    return Err(RoleChangeError::LastAdmin);
  }
  Ok(())
}

/// The roles the actor can offer for the target (empty when none of them works).
pub fn assignable_roles(actor: &RoleActor, target: &RoleTarget, admin_count: usize) -> Vec<ChannelRole> {
  ASSIGNABLE_CHANNEL_ROLES
    .iter()
    .filter(|role| check_channel_role_change(actor, target, role, admin_count).is_ok())
    .cloned()
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  const OTHERS: usize = 2;

  mod who_may_manage_roles {
    use super::*;

    #[test]
    fn channel_admins_workspace_admins_and_owners_may_the_rest_may_not() {
      assert!(can_manage_channel_roles(&actor(
        Role::Member,
        false,
        Some(ChannelRole::Admin)
      )));
      assert!(can_manage_channel_roles(&actor(Role::Admin, false, None)));
      assert!(can_manage_channel_roles(&actor(Role::Owner, false, None)));
      assert!(can_manage_channel_roles(&actor(
        Role::Member,
        true,
        Some(ChannelRole::Member)
      )));
      assert!(!can_manage_channel_roles(&actor(
        Role::Member,
        false,
        Some(ChannelRole::Moderator)
      )));
      assert!(!can_manage_channel_roles(&actor(
        Role::Member,
        false,
        Some(ChannelRole::Member)
      )));
      assert!(!can_manage_channel_roles(&actor(Role::Member, false, None)));
      assert!(!can_manage_channel_roles(&actor(
        Role::Guest,
        false,
        Some(ChannelRole::Admin)
      )));
    }

    #[test]
    fn a_space_admin_outside_a_private_channel_may_not() {
      assert!(!can_manage_channel_roles(&actor(Role::Member, true, None)));
    }
  }

  mod role_changes {
    use super::*;

    #[test]
    fn an_admin_can_offer_every_role_to_a_member() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      assert_eq!(
        assignable_roles(&admin, &member(ChannelRole::Member), OTHERS),
        ASSIGNABLE_CHANNEL_ROLES
      );
    }

    #[test]
    fn moderators_and_members_get_no_roles_to_offer() {
      for own in [ChannelRole::Moderator, ChannelRole::Member] {
        let actor = actor(Role::Member, false, Some(own));
        assert!(assignable_roles(&actor, &member(ChannelRole::Member), OTHERS).is_empty());
        assert_eq!(
          check_channel_role_change(
            &actor,
            &member(ChannelRole::Member),
            &ChannelRole::Moderator,
            OTHERS
          ),
          Err(RoleChangeError::NotAllowed)
        );
      }
    }

    #[test]
    fn guests_stay_members() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      let guest = RoleTarget {
        is_guest: true,
        ..member(ChannelRole::Member)
      };
      assert_eq!(assignable_roles(&admin, &guest, OTHERS), [ChannelRole::Member]);
      assert_eq!(
        check_channel_role_change(&admin, &guest, &ChannelRole::Moderator, OTHERS),
        Err(RoleChangeError::GuestRole)
      );
    }

    #[test]
    fn the_last_admin_cannot_be_demoted_but_one_of_two_can() {
      let admin = actor(Role::Admin, false, None);
      let target = member(ChannelRole::Admin);
      assert_eq!(
        check_channel_role_change(&admin, &target, &ChannelRole::Member, 1),
        Err(RoleChangeError::LastAdmin)
      );
      assert_eq!(
        check_channel_role_change(&admin, &target, &ChannelRole::Member, 2),
        Ok(())
      );
      assert_eq!(
        check_channel_role_change(&admin, &target, &ChannelRole::Admin, 1),
        Ok(())
      );
    }

    #[test]
    fn nobody_changes_their_own_role_and_unknown_roles_are_refused() {
      let admin = actor(Role::Owner, false, Some(ChannelRole::Admin));
      let own = RoleTarget {
        is_self: true,
        ..member(ChannelRole::Admin)
      };
      assert_eq!(
        check_channel_role_change(&admin, &own, &ChannelRole::Member, OTHERS),
        Err(RoleChangeError::OwnRole)
      );
      assert_eq!(
        check_channel_role_change(
          &admin,
          &member(ChannelRole::Member),
          &ChannelRole::Unknown("x".into()),
          OTHERS
        ),
        Err(RoleChangeError::UnknownRole)
      );
    }
  }

  fn actor(instance_role: Role, is_space_admin: bool, maybe_own_role: Option<ChannelRole>) -> RoleActor {
    RoleActor {
      instance_role,
      is_space_admin,
      maybe_own_role,
    }
  }

  fn member(current: ChannelRole) -> RoleTarget {
    RoleTarget {
      is_guest: false,
      is_self: false,
      current,
    }
  }
}
