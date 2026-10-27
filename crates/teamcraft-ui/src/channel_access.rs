//! What the signed-in user may do in a conversation, for every action that depends on it (issue
//! #691: an action the user can't take is not offered). The server says which channel permissions
//! the user holds ([`crate::model::Conversation::maybe_permissions`]); without that answer (an
//! older server) the permission rules of `teamcraft_client_api::role_rules` are fed with what this
//! device knows (the instance role, the space admin flag, the own channel role and the space's
//! permission scheme). The server enforces the same rules on every request; this only keeps the UI
//! from offering what will be refused.

use leptos::prelude::*;
use teamcraft_client_api::model::{ChannelMember, Conversation, SpaceId};
use teamcraft_client_api::role_rules::{RoleActor, can_in_channel, can_in_workspace, scheme_of};
use teamcraft_proto::{
  ChannelRole, ConversationVisibility, GuestDmStart, Permission, PermissionScheme, Role, UserId,
};

use crate::model::{ConversationId, ConversationKind, Space};
use crate::state::AppState;

/// Whether the signed-in user is a guest (external): guests browse no channels, invite nobody and
/// create nothing (admin.md §1).
pub fn is_guest(state: AppState) -> bool {
  state.read(|backend| backend.my_role()) == Role::Guest || state.me().is_guest
}

/// Whether the user may open a conversation with these people, creating it if there is none yet.
/// Members, admins and owners always may; a guest only where the policy `guests.dm_start` lets
/// them start one (PLAN.md §12), and can always open one that exists. A guest's directory holds
/// only people they share a conversation with, so for them everybody offered counts as shared.
/// The server enforces the same rule; this keeps the UI from offering what it will refuse.
pub fn can_start_direct(state: AppState, users: &[UserId]) -> bool {
  if !is_guest(state) {
    return true;
  }
  let start = state.read(|backend| {
    backend
      .workspace_policy()
      .1
      .guests
      .effective_dm_start(GuestDmStart::DEFAULT)
  });
  may_start_direct(true, state.direct_with(users).is_some(), &start)
}

/// The rule of [`can_start_direct`] without the UI state.
fn may_start_direct(is_guest: bool, has_conversation: bool, start: &GuestDmStart) -> bool {
  !is_guest || has_conversation || start.allows(true)
}

/// Whether the user holds a workspace-wide permission, such as `create_space` (the built-in
/// scheme gives it to admins and owners only). Admins read the instance scheme; for everybody else
/// it is the built-in one.
pub fn can_in_workspace_now(state: AppState, permission: &Permission) -> bool {
  let scheme = scheme_of(&state.read(|backend| backend.permission_scheme(None)));
  can_in_workspace(&scheme, &state.read(|backend| backend.my_role()), permission)
}

/// The space with what the user may do in it, if the user is in it.
pub fn space_of(state: AppState, space: SpaceId) -> Option<Space> {
  state
    .read(|backend| backend.spaces())
    .into_iter()
    .find(|found| found.id == space)
}

/// Whether "Invite people" leads anywhere in the space.
pub fn can_invite_to(state: AppState, space: SpaceId) -> bool {
  space_of(state, space).is_some_and(|found| found.can_invite(is_guest(state)))
}

/// Whether the user may make share links to the conversation's files (`create_share_link`: not
/// guests, and not when the instance switched share links off). Light enough for every file row:
/// the server's answer when there is one, the built-in scheme otherwise.
pub fn can_share_files(state: AppState, conversation: ConversationId) -> bool {
  let maybe_granted = state
    .read(|backend| backend.conversation(conversation))
    .and_then(|found| found.maybe_permissions);
  match maybe_granted {
    Some(granted) => granted.contains(&Permission::CreateShareLink),
    None => !is_guest(state),
  }
}

/// Whether the user may react in the conversation ([`crate::model::Conversation::can_react`], as
/// in the native apps). Light enough for every message row.
pub fn can_react_in(state: AppState, conversation: ConversationId) -> bool {
  state
    .read(|backend| backend.conversation(conversation))
    .is_some_and(|found| found.can_react())
}

/// Whether the user may create a channel in the space.
pub fn can_create_channel_in(state: AppState, space: SpaceId) -> bool {
  !is_guest(state) && space_of(state, space).is_some_and(|found| found.can_create_channels)
}

/// The user's standing in one conversation, reactive.
#[derive(Clone, Copy)]
pub struct ChannelAccess {
  pub actor: Memo<RoleActor>,
  pub scheme: Memo<PermissionScheme>,
  /// Whether the user may post: the conversation's `can_post`, which the backend took from the
  /// server's answer.
  can_post: Memo<bool>,
  is_channel: Memo<bool>,
}

/// What the user may do with the messages of a conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MessagePermissions {
  /// Write, quote, edit and delete the own messages (an edit or a deletion is a message too).
  pub can_post: bool,
  pub can_react: bool,
  /// Pin messages and keep files as bookmarks (`manage_bookmarks`, sent as a message).
  pub can_manage_bookmarks: bool,
}

impl MessagePermissions {
  /// Everything allowed: messages outside a conversation the user is in (search results, saved
  /// items) offer what their conversation allows when it is opened.
  pub const ALL: Self = Self {
    can_post: true,
    can_react: true,
    can_manage_bookmarks: true,
  };
}

impl ChannelAccess {
  pub fn new(state: AppState, conversation: ConversationId) -> Self {
    let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));
    let roles = Memo::new(move |_| state.read(|backend| backend.channel_roles(conversation)));
    let maybe_space = Memo::new(move |_| info.get().and_then(|found| found.maybe_space));
    let scheme = Memo::new(move |_| scheme_for(state, maybe_space.get()));
    let actor = Memo::new(move |_| {
      let maybe_info = info.get();
      roles.with(|list| actor_of(state, maybe_info.as_ref(), list))
    });
    let can_post = Memo::new(move |_| info.with(|found| found.as_ref().is_some_and(|found| found.can_post)));
    let is_channel =
      Memo::new(move |_| info.with(|found| found.as_ref().is_some_and(|found| !found.kind.is_direct_like())));
    Self {
      actor,
      scheme,
      can_post,
      is_channel,
    }
  }

  /// Whether the user holds `permission` in the conversation.
  pub fn can(&self, permission: &Permission) -> bool {
    self
      .scheme
      .with(|scheme| self.actor.with(|actor| can_in_channel(scheme, actor, permission)))
  }

  /// Whether "Add people" works here: channels only (DMs start a new group instead), with
  /// `manage_members`.
  pub fn can_add_members(&self) -> bool {
    self.is_channel.get() && self.can(&Permission::ManageMembers)
  }

  /// Whether the user may remove someone else whose channel role is `role`: moderators and admins
  /// take the right to manage roles as well (the server's rule).
  pub fn can_remove_member(&self, role: Option<&ChannelRole>) -> bool {
    let is_privileged = matches!(role, Some(ChannelRole::Admin | ChannelRole::Moderator));
    self.is_channel.get()
      && self.can(&Permission::ManageMembers)
      && (!is_privileged || self.can(&Permission::ManageChannelRoles))
  }

  pub fn message_permissions(&self) -> MessagePermissions {
    let can_post = self.can_post.get();
    MessagePermissions {
      can_post,
      can_react: self.can(&Permission::AddReaction),
      can_manage_bookmarks: can_post && self.can(&Permission::ManageBookmarks),
    }
  }
}

/// Whether the user may do `permission` in the channel as far as this device knows, read once and
/// untracked (a typed command, the command list). While the own channel role isn't loaded yet it
/// says yes: the server decides then, the UI never hides what it would allow.
pub fn may_now(state: AppState, conversation: ConversationId, permission: &Permission) -> bool {
  untrack(|| {
    let maybe_info = state.read(|backend| backend.conversation(conversation));
    let roles = state.read(|backend| backend.channel_roles(conversation));
    let actor = actor_of(state, maybe_info.as_ref(), &roles);
    if actor.maybe_own_role.is_none() {
      return true;
    }
    let scheme = scheme_for(state, maybe_info.and_then(|found| found.maybe_space));
    can_in_channel(&scheme, &actor, permission)
  })
}

/// Admins can read the space's scheme; for everybody else this is the built-in one.
fn scheme_for(state: AppState, maybe_space: Option<SpaceId>) -> PermissionScheme {
  scheme_of(&state.read(|backend| backend.permission_scheme(maybe_space)))
}

fn actor_of(state: AppState, maybe_info: Option<&Conversation>, roles: &[ChannelMember]) -> RoleActor {
  let me = state.me().id;
  let maybe_space = maybe_info.and_then(|found| found.maybe_space);
  let is_space_admin = maybe_space.is_some_and(|space| {
    state.read(|backend| {
      backend
        .spaces()
        .iter()
        .any(|found| found.id == space && found.can_manage_space)
    })
  });
  RoleActor {
    instance_role: state.read(|backend| backend.my_role()),
    is_space_admin,
    // Until the member list with the roles is loaded, being in the conversation's members makes
    // the user a plain member.
    maybe_own_role: roles
      .iter()
      .find(|member| member.user == me)
      .map(|member| member.role.clone())
      .or_else(|| {
        maybe_info
          .is_some_and(|found| found.member_ids.contains(&me))
          .then_some(ChannelRole::Member)
      }),
    visibility: match maybe_info.map(|found| found.kind) {
      Some(ConversationKind::Public) => ConversationVisibility::Public,
      _ => ConversationVisibility::Private,
    },
    is_archived: false,
    maybe_granted: maybe_info.and_then(|found| found.maybe_permissions.clone()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_guests_are_held_back_from_starting_conversations() {
    for (start, may_guest_start) in [
      (GuestDmStart::Nobody, false),
      (GuestDmStart::Shared, true),
      (GuestDmStart::Anyone, true),
      (GuestDmStart::Unknown("newer".into()), false),
    ] {
      // Members, admins and owners are never held back, whatever the guests' policy says.
      assert!(may_start_direct(false, false, &start), "{start} member");
      assert_eq!(
        may_start_direct(true, false, &start),
        may_guest_start,
        "{start} guest"
      );
      // A guest can always open a conversation somebody started with them.
      assert!(may_start_direct(true, true, &start), "{start} existing");
    }
  }
}
