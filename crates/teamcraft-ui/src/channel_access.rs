//! What the signed-in user may do in a channel, for the settings that depend on it: the
//! permission rules of `teamcraft_client_api::role_rules` fed with what this device knows (the
//! instance role, the space admin flag, the own channel role and the space's permission scheme).
//! The server enforces the same rules; this only keeps the UI from offering what will be refused.

use leptos::prelude::*;
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
    // Admins can read the space's scheme; for everybody else this is the built-in one.
    let scheme =
      Memo::new(move |_| scheme_of(&state.read(|backend| backend.permission_scheme(maybe_space.get()))));
    let actor = Memo::new(move |_| {
      let me = state.me().id;
      let is_space_admin = maybe_space.get().is_some_and(|space| {
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
        maybe_own_role: roles.with(|list| {
          list
            .iter()
            .find(|member| member.user == me)
            .map(|member| member.role.clone())
        }),
        visibility: match info.get().map(|found| found.kind) {
          Some(ConversationKind::Public) => ConversationVisibility::Public,
          _ => ConversationVisibility::Private,
        },
        is_archived: false,
      }
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
