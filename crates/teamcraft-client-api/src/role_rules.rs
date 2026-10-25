//! What the signed-in user may do in a channel, and who may give a channel member which role
//! (admin.md §1, §2.3). The UI uses it to offer only what will work and the core to refuse early;
//! the server enforces the same rules with the same permission scheme ([`teamcraft_proto::is_allowed`])
//! and stays the last word on what is stored.
//!
//! One helper, [`can_in_channel`], answers "may I do X in channel Y" for every channel setting
//! (roles, picture, ...), so the UI gates are the server's rules and not copies of them.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ChannelContext, ChannelModeration, ChannelRole, ConversationVisibility, Permission, PermissionScheme,
  PermissionSchemeResponse, Role, SchemeRole, SpaceContext, SpaceRole, Target, is_allowed,
};

/// The roles a channel member can be given, most privileged first.
pub const ASSIGNABLE_CHANNEL_ROLES: [ChannelRole; 3] =
  [ChannelRole::Admin, ChannelRole::Moderator, ChannelRole::Member];

/// The signed-in user's standing in a channel, from which the permissions follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleActor {
  /// Instance role: owners and admins come from the verified roster.
  pub instance_role: Role,
  pub is_space_admin: bool,
  /// `None`: not a member of the channel.
  pub maybe_own_role: Option<ChannelRole>,
  /// Decides whether a space admin outside the channel can reach it.
  pub visibility: ConversationVisibility,
  pub is_archived: bool,
}

/// Where a person stands in the channel's space, as far as this device knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpaceStanding {
  Member,
  Guest,
  /// Not in the space, so not a possible channel admin or moderator.
  NotMember,
  /// The space's member list isn't loaded or visible; the server decides.
  Unknown,
}

/// The member whose role would change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleTarget {
  /// A guest account of the instance.
  pub is_guest: bool,
  pub space: SpaceStanding,
  pub is_self: bool,
  pub current: ChannelRole,
}

/// Why a role change is not offered or is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RoleChangeError {
  #[error("missing the permission to manage channel roles")]
  NotAllowed,
  #[error("roles of an archived channel can't be changed")]
  Archived,
  #[error("people can't change their own role")]
  OwnRole,
  #[error("guests can't be channel admins or moderators")]
  GuestRole,
  #[error("only people in the channel's space can be channel admins or moderators")]
  NotInSpace,
  #[error("a channel needs at least one admin")]
  LastAdmin,
  #[error("unknown channel role")]
  UnknownRole,
}

/// The scheme in force in a space from the server's answer. People who may not read the admin
/// scheme get an empty answer; for them this is the built-in scheme, and the server still enforces
/// whatever the admins changed.
pub fn scheme_of(response: &PermissionSchemeResponse) -> PermissionScheme {
  if response.roles.is_empty() {
    return PermissionScheme::default();
  }
  let grants = response
    .roles
    .iter()
    .map(|role| (role.role.clone(), role.granted.iter().cloned().collect()))
    .collect::<std::collections::HashMap<SchemeRole, std::collections::HashSet<Permission>>>();
  PermissionScheme::new(grants)
}

/// Whether the actor holds `permission` in this channel.
pub fn can_in_channel(scheme: &PermissionScheme, actor: &RoleActor, permission: &Permission) -> bool {
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
    visibility: actor.visibility.clone(),
    is_archived: actor.is_archived,
    maybe_role: actor.maybe_own_role.clone(),
    moderation: ChannelModeration::default(),
  });
  is_allowed(scheme, &actor.instance_role, permission, &target)
}

/// `manage_channel_roles` in this channel.
pub fn can_manage_channel_roles(scheme: &PermissionScheme, actor: &RoleActor) -> bool {
  can_in_channel(scheme, actor, &Permission::ManageChannelRoles)
}

/// Checks one role change; `admin_count` is the number of channel admins now.
pub fn check_channel_role_change(
  scheme: &PermissionScheme,
  actor: &RoleActor,
  target: &RoleTarget,
  new_role: &ChannelRole,
  admin_count: usize,
) -> Result<(), RoleChangeError> {
  if matches!(new_role, ChannelRole::Unknown(_)) {
    return Err(RoleChangeError::UnknownRole);
  }
  if !can_manage_channel_roles(scheme, actor) {
    return Err(RoleChangeError::NotAllowed);
  }
  if actor.is_archived {
    return Err(RoleChangeError::Archived);
  }
  if target.is_self {
    return Err(RoleChangeError::OwnRole);
  }
  if *new_role != ChannelRole::Member {
    if target.is_guest || target.space == SpaceStanding::Guest {
      return Err(RoleChangeError::GuestRole);
    }
    if target.space == SpaceStanding::NotMember {
      return Err(RoleChangeError::NotInSpace);
    }
  }
  if target.current == ChannelRole::Admin && *new_role != ChannelRole::Admin && admin_count <= 1 {
    return Err(RoleChangeError::LastAdmin);
  }
  Ok(())
}

/// The roles the actor can offer for the target (empty when none of them works).
pub fn assignable_roles(
  scheme: &PermissionScheme,
  actor: &RoleActor,
  target: &RoleTarget,
  admin_count: usize,
) -> Vec<ChannelRole> {
  ASSIGNABLE_CHANNEL_ROLES
    .iter()
    .filter(|role| check_channel_role_change(scheme, actor, target, role, admin_count).is_ok())
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
      assert!(can(&actor(Role::Member, false, Some(ChannelRole::Admin))));
      assert!(can(&actor(Role::Admin, false, None)));
      assert!(can(&actor(Role::Owner, false, None)));
      assert!(can(&actor(Role::Member, true, Some(ChannelRole::Member))));
      assert!(!can(&actor(Role::Member, false, Some(ChannelRole::Moderator))));
      assert!(!can(&actor(Role::Member, false, Some(ChannelRole::Member))));
      assert!(!can(&actor(Role::Member, false, None)));
      assert!(!can(&actor(Role::Guest, false, Some(ChannelRole::Admin))));
    }

    #[test]
    fn a_space_admin_outside_a_channel_reaches_public_ones_only() {
      let mut outside = actor(Role::Member, true, None);
      assert!(!can(&outside), "private");
      outside.visibility = ConversationVisibility::Public;
      assert!(can(&outside), "public");
    }

    #[test]
    fn the_spaces_own_scheme_decides_not_the_built_in_one() {
      let response = PermissionSchemeResponse {
        roles: vec![teamcraft_proto::SchemeRoleResponse {
          role: SchemeRole::ChannelAdmin,
          granted: vec![Permission::ManageMembers],
          overrides: Vec::new(),
        }],
      };
      let custom = scheme_of(&response);
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      assert!(!can_manage_channel_roles(&custom, &admin));
      assert!(can_in_channel(&custom, &admin, &Permission::ManageMembers));
      // No answer (people who may not read the scheme): the built-in one.
      let fallback = scheme_of(&PermissionSchemeResponse { roles: Vec::new() });
      assert!(can_manage_channel_roles(&fallback, &admin));
    }
  }

  mod role_changes {
    use super::*;

    #[test]
    fn an_admin_can_offer_every_role_to_a_member() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      assert_eq!(
        assignable_roles(&scheme(), &admin, &member(ChannelRole::Member), OTHERS),
        ASSIGNABLE_CHANNEL_ROLES
      );
    }

    #[test]
    fn moderators_and_members_get_no_roles_to_offer() {
      for own in [ChannelRole::Moderator, ChannelRole::Member] {
        let actor = actor(Role::Member, false, Some(own));
        assert!(assignable_roles(&scheme(), &actor, &member(ChannelRole::Member), OTHERS).is_empty());
        assert_eq!(
          check(
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
    fn a_moderator_as_target_can_be_demoted_by_an_admin_and_keeps_the_role_when_unchanged() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      let target = member(ChannelRole::Moderator);
      assert_eq!(check(&admin, &target, &ChannelRole::Member, OTHERS), Ok(()));
      assert_eq!(check(&admin, &target, &ChannelRole::Admin, OTHERS), Ok(()));
      // Demoting the moderator doesn't touch the admin count rule.
      assert_eq!(check(&admin, &target, &ChannelRole::Member, 1), Ok(()));
    }

    #[test]
    fn guests_of_the_instance_or_the_space_stay_members() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      for guest in [
        RoleTarget {
          is_guest: true,
          ..member(ChannelRole::Member)
        },
        RoleTarget {
          space: SpaceStanding::Guest,
          ..member(ChannelRole::Member)
        },
      ] {
        assert_eq!(
          assignable_roles(&scheme(), &admin, &guest, OTHERS),
          [ChannelRole::Member]
        );
        assert_eq!(
          check(&admin, &guest, &ChannelRole::Moderator, OTHERS),
          Err(RoleChangeError::GuestRole)
        );
      }
    }

    #[test]
    fn people_outside_the_channels_space_cannot_get_a_role_and_unknown_standing_leaves_it_to_the_server() {
      let admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      let outsider = RoleTarget {
        space: SpaceStanding::NotMember,
        ..member(ChannelRole::Member)
      };
      assert_eq!(
        check(&admin, &outsider, &ChannelRole::Admin, OTHERS),
        Err(RoleChangeError::NotInSpace)
      );
      let unknown = RoleTarget {
        space: SpaceStanding::Unknown,
        ..member(ChannelRole::Member)
      };
      assert_eq!(check(&admin, &unknown, &ChannelRole::Admin, OTHERS), Ok(()));
    }

    #[test]
    fn roles_of_an_archived_channel_stay_as_they_are() {
      let mut admin = actor(Role::Member, false, Some(ChannelRole::Admin));
      admin.is_archived = true;
      assert_eq!(
        check(
          &admin,
          &member(ChannelRole::Member),
          &ChannelRole::Moderator,
          OTHERS
        ),
        Err(RoleChangeError::Archived)
      );
    }

    #[test]
    fn the_last_admin_cannot_be_demoted_but_one_of_two_can() {
      let admin = actor(Role::Admin, false, None);
      let target = member(ChannelRole::Admin);
      assert_eq!(
        check(&admin, &target, &ChannelRole::Member, 1),
        Err(RoleChangeError::LastAdmin)
      );
      assert_eq!(check(&admin, &target, &ChannelRole::Member, 2), Ok(()));
      assert_eq!(check(&admin, &target, &ChannelRole::Admin, 1), Ok(()));
    }

    #[test]
    fn nobody_changes_their_own_role_and_unknown_roles_are_refused() {
      let admin = actor(Role::Owner, false, Some(ChannelRole::Admin));
      let own = RoleTarget {
        is_self: true,
        ..member(ChannelRole::Admin)
      };
      assert_eq!(
        check(&admin, &own, &ChannelRole::Member, OTHERS),
        Err(RoleChangeError::OwnRole)
      );
      assert_eq!(
        check(
          &admin,
          &member(ChannelRole::Member),
          &ChannelRole::Unknown("x".into()),
          OTHERS
        ),
        Err(RoleChangeError::UnknownRole)
      );
    }
  }

  fn scheme() -> PermissionScheme {
    PermissionScheme::default()
  }

  fn can(actor: &RoleActor) -> bool {
    can_manage_channel_roles(&scheme(), actor)
  }

  fn check(
    actor: &RoleActor,
    target: &RoleTarget,
    new_role: &ChannelRole,
    admin_count: usize,
  ) -> Result<(), RoleChangeError> {
    check_channel_role_change(&scheme(), actor, target, new_role, admin_count)
  }

  fn actor(instance_role: Role, is_space_admin: bool, maybe_own_role: Option<ChannelRole>) -> RoleActor {
    RoleActor {
      instance_role,
      is_space_admin,
      maybe_own_role,
      visibility: ConversationVisibility::Private,
      is_archived: false,
    }
  }

  fn member(current: ChannelRole) -> RoleTarget {
    RoleTarget {
      is_guest: false,
      space: SpaceStanding::Member,
      is_self: false,
      current,
    }
  }
}
