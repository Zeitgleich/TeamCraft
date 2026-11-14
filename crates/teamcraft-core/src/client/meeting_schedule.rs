//! The schedule the organizer set for a meeting on this device, and the admitters it signs from
//! it (#1919, e2ee.md §4.6, "Who is a guest, and until when"): a server that lengthens a meeting
//! after it was scheduled here gets no longer signed validity for the meeting's guests.

use teamcraft_client_api::meeting_schedule::{self, MeetingSchedule, MeetingTimes, ScheduleNote};
use teamcraft_proto::{ConversationId, MeetingId};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// The schedule of a meeting as the organizer set it on this device: the admitters' signed validity
/// follows it, never a later end the server reports.
const SCHEDULE_RECORD_PREFIX: &str = "meeting-schedule-record/";

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Keeps what the organizer set of `meeting`'s schedule on this device: a meeting scheduled here
  /// replaces the record, an edit changes what it changed ([`MeetingSchedule::after`]).
  pub fn note_meeting_schedule(&self, meeting: MeetingId, note: ScheduleNote) -> Result<(), CoreError> {
    let max_instant_ms = i64::try_from(self.settings.max_instant_meeting.as_millis()).unwrap_or(i64::MAX);
    let maybe_recorded = self.meeting_schedule(meeting)?;
    let Some(schedule) = MeetingSchedule::after(maybe_recorded, note, self.now_ms(), max_instant_ms) else {
      return Ok(());
    };
    records::put_json(&self.store, Table::SyncState, &record_key(meeting), &schedule)
  }

  /// [`Client::sync_meeting_admitters`] for the meeting (or occurrence) the server lists with
  /// `server_times`: valid until its end and the guests' grace, but never later than the schedule
  /// recorded on this device says ([`meeting_schedule::admitters_until`]). The apps sign through
  /// this, never with an end of their own.
  pub async fn sync_scheduled_meeting_admitters(
    &mut self,
    meeting: MeetingId,
    conversation: ConversationId,
    server_times: MeetingTimes,
  ) -> Result<bool, CoreError> {
    let expires_at_ms = meeting_schedule::admitters_until(
      server_times,
      self.meeting_schedule(meeting)?.as_ref(),
      self.now_ms(),
    );
    self
      .sync_meeting_admitters(meeting, conversation, expires_at_ms)
      .await
  }

  /// The schedule the organizer set for `meeting` on this device, if it did.
  fn meeting_schedule(&self, meeting: MeetingId) -> Result<Option<MeetingSchedule>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &record_key(meeting))
  }
}

fn record_key(meeting: MeetingId) -> String {
  format!("{SCHEDULE_RECORD_PREFIX}{meeting}")
}
