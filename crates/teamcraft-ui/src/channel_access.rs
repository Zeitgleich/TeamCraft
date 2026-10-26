//! What the signed-in user may do in a channel, for the settings that depend on it: the
//! permission rules of `teamcraft_client_api::role_rules` fed with what this device knows (the
//! instance role, the space admin flag, the own channel role and the space's permission scheme).
//! The server enforces the same rules; this only keeps the UI from offering what will be refused.

use leptos::prelude::*;
use teamcraft_client_api::model::{ChannelMember, Conversation, SpaceId};
use teamcraft_client_api::role_rules::{RoleActor, can_in_channel, scheme_of};
use teamcraft_proto::{ConversationVisibility, Permission, PermissionScheme};

use crate::model::{ConversationId, ConversationKind};
use crate::state::AppState;

/// The user's standing in one channel, reactive.
#[derive(Clone, Copy)]
pub struct ChannelAccess {
  pub actor: Memo<RoleActor>,
  pub scheme: Memo<PermissionScheme>,
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
    Self { actor, scheme }
  }

  /// Whether the user holds `permission` in the channel.
  pub fn can(&self, permission: &Permission) -> bool {
    self
      .scheme
      .with(|scheme| self.actor.with(|actor| can_in_channel(scheme, actor, permission)))
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
    maybe_own_role: roles
      .iter()
      .find(|member| member.user == me)
      .map(|member| member.role.clone()),
    visibility: match maybe_info.map(|found| found.kind) {
      Some(ConversationKind::Public) => ConversationVisibility::Public,
      _ => ConversationVisibility::Private,
    },
    is_archived: false,
  }
}
