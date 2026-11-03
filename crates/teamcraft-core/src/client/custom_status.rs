//! The user's own custom status (PLAN.md §4: server-visible, low sensitivity).
//!
//! A change shows at once: it is written into the presence log the UI reads, then queued like the
//! manual presence (`pending_actions`), so it reaches the server when the device is back online and
//! survives a restart. Until the server took it, presence the server reports for the user (a
//! refresh after a reconnect, a `presence_update`) keeps the queued status, so the display doesn't
//! jump back. The server's `presence_update` reconciles afterwards; a refusal (statuses switched
//! off, not a preset) restores the server's status and is reported: to the caller when the user
//! just set it, otherwise through [`Client::take_background_failures`].
//!
//! The queued status keeps its end on this device's clock; it goes to the server as a duration
//! (`SetCustomStatusRequest::for_status`), so a clock that is off doesn't get it refused.

use teamcraft_proto::{CustomStatus, PresenceState, UserPresence};

use super::pending_actions::StatusChange;
use super::{Client, PendingAction};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::read_state;
use crate::records;
use crate::store::{LocalStore, Table};

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sets the user's custom status (its end on this device's clock), or clears it with `None`.
  /// Never fails for being offline; fails if the server refused it.
  pub async fn set_custom_status(&mut self, maybe_status: Option<CustomStatus>) -> Result<(), CoreError> {
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

  /// Failures of actions the user took earlier that the server refused while the queue was sent
  /// in the background (after a reconnect, on the upkeep tick): the host shows them, since no
  /// button waits for them (issue #322). Taken once.
  pub fn take_background_failures(&mut self) -> Vec<CoreError> {
    std::mem::take(&mut self.background_failures)
  }

  /// Presence the server reported, with the user's own status change that still waits in the
  /// queue laid over it.
  pub(super) fn with_queued_own_status(&self, mut presence: UserPresence) -> Result<UserPresence, CoreError> {
    if presence.user_id != self.account.user_id {
      return Ok(presence);
    }
    if let Some(change) = self.queued_status_change()? {
      presence.maybe_status = change.applied_to(presence.maybe_status);
    }
    Ok(presence)
  }

  /// Writes the change into the presence log right away (the UI reads it from there).
  fn show_own_status(&self, change: &StatusChange) -> Result<(), CoreError> {
    let me = self.account.user_id;
    let state: PresenceState =
      records::get_json(&self.store, Table::Presence, &me.to_string())?.unwrap_or(PresenceState::Online);
    let maybe_known: Option<teamcraft_proto::live::RevisedPresence> =
      records::get_json(&self.store, Table::AnnouncedPresence, &me.to_string())?;
    let maybe_shown = maybe_known.and_then(|known| known.presence.maybe_status);
    read_state::note_announced_presence(
      &self.store,
      UserPresence {
        user_id: me,
        state,
        maybe_status: change.clone().applied_to(maybe_shown),
        maybe_last_seen: None,
      },
      self.settings.max_announced_presence,
    )
    .map(|_| ())
  }
}

impl StatusChange {
  /// The status shown after the change, given the one shown before. A clear removes only the
  /// user's own status: one the server derived ("in a call", "in a meeting") stays.
  fn applied_to(self, maybe_shown: Option<CustomStatus>) -> Option<CustomStatus> {
    match self {
      Self::Set { status } => Some(status),
      Self::Clear => maybe_shown.filter(|shown| shown.maybe_activity.is_some()),
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::PresenceActivity;

  use super::*;

  #[test]
  fn a_clear_keeps_a_derived_status_and_drops_the_own_one() {
    let in_call = CustomStatus {
      maybe_emoji: None,
      maybe_text: None,
      maybe_expires_at: None,
      maybe_activity: Some(PresenceActivity::InCall),
    };
    let own = CustomStatus {
      maybe_emoji: Some("🌴".into()),
      maybe_text: Some("Vacation".into()),
      maybe_expires_at: None,
      maybe_activity: None,
    };
    assert_eq!(
      StatusChange::Clear.applied_to(Some(in_call.clone())),
      Some(in_call)
    );
    assert_eq!(StatusChange::Clear.applied_to(Some(own.clone())), None);
    assert_eq!(
      StatusChange::Set { status: own.clone() }.applied_to(None),
      Some(own)
    );
  }
}
