//! Last seen, privacy choices and the call history in the UI adapter: reads come from the local
//! store; a read that finds nothing (or something old) queues a refresh and answers with what it
//! has. Repeated reads don't repeat the request within [`RETRY_AFTER_MS`].

use std::collections::HashMap;

use teamcraft_client_api::calls::{CallDirection, CallHistoryItem};
use teamcraft_client_api::model::{TimestampMs, UserId};
use teamcraft_client_api::privacy::PrivacySettings;
use teamcraft_proto::CallHistoryEntry;

use crate::client::people::{
  CALL_HISTORY_KEY, CallHistoryRecord, LastSeenRecord, PRIVACY_KEY, PrivacyRecord, last_seen_key,
};
use crate::records;
use crate::store::{LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};

/// Server answers are fresh for this long.
const FRESH_FOR_MS: i64 = 60_000;
/// A request that got no answer (offline, error) isn't repeated sooner than this.
const RETRY_AFTER_MS: i64 = 15_000;

/// When each kind of refresh was last requested.
#[derive(Debug, Default)]
pub(crate) struct RequestLog {
  last_seen: HashMap<UserId, i64>,
  maybe_privacy: Option<i64>,
  maybe_call_history: Option<i64>,
}

impl<S: LocalStore> CoreChatBackend<S> {
  pub(crate) fn ui_last_seen(&self, user: UserId) -> Option<TimestampMs> {
    let maybe_record: Option<LastSeenRecord> = self.read(records::get_json(
      self.store_ref(),
      Table::Presence,
      &last_seen_key(user),
    ));
    let now = self.now_ms();
    let is_stale = maybe_record.is_none_or(|record| now - record.fetched_at_ms > FRESH_FOR_MS);
    if is_stale {
      let mut log = self.requests.borrow_mut();
      let is_due = log
        .last_seen
        .get(&user)
        .is_none_or(|requested| now - requested > RETRY_AFTER_MS);
      if is_due {
        log.last_seen.insert(user, now);
        drop(log);
        self.command(CoreCommand::RefreshPresence { users: vec![user] });
      }
    }
    maybe_record.and_then(|record| record.maybe_at_ms)
  }

  pub(crate) fn ui_privacy(&self) -> PrivacySettings {
    let maybe_record: Option<PrivacyRecord> =
      self.read(records::get_json(self.store_ref(), Table::SyncState, PRIVACY_KEY));
    let now = self.now_ms();
    if maybe_record
      .as_ref()
      .is_none_or(|record| now - record.fetched_at_ms > FRESH_FOR_MS)
    {
      let mut log = self.requests.borrow_mut();
      if log
        .maybe_privacy
        .is_none_or(|requested| now - requested > RETRY_AFTER_MS)
      {
        log.maybe_privacy = Some(now);
        drop(log);
        self.command(CoreCommand::RefreshPrivacy);
      }
    }
    maybe_record.map_or_else(PrivacySettings::default, |record| PrivacySettings {
      last_seen: record.privacy.last_seen,
      is_last_seen_available: record.privacy.is_last_seen_available,
    })
  }

  pub(crate) fn ui_call_history(&self) -> Vec<CallHistoryItem> {
    let maybe_record: Option<CallHistoryRecord> = self.read(records::get_json(
      self.store_ref(),
      Table::SyncState,
      CALL_HISTORY_KEY,
    ));
    if maybe_record.is_none() {
      self.request_call_history(false);
    }
    let me = self.me_id();
    maybe_record
      .map(|record| record.calls.iter().map(|call| history_item(me, call)).collect())
      .unwrap_or_default()
  }

  /// The list was opened: fetch the newest calls (once per [`RETRY_AFTER_MS`]).
  pub(crate) fn ui_refresh_call_history(&self) {
    self.request_call_history(true);
  }

  fn request_call_history(&self, is_forced: bool) {
    let now = self.now_ms();
    let mut log = self.requests.borrow_mut();
    let is_due = log
      .maybe_call_history
      .is_none_or(|requested| now - requested > if is_forced { 1_000 } else { RETRY_AFTER_MS });
    if is_due {
      log.maybe_call_history = Some(now);
      drop(log);
      self.command(CoreCommand::RefreshCallHistory);
    }
  }
}

/// How a call looks to `me`.
fn history_item(me: UserId, call: &CallHistoryEntry) -> CallHistoryItem {
  let direction = if call.is_missed {
    CallDirection::Missed
  } else if call.maybe_started_by == Some(me) {
    CallDirection::Outgoing
  } else {
    CallDirection::Incoming
  };
  CallHistoryItem {
    id: call.call_id.to_string(),
    conversation: call.conversation_id,
    direction,
    started_at: call.started_at.0,
    maybe_ended_at: call.maybe_ended_at.map(|at| at.0),
    maybe_started_by: call.maybe_started_by,
    participants: call.participants.clone(),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_proto::UnixMillis;

  use super::*;

  #[test]
  fn calls_are_missed_outgoing_or_incoming() {
    let me: UserId = id_from_raw(1);
    let other: UserId = id_from_raw(2);
    let call = |started_by, is_missed| CallHistoryEntry {
      call_id: id_from_raw(9),
      conversation_id: id_from_raw(5),
      maybe_started_by: started_by,
      started_at: UnixMillis(1_000),
      maybe_ended_at: Some(UnixMillis(4_000)),
      participants: vec![me, other],
      is_missed,
    };
    assert_eq!(
      history_item(me, &call(Some(other), true)).direction,
      CallDirection::Missed
    );
    assert_eq!(
      history_item(me, &call(Some(me), false)).direction,
      CallDirection::Outgoing
    );
    assert_eq!(
      history_item(me, &call(Some(other), false)).direction,
      CallDirection::Incoming
    );
    assert_eq!(
      history_item(me, &call(None, false)).direction,
      CallDirection::Incoming
    );
    assert_eq!(
      history_item(me, &call(Some(me), false)).duration_ms(),
      Some(3_000)
    );
  }
}
