//! The user's own custom status (PLAN.md §4: server-visible, low sensitivity).
//!
//! A change shows at once: it is written into the presence log the UI reads, then queued like the
//! manual presence (`pending_actions`), so it reaches the server when the device is back online and
//! survives a restart. Until the server took it, presence the server reports for the user (a
//! refresh after a reconnect, a `presence_update`) keeps the queued status, so the display doesn't
//! jump back. The server's `presence_update` reconciles afterwards; a refusal (statuses switched
//! off, not a preset) restores the server's status and is reported to the caller.

use teamcraft_proto::{PresenceState, SetCustomStatusRequest, UserPresence};

use super::pending_actions::StatusChange;
use super::{Client, PendingAction};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::read_state;
use crate::records;
use crate::store::{LocalStore, Table};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sets the user's custom status, or clears it with `None`. Never fails for being offline;
  /// fails if the server refused it.
  pub async fn set_custom_status(
    &mut self,
    maybe_status: Option<SetCustomStatusRequest>,
  ) -> Result<(), CoreError> {
    let change = match maybe_status {
      Some(status) => StatusChange::Set { status },
      None => StatusChange::Clear,
    };
    self.show_own_status(&change)?;
    self.store_action(PendingAction::CustomStatus { change })?;
    let outcome = self.flush_queue().await?;
    match outcome
      .refused
      .into_iter()
      .find(|(action, _)| matches!(action, PendingAction::CustomStatus { .. }))
    {
      Some((_, error)) => Err(CoreError::Api(error)),
      None => Ok(()),
    }
  }

  /// Presence the server reported, with the user's own status change that still waits in the
  /// queue laid over it.
  pub(super) fn with_queued_own_status(&self, mut presence: UserPresence) -> Result<UserPresence, CoreError> {
    if presence.user_id != self.account.user_id {
      return Ok(presence);
    }
    if let Some(change) = self.queued_status_change()? {
      presence.maybe_status = change.into_status();
    }
    Ok(presence)
  }

  /// Writes the change into the presence log right away (the UI reads it from there).
  fn show_own_status(&self, change: &StatusChange) -> Result<(), CoreError> {
    let me = self.account.user_id;
    let state: PresenceState =
      records::get_json(&self.store, Table::Presence, &me.to_string())?.unwrap_or(PresenceState::Online);
    read_state::note_announced_presence(
      &self.store,
      UserPresence {
        user_id: me,
        state,
        maybe_status: change.clone().into_status(),
        maybe_last_seen: None,
      },
      self.settings.max_announced_presence,
    )
    .map(|_| ())
  }
}

impl StatusChange {
  fn into_status(self) -> Option<teamcraft_proto::CustomStatus> {
    match self {
      Self::Set { status } => Some(status),
      Self::Clear => None,
    }
  }
}
