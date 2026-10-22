//! What the server tells about people and the user's calls, kept in the local store for the UI:
//! presence with "last seen" (PLAN.md §4), the user's privacy choices and the call history
//! (PLAN.md §8), plus the activity feed's read mark (account data).

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  CallHistoryEntry, LastSeenVisibility, ListCallHistoryQuery, PresenceState, PrivacyResponse,
  UpdatePrivacyRequest, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Key of the privacy record in [`Table::SyncState`].
pub const PRIVACY_KEY: &str = "privacy";
/// Key of the call history record in [`Table::SyncState`].
pub const CALL_HISTORY_KEY: &str = "call_history";
/// Calls fetched for the history list.
const CALL_HISTORY_PAGE: u32 = 100;

/// When the person was last active, as the server last told this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSeenRecord {
  /// Absent: the person is online, hides it, or was never seen.
  #[serde(rename = "at", default)]
  pub maybe_at_ms: Option<i64>,
  pub fetched_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyRecord {
  pub privacy: PrivacyResponse,
  pub fetched_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHistoryRecord {
  pub calls: Vec<CallHistoryEntry>,
  pub fetched_at_ms: i64,
}

/// Key of a person's last seen record in [`Table::Presence`] (their presence state has the plain ID).
pub fn last_seen_key(user: UserId) -> String {
  format!("last_seen:{user}")
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Asks the server for the presence and last seen of the given people (at most 200 at once).
  pub async fn refresh_presence(&mut self, users: &[UserId]) -> Result<(), CoreError> {
    if users.is_empty() {
      return Ok(());
    }
    let response = self.api.list_presence(users).await?;
    let fetched_at_ms = self.now_ms();
    for presence in response.presences {
      let state = presence.state.clone();
      records::put_json(
        &self.store,
        Table::Presence,
        &presence.user_id.to_string(),
        &state,
      )?;
      records::put_json(
        &self.store,
        Table::Presence,
        &last_seen_key(presence.user_id),
        &LastSeenRecord {
          maybe_at_ms: presence
            .maybe_last_seen
            .filter(|_| state == PresenceState::Offline)
            .map(|at| at.0),
          fetched_at_ms,
        },
      )?;
    }
    Ok(())
  }

  /// Reads the user's privacy choices from the server.
  pub async fn refresh_privacy(&mut self) -> Result<PrivacyResponse, CoreError> {
    let privacy = self.api.get_privacy().await?;
    self.store_privacy(&privacy)?;
    Ok(privacy)
  }

  /// Changes who may see when the user was last active.
  pub async fn set_last_seen_visibility(&mut self, visibility: LastSeenVisibility) -> Result<(), CoreError> {
    let privacy = self
      .api
      .put_privacy(&UpdatePrivacyRequest {
        last_seen: visibility,
      })
      .await?;
    self.store_privacy(&privacy)
  }

  /// Reads the user's newest calls from the server.
  pub async fn refresh_call_history(&mut self) -> Result<(), CoreError> {
    let response = self
      .api
      .list_call_history(&ListCallHistoryQuery {
        maybe_before: None,
        maybe_limit: Some(CALL_HISTORY_PAGE),
      })
      .await?;
    records::put_json(
      &self.store,
      Table::SyncState,
      CALL_HISTORY_KEY,
      &CallHistoryRecord {
        calls: response.calls,
        fetched_at_ms: self.now_ms(),
      },
    )
  }

  /// Everything in the activity feed sent up to `up_to_ms` counts as read on all devices.
  pub fn mark_activity_read(&mut self, up_to_ms: i64) -> Result<(), CoreError> {
    self.update_account_data(|data, now| data.mark_activity_read(up_to_ms, now))
  }

  fn store_privacy(&self, privacy: &PrivacyResponse) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      PRIVACY_KEY,
      &PrivacyRecord {
        privacy: privacy.clone(),
        fetched_at_ms: self.now_ms(),
      },
    )
  }
}
