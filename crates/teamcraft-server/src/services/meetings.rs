//! Meetings: scheduling, changing and cancelling them, the meeting chat, invitees and their
//! answers (PLAN.md §9, ARCHITECTURE.md §6.4).
//!
//! The title, time and invitee list are server-visible by design: they live in the calendar and in
//! the invitation e-mails anyway. The meeting chat and the media are end-to-end encrypted.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use teamcraft_db::{
  AttendeeDiff, AttendeeKey, MeetingChanges, NewAttendee, NewConversation, NewMeeting, StoredAttendee,
  StoredMeeting,
};
use teamcraft_proto::{
  ConversationKind, ConversationVisibility, CreateInstantMeetingRequest, CreateMeetingRequest, GatewayEvent,
  HistoryPolicy, ListMeetingsQuery, ListMeetingsResponse, MeetingAttendeeResponse, MeetingId, MeetingInvitee,
  MeetingKind, MeetingRecurrence, MeetingReplyResponse, MeetingResponse, MeetingStatus, RecurrenceFrequency,
  RsvpStatus, SetRsvpRequest, UnixMillis, UpdateMeetingRequest, UserId,
};
use time::{Duration, OffsetDateTime};
use tracing::warn;

use super::access::is_guest;
use super::convert::{offset_date_time, presented_name, unix_millis};
use super::{validate_name, validate_text};
use crate::auth::{AuthError, AuthenticatedUser};
use crate::brand::Brand;
use crate::config::MailTexts;
use crate::error::ApiError;
use crate::meetings::MeetingService;
use crate::meetings::ical::{self, EventAttendee, EventData, EventPerson, EventStatus, Method};
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

const MAX_TITLE_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 5_000;
const MAX_EMAIL_CHARS: usize = 254;
const MAX_RECURRENCE_INTERVAL: u32 = 100;
const MAX_RECURRENCE_COUNT: u32 = 1_000;
const DEFAULT_LIST_LIMIT: i64 = 100;
const MAX_LIST_LIMIT: i64 = 500;
/// "Meet now" meetings stay open this long.
const INSTANT_MEETING_DURATION: Duration = Duration::hours(8);
const DEFAULT_INSTANT_TITLE: &str = "Instant meeting";

/// Schedules a meeting: creates the meeting chat, stores the event in the calendar and tells the
/// invited users.
pub async fn create_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateMeetingRequest,
) -> Result<MeetingResponse, ApiError> {
  let service = service_of(state)?;
  require_member_account(caller)?;
  let title = validate_name(&request.title, "title", MAX_TITLE_CHARS)?;
  let maybe_description = validated_description(request.maybe_description.as_deref())?;
  let (starts_at, ends_at) = validated_times(request.starts_at, request.ends_at)?;
  let maybe_rule = request
    .maybe_recurrence
    .as_ref()
    .map(recurrence_rule)
    .transpose()?;
  let attendees = resolve_invitees(state, caller.user.id, &request.invitees).await?;

  let id: MeetingId = state.next_id()?;
  let uid = service.uid(id);
  let new = NewMeeting {
    id,
    uid: &uid,
    organizer: caller.user.id,
    conversation: NewConversation {
      id: state.next_id()?,
      maybe_space: None,
      kind: ConversationKind::Meeting,
      maybe_name: Some(&title),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Private,
      history_policy: HistoryPolicy::Full,
      maybe_category: None,
      maybe_participants_key: None,
      is_integration: false,
    },
    title: &title,
    maybe_description: maybe_description.as_deref(),
    starts_at,
    ends_at,
    maybe_recurrence_rule: maybe_rule.as_deref(),
    kind: MeetingKind::Scheduled.as_str(),
    has_lobby: request.has_lobby,
    attendees: &attendees,
    maybe_idempotency_key: request.maybe_idempotency_key,
  };
  let created = state.database.create_meeting(&new).await?;
  if !created.is_new {
    // A retry of an earlier request: return that meeting; calendar entry and invitations were
    // handled by the first request.
    let meeting = load_meeting(state, created.id).await?;
    return respond(state, service, &meeting, Vec::new()).await;
  }

  let meeting = load_meeting(state, id).await?;
  if let Err(error) = sync_calendar(state, service, &meeting).await {
    // The calendar is the system of record for invitations; without the event the meeting would
    // be invisible there, so the creation is undone.
    state.database.delete_meeting(&meeting).await?;
    return Err(error);
  }
  announce(state, &meeting).await?;
  let attendees = state.database.list_attendees(id).await?;
  let undelivered = send_invitations(state, service, &meeting, MailKind::Invitation, &attendees).await?;
  respond(state, service, &meeting, undelivered).await
}

/// "Meet now": a meeting that starts immediately and is shared by link; no calendar entry, no e-mail.
pub async fn create_instant_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateInstantMeetingRequest,
) -> Result<MeetingResponse, ApiError> {
  let service = service_of(state)?;
  require_member_account(caller)?;
  let title = match request.maybe_title.as_deref() {
    Some(title) => validate_name(title, "title", MAX_TITLE_CHARS)?,
    None => DEFAULT_INSTANT_TITLE.to_owned(),
  };
  let now = OffsetDateTime::now_utc();
  let id: MeetingId = state.next_id()?;
  let uid = service.uid(id);
  let created = state
    .database
    .create_meeting(&NewMeeting {
      id,
      uid: &uid,
      organizer: caller.user.id,
      conversation: NewConversation {
        id: state.next_id()?,
        maybe_space: None,
        kind: ConversationKind::Meeting,
        maybe_name: Some(&title),
        maybe_topic: None,
        maybe_description: None,
        visibility: ConversationVisibility::Private,
        history_policy: HistoryPolicy::Full,
        maybe_category: None,
        maybe_participants_key: None,
        is_integration: false,
      },
      title: &title,
      maybe_description: None,
      starts_at: now,
      ends_at: now + INSTANT_MEETING_DURATION,
      maybe_recurrence_rule: None,
      kind: MeetingKind::Instant.as_str(),
      has_lobby: true,
      attendees: &[],
      maybe_idempotency_key: request.maybe_idempotency_key,
    })
    .await?;
  let meeting = load_meeting(state, created.id).await?;
  respond(state, service, &meeting, Vec::new()).await
}

/// One meeting, for the organizer, invited users and everybody in the meeting chat.
pub async fn get_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<MeetingResponse, ApiError> {
  let service = service_of(state)?;
  let meeting = visible_meeting(state, caller, id).await?;
  respond(state, service, &meeting, Vec::new()).await
}

/// The caller's meetings in a time range, soonest first.
pub async fn list_meetings(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: ListMeetingsQuery,
) -> Result<ListMeetingsResponse, ApiError> {
  let service = service_of(state)?;
  let from = match query.maybe_from {
    Some(millis) => time_from_millis(millis)?,
    None => OffsetDateTime::now_utc(),
  };
  let maybe_to = query.maybe_to.map(time_from_millis).transpose()?;
  let limit = query
    .maybe_limit
    .map_or(DEFAULT_LIST_LIMIT, i64::from)
    .clamp(1, MAX_LIST_LIMIT);
  let stored = state
    .database
    .list_meetings_of_user(caller.user.id, from, maybe_to, limit)
    .await?;
  let mut meetings = Vec::with_capacity(stored.len());
  for meeting in &stored {
    meetings.push(respond(state, service, meeting, Vec::new()).await?);
  }
  Ok(ListMeetingsResponse {
    meetings,
    maybe_server_now: Some(unix_millis(OffsetDateTime::now_utc())),
  })
}

/// Changes a meeting (organizer only): the calendar event is replaced and the invited users hear
/// about it.
pub async fn update_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  request: UpdateMeetingRequest,
) -> Result<MeetingResponse, ApiError> {
  let service = service_of(state)?;
  let before = organizer_meeting(state, caller, id).await?;
  let title = request
    .maybe_title
    .as_deref()
    .map(|title| validate_name(title, "title", MAX_TITLE_CHARS))
    .transpose()?;
  let maybe_description = request
    .maybe_description
    .as_deref()
    .map(|text| validate_text(text, "description", MAX_DESCRIPTION_CHARS).map(|()| text))
    .transpose()?;
  let (starts_at, ends_at) = validated_times(
    request
      .maybe_starts_at
      .unwrap_or_else(|| unix_millis(before.starts_at)),
    request
      .maybe_ends_at
      .unwrap_or_else(|| unix_millis(before.ends_at)),
  )?;
  let wanted = match &request.maybe_invitees {
    Some(invitees) => Some(resolve_invitees(state, caller.user.id, invitees).await?),
    None => None,
  };

  let users_before = state.database.meeting_user_ids(&before).await?;
  let Some(updated) = state
    .database
    .update_meeting(
      id,
      &MeetingChanges {
        maybe_title: title.as_deref(),
        maybe_description,
        maybe_starts_at: request.maybe_starts_at.map(|_| starts_at),
        maybe_ends_at: request.maybe_ends_at.map(|_| ends_at),
        maybe_has_lobby: request.maybe_has_lobby,
      },
    )
    .await?
  else {
    return Err(ApiError::meeting_not_found());
  };
  let diff = match &wanted {
    Some(wanted) => state.database.replace_attendees(&updated, wanted).await?,
    None => AttendeeDiff::default(),
  };
  sync_calendar(state, service, &updated).await?;

  let mut audience = users_before;
  audience.extend(state.database.meeting_user_ids(&updated).await?);
  audience.sort_unstable();
  audience.dedup();
  state
    .hub
    .publish(audience, &GatewayEvent::MeetingUpdate { meeting_id: id });

  let mut undelivered =
    send_invitations(state, service, &updated, MailKind::Cancellation, &diff.removed).await?;
  let current = state.database.list_attendees(id).await?;
  let (added, kept): (Vec<_>, Vec<_>) = current
    .into_iter()
    .partition(|attendee| diff.added.iter().any(|new| new.id == attendee.id));
  undelivered.extend(send_invitations(state, service, &updated, MailKind::Invitation, &added).await?);
  undelivered.extend(send_invitations(state, service, &updated, MailKind::Update, &kept).await?);
  respond(state, service, &updated, undelivered).await
}

/// Cancels a meeting (organizer only): the event leaves the calendar and the chat is archived but
/// stays readable.
pub async fn cancel_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<MeetingResponse, ApiError> {
  let service = service_of(state)?;
  let meeting = organizer_meeting(state, caller, id).await?;
  if !state.database.cancel_meeting(&meeting).await? {
    return Err(ApiError::Conflict("the meeting is not scheduled".into()));
  }
  let cancelled = load_meeting(state, id).await?;
  if let Some(calendar) = service.calendar() {
    // A calendar that is down must not keep a meeting alive that the organizer cancelled.
    if let Err(error) = calendar.delete_event(&meeting.id.to_string()).await {
      warn!(%error, meeting_id = %id, "removing the calendar event failed");
    }
  }
  announce(state, &cancelled).await?;
  let attendees = state.database.list_attendees(id).await?;
  let undelivered = send_invitations(state, service, &cancelled, MailKind::Cancellation, &attendees).await?;
  respond(state, service, &cancelled, undelivered).await
}

/// The caller's answer to an invitation.
pub async fn set_rsvp(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
  request: SetRsvpRequest,
) -> Result<(), ApiError> {
  let service = service_of(state)?;
  if matches!(request.status, RsvpStatus::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown RSVP status".into()));
  }
  let meeting = visible_meeting(state, caller, id).await?;
  if !state
    .database
    .set_rsvp(id, &AttendeeKey::User(caller.user.id), request.status.as_str())
    .await?
  {
    return Err(ApiError::Forbidden("only invitees can answer".into()));
  }
  after_rsvp(state, service, &meeting).await
}

/// Checks the bearer token of the inbox handler that forwards RSVP e-mail.
pub fn authenticate_inbox(state: &AppState, maybe_presented: Option<&str>) -> Result<(), ApiError> {
  let service = service_of(state)?;
  let presented = maybe_presented.ok_or(AuthError::MissingToken)?;
  match service.check_inbox_token(presented.as_bytes()) {
    Some(true) => Ok(()),
    Some(false) => Err(AuthError::InvalidToken("not the inbox token".into()).into()),
    None => Err(ApiError::Unavailable(
      "receiving replies is not configured".into(),
    )),
  }
}

/// Applies the answers of an iMIP REPLY (RFC 6047) received by e-mail. Answers for unknown
/// meetings or people are ignored; a malformed object is rejected.
///
/// `sender` is the authenticated envelope sender the inbox handler saw. A reply is only accepted
/// from the attendee it answers for: anybody who knows the meeting UID and the invitees' addresses
/// (they are in every invitation) could otherwise change everybody's answer.
pub async fn receive_reply(
  state: &AppState,
  ics: &str,
  sender: &str,
) -> Result<MeetingReplyResponse, ApiError> {
  let reply = ical::parse_reply(ics).map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  let sender = address_of(sender);
  if reply.entries.iter().any(|entry| entry.email != sender) {
    return Err(ApiError::Forbidden(
      "the sender is not the attendee the reply answers for".into(),
    ));
  }
  let mut applied = 0;
  for entry in &reply.entries {
    if apply_email_rsvp(state, &reply.uid, &entry.email, &entry.rsvp).await? {
      applied += 1;
    }
  }
  Ok(MeetingReplyResponse { applied })
}

/// The bare, lower-cased address of `Name <user@example.org>` or `user@example.org`.
fn address_of(sender: &str) -> String {
  let inner = match (sender.rfind('<'), sender.rfind('>')) {
    (Some(start), Some(end)) if start < end => &sender[start + 1..end],
    _ => sender,
  };
  inner.trim().to_lowercase()
}

/// Applies an answer received by e-mail (an iMIP REPLY). Unknown meetings and people are ignored
/// by the caller; this reports whether the answer matched an invitee.
pub async fn apply_email_rsvp(
  state: &AppState,
  uid: &str,
  email: &str,
  status: &RsvpStatus,
) -> Result<bool, ApiError> {
  let service = service_of(state)?;
  let Some(meeting) = state.database.get_meeting_by_uid(uid).await? else {
    return Ok(false);
  };
  let email = email.to_lowercase();
  if !state
    .database
    .set_rsvp(meeting.id, &AttendeeKey::Email(&email), status.as_str())
    .await?
  {
    return Ok(false);
  }
  after_rsvp(state, service, &meeting).await?;
  Ok(true)
}

async fn after_rsvp(
  state: &AppState,
  service: &MeetingService,
  meeting: &StoredMeeting,
) -> Result<(), ApiError> {
  let current = load_meeting(state, meeting.id).await?;
  // The calendar shows the answers; a failure there must not lose the answer itself.
  if let Err(error) = sync_calendar(state, service, &current).await {
    warn!(%error, meeting_id = %meeting.id, "updating the calendar event after an answer failed");
  }
  announce(state, &current).await
}

/// Writes the meeting's event to the calendar, if one is configured.
async fn sync_calendar(
  state: &AppState,
  service: &MeetingService,
  meeting: &StoredMeeting,
) -> Result<(), ApiError> {
  let Some(calendar) = service.calendar() else {
    return Ok(());
  };
  if meeting.kind != MeetingKind::Scheduled.as_str() {
    return Ok(());
  }
  let brand = organizer_brand(state, meeting).await?;
  let join_url = join_url_on(service, &brand, meeting.id);
  let event = event_for(state, service, &brand, meeting, join_url).await?;
  let ics = ical::render_event(&event.data(), None);
  calendar
    .put_event(&meeting.id.to_string(), &ics)
    .await
    .map_err(|error| ApiError::Unavailable(format!("the calendar rejected the meeting: {error}")))
}

/// Owned parts of an iCalendar event, so it can be built from database rows.
struct OwnedEvent {
  uid: String,
  meeting: StoredMeeting,
  join_url: String,
  organizer: EventPerson,
  attendees: Vec<EventAttendee>,
}

impl OwnedEvent {
  fn data(&self) -> EventData<'_> {
    let status = if self.meeting.status == MeetingStatus::Cancelled.as_str() {
      EventStatus::Cancelled
    } else {
      EventStatus::Confirmed
    };
    self.data_with_status(status)
  }

  fn data_with_status(&self, status: EventStatus) -> EventData<'_> {
    EventData {
      uid: &self.uid,
      sequence: self.meeting.sequence,
      stamp: OffsetDateTime::now_utc(),
      summary: &self.meeting.title,
      maybe_description: self.meeting.maybe_description.as_deref(),
      starts_at: self.meeting.starts_at,
      ends_at: self.meeting.ends_at,
      maybe_recurrence_rule: self.meeting.maybe_recurrence_rule.as_deref(),
      status,
      join_url: &self.join_url,
      organizer: self.organizer.clone(),
      attendees: self.attendees.clone(),
    }
  }
}

async fn event_for(
  state: &AppState,
  service: &MeetingService,
  brand: &Brand,
  meeting: &StoredMeeting,
  join_url: String,
) -> Result<OwnedEvent, ApiError> {
  let attendees = state.database.list_attendees(meeting.id).await?;
  let user_ids: Vec<UserId> = attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user)
    .chain([meeting.organizer])
    .collect();
  let emails = state.database.get_user_emails(&user_ids).await?;
  let names = presented_names(state, &user_ids).await?;
  let organizer = EventPerson {
    email: emails
      .get(&meeting.organizer)
      .cloned()
      .unwrap_or_else(|| service.organizer_fallback_address().to_owned()),
    // Without an address of their own the organizer is the brand's calendar identity.
    maybe_name: match (
      emails.contains_key(&meeting.organizer),
      &brand.mail.maybe_imip_organizer_name,
    ) {
      (false, Some(brand_name)) => Some(brand_name.clone()),
      _ => names.get(&meeting.organizer).cloned(),
    },
  };
  let attendees = attendees
    .iter()
    .filter_map(|attendee| {
      let email = attendee
        .maybe_email
        .clone()
        .or_else(|| attendee.maybe_user.and_then(|user| emails.get(&user).cloned()))?;
      Some(EventAttendee {
        person: EventPerson {
          email,
          maybe_name: displayed_name(attendee, &names),
        },
        rsvp: RsvpStatus::from(attendee.rsvp.clone()),
      })
    })
    .collect();
  Ok(OwnedEvent {
    uid: meeting.uid.clone(),
    meeting: meeting.clone(),
    join_url,
    organizer,
    attendees,
  })
}

/// Which e-mail template and iMIP method an invitation uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MailKind {
  Invitation,
  Update,
  Cancellation,
}

/// Sends the iMIP e-mail of `kind` to the attendees that have an address and returns the addresses
/// it could not be sent to. External invitees get a personal join link; the others the member link.
async fn send_invitations(
  state: &AppState,
  service: &MeetingService,
  meeting: &StoredMeeting,
  kind: MailKind,
  recipients: &[StoredAttendee],
) -> Result<Vec<String>, ApiError> {
  let Some(mailer) = service.mailer() else {
    return Ok(Vec::new());
  };
  let user_ids: Vec<UserId> = recipients
    .iter()
    .filter_map(|attendee| attendee.maybe_user)
    .collect();
  let emails = state.database.get_user_emails(&user_ids).await?;
  let names = presented_names(
    state,
    &user_ids
      .iter()
      .copied()
      .chain([meeting.organizer])
      .collect::<Vec<_>>(),
  )
  .await?;
  let organizer_name = names.get(&meeting.organizer).cloned().unwrap_or_default();
  let brand = organizer_brand(state, meeting).await?;
  let mut undelivered = Vec::new();
  // The instance's mail identity is not for bulk mail: an organizer has an hourly budget. What
  // exceeds it counts as undelivered (the invitees still see the meeting in the app).
  let mailable: Vec<&StoredAttendee> = recipients
    .iter()
    .filter(|attendee| {
      attendee.maybe_email.is_some() || attendee.maybe_user.is_some_and(|user| emails.contains_key(&user))
    })
    .collect();
  let budget = state.settings.get().limits.max_meeting_mails_per_user_per_hour;
  let is_within_budget = u32::try_from(mailable.len())
    .is_ok_and(|count| state.rate_limits.spend_mails(meeting.organizer, count, budget));
  if !is_within_budget {
    warn!(meeting_id = %meeting.id, "the organizer's hourly e-mail budget is used up");
    return Ok(
      mailable
        .iter()
        .filter_map(|attendee| {
          attendee
            .maybe_email
            .clone()
            .or_else(|| attendee.maybe_user.and_then(|user| emails.get(&user).cloned()))
        })
        .collect(),
    );
  }
  for attendee in recipients {
    let Some(address) = attendee
      .maybe_email
      .clone()
      .or_else(|| attendee.maybe_user.and_then(|user| emails.get(&user).cloned()))
    else {
      continue;
    };
    let join_url = match attendee.maybe_user {
      Some(_) => join_url_on(service, &brand, meeting.id),
      None => personal_join_url(state, service, &brand, meeting, &address).await?,
    };
    let texts = &state.settings.get().meeting_mail.texts;
    let (subject, text, method, status) = compose(texts, meeting, kind, &organizer_name, &join_url);
    let event = event_for(state, service, &brand, meeting, join_url).await?;
    let mail = OutgoingMail {
      to_address: address.clone(),
      maybe_to_name: displayed_name(attendee, &names),
      subject,
      text,
      method,
      ics: ical::render_event(&event.data_with_status(status), Some(method)),
      maybe_from_name: None,
      maybe_from_address: None,
    }
    .as_brand(&brand);
    if let Err(error) = mailer.send(&mail).await {
      warn!(%error, meeting_id = %meeting.id, "sending an invitation failed");
      undelivered.push(address);
    }
  }
  Ok(undelivered)
}

/// The personal link for an external invitee: one open invite per address and meeting.
async fn personal_join_url(
  state: &AppState,
  service: &MeetingService,
  brand: &Brand,
  meeting: &StoredMeeting,
  email: &str,
) -> Result<String, ApiError> {
  let invite = state
    .database
    .ensure_email_invite(meeting.id, email, state.next_id()?, meeting.organizer)
    .await?;
  let token = service
    .tokens()
    .sign_invite(invite, meeting.id)
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  Ok(format!(
    "{}#invite={token}",
    join_url_on(service, brand, meeting.id)
  ))
}

/// The brand meeting mail goes out as: the organizer's home brand (their e-mail domain), else the
/// default brand.
async fn organizer_brand(state: &AppState, meeting: &StoredMeeting) -> Result<Arc<Brand>, ApiError> {
  let emails = state.database.get_user_emails(&[meeting.organizer]).await?;
  let brands = state.brands.current();
  Ok(Arc::clone(
    emails
      .get(&meeting.organizer)
      .map_or(brands.default_brand(), |email| brands.for_email(email)),
  ))
}

/// The join link on the brand's own host, or the service's base when the brand has none.
fn join_url_on(service: &MeetingService, brand: &Brand, meeting: MeetingId) -> String {
  let origin = brand.origin();
  if origin.is_empty() {
    service.join_url(meeting)
  } else {
    format!("{origin}/meeting/{meeting}")
  }
}

/// Subject, text, iMIP method and event status of a mail from the configured templates.
fn compose(
  texts: &MailTexts,
  meeting: &StoredMeeting,
  kind: MailKind,
  organizer_name: &str,
  join_url: &str,
) -> (String, String, Method, EventStatus) {
  let (subject, body, method, status) = match kind {
    MailKind::Invitation => (
      &texts.invitation_subject,
      &texts.invitation_body,
      Method::Request,
      EventStatus::Confirmed,
    ),
    MailKind::Update => (
      &texts.update_subject,
      &texts.update_body,
      Method::Request,
      EventStatus::Confirmed,
    ),
    MailKind::Cancellation => (
      &texts.cancellation_subject,
      &texts.cancellation_body,
      Method::Cancel,
      EventStatus::Cancelled,
    ),
  };
  let start = format_human(meeting.starts_at);
  let end = format_human(meeting.ends_at);
  let description = meeting
    .maybe_description
    .as_deref()
    .map(|text| format!("{text}\n"))
    .unwrap_or_default();
  let values = [
    ("title", meeting.title.as_str()),
    ("organizer", organizer_name),
    ("start", start.as_str()),
    ("end", end.as_str()),
    ("description", description.as_str()),
    ("join_url", join_url),
  ];
  (fill(subject, &values), fill(body, &values), method, status)
}

/// Replaces `{name}` placeholders in a single pass, so substituted values are never expanded again.
fn fill(template: &str, values: &[(&str, &str)]) -> String {
  let mut out = String::with_capacity(template.len());
  let mut rest = template;
  while let Some(open) = rest.find('{') {
    out.push_str(&rest[..open]);
    let after = &rest[open + 1..];
    match after.find('}').and_then(|close| {
      let name = &after[..close];
      values
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| (close, *value))
    }) {
      Some((close, value)) => {
        out.push_str(value);
        rest = &after[close + 1..];
      },
      None => {
        out.push('{');
        rest = after;
      },
    }
  }
  out.push_str(rest);
  out
}

/// `2030-01-01 09:00 UTC`.
fn format_human(time: OffsetDateTime) -> String {
  let utc = time.to_offset(time::UtcOffset::UTC);
  format!(
    "{:04}-{:02}-{:02} {:02}:{:02} UTC",
    utc.year(),
    u8::from(utc.month()),
    utc.day(),
    utc.hour(),
    utc.minute()
  )
}

/// Turns the invitee list of a request into attendee rows: users are looked up, external people are
/// identified by their lower-cased e-mail address; duplicates and the organizer are dropped.
async fn resolve_invitees(
  state: &AppState,
  organizer: UserId,
  invitees: &[MeetingInvitee],
) -> Result<Vec<NewAttendee>, ApiError> {
  let max_invitees = state.settings.get().meeting_mail.max_invitees;
  if invitees.len() > max_invitees {
    return Err(ApiError::InvalidRequest(format!(
      "at most {max_invitees} invitees"
    )));
  }
  let user_ids: Vec<UserId> = invitees
    .iter()
    .filter_map(|invitee| match invitee {
      MeetingInvitee::User { user_id } => Some(*user_id),
      MeetingInvitee::External { .. } => None,
    })
    .collect();
  let users: HashMap<UserId, _> = state
    .database
    .get_users(&user_ids)
    .await?
    .into_iter()
    .map(|user| (user.id, user))
    .collect();

  let mut seen_users = HashSet::new();
  let mut seen_emails = HashSet::new();
  let mut attendees = Vec::new();
  for invitee in invitees {
    match invitee {
      MeetingInvitee::User { user_id } => {
        let user = users
          .get(user_id)
          .filter(|user| !user.is_deactivated)
          .ok_or_else(|| ApiError::InvalidRequest(format!("unknown user {user_id}")))?;
        if *user_id != organizer && seen_users.insert(*user_id) {
          attendees.push(NewAttendee {
            id: state.next_id()?,
            maybe_user: Some(*user_id),
            maybe_email: None,
            maybe_name: Some(user.display_name.clone()),
          });
        }
      },
      MeetingInvitee::External { email, maybe_name } => {
        let email = validated_email(email)?;
        // External invitees get a guest session: the guest rules apply to them as well.
        super::invites::check_guest_domain(state, &email)?;
        let maybe_name = maybe_name
          .as_deref()
          .map(|name| validate_name(name, "invitee name", MAX_TITLE_CHARS))
          .transpose()?;
        if seen_emails.insert(email.clone()) {
          attendees.push(NewAttendee {
            id: state.next_id()?,
            maybe_user: None,
            maybe_email: Some(email),
            maybe_name,
          });
        }
      },
    }
  }
  Ok(attendees)
}

/// A conservative address check: one `@`, a dotted domain, no whitespace, quotes, angle brackets or
/// control characters (the address ends up in iCalendar and e-mail headers).
fn validated_email(raw: &str) -> Result<String, ApiError> {
  let email = raw.trim().to_lowercase();
  let invalid = || ApiError::InvalidRequest(format!("{raw:?} is not a valid e-mail address"));
  let (local, domain) = email.split_once('@').ok_or_else(invalid)?;
  let is_clean = |part: &str| {
    !part.is_empty()
      && part.chars().all(|c| {
        !c.is_control()
          && !c.is_whitespace()
          && !matches!(c, '"' | '<' | '>' | '\\' | ',' | ';' | '@' | '(' | ')')
      })
  };
  if email.len() > MAX_EMAIL_CHARS || !is_clean(local) || !is_clean(domain) || !domain.contains('.') {
    return Err(invalid());
  }
  if domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") {
    return Err(invalid());
  }
  Ok(email)
}

fn recurrence_rule(recurrence: &MeetingRecurrence) -> Result<String, ApiError> {
  let freq = match recurrence.frequency {
    RecurrenceFrequency::Daily => "DAILY",
    RecurrenceFrequency::Weekly => "WEEKLY",
    RecurrenceFrequency::Monthly => "MONTHLY",
    RecurrenceFrequency::Unknown(_) => {
      return Err(ApiError::InvalidRequest("unknown recurrence frequency".into()));
    },
  };
  if !(1..=MAX_RECURRENCE_INTERVAL).contains(&recurrence.interval) {
    return Err(ApiError::InvalidRequest(format!(
      "the recurrence interval must be between 1 and {MAX_RECURRENCE_INTERVAL}"
    )));
  }
  let mut rule = format!("FREQ={freq};INTERVAL={}", recurrence.interval);
  if let Some(count) = recurrence.maybe_count {
    if !(1..=MAX_RECURRENCE_COUNT).contains(&count) {
      return Err(ApiError::InvalidRequest(format!(
        "the recurrence count must be between 1 and {MAX_RECURRENCE_COUNT}"
      )));
    }
    rule.push_str(&format!(";COUNT={count}"));
  }
  Ok(rule)
}

fn validated_description(maybe_text: Option<&str>) -> Result<Option<String>, ApiError> {
  match maybe_text.map(str::trim).filter(|text| !text.is_empty()) {
    Some(text) => {
      validate_text(text, "description", MAX_DESCRIPTION_CHARS)?;
      Ok(Some(text.to_owned()))
    },
    None => Ok(None),
  }
}

fn validated_times(
  starts_at: UnixMillis,
  ends_at: UnixMillis,
) -> Result<(OffsetDateTime, OffsetDateTime), ApiError> {
  let (start, end) = (time_from_millis(starts_at)?, time_from_millis(ends_at)?);
  if end <= start {
    return Err(ApiError::InvalidRequest("ends_at must be after starts_at".into()));
  }
  Ok((start, end))
}

fn time_from_millis(millis: UnixMillis) -> Result<OffsetDateTime, ApiError> {
  offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("time is out of range".into()))
}

/// Guests don't schedule meetings.
fn require_member_account(caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if is_guest(caller) {
    return Err(ApiError::Forbidden("guests can't create meetings".into()));
  }
  Ok(())
}

fn service_of(state: &AppState) -> Result<&MeetingService, ApiError> {
  state
    .maybe_meetings
    .as_deref()
    .ok_or_else(|| ApiError::Unavailable("meetings are not configured".into()))
}

async fn load_meeting(state: &AppState, id: MeetingId) -> Result<StoredMeeting, ApiError> {
  state
    .database
    .get_meeting(id)
    .await?
    .ok_or_else(ApiError::meeting_not_found)
}

/// The meeting, if the caller organizes it, is invited or is in its chat; everybody else gets "not found".
async fn visible_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<StoredMeeting, ApiError> {
  let meeting = load_meeting(state, id).await?;
  let members = state
    .database
    .conversation_member_ids(meeting.conversation)
    .await?;
  if members.contains(&caller.user.id) {
    Ok(meeting)
  } else {
    Err(ApiError::meeting_not_found())
  }
}

async fn organizer_meeting(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: MeetingId,
) -> Result<StoredMeeting, ApiError> {
  let meeting = visible_meeting(state, caller, id).await?;
  if meeting.organizer != caller.user.id {
    return Err(ApiError::Forbidden("only the organizer can do that".into()));
  }
  if meeting.status != MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is not scheduled".into()));
  }
  Ok(meeting)
}

/// Tells the organizer and the invited users to refetch the meeting.
async fn announce(state: &AppState, meeting: &StoredMeeting) -> Result<(), ApiError> {
  let users = state.database.meeting_user_ids(meeting).await?;
  state.hub.publish(
    users,
    &GatewayEvent::MeetingUpdate {
      meeting_id: meeting.id,
    },
  );
  Ok(())
}

/// What people see of the users among `attendees`, as the directory policy wants it (the names
/// stored with the attendee rows are snapshots of the raw display names, never shown).
async fn presented_attendee_names(
  state: &AppState,
  attendees: &[StoredAttendee],
) -> Result<HashMap<UserId, String>, ApiError> {
  let ids: Vec<UserId> = attendees
    .iter()
    .filter_map(|attendee| attendee.maybe_user)
    .collect();
  presented_names(state, &ids).await
}

async fn presented_names(state: &AppState, ids: &[UserId]) -> Result<HashMap<UserId, String>, ApiError> {
  let policy = state.policy.get();
  Ok(
    state
      .database
      .get_users(ids)
      .await?
      .iter()
      .map(|user| (user.id, presented_name(&policy.directory, user)))
      .collect(),
  )
}

async fn respond(
  state: &AppState,
  service: &MeetingService,
  meeting: &StoredMeeting,
  undelivered_invitations: Vec<String>,
) -> Result<MeetingResponse, ApiError> {
  let attendees = state.database.list_attendees(meeting.id).await?;
  let names = presented_attendee_names(state, &attendees).await?;
  Ok(MeetingResponse {
    id: meeting.id,
    title: meeting.title.clone(),
    maybe_description: meeting.maybe_description.clone(),
    starts_at: unix_millis(meeting.starts_at),
    ends_at: unix_millis(meeting.ends_at),
    kind: MeetingKind::from(meeting.kind.clone()),
    status: MeetingStatus::from(meeting.status.clone()),
    has_lobby: meeting.has_lobby,
    organizer_user_id: meeting.organizer,
    conversation_id: meeting.conversation,
    join_url: service.join_url(meeting.id),
    maybe_recurrence: meeting
      .maybe_recurrence_rule
      .as_deref()
      .and_then(parse_recurrence_rule),
    attendees: attendees
      .iter()
      .map(|attendee| attendee_response(attendee, &names))
      .collect(),
    undelivered_invitations,
  })
}

fn attendee_response(attendee: &StoredAttendee, names: &HashMap<UserId, String>) -> MeetingAttendeeResponse {
  MeetingAttendeeResponse {
    maybe_user_id: attendee.maybe_user,
    maybe_email: attendee.maybe_email.clone(),
    maybe_name: displayed_name(attendee, names),
    rsvp: RsvpStatus::from(attendee.rsvp.clone()),
  }
}

/// A user attendee under the directory policy's name, an external one under the name the organizer
/// gave.
fn displayed_name(attendee: &StoredAttendee, names: &HashMap<UserId, String>) -> Option<String> {
  match attendee.maybe_user {
    Some(user) => names.get(&user).cloned().or_else(|| attendee.maybe_name.clone()),
    None => attendee.maybe_name.clone(),
  }
}

/// Reads back the rule `recurrence_rule` wrote.
fn parse_recurrence_rule(rule: &str) -> Option<MeetingRecurrence> {
  let mut frequency = None;
  let mut interval = 1;
  let mut maybe_count = None;
  for part in rule.split(';') {
    let (key, value) = part.split_once('=')?;
    match key {
      "FREQ" => {
        frequency = Some(match value {
          "DAILY" => RecurrenceFrequency::Daily,
          "WEEKLY" => RecurrenceFrequency::Weekly,
          "MONTHLY" => RecurrenceFrequency::Monthly,
          _ => return None,
        })
      },
      "INTERVAL" => interval = value.parse().ok()?,
      "COUNT" => maybe_count = Some(value.parse().ok()?),
      _ => {},
    }
  }
  Some(MeetingRecurrence {
    frequency: frequency?,
    interval,
    maybe_count,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod emails {
    use super::*;

    #[test]
    fn addresses_are_trimmed_and_lowercased() {
      assert_eq!(validated_email("  Gus@Example.ORG ").unwrap(), "gus@example.org");
    }

    #[test]
    fn header_injection_and_malformed_addresses_are_rejected() {
      for bad in [
        "",
        "no-at-sign",
        "a@b",
        "@example.org",
        "a@",
        "a b@example.org",
        "a@exa mple.org",
        "a@example.org\r\nBcc: x@example.org",
        "a\"b@example.org",
        "<a@example.org>",
        "a@@example.org",
        "a@.example.org",
        "a@example..org",
        "a@example.org.",
      ] {
        assert!(validated_email(bad).is_err(), "{bad:?}");
      }
      assert!(validated_email(&format!("{}@example.org", "a".repeat(250))).is_err());
    }
  }

  mod recurrence {
    use super::*;

    #[test]
    fn rules_round_trip() {
      let weekly = MeetingRecurrence {
        frequency: RecurrenceFrequency::Weekly,
        interval: 2,
        maybe_count: Some(5),
      };
      let rule = recurrence_rule(&weekly).unwrap();
      assert_eq!(rule, "FREQ=WEEKLY;INTERVAL=2;COUNT=5");
      assert_eq!(parse_recurrence_rule(&rule), Some(weekly));
      let open = MeetingRecurrence {
        frequency: RecurrenceFrequency::Daily,
        interval: 1,
        maybe_count: None,
      };
      assert_eq!(
        parse_recurrence_rule(&recurrence_rule(&open).unwrap()),
        Some(open)
      );
    }

    #[test]
    fn out_of_range_values_are_rejected() {
      let base = MeetingRecurrence {
        frequency: RecurrenceFrequency::Daily,
        interval: 1,
        maybe_count: None,
      };
      for bad in [
        MeetingRecurrence {
          interval: 0,
          ..base.clone()
        },
        MeetingRecurrence {
          interval: 101,
          ..base.clone()
        },
        MeetingRecurrence {
          maybe_count: Some(0),
          ..base.clone()
        },
        MeetingRecurrence {
          maybe_count: Some(1001),
          ..base.clone()
        },
        MeetingRecurrence {
          frequency: RecurrenceFrequency::Unknown("hourly".into()),
          ..base
        },
      ] {
        assert!(recurrence_rule(&bad).is_err(), "{bad:?}");
      }
    }
  }

  mod templates {
    use super::*;

    #[test]
    fn placeholders_are_replaced_once_and_unknown_ones_stay() {
      let values = [("title", "A {join_url}"), ("join_url", "https://x")];
      assert_eq!(
        fill("T: {title} / {join_url} / {nope} {", &values),
        "T: A {join_url} / https://x / {nope} {"
      );
    }

    #[test]
    fn times_are_shown_in_utc() {
      let time = OffsetDateTime::from_unix_timestamp(1_893_488_400).unwrap();
      assert_eq!(format_human(time), "2030-01-01 09:00 UTC");
    }
  }

  mod times {
    use super::*;

    #[test]
    fn the_end_must_follow_the_start() {
      assert!(validated_times(UnixMillis(2_000), UnixMillis(1_000)).is_err());
      assert!(validated_times(UnixMillis(1_000), UnixMillis(1_000)).is_err());
      assert!(validated_times(UnixMillis(1_000), UnixMillis(2_000)).is_ok());
    }
  }
}
