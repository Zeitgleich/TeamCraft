//! Scheduling and changing a meeting as its organizer (docs/meetings.md), and the upkeep the
//! organizer's device does around it: the edits whose chat change is still owed, and the signed
//! admitters of meetings that run or start soon (#1103). Desktop and web (`teamcraft-ui`) and the
//! mobile apps (through teamcraft-ffi) decide alike; the app keeps the records where it can.
//!
//! [`MeetingDraft`] is the schedule form in plain times, with its one validation: the mobile apps
//! fill it through teamcraft-ffi, desktop and web from their text inputs
//! (`teamcraft_ui::meetings::ScheduleForm`).

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  CreateMeetingRequest, IdempotencyKey, MeetingId, MeetingInvitee, MeetingRecurrence, MeetingResponse,
  MeetingStatus, RecurrenceFrequency, UnixMillis, UpdateMeetingRequest, Weekday,
};

use crate::meeting_invitees::attending_users;
use crate::meeting_schedule::MeetingTimes;
use crate::model::{ConversationId, TimestampMs, UserId};

pub const MS_PER_MINUTE: i64 = 60_000;
const MS_PER_HOUR: i64 = 3_600_000;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;
/// How long a meeting may be scheduled for in one go (the server's limit).
pub const MAX_DURATION_MINUTES: u32 = 24 * 60;
/// Largest "every N days, weeks or months" and number of meetings of a series (the server's limits).
pub const MAX_INTERVAL: u32 = 100;
pub const MAX_COUNT: u32 = 1_000;
/// The days of the week, Monday first, as the wire names them.
pub const WEEKDAYS: [Weekday; 7] = [
  Weekday::Monday,
  Weekday::Tuesday,
  Weekday::Wednesday,
  Weekday::Thursday,
  Weekday::Friday,
  Weekday::Saturday,
  Weekday::Sunday,
];
/// A new meeting starts at least this far ahead of now, at a full hour.
const MIN_LEAD_MS: i64 = 10 * 60_000;
/// The organizer's device keeps the signed admitters current for meetings that run or start within
/// this time (#1103); later ones keep the list signed when they were scheduled or edited.
pub const ADMITTERS_LEAD_MS: i64 = 24 * 60 * 60_000;
/// How often per meeting the organizer's device has the core compare its signed admitters.
pub const ADMITTERS_SYNC_INTERVAL_MS: i64 = 10 * 60_000;
/// A pending edit younger than this may still be in flight in this session; only older ones are
/// reconciled.
const PENDING_EDIT_GRACE_MS: i64 = 2 * 60_000;
/// A pending edit older than this is dropped: the meeting is long gone from the list.
const PENDING_EDIT_MAX_AGE_MS: i64 = 7 * 24 * 3_600_000;

/// A meeting the organizer schedules or changes, as a form collects it: the apps' sheet and,
/// through `teamcraft_ui::meetings::ScheduleForm`, the dialog of desktop and web.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeetingDraft {
  pub title: String,
  pub description: String,
  /// `None` while the start field holds no time (a text input).
  pub maybe_starts_at: Option<TimestampMs>,
  /// The start's offset from UTC in the user's time zone, in minutes (east positive): a series'
  /// days of the week are the user's local days.
  pub utc_offset_minutes: i32,
  pub duration_minutes: u32,
  /// The invited TeamCraft users, the organizer left out.
  pub members: Vec<UserId>,
  /// People invited by e-mail address (no account).
  pub externals: Vec<ExternalInvitee>,
  pub repeat: RepeatRule,
  pub has_lobby: bool,
  /// The notetaker's account when "Notetaker joins the calls" is ticked (e2ee.md §8.2): invited
  /// like a member, so it is in the chat's group before any call.
  pub maybe_notetaker: Option<UserId>,
  /// Invited people who host with the organizer (#1720); signed by the organizer's device once the
  /// meeting is saved (`meeting_hosts::co_hosts_from_dialog`).
  pub co_hosts: Vec<UserId>,
  /// Made when the form opens and kept for every submit of it, so a retry after a lost answer
  /// schedules the meeting only once. `None` for an edit.
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// How a meeting repeats, as the form has it: every `interval` days, weeks or months, on which days
/// of the week, and until when.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepeatRule {
  pub repeat: Repeat,
  /// Every n-th day, week or month; 0 when the field holds no number.
  pub interval: u32,
  /// The days of the week a daily or weekly series meets on, Monday first. All of them: every day.
  pub weekdays: [bool; 7],
  pub ends: SeriesEnd,
  /// Number of meetings of a series that ends after a count; 0 when the field holds no number.
  pub count: u32,
  /// The last moment of the last day of a series that ends on a date, local time; `None` while the
  /// field holds no date.
  pub maybe_until: Option<TimestampMs>,
  /// The series leaves out public holidays (#1053).
  pub skips_holidays: bool,
}

/// How often a meeting repeats, as the form offers it: the unit of "every N …".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
  Never,
  Daily,
  Weekly,
  Monthly,
}

/// How a series ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeriesEnd {
  Never,
  AfterCount,
  OnDate,
}

/// Somebody invited by e-mail address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalInvitee {
  pub email: String,
  pub name: String,
}

/// Why a draft can't be saved; the form shows each at its field ([`DraftProblem::field`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum DraftProblem {
  MissingTitle,
  /// The start field holds no time.
  InvalidStart,
  StartInThePast,
  InvalidDuration,
  InvalidInterval,
  NoWeekday,
  /// The first meeting is the start itself: a start off the chosen days would surprise.
  StartOffWeekdays,
  InvalidCount,
  InvalidUntil,
  UntilBeforeStart,
  /// This address can't be one.
  InvalidEmail(String),
}

/// The fields of the form a problem can belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DraftField {
  Title,
  Start,
  Duration,
  Interval,
  Weekdays,
  Count,
  Until,
  Externals,
}

impl MeetingDraft {
  /// A new meeting: 30 minutes at the next full hour with some lead, with a lobby. `utc_offset_minutes`
  /// is the user's offset at that start.
  pub fn new(now: TimestampMs, utc_offset_minutes: i32) -> Self {
    Self {
      title: String::new(),
      description: String::new(),
      maybe_starts_at: Some(suggested_start(now)),
      utc_offset_minutes,
      duration_minutes: 30,
      members: Vec::new(),
      externals: Vec::new(),
      repeat: RepeatRule::default(),
      has_lobby: true,
      maybe_notetaker: None,
      co_hosts: Vec::new(),
      maybe_idempotency_key: Some(IdempotencyKey::random()),
    }
  }

  /// The form of an edit: what the saved meeting says now. The organizer `me` is no invitee.
  /// `utc_offset_minutes` is the user's offset at the meeting's start.
  pub fn from_meeting(meeting: &MeetingResponse, me: UserId, utc_offset_minutes: i32) -> Self {
    let minutes = (meeting.ends_at.0 - meeting.starts_at.0) / MS_PER_MINUTE;
    let mut members = Vec::new();
    for user in attending_users(meeting) {
      if user != me && user != meeting.organizer_user_id && !members.contains(&user) {
        members.push(user);
      }
    }
    let externals = meeting
      .attendees
      .iter()
      .filter_map(|attendee| match (attendee.maybe_user_id, &attendee.maybe_email) {
        (None, Some(email)) => Some(ExternalInvitee {
          email: email.clone(),
          name: attendee.maybe_name.clone().unwrap_or_default(),
        }),
        _ => None,
      })
      .collect();
    Self {
      title: meeting.title.clone(),
      description: meeting.maybe_description.clone().unwrap_or_default(),
      maybe_starts_at: Some(meeting.starts_at.0),
      utc_offset_minutes,
      duration_minutes: u32::try_from(minutes).unwrap_or(0),
      members,
      externals,
      repeat: meeting
        .maybe_recurrence
        .as_ref()
        .map(RepeatRule::of)
        .unwrap_or_default(),
      has_lobby: meeting.has_lobby,
      maybe_notetaker: None,
      co_hosts: meeting.co_host_user_ids.clone(),
      maybe_idempotency_key: None,
    }
  }

  /// The notetaker `maybe_bot` attends: it is the "Notetaker joins the calls" box, not a member in
  /// the people list.
  pub fn with_notetaker(mut self, maybe_bot: Option<UserId>) -> Self {
    if let Some(bot) = maybe_bot
      && self.members.contains(&bot)
    {
      self.members.retain(|member| *member != bot);
      self.maybe_notetaker = Some(bot);
    }
    self
  }

  /// Picks how the meeting repeats, with the days that go with it: every day for a daily series,
  /// the start's day for a weekly one.
  pub fn set_repeat(&mut self, repeat: Repeat) {
    if repeat == self.repeat.repeat {
      return;
    }
    self.repeat.repeat = repeat;
    self.repeat.weekdays = match (repeat, self.start_weekday()) {
      (Repeat::Weekly, Some(day)) => only_day(day),
      _ => [true; 7],
    };
  }

  /// What keeps the draft from being saved, in the order of the fields. `maybe_saved_start` is the
  /// start of the meeting being edited: unchanged, it may lie in the past (a meeting under way can
  /// still get a new invitee).
  pub fn problems(&self, now: TimestampMs, maybe_saved_start: Option<TimestampMs>) -> Vec<DraftProblem> {
    match self.checked(now, maybe_saved_start) {
      Ok(_) => Vec::new(),
      Err(problems) => problems,
    }
  }

  /// The request that schedules the meeting, or every reason it can't be sent.
  pub fn to_request(&self, now: TimestampMs) -> Result<CreateMeetingRequest, Vec<DraftProblem>> {
    self.checked(now, None)
  }

  /// The change to save to `before`: the time only when it moved, the invitees as the whole list,
  /// the repeat rule when it changed.
  pub fn to_update_request(
    &self,
    now: TimestampMs,
    before: &MeetingResponse,
  ) -> Result<UpdateMeetingRequest, Vec<DraftProblem>> {
    let create = self.checked(now, Some(before.starts_at.0))?;
    // Whole minutes: seconds of the saved times are not a change.
    let has_moved = (create.starts_at.0 - before.starts_at.0).abs() >= MS_PER_MINUTE
      || (create.ends_at.0 - before.ends_at.0).abs() >= MS_PER_MINUTE;
    Ok(UpdateMeetingRequest {
      maybe_title: Some(create.title),
      maybe_description: Some(create.maybe_description.unwrap_or_default()),
      maybe_starts_at: has_moved.then_some(create.starts_at),
      maybe_ends_at: has_moved.then_some(create.ends_at),
      maybe_invitees: Some(create.invitees),
      maybe_has_lobby: Some(create.has_lobby),
      maybe_recurrence: create
        .maybe_recurrence
        .filter(|recurrence| before.maybe_recurrence.as_ref() != Some(recurrence)),
    })
  }

  /// The local day of the week of the start, Monday first.
  pub fn start_weekday(&self) -> Option<usize> {
    let start = self.maybe_starts_at?;
    let local_day = (start + i64::from(self.utc_offset_minutes) * MS_PER_MINUTE).div_euclid(MS_PER_DAY);
    // 1970-01-01 was a Thursday.
    usize::try_from((local_day + 3).rem_euclid(7)).ok()
  }

  /// The create request, or every problem in the order of the fields. `maybe_unchanged_start` needs
  /// no future check (the saved start of a meeting being edited).
  fn checked(
    &self,
    now: TimestampMs,
    maybe_unchanged_start: Option<TimestampMs>,
  ) -> Result<CreateMeetingRequest, Vec<DraftProblem>> {
    let mut problems = Vec::new();
    let title = self.title.trim();
    if title.is_empty() {
      problems.push(DraftProblem::MissingTitle);
    }
    match self.maybe_starts_at {
      None => problems.push(DraftProblem::InvalidStart),
      Some(starts_at) => {
        let is_unchanged =
          maybe_unchanged_start.is_some_and(|saved| (starts_at - saved).abs() < MS_PER_MINUTE);
        if starts_at < now - MS_PER_MINUTE && !is_unchanged {
          problems.push(DraftProblem::StartInThePast);
        }
      },
    }
    if self.duration_minutes == 0 || self.duration_minutes > MAX_DURATION_MINUTES {
      problems.push(DraftProblem::InvalidDuration);
    }
    let maybe_recurrence = self.recurrence(&mut problems);
    let invitees = self.invitees(&mut problems);
    let Some(starts_at) = self.maybe_starts_at.filter(|_| problems.is_empty()) else {
      return Err(problems);
    };
    Ok(CreateMeetingRequest {
      title: title.to_owned(),
      maybe_description: Some(self.description.trim().to_owned()).filter(|text| !text.is_empty()),
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + i64::from(self.duration_minutes) * MS_PER_MINUTE),
      invitees,
      maybe_recurrence,
      has_lobby: self.has_lobby,
      // The caller sets it: the form doesn't know where it is opened.
      maybe_space: None,
      maybe_idempotency_key: self.maybe_idempotency_key,
    })
  }

  /// The repeat rule of the draft, noting what is wrong with it.
  fn recurrence(&self, problems: &mut Vec<DraftProblem>) -> Option<MeetingRecurrence> {
    let rule = &self.repeat;
    let frequency = rule.repeat.frequency()?;
    if rule.interval == 0 || rule.interval > MAX_INTERVAL {
      problems.push(DraftProblem::InvalidInterval);
    }
    let maybe_start_day = self.start_weekday();
    // Every day, or a weekly series on its start's day: the rule needs no days.
    let is_plain = !rule.weekdays.contains(&false)
      || (rule.repeat == Repeat::Weekly && maybe_start_day.map(only_day) == Some(rule.weekdays));
    let weekdays: Vec<Weekday> = if rule.repeat.has_weekdays() && !is_plain {
      WEEKDAYS
        .into_iter()
        .zip(rule.weekdays)
        .filter_map(|(day, is_on)| is_on.then_some(day))
        .collect()
    } else {
      Vec::new()
    };
    if rule.repeat.has_weekdays() && !rule.weekdays.contains(&true) {
      problems.push(DraftProblem::NoWeekday);
    } else if rule.repeat.has_weekdays()
      && let Some(day) = maybe_start_day
      && !rule.weekdays[day]
    {
      problems.push(DraftProblem::StartOffWeekdays);
    }
    let (mut maybe_count, mut maybe_until) = (None, None);
    match rule.ends {
      SeriesEnd::Never => {},
      SeriesEnd::AfterCount if (1..=MAX_COUNT).contains(&rule.count) => maybe_count = Some(rule.count),
      SeriesEnd::AfterCount => problems.push(DraftProblem::InvalidCount),
      SeriesEnd::OnDate => match rule.maybe_until {
        None => problems.push(DraftProblem::InvalidUntil),
        Some(until) if self.maybe_starts_at.is_some_and(|start| until < start) => {
          problems.push(DraftProblem::UntilBeforeStart);
        },
        Some(until) => maybe_until = Some(UnixMillis(until)),
      },
    }
    Some(MeetingRecurrence {
      frequency,
      interval: rule.interval,
      weekdays,
      maybe_count,
      maybe_until,
      skips_holidays: rule.skips_holidays,
    })
  }

  /// Everyone the draft invites, each once: the members, the notetaker, the e-mail addresses
  /// (noting the ones that can't be addresses).
  fn invitees(&self, problems: &mut Vec<DraftProblem>) -> Vec<MeetingInvitee> {
    let mut users: Vec<UserId> = Vec::new();
    for user in self.members.iter().chain(&self.maybe_notetaker) {
      if !users.contains(user) {
        users.push(*user);
      }
    }
    let mut invitees: Vec<MeetingInvitee> = users
      .into_iter()
      .map(|user_id| MeetingInvitee::User { user_id })
      .collect();
    for external in &self.externals {
      let email = external.email.trim();
      if !is_plausible_email(email) {
        problems.push(DraftProblem::InvalidEmail(email.to_owned()));
        continue;
      }
      invitees.push(MeetingInvitee::External {
        email: email.to_owned(),
        maybe_name: Some(external.name.trim().to_owned()).filter(|name| !name.is_empty()),
      });
    }
    invitees
  }
}

impl Default for RepeatRule {
  /// A meeting that doesn't repeat; once it does, every day or week until cancelled.
  fn default() -> Self {
    Self {
      repeat: Repeat::Never,
      interval: 1,
      weekdays: [true; 7],
      ends: SeriesEnd::Never,
      count: 10,
      maybe_until: None,
      skips_holidays: false,
    }
  }
}

impl RepeatRule {
  /// The form of a saved series' rule.
  fn of(recurrence: &MeetingRecurrence) -> Self {
    let mut rule = Self {
      repeat: Repeat::of(&recurrence.frequency),
      interval: recurrence.interval,
      skips_holidays: recurrence.skips_holidays,
      ..Self::default()
    };
    if !recurrence.weekdays.is_empty() {
      rule.weekdays = WEEKDAYS.map(|day| recurrence.weekdays.contains(&day));
    }
    if let Some(count) = recurrence.maybe_count {
      rule.ends = SeriesEnd::AfterCount;
      rule.count = count;
    }
    if let Some(until) = recurrence.maybe_until {
      rule.ends = SeriesEnd::OnDate;
      rule.maybe_until = Some(until.0);
    }
    rule
  }
}

impl Repeat {
  pub const ALL: [Repeat; 4] = [Self::Never, Self::Daily, Self::Weekly, Self::Monthly];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Never => "never",
      Self::Daily => "daily",
      Self::Weekly => "weekly",
      Self::Monthly => "monthly",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|repeat| repeat.slug() == slug)
  }

  /// Whether the series can be limited to days of the week.
  pub fn has_weekdays(self) -> bool {
    matches!(self, Self::Daily | Self::Weekly)
  }

  fn frequency(self) -> Option<RecurrenceFrequency> {
    match self {
      Self::Never => None,
      Self::Daily => Some(RecurrenceFrequency::Daily),
      Self::Weekly => Some(RecurrenceFrequency::Weekly),
      Self::Monthly => Some(RecurrenceFrequency::Monthly),
    }
  }

  fn of(frequency: &RecurrenceFrequency) -> Self {
    match frequency {
      RecurrenceFrequency::Daily => Self::Daily,
      RecurrenceFrequency::Weekly => Self::Weekly,
      RecurrenceFrequency::Monthly => Self::Monthly,
      RecurrenceFrequency::Unknown(_) => Self::Never,
    }
  }
}

impl SeriesEnd {
  pub const ALL: [SeriesEnd; 3] = [Self::Never, Self::AfterCount, Self::OnDate];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Never => "never",
      Self::AfterCount => "count",
      Self::OnDate => "date",
    }
  }
}

impl DraftProblem {
  pub fn field(&self) -> DraftField {
    match self {
      Self::MissingTitle => DraftField::Title,
      Self::InvalidStart | Self::StartInThePast => DraftField::Start,
      Self::InvalidDuration => DraftField::Duration,
      Self::InvalidInterval => DraftField::Interval,
      Self::NoWeekday | Self::StartOffWeekdays => DraftField::Weekdays,
      Self::InvalidCount => DraftField::Count,
      Self::InvalidUntil | Self::UntilBeforeStart => DraftField::Until,
      Self::InvalidEmail(_) => DraftField::Externals,
    }
  }
}

/// A cheap sanity check; the server and the mail system decide whether an address works.
pub fn is_plausible_email(text: &str) -> bool {
  let Some((local, domain)) = text.split_once('@') else {
    return false;
  };
  !local.is_empty()
    && !text.contains(char::is_whitespace)
    && domain.contains('.')
    && !domain.starts_with('.')
    && !domain.ends_with('.')
    && !domain.contains('@')
}

fn only_day(day: usize) -> [bool; 7] {
  let mut only = [false; 7];
  if let Some(slot) = only.get_mut(day) {
    *slot = true;
  }
  only
}

/// The next full hour that is at least ten minutes away.
pub fn suggested_start(now: TimestampMs) -> TimestampMs {
  let earliest = now + MIN_LEAD_MS;
  earliest.div_euclid(MS_PER_HOUR) * MS_PER_HOUR + MS_PER_HOUR
}

/// The TeamCraft users a meeting request invites (externals have no account to add).
pub fn invited_users(request: &CreateMeetingRequest) -> Vec<UserId> {
  request
    .invitees
    .iter()
    .filter_map(|invitee| match invitee {
      MeetingInvitee::User { user_id } => Some(*user_id),
      MeetingInvitee::External { .. } => None,
    })
    .collect()
}

/// Who joined and who left a meeting's invitee list when the organizer saved a change: the whole
/// list now (for the chat's encryption group) and the users taken off it. Both come from the
/// organizer's own saved request and the meeting as it was, never from the server's member list
/// (e2ee.md §4.6, "Meeting chats").
pub fn invitee_change(
  before: &MeetingResponse,
  request: &UpdateMeetingRequest,
) -> Option<(Vec<UserId>, Vec<UserId>)> {
  let invitees = request.maybe_invitees.as_ref()?;
  let now: Vec<UserId> = invitees
    .iter()
    .filter_map(|invitee| match invitee {
      MeetingInvitee::User { user_id } => Some(*user_id),
      MeetingInvitee::External { .. } => None,
    })
    .collect();
  let removed = attending_users(before)
    .into_iter()
    .filter(|user| !now.contains(user) && *user != before.organizer_user_id)
    .collect();
  Some((now, removed))
}

/// Whether the meeting has ended or was cancelled (shown dimmed, no join).
pub fn is_over(meeting: &MeetingResponse, now: TimestampMs) -> bool {
  meeting.status != MeetingStatus::Scheduled || meeting.ends_at.0 <= now
}

/// A meeting whose signed admitters the organizer's device compares now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmittersDue {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  /// The meeting's (occurrence's) times as the server lists them; the core caps the signed end
  /// by its own record (`meeting_schedule::admitters_until`).
  pub server_times: MeetingTimes,
}

/// The meetings `me` organizes that run or start within [`ADMITTERS_LEAD_MS`] and aren't over:
/// who is in the meeting chat's group changes, and a series' next meeting needs a list that lasts
/// until its end (#1103). The core signs anew only when that differs; the app asks at most every
/// [`ADMITTERS_SYNC_INTERVAL_MS`] per meeting.
pub fn admitters_due(meetings: &[MeetingResponse], me: UserId, now: TimestampMs) -> Vec<AdmittersDue> {
  let mut due: Vec<AdmittersDue> = Vec::new();
  for meeting in meetings {
    if meeting.organizer_user_id != me
      || is_over(meeting, now)
      || meeting.starts_at.0 > now + ADMITTERS_LEAD_MS
      || due.iter().any(|known| known.meeting == meeting.id)
    {
      continue;
    }
    due.push(AdmittersDue {
      meeting: meeting.id,
      conversation: meeting.conversation_id,
      server_times: MeetingTimes::of(meeting),
    });
  }
  due
}

// ---- Edits whose chat change is still owed ----------------------------------------------------

/// Users the organizer is about to take off a meeting, written down before the save is sent. If the
/// app dies between the save and the commit that removes them from the chat's group, the next start
/// finishes it ([`plan_reconcile`]); the core's own record of the removal only exists once
/// `update_meeting_chat_members` ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEdit {
  pub meeting: MeetingId,
  pub removed: Vec<UserId>,
  pub noted_at: TimestampMs,
}

/// The chat change a pending edit still owes: the meeting's chat, the attendees as the server lists
/// them now, and the users to take out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedChatChange {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  pub attending: Vec<UserId>,
  pub removed: Vec<UserId>,
}

/// `edits` with the edit of `meeting` that removes `removed` noted (replacing an older one of it).
/// An edit that removes nobody owes nothing.
pub fn noting_pending_edit(
  mut edits: Vec<PendingEdit>,
  meeting: MeetingId,
  removed: Vec<UserId>,
  now: TimestampMs,
) -> Vec<PendingEdit> {
  if removed.is_empty() {
    return edits;
  }
  edits.retain(|edit| edit.meeting != meeting);
  edits.push(PendingEdit {
    meeting,
    removed,
    noted_at: now,
  });
  edits
}

/// Finishes edits that were saved but not applied to the chat's group (the app was closed in
/// between), judging by the meeting list: users no longer attending are removed, an edit that never
/// reached the server is forgotten. The records to keep, and the chat changes to make.
pub fn plan_reconcile(
  edits: &[PendingEdit],
  now: TimestampMs,
  meetings: &[MeetingResponse],
) -> (Vec<PendingEdit>, Vec<OwedChatChange>) {
  let mut kept = Vec::new();
  let mut owed = Vec::new();
  for edit in edits {
    let age = now - edit.noted_at;
    if age > PENDING_EDIT_MAX_AGE_MS {
      continue;
    }
    if age < PENDING_EDIT_GRACE_MS {
      kept.push(edit.clone());
      continue;
    }
    // Not in the list (not loaded, or over): decided on a later list, until it ages out.
    let Some(meeting) = meetings.iter().find(|meeting| meeting.id == edit.meeting) else {
      kept.push(edit.clone());
      continue;
    };
    let attending = attending_users(meeting);
    let removed: Vec<UserId> = edit
      .removed
      .iter()
      .copied()
      .filter(|user| !attending.contains(user))
      .collect();
    if !removed.is_empty() {
      owed.push(OwedChatChange {
        meeting: meeting.id,
        conversation: meeting.conversation_id,
        attending,
        removed,
      });
    }
  }
  (kept, owed)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, MeetingAttendeeResponse, MeetingKind, RsvpStatus, Snowflake};

  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;
  /// 2027-01-18 00:00 UTC, a Monday after `NOW`.
  const MONDAY_UTC: TimestampMs = 1_800_230_400_000;

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn attendee(maybe_user_id: Option<UserId>, maybe_email: Option<&str>) -> MeetingAttendeeResponse {
    MeetingAttendeeResponse {
      maybe_user_id,
      maybe_email: maybe_email.map(str::to_owned),
      maybe_name: None,
      rsvp: RsvpStatus::NeedsAction,
    }
  }

  fn meeting(organizer: UserId) -> MeetingResponse {
    MeetingResponse {
      id: id(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(NOW + 2 * MS_PER_HOUR),
      ends_at: UnixMillis(NOW + 2 * MS_PER_HOUR + 30 * MS_PER_MINUTE),
      kind: MeetingKind::Scheduled,
      status: MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: organizer,
      conversation_id: id(9),
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: Vec::new(),
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    }
  }

  fn filled_draft() -> MeetingDraft {
    let mut draft = MeetingDraft::new(NOW, 0);
    draft.title = "Planning".into();
    draft
  }

  #[test]
  fn a_new_draft_starts_at_a_full_hour_with_some_lead() {
    let draft = MeetingDraft::new(NOW, 0);
    let start = draft.maybe_starts_at.expect("a start");
    assert_eq!(start % MS_PER_HOUR, 0);
    assert!(start >= NOW + MIN_LEAD_MS);
    assert!(draft.maybe_idempotency_key.is_some());
    assert_eq!(draft.problems(NOW, None), vec![DraftProblem::MissingTitle]);
  }

  #[test]
  fn a_draft_is_refused_for_what_the_server_would_refuse() {
    let mut draft = MeetingDraft::new(NOW, 0);
    draft.maybe_starts_at = Some(NOW - 5 * MS_PER_MINUTE);
    draft.duration_minutes = 0;
    assert_eq!(
      draft.to_request(NOW),
      Err(vec![
        DraftProblem::MissingTitle,
        DraftProblem::StartInThePast,
        DraftProblem::InvalidDuration
      ])
    );
    draft.duration_minutes = MAX_DURATION_MINUTES + 1;
    assert!(draft.problems(NOW, None).contains(&DraftProblem::InvalidDuration));
  }

  #[test]
  fn a_valid_draft_invites_each_member_once() {
    let mut draft = MeetingDraft::new(NOW, 0);
    draft.title = "  Planning ".into();
    draft.members = vec![id(2), id(3), id(2)];
    draft.duration_minutes = 45;
    let request = draft.to_request(NOW).expect("valid");
    assert_eq!(request.title, "Planning");
    assert_eq!(request.maybe_description, None);
    assert_eq!(request.ends_at.0 - request.starts_at.0, 45 * MS_PER_MINUTE);
    assert_eq!(invited_users(&request), vec![id(2), id(3)]);
    assert_eq!(request.maybe_idempotency_key, draft.maybe_idempotency_key);
  }

  #[test]
  fn an_edit_sends_the_whole_invitee_list_and_the_time_only_when_it_moved() {
    let organizer = id(1);
    let mut before = meeting(organizer);
    before.attendees = vec![
      attendee(Some(organizer), None),
      attendee(Some(id(2)), None),
      attendee(None, Some("guest@example.com")),
    ];
    let mut draft = MeetingDraft::from_meeting(&before, organizer, 0);
    assert_eq!(draft.members, vec![id(2)]);
    assert_eq!(draft.externals.len(), 1);
    assert_eq!(draft.duration_minutes, 30);
    draft.members = vec![id(3)];
    let request = draft.to_update_request(NOW, &before).expect("valid");
    assert_eq!(request.maybe_starts_at, None);
    assert_eq!(request.maybe_recurrence, None);
    assert_eq!(
      request.maybe_invitees,
      Some(vec![
        MeetingInvitee::User { user_id: id(3) },
        MeetingInvitee::External {
          email: "guest@example.com".into(),
          maybe_name: None
        },
      ])
    );
    let (invitees, removed) = invitee_change(&before, &request).expect("invitees sent");
    assert_eq!((invitees, removed), (vec![id(3)], vec![id(2)]));
    draft.duration_minutes = 60;
    assert!(
      draft
        .to_update_request(NOW, &before)
        .unwrap()
        .maybe_ends_at
        .is_some()
    );
  }

  #[test]
  fn a_meeting_under_way_can_be_edited_but_not_moved_into_the_past() {
    let mut before = meeting(id(1));
    before.starts_at = UnixMillis(NOW - 10 * MS_PER_MINUTE);
    before.ends_at = UnixMillis(NOW + 20 * MS_PER_MINUTE);
    let mut draft = MeetingDraft::from_meeting(&before, id(1), 0);
    draft.title = "Renamed".into();
    assert!(draft.to_update_request(NOW, &before).is_ok());
    draft.maybe_starts_at = Some(NOW - 60 * MS_PER_MINUTE);
    assert_eq!(
      draft.to_update_request(NOW, &before),
      Err(vec![DraftProblem::StartInThePast])
    );
  }

  #[test]
  fn emails_need_a_local_part_and_a_dotted_domain() {
    for good in ["a@b.co", "first.last@example.com"] {
      assert!(is_plausible_email(good), "{good}");
    }
    for bad in ["", "a", "@b.co", "a@b", "a@.co", "a@b.", "a b@c.de", "a@b@c.de"] {
      assert!(!is_plausible_email(bad), "{bad}");
    }
  }

  #[test]
  fn the_draft_refuses_what_the_server_would() {
    type Edit = fn(&mut MeetingDraft);
    let cases: [(Edit, DraftProblem); 11] = [
      (|draft| draft.title = "  ".into(), DraftProblem::MissingTitle),
      (|draft| draft.maybe_starts_at = None, DraftProblem::InvalidStart),
      (
        |draft| draft.maybe_starts_at = Some(NOW - MS_PER_HOUR),
        DraftProblem::StartInThePast,
      ),
      (|draft| draft.duration_minutes = 0, DraftProblem::InvalidDuration),
      (
        |draft| {
          draft.set_repeat(Repeat::Daily);
          draft.repeat.interval = MAX_INTERVAL + 1;
        },
        DraftProblem::InvalidInterval,
      ),
      (
        |draft| {
          draft.set_repeat(Repeat::Daily);
          draft.repeat.weekdays = [false; 7];
        },
        DraftProblem::NoWeekday,
      ),
      (
        |draft| {
          draft.set_repeat(Repeat::Weekly);
          draft.repeat.weekdays = only_day((draft.start_weekday().unwrap() + 1) % 7);
        },
        DraftProblem::StartOffWeekdays,
      ),
      (
        |draft| {
          draft.set_repeat(Repeat::Monthly);
          draft.repeat.ends = SeriesEnd::AfterCount;
          draft.repeat.count = 0;
        },
        DraftProblem::InvalidCount,
      ),
      (
        |draft| {
          draft.set_repeat(Repeat::Monthly);
          draft.repeat.ends = SeriesEnd::OnDate;
        },
        DraftProblem::InvalidUntil,
      ),
      (
        |draft| {
          draft.set_repeat(Repeat::Monthly);
          draft.repeat.ends = SeriesEnd::OnDate;
          draft.repeat.maybe_until = Some(NOW);
        },
        DraftProblem::UntilBeforeStart,
      ),
      (
        |draft| {
          draft.externals = vec![ExternalInvitee {
            email: " nobody ".into(),
            name: String::new(),
          }]
        },
        DraftProblem::InvalidEmail("nobody".into()),
      ),
    ];
    for (edit, expected) in cases {
      let mut draft = filled_draft();
      edit(&mut draft);
      let field = expected.field();
      assert_eq!(draft.to_request(NOW), Err(vec![expected]));
      assert_eq!(draft.problems(NOW, None)[0].field(), field);
    }
  }

  mod repeat {
    use super::*;

    #[test]
    fn the_days_of_the_week_are_the_users_local_days() {
      // 23:30 UTC on a Monday is Tuesday in UTC+1.
      let mut draft = filled_draft();
      draft.maybe_starts_at = Some(MONDAY_UTC + 23 * MS_PER_HOUR + 30 * MS_PER_MINUTE);
      assert_eq!(draft.start_weekday(), Some(0));
      draft.utc_offset_minutes = 60;
      assert_eq!(draft.start_weekday(), Some(1));
      draft.set_repeat(Repeat::Weekly);
      assert_eq!(draft.repeat.weekdays, only_day(1));
    }

    #[test]
    fn a_daily_series_on_work_days_names_them() {
      let mut draft = monday_draft();
      draft.set_repeat(Repeat::Daily);
      draft.repeat.weekdays = [true, true, true, true, true, false, false];
      let recurrence = draft.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!(recurrence.weekdays, WEEKDAYS[..5].to_vec());
      draft.repeat.weekdays = [true; 7];
      let every_day = draft.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert!(every_day.weekdays.is_empty());
    }

    #[test]
    fn a_weekly_series_on_its_start_day_names_no_days_and_can_add_others() {
      let mut draft = monday_draft();
      draft.set_repeat(Repeat::Weekly);
      assert_eq!(draft.repeat.weekdays, only_day(0));
      assert!(
        draft
          .to_request(NOW)
          .unwrap()
          .maybe_recurrence
          .unwrap()
          .weekdays
          .is_empty()
      );
      draft.repeat.weekdays[3] = true;
      let recurrence = draft.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!(recurrence.weekdays, vec![Weekday::Monday, Weekday::Thursday]);
    }

    #[test]
    fn a_series_ends_after_a_count_or_on_a_day() {
      let mut draft = monday_draft();
      draft.set_repeat(Repeat::Daily);
      draft.repeat.skips_holidays = true;
      draft.repeat.ends = SeriesEnd::OnDate;
      let until = MONDAY_UTC + 4 * MS_PER_DAY - 1_000;
      draft.repeat.maybe_until = Some(until);
      let recurrence = draft.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!(recurrence.maybe_until, Some(UnixMillis(until)));
      assert!(recurrence.skips_holidays);
      draft.repeat.ends = SeriesEnd::AfterCount;
      draft.repeat.count = 3;
      let counted = draft.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!((counted.maybe_count, counted.maybe_until), (Some(3), None));
    }

    #[test]
    fn a_saved_series_comes_back_into_the_draft_and_is_not_sent_unchanged() {
      let organizer = id(1);
      let mut draft = monday_draft();
      draft.set_repeat(Repeat::Daily);
      draft.repeat.weekdays = [true, true, true, true, true, false, false];
      draft.repeat.ends = SeriesEnd::AfterCount;
      draft.repeat.count = 20;
      let request = draft.to_request(NOW).unwrap();
      let mut saved = meeting(organizer);
      saved.starts_at = request.starts_at;
      saved.ends_at = request.ends_at;
      saved.maybe_recurrence = request.maybe_recurrence.clone();
      let mut again = MeetingDraft::from_meeting(&saved, organizer, 0);
      assert_eq!(again.repeat, draft.repeat);
      assert_eq!(
        again.to_update_request(NOW, &saved).unwrap().maybe_recurrence,
        None
      );
      again.repeat.interval = 2;
      assert!(
        again
          .to_update_request(NOW, &saved)
          .unwrap()
          .maybe_recurrence
          .is_some()
      );
    }

    /// A Monday, 10:00 UTC, well after `NOW`.
    fn monday_draft() -> MeetingDraft {
      let mut draft = filled_draft();
      draft.maybe_starts_at = Some(MONDAY_UTC + 10 * MS_PER_HOUR);
      draft
    }
  }

  #[test]
  fn the_notetaker_is_its_box_and_invited_like_a_member() {
    let organizer = id(1);
    let bot = id(4);
    let mut before = meeting(organizer);
    before.attendees = vec![attendee(Some(organizer), None), attendee(Some(bot), None)];
    before.co_host_user_ids = vec![id(2)];
    let draft = MeetingDraft::from_meeting(&before, organizer, 0).with_notetaker(Some(bot));
    assert_eq!(
      (draft.members.clone(), draft.maybe_notetaker),
      (Vec::new(), Some(bot))
    );
    assert_eq!(draft.co_hosts, vec![id(2)]);
    let mut draft = filled_draft();
    draft.members = vec![id(2)];
    draft.maybe_notetaker = Some(bot);
    let request = draft.to_request(NOW).unwrap();
    assert_eq!(invited_users(&request), vec![id(2), bot]);
  }

  #[test]
  fn admitters_are_kept_for_own_meetings_that_run_or_start_within_a_day() {
    let me = id(1);
    let soon = meeting(me);
    let mut later = meeting(me);
    later.id = id(8);
    later.starts_at = UnixMillis(NOW + ADMITTERS_LEAD_MS + MS_PER_HOUR);
    let mut cancelled = meeting(me);
    cancelled.id = id(10);
    cancelled.status = MeetingStatus::Cancelled;
    let mut others = meeting(id(2));
    others.id = id(11);
    // A series is listed once per occurrence: asked for once.
    let due = admitters_due(&[soon.clone(), soon.clone(), later, cancelled, others], me, NOW);
    assert_eq!(
      due,
      vec![AdmittersDue {
        meeting: soon.id,
        conversation: soon.conversation_id,
        server_times: MeetingTimes::of(&soon),
      }]
    );
  }

  #[test]
  fn a_saved_edit_whose_removal_is_owed_is_finished_and_an_unsaved_one_forgotten() {
    let organizer = id(1);
    let mut listed = meeting(organizer);
    listed.attendees = vec![attendee(Some(organizer), None), attendee(Some(id(3)), None)];
    let edit = |removed: Vec<UserId>, noted_at| PendingEdit {
      meeting: listed.id,
      removed,
      noted_at,
    };
    let old = NOW - 10 * MS_PER_MINUTE;
    // User 2 is off the saved list: owed. User 3 is still on it: the save never happened.
    let (kept, owed) = plan_reconcile(&[edit(vec![id(2), id(3)], old)], NOW, &[listed.clone()]);
    assert!(kept.is_empty());
    assert_eq!(
      owed,
      vec![OwedChatChange {
        meeting: listed.id,
        conversation: listed.conversation_id,
        attending: vec![organizer, id(3)],
        removed: vec![id(2)],
      }]
    );
    let (kept, owed) = plan_reconcile(&[edit(vec![id(3)], old)], NOW, &[listed.clone()]);
    assert!(kept.is_empty() && owed.is_empty());
    // In flight in this session, or the meeting not listed yet: kept for later.
    let fresh = edit(vec![id(2)], NOW - 1_000);
    assert_eq!(
      plan_reconcile(std::slice::from_ref(&fresh), NOW, &[listed.clone()]).0,
      vec![fresh]
    );
    let unlisted = edit(vec![id(2)], old);
    assert_eq!(
      plan_reconcile(std::slice::from_ref(&unlisted), NOW, &[]).0,
      vec![unlisted]
    );
    // Too old: dropped.
    let ancient = edit(vec![id(2)], NOW - PENDING_EDIT_MAX_AGE_MS - 1);
    assert_eq!(plan_reconcile(&[ancient], NOW, &[]).0, Vec::new());
  }

  #[test]
  fn a_save_with_an_unknown_outcome_keeps_its_record_until_the_list_shows_the_result() {
    let organizer = id(1);
    let mut listed = meeting(organizer);
    listed.attendees = vec![attendee(Some(organizer), None), attendee(Some(id(2)), None)];
    let record = PendingEdit {
      meeting: listed.id,
      removed: vec![id(2)],
      noted_at: NOW - 30_000,
    };
    let (kept, owed) = plan_reconcile(std::slice::from_ref(&record), NOW, std::slice::from_ref(&listed));
    assert_eq!((kept.len(), owed.len()), (1, 0));
    listed.attendees.pop();
    let (kept, owed) = plan_reconcile(std::slice::from_ref(&record), NOW + 10 * MS_PER_MINUTE, &[listed]);
    assert!(kept.is_empty());
    assert_eq!(owed.len(), 1);
  }

  #[test]
  fn noting_an_edit_replaces_the_older_one_of_the_meeting_and_ignores_empty_ones() {
    let edits = noting_pending_edit(Vec::new(), id(7), vec![id(2)], NOW);
    let edits = noting_pending_edit(edits, id(7), vec![id(3)], NOW + 1);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].removed, vec![id(3)]);
    assert_eq!(noting_pending_edit(edits.clone(), id(8), Vec::new(), NOW), edits);
  }
}
