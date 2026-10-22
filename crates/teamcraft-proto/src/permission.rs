//! Permission names and the central evaluator (PLAN.md §13, ARCHITECTURE.md §5.2).
//!
//! The model follows Mattermost: roles are granted permissions by a scheme at instance (or space)
//! scope; a channel can only *remove* permissions from its members and guests, never add any.
//! Effective permission = granted by the scheme **and** not removed at channel scope. Server and
//! clients call [`is_allowed`]; nothing else decides permissions.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationVisibility, Role, SpaceId};

wire_enum! {
  /// A thing a user may be allowed to do. The first block is the PLAN.md §13 list; the last three
  /// are space-level additions.
  pub enum Permission {
    CreatePost => "create_post",
    AddReaction => "add_reaction",
    RemoveOthersReactions => "remove_others_reactions",
    EditOthersPosts => "edit_others_posts",
    DeleteOthersPosts => "delete_others_posts",
    UseChannelMentions => "use_channel_mentions",
    UseGroupMentions => "use_group_mentions",
    ManageBookmarks => "manage_bookmarks",
    ManageMembers => "manage_members",
    ManageChannelRoles => "manage_channel_roles",
    ManageChannelProperties => "manage_channel_properties",
    ConvertChannelPrivacy => "convert_channel_privacy",
    CreateDm => "create_dm",
    CreateGroupDm => "create_group_dm",
    CreateChannel => "create_channel",
    CreateUserGroup => "create_user_group",
    ManageUserGroupMembers => "manage_user_group_members",
    InviteGuest => "invite_guest",
    PromoteGuest => "promote_guest",
    DemoteToGuest => "demote_to_guest",
    CreateEmoji => "create_emoji",
    DeleteOthersEmoji => "delete_others_emoji",
    ManageWebhooks => "manage_webhooks",
    CreateBot => "create_bot",
    UseSlashCommands => "use_slash_commands",
    CreateShareLink => "create_share_link",
    ReadAuditLog => "read_audit_log",
    ManageBranding => "manage_branding",
    CreateSpace => "create_space",
    ManageSpaceProperties => "manage_space_properties",
    ManageSpaceMembers => "manage_space_members",
    /// Administration: all spaces and channels (metadata), users, devices, workspace policy (read).
    ManageWorkspace => "manage_workspace",
    /// Deactivate users, remove devices.
    ManageUsers => "manage_users",
    /// Append to the signed admin roster and workspace policy. Owners only.
    SignWorkspace => "sign_workspace",
  }
}

wire_enum! {
  /// A role entry of a permission scheme. The first four are the instance roles.
  pub enum SchemeRole {
    Guest => "guest",
    Member => "member",
    Admin => "admin",
    Owner => "owner",
    SpaceAdmin => "space_admin",
    ChannelAdmin => "channel_admin",
    ChannelModerator => "channel_moderator",
  }
}

wire_enum! {
  /// Role inside a space.
  pub enum SpaceRole {
    Guest => "guest",
    Member => "member",
    SpaceAdmin => "space_admin",
  }
}

wire_enum! {
  /// Role inside a conversation: `Admin` is the channel owner, `Moderator` keeps order (removes
  /// members, deletes others' posts) without managing the channel itself.
  pub enum ChannelRole {
    Member => "member",
    Moderator => "moderator",
    Admin => "admin",
  }
}

/// Where a permission applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionScope {
  Instance,
  Space,
  /// Acts on content; requires channel membership (also for admins, who can't read channels).
  ChannelContent,
  /// Manages the channel itself; may be done by admins who are not members.
  ChannelManagement,
}

impl Permission {
  /// `None` for permissions this version doesn't know.
  pub fn scope(&self) -> Option<PermissionScope> {
    use Permission as P;
    use PermissionScope as S;
    Some(match self {
      P::CreateDm
      | P::CreateGroupDm
      | P::CreateUserGroup
      | P::ManageUserGroupMembers
      | P::PromoteGuest
      | P::DemoteToGuest
      | P::CreateEmoji
      | P::DeleteOthersEmoji
      | P::ManageWebhooks
      | P::CreateBot
      | P::UseSlashCommands
      | P::ReadAuditLog
      | P::ManageBranding
      | P::CreateSpace
      | P::ManageWorkspace
      | P::ManageUsers
      | P::SignWorkspace => S::Instance,
      P::CreateChannel | P::InviteGuest | P::ManageSpaceProperties | P::ManageSpaceMembers => S::Space,
      P::CreatePost
      | P::AddReaction
      | P::RemoveOthersReactions
      | P::EditOthersPosts
      | P::DeleteOthersPosts
      | P::UseChannelMentions
      | P::UseGroupMentions
      | P::ManageBookmarks
      | P::CreateShareLink => S::ChannelContent,
      P::ManageMembers | P::ManageChannelRoles | P::ManageChannelProperties | P::ConvertChannelPrivacy => {
        S::ChannelManagement
      },
      P::Unknown(_) => return None,
    })
  }

  /// Permissions a channel may remove from its members and guests (PLAN.md §13, "Channel moderation").
  pub fn is_channel_moderatable(&self) -> bool {
    matches!(
      self,
      Self::CreatePost
        | Self::AddReaction
        | Self::UseChannelMentions
        | Self::UseGroupMentions
        | Self::ManageBookmarks
        | Self::ManageMembers
    )
  }
}

/// Permissions granted per role. The default is the built-in scheme; a space can use its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionScheme {
  grants: HashMap<SchemeRole, HashSet<Permission>>,
}

impl PermissionScheme {
  pub fn new(grants: HashMap<SchemeRole, HashSet<Permission>>) -> Self {
    Self { grants }
  }

  pub fn grants(&self, role: &SchemeRole, permission: &Permission) -> bool {
    self.grants.get(role).is_some_and(|set| set.contains(permission))
  }

  /// What the role may do, sorted by name.
  pub fn granted_to(&self, role: &SchemeRole) -> Vec<Permission> {
    let mut granted: Vec<Permission> = self.grants.get(role).into_iter().flatten().cloned().collect();
    granted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    granted
  }

  /// A copy with grants added (`true`) and revoked (`false`).
  pub fn with_changes(&self, changes: &[(SchemeRole, Permission, bool)]) -> Self {
    let mut grants = self.grants.clone();
    for (role, permission, is_granted) in changes {
      let set = grants.entry(role.clone()).or_default();
      if *is_granted {
        set.insert(permission.clone());
      } else {
        set.remove(permission);
      }
    }
    Self { grants }
  }
}

/// Why a scheme change is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SchemeChangeError {
  #[error("the owner always has every permission")]
  OwnerIsFixed,
  #[error("only owners sign the roster and the policy")]
  SigningIsFixed,
  #[error("unknown role or permission")]
  Unknown,
  #[error("this permission cannot be granted to guests")]
  NotForGuests,
  #[error("this role or permission has no meaning inside a space")]
  NotInSpace,
}

/// Checks that granting or revoking `permission` for `role` is allowed at the instance or in a space.
pub fn check_scheme_change(
  role: &SchemeRole,
  permission: &Permission,
  is_grant: bool,
  is_in_space: bool,
) -> Result<(), SchemeChangeError> {
  let Some(scope) = permission.scope() else {
    return Err(SchemeChangeError::Unknown);
  };
  if matches!(role, SchemeRole::Unknown(_)) {
    return Err(SchemeChangeError::Unknown);
  }
  if *role == SchemeRole::Owner {
    return Err(SchemeChangeError::OwnerIsFixed);
  }
  if *permission == Permission::SignWorkspace {
    return Err(SchemeChangeError::SigningIsFixed);
  }
  if is_in_space
    && (matches!(role, SchemeRole::Admin | SchemeRole::Owner) || scope == PermissionScope::Instance)
  {
    return Err(SchemeChangeError::NotInSpace);
  }
  let is_guest_safe = matches!(scope, PermissionScope::ChannelContent)
    || matches!(permission, Permission::CreateDm | Permission::CreateGroupDm);
  if is_grant && *role == SchemeRole::Guest && !is_guest_safe {
    return Err(SchemeChangeError::NotForGuests);
  }
  Ok(())
}

impl Default for PermissionScheme {
  fn default() -> Self {
    use Permission as P;
    let guest = [P::CreatePost, P::AddReaction, P::CreateDm, P::CreateGroupDm];
    let member = [
      P::UseChannelMentions,
      P::UseGroupMentions,
      P::ManageBookmarks,
      P::CreateChannel,
      P::CreateEmoji,
      P::UseSlashCommands,
      P::CreateShareLink,
    ];
    let channel_admin = [
      P::ManageMembers,
      P::ManageChannelRoles,
      P::ManageChannelProperties,
      P::ConvertChannelPrivacy,
      P::DeleteOthersPosts,
      P::RemoveOthersReactions,
    ];
    let channel_moderator = [P::ManageMembers, P::DeleteOthersPosts, P::RemoveOthersReactions];
    let space_admin = [
      P::ManageSpaceProperties,
      P::ManageSpaceMembers,
      P::InviteGuest,
      P::CreateUserGroup,
      P::ManageUserGroupMembers,
    ];
    let everything = ALL_KNOWN.to_vec();

    let guest_set: HashSet<_> = guest.iter().cloned().collect();
    let member_set: HashSet<_> = guest_set.iter().cloned().chain(member).collect();
    let channel_admin_set: HashSet<_> = channel_admin.into_iter().collect();
    let channel_moderator_set: HashSet<_> = channel_moderator.into_iter().collect();
    let space_admin_set: HashSet<_> = channel_admin_set.iter().cloned().chain(space_admin).collect();
    let owner_set: HashSet<_> = everything.into_iter().collect();
    // Only owners sign the roster and the policy (admin.md §2.3).
    let admin_set: HashSet<_> = owner_set
      .iter()
      .filter(|p| **p != P::SignWorkspace)
      .cloned()
      .collect();

    Self::new(HashMap::from([
      (SchemeRole::Guest, guest_set),
      (SchemeRole::Member, member_set),
      (SchemeRole::SpaceAdmin, space_admin_set),
      (SchemeRole::ChannelAdmin, channel_admin_set),
      (SchemeRole::ChannelModerator, channel_moderator_set),
      (SchemeRole::Admin, admin_set),
      (SchemeRole::Owner, owner_set),
    ]))
  }
}

/// All permissions this version knows.
pub const ALL_KNOWN: [Permission; 34] = [
  Permission::CreatePost,
  Permission::AddReaction,
  Permission::RemoveOthersReactions,
  Permission::EditOthersPosts,
  Permission::DeleteOthersPosts,
  Permission::UseChannelMentions,
  Permission::UseGroupMentions,
  Permission::ManageBookmarks,
  Permission::ManageMembers,
  Permission::ManageChannelRoles,
  Permission::ManageChannelProperties,
  Permission::ConvertChannelPrivacy,
  Permission::CreateDm,
  Permission::CreateGroupDm,
  Permission::CreateChannel,
  Permission::CreateUserGroup,
  Permission::ManageUserGroupMembers,
  Permission::InviteGuest,
  Permission::PromoteGuest,
  Permission::DemoteToGuest,
  Permission::CreateEmoji,
  Permission::DeleteOthersEmoji,
  Permission::ManageWebhooks,
  Permission::CreateBot,
  Permission::UseSlashCommands,
  Permission::CreateShareLink,
  Permission::ReadAuditLog,
  Permission::ManageBranding,
  Permission::CreateSpace,
  Permission::ManageSpaceProperties,
  Permission::ManageSpaceMembers,
  Permission::ManageWorkspace,
  Permission::ManageUsers,
  Permission::SignWorkspace,
];

/// What a channel removes from its members and guests (set by channel admins, never grants).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelModeration {
  #[serde(default)]
  pub member_denied: HashSet<Permission>,
  #[serde(default)]
  pub guest_denied: HashSet<Permission>,
}

/// The actor's standing in a space; `None` role means not a member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpaceContext {
  pub maybe_role: Option<SpaceRole>,
  /// Which space it is, so the server can apply the space's own scheme.
  pub maybe_space_id: Option<SpaceId>,
}

/// The conversation a permission is evaluated for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelContext {
  pub maybe_space: Option<SpaceContext>,
  pub visibility: ConversationVisibility,
  pub is_archived: bool,
  /// `None`: the actor is not a member.
  pub maybe_role: Option<ChannelRole>,
  pub moderation: ChannelModeration,
}

/// What the permission is evaluated against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
  Instance,
  Space(SpaceContext),
  Channel(ChannelContext),
}

/// The one place that decides whether `instance_role` may use `permission` on `target`.
pub fn is_allowed(
  scheme: &PermissionScheme,
  instance_role: &Role,
  permission: &Permission,
  target: &Target,
) -> bool {
  let Some(scope) = permission.scope() else {
    return false;
  };
  let Some(base_role) = scheme_role(instance_role) else {
    return false;
  };

  let (maybe_space, maybe_channel) = match target {
    Target::Instance => (None, None),
    Target::Space(space) => (Some(space), None),
    Target::Channel(channel) => (channel.maybe_space.as_ref(), Some(channel)),
  };

  let is_instance_admin = matches!(instance_role, Role::Admin | Role::Owner);
  let is_guest = *instance_role == Role::Guest
    || maybe_space.is_some_and(|space| space.maybe_role == Some(SpaceRole::Guest));
  let is_space_admin = !is_guest && maybe_space.is_some_and(|s| s.maybe_role == Some(SpaceRole::SpaceAdmin));
  let is_channel_admin = !is_guest && maybe_channel.is_some_and(|c| c.maybe_role == Some(ChannelRole::Admin));

  let is_channel_moderator =
    !is_guest && maybe_channel.is_some_and(|c| c.maybe_role == Some(ChannelRole::Moderator));

  let effective_base = if is_guest { SchemeRole::Guest } else { base_role };
  let is_granted = scheme.grants(&effective_base, permission)
    || (is_space_admin && scheme.grants(&SchemeRole::SpaceAdmin, permission))
    || (is_channel_admin && scheme.grants(&SchemeRole::ChannelAdmin, permission))
    || (is_channel_moderator && scheme.grants(&SchemeRole::ChannelModerator, permission));
  if !is_granted {
    return false;
  }

  match scope {
    PermissionScope::Instance => true,
    PermissionScope::Space => match target {
      Target::Instance => false,
      _ => is_instance_admin || maybe_space.is_some_and(|space| space.maybe_role.is_some()),
    },
    PermissionScope::ChannelContent | PermissionScope::ChannelManagement => {
      let Some(channel) = maybe_channel else {
        return false;
      };
      let is_member = channel.maybe_role.is_some();
      let may_reach = match scope {
        PermissionScope::ChannelContent => is_member && !channel.is_archived,
        _ => {
          is_member
            || is_instance_admin
            || (is_space_admin && channel.visibility == ConversationVisibility::Public)
        },
      };
      let is_exempt_from_moderation =
        is_instance_admin || is_space_admin || is_channel_admin || is_channel_moderator;
      let is_denied_by_channel = permission.is_channel_moderatable()
        && !is_exempt_from_moderation
        && if is_guest {
          channel.moderation.guest_denied.contains(permission)
        } else {
          channel.moderation.member_denied.contains(permission)
        };
      may_reach && !is_denied_by_channel
    },
  }
}

fn scheme_role(role: &Role) -> Option<SchemeRole> {
  match role {
    Role::Guest => Some(SchemeRole::Guest),
    Role::Member => Some(SchemeRole::Member),
    Role::Admin => Some(SchemeRole::Admin),
    Role::Owner => Some(SchemeRole::Owner),
    Role::Unknown(_) => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const GUEST_ALLOWED: &[Permission] = &[
    Permission::CreatePost,
    Permission::AddReaction,
    Permission::CreateDm,
    Permission::CreateGroupDm,
  ];

  const MEMBER_EXTRA: &[Permission] = &[
    Permission::UseChannelMentions,
    Permission::UseGroupMentions,
    Permission::ManageBookmarks,
    Permission::CreateChannel,
    Permission::CreateEmoji,
    Permission::UseSlashCommands,
    Permission::CreateShareLink,
  ];

  mod channel_moderator {
    use super::*;

    fn channel(role: Option<ChannelRole>) -> Target {
      Target::Channel(ChannelContext {
        maybe_space: Some(SpaceContext {
          maybe_role: Some(SpaceRole::Member),
          maybe_space_id: None,
        }),
        visibility: ConversationVisibility::Private,
        is_archived: false,
        maybe_role: role,
        moderation: ChannelModeration {
          member_denied: HashSet::from([Permission::ManageMembers, Permission::CreatePost]),
          guest_denied: HashSet::new(),
        },
      })
    }

    fn allowed(role: Option<ChannelRole>, permission: Permission) -> bool {
      is_allowed(
        &PermissionScheme::default(),
        &Role::Member,
        &permission,
        &channel(role),
      )
    }

    #[test]
    fn a_moderator_removes_members_and_posts_but_does_not_manage_the_channel() {
      let moderator = Some(ChannelRole::Moderator);
      assert!(allowed(moderator.clone(), Permission::ManageMembers));
      assert!(allowed(moderator.clone(), Permission::DeleteOthersPosts));
      assert!(allowed(moderator.clone(), Permission::RemoveOthersReactions));
      assert!(
        allowed(moderator.clone(), Permission::CreatePost),
        "not bound by the channel's limits"
      );
      for permission in [
        Permission::ManageChannelRoles,
        Permission::ManageChannelProperties,
        Permission::ConvertChannelPrivacy,
      ] {
        assert!(!allowed(moderator.clone(), permission.clone()), "{permission:?}");
      }
    }

    #[test]
    fn plain_members_have_none_of_it_and_admins_have_all() {
      assert!(!allowed(Some(ChannelRole::Member), Permission::DeleteOthersPosts));
      assert!(!allowed(Some(ChannelRole::Member), Permission::ManageMembers));
      assert!(allowed(
        Some(ChannelRole::Admin),
        Permission::ConvertChannelPrivacy
      ));
      assert!(!allowed(None, Permission::DeleteOthersPosts));
    }
  }

  mod scheme_changes {
    use super::*;

    #[test]
    fn changes_grant_and_revoke_on_a_copy() {
      let base = PermissionScheme::default();
      let changed = base.with_changes(&[
        (SchemeRole::Member, Permission::CreateUserGroup, true),
        (SchemeRole::Member, Permission::CreateChannel, false),
      ]);
      assert!(changed.grants(&SchemeRole::Member, &Permission::CreateUserGroup));
      assert!(!changed.grants(&SchemeRole::Member, &Permission::CreateChannel));
      assert!(!base.grants(&SchemeRole::Member, &Permission::CreateUserGroup));
      assert!(base.grants(&SchemeRole::Member, &Permission::CreateChannel));
      assert!(
        changed
          .granted_to(&SchemeRole::Member)
          .windows(2)
          .all(|w| w[0].as_str() <= w[1].as_str())
      );
    }

    #[test]
    fn owners_signing_guest_privileges_and_instance_permissions_in_spaces_are_protected() {
      use SchemeChangeError as E;
      let check =
        |role, permission, grant, in_space| check_scheme_change(&role, &permission, grant, in_space);
      assert_eq!(
        check(SchemeRole::Owner, Permission::CreatePost, false, false),
        Err(E::OwnerIsFixed)
      );
      assert_eq!(
        check(SchemeRole::Admin, Permission::SignWorkspace, true, false),
        Err(E::SigningIsFixed)
      );
      assert_eq!(
        check(SchemeRole::Guest, Permission::CreateChannel, true, false),
        Err(E::NotForGuests)
      );
      assert_eq!(
        check(SchemeRole::Guest, Permission::InviteGuest, true, false),
        Err(E::NotForGuests)
      );
      assert_eq!(
        check(SchemeRole::Guest, Permission::UseChannelMentions, true, false),
        Ok(())
      );
      assert_eq!(
        check(SchemeRole::Guest, Permission::CreateChannel, false, false),
        Ok(())
      );
      assert_eq!(
        check(SchemeRole::Member, Permission::CreateUserGroup, true, true),
        Err(E::NotInSpace)
      );
      assert_eq!(
        check(SchemeRole::Admin, Permission::CreatePost, true, true),
        Err(E::NotInSpace)
      );
      assert_eq!(
        check(SchemeRole::Member, Permission::CreateChannel, false, true),
        Ok(())
      );
      assert_eq!(
        check(
          SchemeRole::Unknown("x".into()),
          Permission::CreatePost,
          true,
          false
        ),
        Err(E::Unknown)
      );
      assert_eq!(
        check(SchemeRole::Member, Permission::Unknown("x".into()), true, false),
        Err(E::Unknown)
      );
    }
  }

  mod roles_in_a_plain_channel_they_belong_to {
    use super::*;

    #[test]
    fn guest_gets_exactly_the_guest_permissions() {
      let allowed = allowed_for(&Role::Guest, &member_channel(), Some(guest_space()));
      assert_eq!(allowed, set(GUEST_ALLOWED));
    }

    #[test]
    fn member_gets_guest_plus_member_permissions() {
      let allowed = allowed_for(&Role::Member, &member_channel(), Some(member_space()));
      let expected: HashSet<_> = GUEST_ALLOWED.iter().chain(MEMBER_EXTRA).cloned().collect();
      assert_eq!(allowed, expected);
    }

    #[test]
    fn owners_get_everything_and_admins_everything_but_signing() {
      let owner = allowed_for(&Role::Owner, &member_channel(), Some(member_space()));
      assert_eq!(owner, set(&ALL_KNOWN));
      let admin = allowed_for(&Role::Admin, &member_channel(), Some(member_space()));
      let mut expected = set(&ALL_KNOWN);
      expected.remove(&Permission::SignWorkspace);
      assert_eq!(admin, expected);
    }

    #[test]
    fn guest_never_gets_a_forbidden_permission() {
      for permission in [
        Permission::CreateChannel,
        Permission::InviteGuest,
        Permission::CreateUserGroup,
        Permission::ManageMembers,
        Permission::PromoteGuest,
        Permission::CreateSpace,
        Permission::ReadAuditLog,
        Permission::UseChannelMentions,
      ] {
        assert!(!check(
          &Role::Guest,
          &permission,
          &channel_target(Some(guest_space()), member_channel_ctx())
        ));
      }
    }
  }

  mod elevated_space_and_channel_roles {
    use super::*;

    #[test]
    fn space_admin_manages_the_space_and_its_channels() {
      let space = SpaceContext {
        maybe_space_id: None,
        maybe_role: Some(SpaceRole::SpaceAdmin),
      };
      for permission in [
        Permission::ManageSpaceProperties,
        Permission::ManageSpaceMembers,
        Permission::InviteGuest,
        Permission::CreateUserGroup,
      ] {
        assert!(
          check(&Role::Member, &permission, &Target::Space(space.clone())),
          "{permission}"
        );
      }
      assert!(!check(
        &Role::Member,
        &Permission::ReadAuditLog,
        &Target::Space(space.clone())
      ));
      assert!(!check(
        &Role::Member,
        &Permission::ManageBranding,
        &Target::Instance
      ));
    }

    #[test]
    fn space_admin_manages_public_but_not_foreign_private_channels() {
      let space = SpaceContext {
        maybe_space_id: None,
        maybe_role: Some(SpaceRole::SpaceAdmin),
      };
      let public = non_member_channel(Some(space.clone()), ConversationVisibility::Public);
      let private = non_member_channel(Some(space), ConversationVisibility::Private);
      assert!(check(
        &Role::Member,
        &Permission::ManageMembers,
        &Target::Channel(public.clone())
      ));
      assert!(!check(
        &Role::Member,
        &Permission::ManageMembers,
        &Target::Channel(private)
      ));
      // Managing is not reading: no posting without membership.
      assert!(!check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(public)
      ));
    }

    #[test]
    fn channel_admin_manages_only_their_channel() {
      let mut ctx = member_channel_ctx();
      ctx.maybe_role = Some(ChannelRole::Admin);
      for permission in [
        Permission::ManageMembers,
        Permission::ManageChannelRoles,
        Permission::ManageChannelProperties,
        Permission::ConvertChannelPrivacy,
        Permission::DeleteOthersPosts,
      ] {
        assert!(
          check(&Role::Member, &permission, &Target::Channel(ctx.clone())),
          "{permission}"
        );
      }
      assert!(!check(
        &Role::Member,
        &Permission::InviteGuest,
        &Target::Channel(ctx)
      ));
    }

    #[test]
    fn guest_cannot_gain_privileges_from_admin_roles() {
      let mut ctx = member_channel_ctx();
      ctx.maybe_role = Some(ChannelRole::Admin);
      ctx.maybe_space = Some(SpaceContext {
        maybe_space_id: None,
        maybe_role: Some(SpaceRole::SpaceAdmin),
      });
      assert!(!check(
        &Role::Guest,
        &Permission::ManageMembers,
        &Target::Channel(ctx.clone())
      ));
      assert!(!check(
        &Role::Guest,
        &Permission::InviteGuest,
        &Target::Space(ctx.maybe_space.clone().unwrap())
      ));
    }

    #[test]
    fn space_guest_of_an_employee_account_is_treated_as_guest() {
      let space = SpaceContext {
        maybe_space_id: None,
        maybe_role: Some(SpaceRole::Guest),
      };
      assert!(!check(
        &Role::Member,
        &Permission::CreateChannel,
        &Target::Space(space)
      ));
    }
  }

  mod instance_admins {
    use super::*;

    #[test]
    fn manage_private_channels_without_being_members() {
      let ctx = non_member_channel(Some(member_space()), ConversationVisibility::Private);
      for permission in [
        Permission::ManageMembers,
        Permission::ManageChannelRoles,
        Permission::ManageChannelProperties,
        Permission::ConvertChannelPrivacy,
      ] {
        assert!(
          check(&Role::Admin, &permission, &Target::Channel(ctx.clone())),
          "{permission}"
        );
      }
    }

    #[test]
    fn cannot_read_or_post_without_membership() {
      let ctx = non_member_channel(Some(member_space()), ConversationVisibility::Private);
      for permission in [
        Permission::CreatePost,
        Permission::AddReaction,
        Permission::DeleteOthersPosts,
      ] {
        assert!(
          !check(&Role::Owner, &permission, &Target::Channel(ctx.clone())),
          "{permission}"
        );
      }
    }

    #[test]
    fn manage_spaces_they_are_not_members_of() {
      let space = SpaceContext {
        maybe_role: None,
        maybe_space_id: None,
      };
      assert!(check(
        &Role::Admin,
        &Permission::ManageSpaceMembers,
        &Target::Space(space.clone())
      ));
      assert!(!check(
        &Role::Member,
        &Permission::ManageSpaceMembers,
        &Target::Space(space)
      ));
    }
  }

  mod channel_restrictions {
    use super::*;

    #[test]
    fn channel_can_remove_permissions_from_members_and_guests_separately() {
      let mut ctx = member_channel_ctx();
      ctx.moderation.member_denied = set(&[Permission::CreatePost]);
      ctx.moderation.guest_denied = set(&[Permission::AddReaction]);

      assert!(!check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(ctx.clone())
      ));
      assert!(check(
        &Role::Member,
        &Permission::AddReaction,
        &Target::Channel(ctx.clone())
      ));
      let mut guest_ctx = ctx;
      guest_ctx.maybe_space = Some(guest_space());
      assert!(check(
        &Role::Guest,
        &Permission::CreatePost,
        &Target::Channel(guest_ctx.clone())
      ));
      assert!(!check(
        &Role::Guest,
        &Permission::AddReaction,
        &Target::Channel(guest_ctx)
      ));
    }

    #[test]
    fn announcement_channel_still_lets_admins_post() {
      let mut ctx = member_channel_ctx();
      ctx.moderation.member_denied = set(&[Permission::CreatePost]);
      ctx.moderation.guest_denied = set(&[Permission::CreatePost]);

      let mut channel_admin = ctx.clone();
      channel_admin.maybe_role = Some(ChannelRole::Admin);
      assert!(check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(channel_admin)
      ));
      assert!(check(
        &Role::Admin,
        &Permission::CreatePost,
        &Target::Channel(ctx.clone())
      ));
      assert!(!check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(ctx)
      ));
    }

    #[test]
    fn channel_can_never_grant() {
      // Listing a permission as "denied" for nobody adds nothing, and non-moderatable entries are ignored.
      let mut ctx = member_channel_ctx();
      ctx.moderation.member_denied = set(&[Permission::CreateChannel, Permission::DeleteOthersPosts]);
      assert!(!check(
        &Role::Member,
        &Permission::DeleteOthersPosts,
        &Target::Channel(ctx.clone())
      ));
      assert!(!check(
        &Role::Guest,
        &Permission::UseChannelMentions,
        &Target::Channel(ctx)
      ));
    }

    #[test]
    fn archived_channels_are_read_only_but_can_be_managed() {
      let mut ctx = member_channel_ctx();
      ctx.is_archived = true;
      ctx.maybe_role = Some(ChannelRole::Admin);
      assert!(!check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(ctx.clone())
      ));
      assert!(!check(
        &Role::Admin,
        &Permission::AddReaction,
        &Target::Channel(ctx.clone())
      ));
      assert!(check(
        &Role::Member,
        &Permission::ManageChannelProperties,
        &Target::Channel(ctx)
      ));
    }

    #[test]
    fn non_members_cannot_use_content_permissions() {
      let ctx = non_member_channel(Some(member_space()), ConversationVisibility::Public);
      assert!(!check(
        &Role::Member,
        &Permission::CreatePost,
        &Target::Channel(ctx)
      ));
    }
  }

  mod unknown_values {
    use super::*;

    #[test]
    fn unknown_roles_and_permissions_are_denied() {
      let target = channel_target(Some(member_space()), member_channel_ctx());
      assert!(!check(
        &Role::Unknown("auditor".into()),
        &Permission::CreatePost,
        &target
      ));
      assert!(!check(
        &Role::Owner,
        &Permission::Unknown("teleport".into()),
        &target
      ));
    }

    #[test]
    fn scope_mismatch_is_denied() {
      assert!(!check(&Role::Owner, &Permission::CreatePost, &Target::Instance));
      assert!(!check(
        &Role::Owner,
        &Permission::CreateChannel,
        &Target::Instance
      ));
    }

    #[test]
    fn custom_scheme_replaces_defaults() {
      let scheme = PermissionScheme::new(HashMap::from([(
        SchemeRole::Member,
        set(&[Permission::CreatePost]),
      )]));
      let target = channel_target(Some(member_space()), member_channel_ctx());
      assert!(is_allowed(
        &scheme,
        &Role::Member,
        &Permission::CreatePost,
        &target
      ));
      assert!(!is_allowed(
        &scheme,
        &Role::Member,
        &Permission::AddReaction,
        &target
      ));
    }

    #[test]
    fn moderation_round_trips_through_json() {
      let moderation = ChannelModeration {
        member_denied: set(&[Permission::CreatePost]),
        guest_denied: HashSet::new(),
      };
      let json = serde_json::to_string(&moderation).unwrap();
      assert_eq!(
        serde_json::from_str::<ChannelModeration>(&json).unwrap(),
        moderation
      );
      let with_unknown: ChannelModeration = serde_json::from_str(r#"{"member_denied":["fly"]}"#).unwrap();
      assert!(
        with_unknown
          .member_denied
          .contains(&Permission::Unknown("fly".into()))
      );
    }
  }

  fn check(role: &Role, permission: &Permission, target: &Target) -> bool {
    is_allowed(&PermissionScheme::default(), role, permission, target)
  }

  /// Every known permission the role is granted for a channel it is an ordinary member of.
  fn allowed_for(
    role: &Role,
    ctx: &ChannelContext,
    maybe_space: Option<SpaceContext>,
  ) -> HashSet<Permission> {
    let mut channel = ctx.clone();
    channel.maybe_space = maybe_space.clone();
    let channel_target = Target::Channel(channel);
    ALL_KNOWN
      .iter()
      .filter(|permission| {
        let target = match permission.scope() {
          Some(PermissionScope::Instance) => Target::Instance,
          Some(PermissionScope::Space) => Target::Space(maybe_space.clone().unwrap_or(SpaceContext {
            maybe_role: None,
            maybe_space_id: None,
          })),
          _ => channel_target.clone(),
        };
        check(role, permission, &target)
      })
      .cloned()
      .collect()
  }

  fn set(permissions: &[Permission]) -> HashSet<Permission> {
    permissions.iter().cloned().collect()
  }

  fn member_space() -> SpaceContext {
    SpaceContext {
      maybe_space_id: None,
      maybe_role: Some(SpaceRole::Member),
    }
  }

  fn guest_space() -> SpaceContext {
    SpaceContext {
      maybe_space_id: None,
      maybe_role: Some(SpaceRole::Guest),
    }
  }

  fn member_channel_ctx() -> ChannelContext {
    ChannelContext {
      maybe_space: Some(member_space()),
      visibility: ConversationVisibility::Private,
      is_archived: false,
      maybe_role: Some(ChannelRole::Member),
      moderation: ChannelModeration::default(),
    }
  }

  fn member_channel() -> ChannelContext {
    member_channel_ctx()
  }

  fn non_member_channel(
    maybe_space: Option<SpaceContext>,
    visibility: ConversationVisibility,
  ) -> ChannelContext {
    ChannelContext {
      maybe_space,
      visibility,
      maybe_role: None,
      ..member_channel_ctx()
    }
  }

  fn channel_target(maybe_space: Option<SpaceContext>, mut ctx: ChannelContext) -> Target {
    ctx.maybe_space = maybe_space;
    Target::Channel(ctx)
  }
}
