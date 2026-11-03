//! Last seen, privacy choices, the call history and the own devices in the UI adapter: reads come from the local
//! store; a read that finds nothing (or something old) queues a refresh and answers with what it
//! has. Repeated reads don't repeat the request within [`RETRY_AFTER_MS`].

use std::collections::HashMap;

use teamcraft_client_api::admin::{DeviceInfo, DevicePushStatus, DeviceVerification};
use teamcraft_client_api::backend::{LoadingList, filter_people};
use teamcraft_client_api::calls::{CallDirection, CallHistoryItem};
use teamcraft_client_api::model::{TimestampMs, User, UserId};
use teamcraft_client_api::privacy::PrivacySettings;
use teamcraft_proto::CallHistoryEntry;

use crate::client::OwnDeviceCheck;
use crate::client::devices::{OWN_DEVICES_KEY, OwnDevicesRecord};
use crate::client::people::{
  CALL_HISTORY_KEY, CallHistoryRecord, DirectoryCache, LastSeenRecord, PRIVACY_KEY, PrivacyRecord,
  last_seen_key,
};
use crate::records;
use crate::store::{LocalStore, Table};
use crate::ui_backend::{CoreChatBackend, CoreCommand};

/// Server answers are fresh for this long.
const FRESH_FOR_MS: i64 = 60_000;
/// A request that got no answer (offline, error) isn't repeated sooner than this.
const RETRY_AFTER_MS: i64 = 15_000;
/// How long the outcome of a test push shows next to the device.
const TEST_OUTCOME_SHOWN_MS: i64 = 10 * 60_000;
/// Directory searches remembered for [`FRESH_FOR_MS`]; beyond this many the oldest go first.
const MAX_REMEMBERED_SEARCHES: usize = 64;

/// When each kind of refresh was last requested.
#[derive(Debug, Default)]
pub(crate) struct RequestLog {
  last_seen: HashMap<UserId, i64>,
  /// Directory searches by normalized query.
  directory: HashMap<String, i64>,
  maybe_privacy: Option<i64>,
  maybe_call_history: Option<i64>,
  maybe_own_devices: Option<i64>,
}

impl<S: LocalStore> CoreChatBackend<S> {
  /// People by name from the local store (which keeps earlier directory answers) and, unless the
  /// same query went to the server within [`FRESH_FOR_MS`], a directory search whose answer shows
  /// on the next read.
  pub(crate) fn ui_search_people(&self, query: &str) -> Vec<User> {
    let normalized = query.trim().to_lowercase();
    let now = self.now_ms();
    let mut log = self.requests.borrow_mut();
    let is_due = log
      .directory
      .get(&normalized)
      .is_none_or(|requested| now - requested > FRESH_FOR_MS);
    if is_due {
      remember_search(&mut log.directory, normalized.clone(), now);
      drop(log);
      self.command(CoreCommand::SearchDirectory {
        query: normalized.clone(),
      });
    }
    filter_people(self.known_people(), &normalized)
  }

  /// The people of the user's spaces and conversations and, once each, those the directory found
  /// lately; whether somebody has a device comes from the directory.
  fn known_people(&self) -> Vec<User> {
    let cache = self.read(DirectoryCache::load(self.store_ref(), self.now_ms()));
    let mut summaries = self.summaries();
    for entry in cache.entries {
      summaries
        .entry(entry.user.id)
        .and_modify(|known| known.maybe_has_devices = entry.user.maybe_has_devices)
        .or_insert(entry.user);
    }
    summaries.values().map(|summary| self.to_user(summary)).collect()
  }

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

  pub(crate) fn ui_is_loaded(&self, list: LoadingList) -> bool {
    match list {
      LoadingList::CallHistory => {
        let maybe_record: Option<CallHistoryRecord> = self.read(records::get_json(
          self.store_ref(),
          Table::SyncState,
          CALL_HISTORY_KEY,
        ));
        maybe_record.is_some()
      },
    }
  }

  /// The list was opened: fetch the newest calls (once per [`RETRY_AFTER_MS`]).
  pub(crate) fn ui_refresh_call_history(&self) {
    self.request_call_history(true);
  }

  pub(crate) fn ui_own_devices(&self) -> Vec<DeviceInfo> {
    let maybe_record: Option<OwnDevicesRecord> = self.read(records::get_json(
      self.store_ref(),
      Table::SyncState,
      OWN_DEVICES_KEY,
    ));
    if maybe_record.is_none() {
      self.request_own_devices(false);
    }
    let now = self.now_ms();
    maybe_record
      .map(|record| own_device_infos(record, now))
      .unwrap_or_default()
  }

  /// The device settings were opened: fetch the list again (once per second at most).
  pub(crate) fn ui_refresh_own_devices(&self) {
    self.request_own_devices(true);
  }

  fn request_own_devices(&self, is_forced: bool) {
    let now = self.now_ms();
    let mut log = self.requests.borrow_mut();
    let is_due = log
      .maybe_own_devices
      .is_none_or(|requested| now - requested > if is_forced { 1_000 } else { RETRY_AFTER_MS });
    if is_due {
      log.maybe_own_devices = Some(now);
      drop(log);
      self.command(CoreCommand::RefreshOwnDevices);
    }
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

/// The server's list as the settings show it: this device first, then the newest. The server
/// keeps no "last seen" for a user's own list, so the time shown is when the device was added.
/// Verification is what this device checked; the push state is the server's (diagnostics).
fn own_device_infos(record: OwnDevicesRecord, now: i64) -> Vec<DeviceInfo> {
  let verification: HashMap<_, _> = record
    .verification
    .iter()
    .map(|(device, check)| {
      let state = match check {
        OwnDeviceCheck::Verified => DeviceVerification::Verified,
        OwnDeviceCheck::Pending => DeviceVerification::Pending,
        OwnDeviceCheck::Unverified => DeviceVerification::Unverified,
      };
      (*device, state)
    })
    .collect();
  // A test outcome is news for a while, not a state of the device.
  let tests: HashMap<_, _> = record
    .tests
    .iter()
    .filter(|test| now.saturating_sub(test.sent_at_ms) < TEST_OUTCOME_SHOWN_MS)
    .map(|test| (test.device, test.outcome.clone()))
    .collect();
  let maybe_push: Option<HashMap<_, _>> = record
    .maybe_push
    .as_ref()
    .map(|devices| devices.iter().map(|health| (health.device_id, health)).collect());
  let mut devices: Vec<DeviceInfo> = record
    .devices
    .into_iter()
    .map(|device| {
      let maybe_push = maybe_push.as_ref().map(|by_device| {
        let maybe_health = by_device.get(&device.id);
        DevicePushStatus {
          maybe_provider: maybe_health
            .and_then(|health| health.maybe_provider.clone())
            .or_else(|| device.maybe_push_provider.clone()),
          maybe_last_sent_at: maybe_health.and_then(|health| health.counts.maybe_last_sent_at.map(|at| at.0)),
          maybe_last_failed_at: maybe_health
            .and_then(|health| health.counts.maybe_last_failed_at.map(|at| at.0)),
          maybe_test: tests.get(&device.id).cloned(),
        }
      });
      DeviceInfo {
        is_current: device.id == record.current,
        verification: verification.get(&device.id).copied().unwrap_or_default(),
        maybe_push,
        id: device.id,
        name: device.name,
        platform: String::new(),
        last_seen: device.created_at.0,
      }
    })
    .collect();
  devices.sort_by_key(|device| (!device.is_current, std::cmp::Reverse(device.last_seen)));
  devices
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

/// Records a search, dropping the oldest when the log is full (its bound).
fn remember_search(log: &mut HashMap<String, i64>, query: String, now: i64) {
  if log.len() >= MAX_REMEMBERED_SEARCHES
    && let Some(oldest) = log
      .iter()
      .min_by_key(|(_, requested)| **requested)
      .map(|(query, _)| query.clone())
  {
    log.remove(&oldest);
  }
  log.insert(query, now);
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::admin::DeviceVerification;
  use teamcraft_client_api::model::id_from_raw;
  use teamcraft_proto::{
    DevicePushHealth, DeviceResponse, PushDeliveryCounts, PushProvider, TestPushOutcome, UnixMillis,
  };

  use super::*;
  use crate::client::devices::OwnDeviceTest;

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

  #[test]
  fn own_devices_list_this_device_first_then_the_newest() {
    let device = |raw: u64, created_at: i64| teamcraft_proto::DeviceResponse {
      id: id_from_raw(raw),
      name: format!("Device {raw}"),
      public_key: teamcraft_proto::Base64Bytes::new(Vec::new()),
      cross_signature: teamcraft_proto::Base64Bytes::new(Vec::new()),
      maybe_push_provider: None,
      created_at: UnixMillis(created_at),
    };
    let infos = own_device_infos(
      OwnDevicesRecord {
        devices: vec![device(1, 100), device(2, 300), device(3, 200)],
        current: id_from_raw(3),
        fetched_at_ms: 400,
        verification: Vec::new(),
        maybe_push: None,
        tests: Vec::new(),
      },
      400,
    );
    let order: Vec<_> = infos
      .iter()
      .map(|info| (info.name.as_str(), info.is_current))
      .collect();
    assert_eq!(
      order,
      [("Device 3", true), ("Device 2", false), ("Device 1", false)]
    );
    assert_eq!(infos[1].last_seen, 300);
    assert!(
      infos.iter().all(|info| info.maybe_push.is_none()),
      "an older server"
    );
    assert!(
      infos
        .iter()
        .all(|info| info.verification == DeviceVerification::Unknown)
    );
  }

  #[test]
  fn own_devices_carry_what_this_device_verified_and_their_push_state() {
    let phone = DeviceResponse {
      maybe_push_provider: Some(PushProvider::Fcm),
      ..device_response(1, "Phone")
    };
    let laptop = device_response(2, "Laptop");
    let infos = own_device_infos(
      OwnDevicesRecord {
        devices: vec![phone, laptop],
        current: id_from_raw(2),
        fetched_at_ms: 400,
        verification: vec![
          (id_from_raw(1), OwnDeviceCheck::Verified),
          (id_from_raw(2), OwnDeviceCheck::Pending),
        ],
        maybe_push: Some(vec![DevicePushHealth {
          device_id: id_from_raw(1),
          maybe_provider: Some(PushProvider::Fcm),
          counts: PushDeliveryCounts {
            sent: 3,
            maybe_last_sent_at: Some(UnixMillis(350)),
            ..PushDeliveryCounts::default()
          },
        }]),
        tests: vec![
          OwnDeviceTest {
            device: id_from_raw(1),
            outcome: TestPushOutcome::Sent,
            sent_at_ms: 390,
          },
          OwnDeviceTest {
            device: id_from_raw(2),
            outcome: TestPushOutcome::NotRegistered,
            sent_at_ms: 400 - TEST_OUTCOME_SHOWN_MS,
          },
        ],
      },
      400,
    );
    let phone = infos.iter().find(|info| info.name == "Phone").unwrap();
    assert_eq!(phone.verification, DeviceVerification::Verified);
    assert_eq!(
      phone.maybe_push,
      Some(DevicePushStatus {
        maybe_provider: Some(PushProvider::Fcm),
        maybe_last_sent_at: Some(350),
        maybe_last_failed_at: None,
        maybe_test: Some(TestPushOutcome::Sent),
      })
    );
    let laptop = infos.iter().find(|info| info.name == "Laptop").unwrap();
    assert_eq!(laptop.verification, DeviceVerification::Pending);
    assert_eq!(
      laptop.maybe_push,
      Some(DevicePushStatus::default()),
      "no push service"
    );
  }

  fn device_response(raw: u64, name: &str) -> DeviceResponse {
    DeviceResponse {
      id: id_from_raw(raw),
      name: name.to_owned(),
      public_key: teamcraft_proto::Base64Bytes::new(Vec::new()),
      cross_signature: teamcraft_proto::Base64Bytes::new(Vec::new()),
      maybe_push_provider: None,
      created_at: UnixMillis(100),
    }
  }
}
