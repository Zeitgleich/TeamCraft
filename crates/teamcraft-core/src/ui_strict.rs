//! Strict channels in the UI adapter (e2ee.md §4.6.2, issue #1897): the stored view becomes the
//! UI's [`StrictChannel`], and the UI's actions become core steps.

use teamcraft_client_api::model::ConversationId;
use teamcraft_client_api::strict_channel::{
  StrictChannel, StrictDevice, StrictReadiness, StrictRejoin, StrictRole, StrictRoleHolder, StrictStop,
};

use crate::client::{StrictStep, StrictView};
use crate::store::LocalStore;
use crate::ui_backend::{CoreChatBackend, CoreCommand};

impl<S: LocalStore> CoreChatBackend<S> {
  /// Runs a strict channel step; it fails as a toast and makes the view afresh either way.
  pub(crate) fn strict_step(&self, conversation: ConversationId, step: StrictStep) {
    self.command(CoreCommand::Strict { conversation, step });
  }
}

/// The UI's model of a stored view.
pub(crate) fn strict_channel_of(view: StrictView) -> StrictChannel {
  let devices = |list: Vec<(teamcraft_proto::UserId, teamcraft_proto::DeviceId)>| {
    list
      .into_iter()
      .map(|(user, device)| StrictDevice { user, device })
      .collect()
  };
  StrictChannel {
    has_group: view.has_group,
    is_strict: view.is_strict,
    roles: view
      .roles
      .into_iter()
      .map(|role| StrictRoleHolder {
        user: role.user,
        role: if role.is_admin {
          StrictRole::Admin
        } else {
          StrictRole::Moderator
        },
      })
      .collect(),
    workspace_signers: view.workspace_signers,
    maybe_readiness: view.maybe_readiness.map(|readiness| StrictReadiness {
      maybe_asked_by: readiness.maybe_asked_by,
      waiting: devices(readiness.waiting),
      refreshing: devices(readiness.refreshing),
      is_ready: readiness.is_ready,
    }),
    maybe_stop: view.maybe_stop.map(|stop| StrictStop {
      at_ms: stop.at_ms,
      maybe_signer: stop.maybe_signer,
    }),
    maybe_rejoin: view.maybe_rejoin.map(|rejoin| StrictRejoin {
      sender: rejoin.sender,
      joined: rejoin.joined,
      gone: rejoin.gone,
      is_due: rejoin.is_due,
      is_accepted: rejoin.is_accepted,
    }),
  }
}
