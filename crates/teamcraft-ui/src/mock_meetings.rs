//! The meetings API of the demo build (`?backend=mock`): a few meetings around the current time
//! (one running, one soon, a weekday series, an invitation without an answer, a cancelled date and
//! past ones), so the meetings page can be looked at without a server. Answers, cancelling,
//! restoring and hiding change the demo data; the rest is refused.

use std::cell::RefCell;

use teamcraft_proto::{
  ListLobbyResponse, ListMeetingsResponse, MeetingAttendeeResponse, MeetingKind, MeetingRecurrence,
  MeetingResponse, MeetingStatus, RecurrenceFrequency, RsvpStatus, SetRsvpRequest, UnixMillis, Weekday,
};

use crate::auth::{ApiMethod, ApiResponse, AuthError, Authenticator, LocalFuture, SignInLook};
use crate::clock::{local_day, local_day_start, now_ms};
use crate::model::{ConversationId, TimestampMs, UserId, id_from_raw};

const MINUTE: i64 = 60_000;
const DAY: i64 = 24 * 60 * MINUTE;
const STATUS_OK: u16 = 200;
const STATUS_NO_CONTENT: u16 = 204;
const STATUS_NOT_FOUND: u16 = 404;

/// The demo's meetings, answering the paths `MeetingsClient` uses.
pub struct MockMeetings {
  meetings: RefCell<Vec<MeetingResponse>>,
}

impl MockMeetings {
  pub fn new(now: TimestampMs) -> Self {
    Self {
      meetings: RefCell::new(demo_meetings(now)),
    }
  }

  fn answer(&self, method: ApiMethod, path: &str, maybe_body: Option<String>) -> ApiResponse {
    let (route, query) = path.split_once('?').unwrap_or((path, ""));
    let parts: Vec<&str> = route.trim_start_matches("/api/v1/meetings").split('/').collect();
    match (method, parts.as_slice()) {
      (ApiMethod::Get, [""]) => self.list(query),
      (ApiMethod::Get, ["", _, "lobby"]) => json(&ListLobbyResponse {
        entries: Vec::new(),
        is_host: true,
      }),
      (ApiMethod::Put, ["", id, "rsvp"]) => {
        let maybe_status = maybe_body
          .and_then(|body| serde_json::from_str::<SetRsvpRequest>(&body).ok())
          .map(|request| request.status);
        match maybe_status {
          Some(status) => self.change(id, |meeting| set_my_answer(meeting, status.clone())),
          None => status(STATUS_NOT_FOUND),
        }
      },
      (ApiMethod::Post, ["", id, "restore"]) => self.change(id, |meeting| {
        meeting.status = MeetingStatus::Scheduled;
        meeting.is_occurrence_cancelled = false;
        meeting.is_occurrence_holiday = false;
      }),
      (ApiMethod::Post, ["", id, "hide"]) => {
        self
          .meetings
          .borrow_mut()
          .retain(|meeting| meeting.id.to_string() != *id);
        status(STATUS_NO_CONTENT)
      },
      (ApiMethod::Delete, ["", id, ..]) => {
        self.change(id, |meeting| meeting.status = MeetingStatus::Cancelled)
      },
      _ => status(STATUS_NOT_FOUND),
    }
  }

  fn list(&self, query: &str) -> ApiResponse {
    let param = |name: &str| {
      query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .and_then(|(_, value)| value.parse::<i64>().ok())
    };
    let from = param("from").unwrap_or_else(now_ms);
    let maybe_to = param("to");
    let meetings = self
      .meetings
      .borrow()
      .iter()
      .filter(|meeting| meeting.ends_at.0 > from && maybe_to.is_none_or(|to| meeting.starts_at.0 < to))
      .cloned()
      .collect();
    json(&ListMeetingsResponse {
      meetings,
      maybe_server_now: Some(UnixMillis(now_ms())),
    })
  }

  fn change(&self, id: &str, edit: impl Fn(&mut MeetingResponse)) -> ApiResponse {
    let mut meetings = self.meetings.borrow_mut();
    let mut found = false;
    for meeting in meetings.iter_mut().filter(|meeting| meeting.id.to_string() == id) {
      edit(meeting);
      found = true;
    }
    status(if found {
      STATUS_NO_CONTENT
    } else {
      STATUS_NOT_FOUND
    })
  }
}

impl Authenticator for MockMeetings {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(async { Ok(true) })
  }

  fn login(&self, _look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { Ok(()) })
  }

  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { Ok(()) })
  }

  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    let response = self.answer(method, path, maybe_body);
    Box::pin(async move { Ok(response) })
  }
}

fn demo_meetings(now: TimestampMs) -> Vec<MeetingResponse> {
  let today = local_day_start(now, 0);
  let rounded = now - now.rem_euclid(5 * MINUTE);
  let mut meetings = vec![
    meeting(1, "Design review", rounded - 20 * MINUTE, 60, me())
      .with_attendees(&[
        (2, RsvpStatus::Accepted),
        (3, RsvpStatus::Tentative),
        (4, RsvpStatus::NeedsAction),
      ])
      .with_description("Walk through the new meetings page and decide on the open questions."),
    meeting(2, "Sprint planning", rounded + 25 * MINUTE, 90, user(2)).with_attendees(&[
      (1, RsvpStatus::NeedsAction),
      (3, RsvpStatus::Accepted),
      (5, RsvpStatus::Accepted),
      (6, RsvpStatus::Tentative),
      (7, RsvpStatus::Declined),
    ]),
    meeting(
      5,
      "Customer call – Project A",
      today + DAY + 14 * 60 * MINUTE,
      45,
      me(),
    )
    .with_attendees(&[(4, RsvpStatus::Accepted)])
    .with_external("contact@example.com"),
    meeting(6, "All hands", today + 5 * DAY + 16 * 60 * MINUTE, 60, user(4)).with_attendees(&[
      (1, RsvpStatus::NeedsAction),
      (2, RsvpStatus::Accepted),
      (3, RsvpStatus::Accepted),
      (5, RsvpStatus::Accepted),
      (6, RsvpStatus::Accepted),
      (7, RsvpStatus::Tentative),
      (8, RsvpStatus::NeedsAction),
    ]),
    meeting(7, "Retrospective", today - DAY + 15 * 60 * MINUTE, 60, user(3))
      .with_attendees(&[(1, RsvpStatus::Accepted), (2, RsvpStatus::Accepted)]),
    meeting(
      8,
      "Kickoff – Project B",
      today - 3 * DAY + 10 * 60 * MINUTE,
      30,
      me(),
    )
    .with_attendees(&[(5, RsvpStatus::Accepted), (6, RsvpStatus::Declined)]),
  ];
  // A weekday stand-up: one entry per date, as the server lists a series.
  for offset in -2..12 {
    let start = today + offset * DAY + 9 * 60 * MINUTE + 30 * MINUTE;
    if is_weekend(start) {
      continue;
    }
    let mut occurrence = meeting(3, "Daily stand-up", start, 15, user(6))
      .with_attendees(&[
        (1, RsvpStatus::Accepted),
        (2, RsvpStatus::Accepted),
        (7, RsvpStatus::Accepted),
      ])
      .with_series(RecurrenceFrequency::Daily, workdays());
    if offset == 3 {
      occurrence.status = MeetingStatus::Cancelled;
      occurrence.is_occurrence_cancelled = true;
    }
    meetings.push(occurrence);
  }
  // A weekly 1:1 the user answered "maybe".
  for week in 0..6 {
    let start = today + week * 7 * DAY + 2 * DAY + 11 * 60 * MINUTE;
    meetings.push(
      meeting(4, "1:1 Sam / Alex", start, 30, user(2))
        .with_attendees(&[(1, RsvpStatus::Tentative)])
        .with_series(RecurrenceFrequency::Weekly, Vec::new()),
    );
  }
  meetings.sort_by_key(|meeting| meeting.starts_at.0);
  meetings
}

fn meeting(id: u64, title: &str, starts_at: TimestampMs, minutes: i64, organizer: UserId) -> MeetingResponse {
  MeetingResponse {
    id: id_from_raw(id),
    title: title.to_owned(),
    maybe_description: None,
    starts_at: UnixMillis(starts_at),
    ends_at: UnixMillis(starts_at + minutes * MINUTE),
    kind: MeetingKind::Scheduled,
    status: MeetingStatus::Scheduled,
    has_lobby: true,
    organizer_user_id: organizer,
    conversation_id: id_from_raw::<ConversationId>(5_000 + id),
    // The demo opens in its first space; the meetings page lists a space's own meetings.
    maybe_space: Some(id_from_raw(1)),
    join_url: format!("https://teamcraft.example.com/meeting/demo-{id}"),
    maybe_recurrence: None,
    is_occurrence_cancelled: false,
    is_occurrence_holiday: false,
    can_host_lobby: organizer == me(),
    co_host_user_ids: Vec::new(),
    attendees: Vec::new(),
    undelivered_invitations: Vec::new(),
  }
}

trait DemoMeeting {
  fn with_attendees(self, people: &[(u64, RsvpStatus)]) -> Self;
  fn with_external(self, email: &str) -> Self;
  fn with_description(self, text: &str) -> Self;
  fn with_series(self, frequency: RecurrenceFrequency, weekdays: Vec<Weekday>) -> Self;
}

impl DemoMeeting for MeetingResponse {
  fn with_attendees(mut self, people: &[(u64, RsvpStatus)]) -> Self {
    self
      .attendees
      .extend(people.iter().map(|(id, rsvp)| MeetingAttendeeResponse {
        maybe_user_id: Some(user(*id)),
        maybe_email: None,
        maybe_name: None,
        rsvp: rsvp.clone(),
      }));
    self
  }

  fn with_external(mut self, email: &str) -> Self {
    self.attendees.push(MeetingAttendeeResponse {
      maybe_user_id: None,
      maybe_email: Some(email.to_owned()),
      maybe_name: None,
      rsvp: RsvpStatus::NeedsAction,
    });
    self
  }

  fn with_description(mut self, text: &str) -> Self {
    self.maybe_description = Some(text.to_owned());
    self
  }

  fn with_series(mut self, frequency: RecurrenceFrequency, weekdays: Vec<Weekday>) -> Self {
    self.maybe_recurrence = Some(MeetingRecurrence {
      frequency,
      interval: 1,
      weekdays,
      maybe_count: None,
      maybe_until: None,
      skips_holidays: false,
    });
    self
  }
}

fn set_my_answer(meeting: &mut MeetingResponse, status: RsvpStatus) {
  for attendee in meeting
    .attendees
    .iter_mut()
    .filter(|attendee| attendee.maybe_user_id == Some(me()))
  {
    attendee.rsvp = status.clone();
  }
}

fn is_weekend(ts: TimestampMs) -> bool {
  // Day 0 of the Unix epoch was a Thursday: Monday is 0 here.
  (local_day(ts) + 3).rem_euclid(7) >= 5
}

fn workdays() -> Vec<Weekday> {
  vec![
    Weekday::Monday,
    Weekday::Tuesday,
    Weekday::Wednesday,
    Weekday::Thursday,
    Weekday::Friday,
  ]
}

fn json<T: serde::Serialize>(value: &T) -> ApiResponse {
  ApiResponse {
    status: STATUS_OK,
    body: serde_json::to_string(value).unwrap_or_default(),
  }
}

fn status(code: u16) -> ApiResponse {
  ApiResponse {
    status: code,
    body: String::new(),
  }
}

fn me() -> UserId {
  user(1)
}

fn user(id: u64) -> UserId {
  id_from_raw(id)
}
