//! The device's copy of the user groups, for expanding group mentions when a message is sent
//! ([`crate::group_mentions`]): read on connect and when the gateway says a group changed.

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::group_mentions::store_rosters;
use crate::store::LocalStore;
use teamcraft_proto::ListUserGroupRostersResponse;

/// The groups are read at most this often for gateway events.
const USER_GROUPS_MIN_REFRESH_MS: i64 = 5_000;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads the groups and their members. Not fatal: a device that can't read them keeps the copy
  /// it has, and a guest (who may not read groups) or an older server leaves it empty.
  pub(crate) async fn refresh_user_groups(&mut self) {
    let rosters = match self.api.list_user_group_rosters().await {
      Ok(rosters) => rosters,
      Err(error) if refuses_groups(&error) => ListUserGroupRostersResponse { groups: Vec::new() },
      Err(error) => {
        tracing::debug!(%error, "could not read the user groups");
        return;
      },
    };
    if let Err(error) = store_rosters(&self.store, &rosters) {
      tracing::warn!(%error, "could not store the user groups");
    }
  }

  /// Reads the groups after a change, but not after each of a provider sync's many events. Like
  /// the guest list, a change that waits does so for a later frame, at the latest a heartbeat.
  pub(super) async fn refresh_stale_user_groups(&mut self) {
    let now = self.now_ms();
    let is_due = self
      .maybe_user_groups_read_ms
      .is_none_or(|read| now.saturating_sub(read) >= USER_GROUPS_MIN_REFRESH_MS);
    if self.are_user_groups_stale && is_due {
      self.are_user_groups_stale = false;
      self.maybe_user_groups_read_ms = Some(now);
      Box::pin(self.refresh_user_groups()).await;
    }
  }
}

fn refuses_groups(error: &ApiError) -> bool {
  error.is_forbidden() || error.is_not_found()
}
