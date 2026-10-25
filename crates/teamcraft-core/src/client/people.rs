//! What the server tells about people and the user's calls, kept in the local store for the UI:
//! presence with "last seen" (PLAN.md §4), the user's privacy choices and the call history
//! (PLAN.md §8), plus the activity feed's read mark (account data).

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  CallHistoryEntry, LastSeenVisibility, ListCallHistoryQuery, PresenceState, PrivacyResponse,
  UpdatePrivacyRequest, UserId, UserPresence, UserSummary,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::store::{LocalStore, Table};
use crate::{read_state, records};

/// Key of the privacy record in [`Table::SyncState`].
pub const PRIVACY_KEY: &str = "privacy";
/// Key of the call history record in [`Table::SyncState`].
pub const CALL_HISTORY_KEY: &str = "call_history";
/// Key of the directory cache in [`Table::SyncState`].
pub const DIRECTORY_CACHE_KEY: &str = "directory_cache";
/// People the directory cache keeps at most (the most recently found).
const MAX_DIRECTORY_CACHE: usize = 200;
/// A directory answer is used this long; older people drop out of the cache.
const DIRECTORY_CACHE_TTL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// Calls fetched for the history list.
const CALL_HISTORY_PAGE: u32 = 100;
/// Most people `GET /presence` answers for in one request.
const MAX_PRESENCE_BATCH: usize = 200;

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

/// People the server's directory answered, most recently found first: colleagues who share
/// nothing with the user yet. Bounded in count and age, and kept apart from [`Table::Users`] (the
/// people of the user's spaces and conversations), so a search doesn't grow what every list shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCache {
  pub entries: Vec<DirectoryEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
  pub user: UserSummary,
  pub fetched_at_ms: i64,
}

impl DirectoryCache {
  /// The cache as stored, without people older than the TTL.
  pub fn load(store: &impl LocalStore, now_ms: i64) -> Result<Self, CoreError> {
    let mut cache: Self =
      records::get_json(store, Table::SyncState, DIRECTORY_CACHE_KEY)?.unwrap_or_default();
    cache
      .entries
      .retain(|entry| now_ms.saturating_sub(entry.fetched_at_ms) <= DIRECTORY_CACHE_TTL_MS);
    Ok(cache)
  }

  /// Puts an answer in front, each person once, and drops the oldest beyond the bound.
  pub fn merge(&mut self, users: &[UserSummary], now_ms: i64) {
    self
      .entries
      .retain(|entry| !users.iter().any(|user| user.id == entry.user.id));
    let found = users.iter().map(|user| DirectoryEntry {
      user: user.clone(),
      fetched_at_ms: now_ms,
    });
    self.entries.splice(0..0, found);
    self.entries.truncate(MAX_DIRECTORY_CACHE);
  }
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
  /// Presence and custom status of everyone this device knows (the people of its spaces and
  /// conversations), in batches of 200, once per gateway connection: `presence_update` events
  /// missed while disconnected are repaired here, and the UI takes the result from the presence
  /// log instead of asking the server itself (issue #252).
  pub(crate) async fn refresh_known_presence(&mut self) -> Result<(), CoreError> {
    let users: Vec<UserId> = self
      .store
      .list(Table::Users)?
      .into_iter()
      .filter_map(|(key, _)| key.parse().ok())
      .collect();
    for batch in users.chunks(MAX_PRESENCE_BATCH) {
      self.refresh_presence(batch).await?;
    }
    Ok(())
  }

  /// Asks the server for the presence and last seen of the given people (at most 200 at once).
  pub async fn refresh_presence(&mut self, users: &[UserId]) -> Result<(), CoreError> {
    if users.is_empty() {
      return Ok(());
    }
    let response = self.api.list_presence(users).await?;
    let fetched_at_ms = self.now_ms();
    for presence in response.presences {
      // The log carries state and status; last seen is read on its own (`ui_last_seen`).
      read_state::note_announced_presence(
        &self.store,
        self.with_queued_own_status(UserPresence {
          maybe_last_seen: None,
          ..presence.clone()
        })?,
        self.settings.max_announced_presence,
      )?;
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

  /// Asks the directory for people matching `query` and keeps them in the [`DirectoryCache`], so
  /// the people page and the search find colleagues who share nothing with the user yet. One page
  /// only: the UI narrows by typing, it never loads everybody. Returns how many were found.
  pub async fn search_directory(&mut self, query: &str) -> Result<usize, CoreError> {
    let response = self.api.search_users(query.trim()).await?;
    let now = self.now_ms();
    let mut cache = DirectoryCache::load(&self.store, now)?;
    cache.merge(&response.users, now);
    records::put_json(&self.store, Table::SyncState, DIRECTORY_CACHE_KEY, &cache)?;
    Ok(response.users.len())
  }

  /// The directory policy doesn't show e-mail addresses (any more): the ones kept from earlier
  /// answers go too, from the people of the user's conversations and from the directory cache.
  /// Writes only the records that had one.
  pub(crate) fn drop_cached_emails(&self) -> Result<(), CoreError> {
    let users: Vec<UserSummary> = records::list_json(&self.store, Table::Users)?;
    for mut user in users {
      if user.maybe_email.take().is_some() {
        records::put_json(&self.store, Table::Users, &user.id.to_string(), &user)?;
      }
    }
    let mut cache = DirectoryCache::load(&self.store, self.now_ms())?;
    let mut had_emails = false;
    for entry in &mut cache.entries {
      had_emails |= entry.user.maybe_email.take().is_some();
    }
    if had_emails {
      records::put_json(&self.store, Table::SyncState, DIRECTORY_CACHE_KEY, &cache)?;
    }
    Ok(())
  }

  /// The person registered a device (`device_add`): the cache stops saying they have none.
  pub(crate) fn remember_has_devices(&self, user: UserId) -> Result<(), CoreError> {
    let mut cache = DirectoryCache::load(&self.store, self.now_ms())?;
    let Some(entry) = cache.entries.iter_mut().find(|entry| entry.user.id == user) else {
      return Ok(());
    };
    if entry.user.maybe_has_devices == Some(true) {
      return Ok(());
    }
    entry.user.maybe_has_devices = Some(true);
    records::put_json(&self.store, Table::SyncState, DIRECTORY_CACHE_KEY, &cache)
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

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake, UserKind};

  use super::*;

  const NOW: i64 = 1_800_000_000_000;

  #[test]
  fn the_directory_cache_keeps_the_newest_answers_each_person_once() {
    let mut cache = DirectoryCache::default();
    cache.merge(&[person(1), person(2)], NOW);
    cache.merge(&[person(2), person(3)], NOW + 1);
    assert_eq!(ids(&cache), vec![person(2).id, person(3).id, person(1).id]);
  }

  #[test]
  fn the_directory_cache_is_bounded() {
    let mut cache = DirectoryCache::default();
    let many: Vec<UserSummary> = (1..=250).map(person).collect();
    cache.merge(&many, NOW);
    assert_eq!(cache.entries.len(), MAX_DIRECTORY_CACHE);
    assert_eq!(cache.entries[0].user.id, person(1).id);
  }

  #[test]
  fn old_directory_answers_are_dropped_on_load() {
    let store = crate::store::MemoryStore::new();
    let mut cache = DirectoryCache::default();
    cache.merge(&[person(1)], NOW);
    cache.merge(&[person(2)], NOW + DIRECTORY_CACHE_TTL_MS);
    records::put_json(&store, Table::SyncState, DIRECTORY_CACHE_KEY, &cache).unwrap();
    let loaded = DirectoryCache::load(&store, NOW + DIRECTORY_CACHE_TTL_MS + 1).unwrap();
    assert_eq!(ids(&loaded), vec![person(2).id]);
  }

  fn ids(cache: &DirectoryCache) -> Vec<UserId> {
    cache.entries.iter().map(|entry| entry.user.id).collect()
  }

  fn person(raw: i64) -> UserSummary {
    UserSummary {
      id: UserId::from_snowflake(Snowflake::new(raw).unwrap()),
      maybe_username: None,
      display_name: format!("Person {raw}"),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
    }
  }
}
