//! What this device keeps per meeting it organizes, and when it goes (#1939): the lobby setting
//! (#987), the schedule (#1919) and the meeting a meeting chat belongs to. The first two are keyed
//! by the meeting, so a chat that is purged or forgotten finds them through the third.

use teamcraft_proto::{ConversationId, MeetingId};

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

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Drops this device's lobby and schedule records of `meeting`: the organizer cancelled it or
  /// ended it for good here, so nothing signs for it any more. A meeting that comes back (a
  /// restored cancellation) is treated like one scheduled on another device: signed until the
  /// server's end, guests of the link wait in the lobby.
  pub fn forget_meeting_records(&self, meeting: MeetingId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &lobby_record_key(meeting))?;
    self
      .store
      .delete(Table::SyncState, &schedule_record_key(meeting))?;
    Ok(())
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
