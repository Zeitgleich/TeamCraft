//! What this device keeps per meeting it organizes, and when it goes (#1939): the lobby setting
//! (#987), the schedule (#1919) and the meeting a meeting chat belongs to. The first two are keyed
//! by the meeting, so a chat that is purged or forgotten finds them through the third. A meeting
//! closed here leaves a mark instead of its schedule: nothing is signed for it past that moment,
//! until the organizer restores it here (#1983).

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MeetingId, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The lobby setting of a meeting as the organizer chose it on this device (#987 review): only this
/// record, never the server's `has_lobby`, lets the device admit guests of the link by itself.
const LOBBY_RECORD_PREFIX: &str = "meeting-lobby-record/";
/// The schedule of a meeting as the organizer set it on this device: the admitters' signed validity
/// follows it, never a later end the server reports (#1919).
const SCHEDULE_RECORD_PREFIX: &str = "meeting-schedule-record/";
/// The meeting the app named for a meeting chat, for its founding (D10, #1747) and to find the
/// meeting's records when the chat goes.
const MEETING_OF_CHAT_PREFIX: &str = "meeting-of-chat/";
/// A meeting this device closed ([`ClosedMeeting`]). Kept for good: a few dozen bytes per meeting,
/// and dropping it would let a server bring the meeting back with a longer signed end.
const CLOSED_RECORD_PREFIX: &str = "meeting-closed/";

/// The mark a meeting leaves when this device closes it (#1939): the organizer cancelled it or
/// ended it here, or its chat was purged or forgotten here (also on the server's word alone).
/// The admitters' validity never runs past `closed_at_ms` again
/// (`meeting_schedule::admitters_until`), so a meeting the server brings back, or a series it
/// passed off as a one-off, gets no longer signature than it had when this device closed it.
///
/// Only the organizer's own restore on this device lifts it again
/// ([`Client::reopen_meeting_records`], #1983): the records it kept go back as they were, so the
/// restored meeting is signed from this device's word again, never from the server's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClosedMeeting {
  pub meeting: MeetingId,
  /// This device's clock when it closed the meeting; the first close counts.
  pub closed_at_ms: i64,
  /// The lobby record as it was at the close, for a restore here.
  #[serde(rename = "has_lobby", default, skip_serializing_if = "Option::is_none")]
  pub maybe_has_lobby: Option<bool>,
  /// The schedule record as it was at the close, for a restore here: kept as stored
  /// (`meeting_schedule::MeetingSchedule`), it goes back unchanged.
  #[serde(rename = "schedule", default, skip_serializing_if = "Option::is_none")]
  pub maybe_schedule: Option<serde_json::Value>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Closes `meeting` on this device: the organizer cancelled it or ended it for good here, or its
  /// chat went. Its mark ([`ClosedMeeting`]) is written first, then the lobby and schedule records
  /// go. A meeting that comes back (a restored cancellation, a re-added organizer) is signed no
  /// longer than until the close, and guests of the link wait in the lobby.
  pub fn forget_meeting_records(&self, meeting: MeetingId) -> Result<(), CoreError> {
    if self.meeting_closed_at(meeting)?.is_none() {
      let closed = ClosedMeeting {
        meeting,
        closed_at_ms: self.now_ms(),
        maybe_has_lobby: records::get_json(&self.store, Table::SyncState, &lobby_record_key(meeting))?,
        maybe_schedule: records::get_json(&self.store, Table::SyncState, &schedule_record_key(meeting))?,
      };
      records::put_json(
        &self.store,
        Table::SyncState,
        &closed_record_key(meeting),
        &closed,
      )?;
    }
    self.store.delete(Table::SyncState, &lobby_record_key(meeting))?;
    self
      .store
      .delete(Table::SyncState, &schedule_record_key(meeting))?;
    Ok(())
  }

  /// The organizer restored `meeting` from this device (#1983): the server took this user's own
  /// restore of the whole meeting, and `organizer` is the restored meeting's organizer. Only when
  /// that is this user does the close mark go, and the lobby and schedule records it kept come
  /// back as they were at the close. The admitters are then signed from those records again
  /// ([`Client::sync_scheduled_meeting_admitters`]), so guests of the restored meeting can be let
  /// in. Called only after this device's own restore, never on the server's word about a
  /// meeting's status: a meeting the server brings back by itself stays capped at the close.
  /// Returns whether a mark was lifted.
  pub fn reopen_meeting_records(&self, meeting: MeetingId, organizer: UserId) -> Result<bool, CoreError> {
    if organizer != self.user_id() {
      tracing::warn!(meeting_id = %meeting, "a restored meeting this user doesn't organize; its mark stays");
      return Ok(false);
    }
    let maybe_closed: Option<ClosedMeeting> =
      records::get_json(&self.store, Table::SyncState, &closed_record_key(meeting))?;
    let Some(closed) = maybe_closed else {
      return Ok(false);
    };
    if let Some(has_lobby) = closed.maybe_has_lobby {
      records::put_json(
        &self.store,
        Table::SyncState,
        &lobby_record_key(meeting),
        &has_lobby,
      )?;
    }
    if let Some(schedule) = &closed.maybe_schedule {
      records::put_json(
        &self.store,
        Table::SyncState,
        &schedule_record_key(meeting),
        schedule,
      )?;
    }
    // The mark goes last: a crash before leaves the meeting closed, never open without its records.
    self.store.delete(Table::SyncState, &closed_record_key(meeting))?;
    tracing::info!(meeting_id = %meeting, "restored here; its close mark is lifted");
    Ok(true)
  }

  /// The meeting chat `conversation` is purged or forgotten here: its meeting's records go with
  /// it, found through the meeting this device noted for the chat. A chat without that note (set
  /// up before #1747, or not a meeting chat) has nothing to find.
  pub(super) fn forget_meeting_of_chat(&self, conversation: ConversationId) -> Result<(), CoreError> {
    if let Some(meeting) = self.noted_meeting_of_chat(conversation)? {
      self.forget_meeting_records(meeting)?;
    }
    self
      .store
      .delete(Table::SyncState, &meeting_of_chat_key(conversation))?;
    Ok(())
  }

  /// When this device closed `meeting`, if it did.
  pub(super) fn meeting_closed_at(&self, meeting: MeetingId) -> Result<Option<i64>, CoreError> {
    let maybe_closed: Option<ClosedMeeting> =
      records::get_json(&self.store, Table::SyncState, &closed_record_key(meeting))?;
    Ok(maybe_closed.map(|closed| closed.closed_at_ms))
  }

  /// The meeting this device noted for the meeting chat `conversation`, if it did.
  pub(super) fn noted_meeting_of_chat(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<MeetingId>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &meeting_of_chat_key(conversation))
  }
}

pub(super) fn lobby_record_key(meeting: MeetingId) -> String {
  format!("{LOBBY_RECORD_PREFIX}{meeting}")
}

pub(super) fn schedule_record_key(meeting: MeetingId) -> String {
  format!("{SCHEDULE_RECORD_PREFIX}{meeting}")
}

pub(super) fn meeting_of_chat_key(conversation: ConversationId) -> String {
  format!("{MEETING_OF_CHAT_PREFIX}{conversation}")
}

fn closed_record_key(meeting: MeetingId) -> String {
  format!("{CLOSED_RECORD_PREFIX}{meeting}")
}
