//! Meetings: scheduling, the list, RSVP, guest links and the lobby, plus the guest side (a personal
//! link, a name, the lobby). The server owns all of it (docs/meetings.md); this module is the REST
//! client and the pure logic around it (form validation, link parsing), tested without a browser.

use std::rc::Rc;

use leptos::prelude::*;
use teamcraft_proto::{
  CreateGuestLinkRequest, CreateInstantMeetingRequest, CreateMeetingRequest, ErrorBody, ErrorCode,
  GuestLinkResponse, IdempotencyKey, JoinMeetingRequest, JoinMeetingResponse, ListLobbyResponse,
  ListMeetingsResponse, LobbyEntry, LobbyState, LobbyStateResponse, MeetingControlAction,
  MeetingControlRequest, MeetingId, MeetingInvitee, MeetingRecurrence, MeetingResponse, RecurrenceFrequency,
  RsvpStatus, SetRsvpRequest, UnixMillis,
};

use crate::auth::fetch::fetch;
use crate::auth::{ApiMethod, ApiResponse, AppConfig, AuthError, Authenticator};
use crate::clock::{now_ms, parse_local_datetime};
use crate::feedback::{ActionFailure, FailureKind};
use crate::meeting_reminders::ServerClock;
use crate::model::{ConversationId, TimestampMs, UserId};
use crate::reactive::SetIfAlive;

/// HTTP status of a refused request for a meeting the caller is not in (or that doesn't exist).
pub const STATUS_NOT_FOUND: u16 = 404;
/// HTTP status of a refused request for a meeting that is over or cancelled.
pub const STATUS_CONFLICT: u16 = 409;
/// Meetings that ended less than this long ago are still listed.
const LIST_LOOKBACK_MS: i64 = 60 * 60_000;
const LIST_LIMIT: u32 = 100;
const MS_PER_MINUTE: i64 = 60_000;
/// How long a meeting may be scheduled for in one go.
const MAX_DURATION_MINUTES: u32 = 24 * 60;
/// Prefix of the page of a meeting; the guest token follows in the fragment (`#invite=`).
const MEETING_PATH_PREFIX: &str = "/meeting/";
const INVITE_FRAGMENT_KEY: &str = "invite=";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeetingError {
  #[error("{0}")]
  Auth(#[from] AuthError),
  #[error("the server refused the request ({status}): {code}")]
  Refused { status: u16, code: String },
  #[error("unreadable response: {0}")]
  Unreadable(String),
}

/// The meetings of the caller and the server's clock when it answered.
pub struct MeetingList {
  pub meetings: Vec<MeetingResponse>,
  /// `None` from servers that don't send their time.
  pub maybe_clock: Option<ServerClock>,
}

/// Authenticated meeting calls of a signed-in member.
#[derive(Clone)]
pub struct MeetingsClient {
  auth: Rc<dyn Authenticator>,
}

impl MeetingsClient {
  pub fn new(auth: Rc<dyn Authenticator>) -> Self {
    Self { auth }
  }

  /// Upcoming and just-ended meetings of the caller, soonest first, with the server's clock.
  pub async fn list(&self, now: TimestampMs) -> Result<MeetingList, MeetingError> {
    let path = format!(
      "/api/v1/meetings?from={}&limit={LIST_LIMIT}",
      now - LIST_LOOKBACK_MS
    );
    let sent_at = now_ms();
    let response = self
      .call::<ListMeetingsResponse>(ApiMethod::Get, &path, None)
      .await?;
    let maybe_clock = response
      .maybe_server_now
      .map(|server_now| ServerClock::measured(server_now.0, sent_at, now_ms()));
    let mut meetings = response.meetings;
    meetings.sort_by_key(|meeting| meeting.starts_at);
    Ok(MeetingList {
      meetings,
      maybe_clock,
    })
  }

  pub async fn create(&self, request: &CreateMeetingRequest) -> Result<MeetingResponse, MeetingError> {
    self
      .call(ApiMethod::Post, "/api/v1/meetings", Some(to_json(request)?))
      .await
  }

  pub async fn start_instant(
    &self,
    maybe_title: Option<String>,
    maybe_idempotency_key: Option<IdempotencyKey>,
  ) -> Result<MeetingResponse, MeetingError> {
    let body = to_json(&CreateInstantMeetingRequest {
      maybe_title,
      maybe_idempotency_key,
    })?;
    self
      .call(ApiMethod::Post, "/api/v1/meetings/instant", Some(body))
      .await
  }

  /// Host controls: mute all, remove a participant, lock, unlock, end for everyone.
  pub async fn control(&self, meeting: MeetingId, action: MeetingControlAction) -> Result<(), MeetingError> {
    let body = to_json(&MeetingControlRequest { action })?;
    self
      .send(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/controls"),
        Some(body),
      )
      .await
      .map(|_| ())
  }

  pub async fn cancel(&self, meeting: MeetingId) -> Result<(), MeetingError> {
    self
      .send(ApiMethod::Delete, &format!("/api/v1/meetings/{meeting}"), None)
      .await
      .map(|_| ())
  }

  pub async fn set_rsvp(&self, meeting: MeetingId, status: RsvpStatus) -> Result<(), MeetingError> {
    let body = to_json(&SetRsvpRequest { status })?;
    self
      .send(
        ApiMethod::Put,
        &format!("/api/v1/meetings/{meeting}/rsvp"),
        Some(body),
      )
      .await
      .map(|_| ())
  }

  /// A personal one-time link for somebody without an account.
  pub async fn guest_link(
    &self,
    meeting: MeetingId,
    maybe_label: Option<String>,
  ) -> Result<GuestLinkResponse, MeetingError> {
    let body = to_json(&CreateGuestLinkRequest { maybe_label })?;
    self
      .call(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/guest-links"),
        Some(body),
      )
      .await
  }

  /// One meeting the caller is in (organizer, invited or admitted); `404` for others.
  pub async fn get(&self, meeting: MeetingId) -> Result<MeetingResponse, MeetingError> {
    self
      .call(ApiMethod::Get, &format!("/api/v1/meetings/{meeting}"), None)
      .await
  }

  /// The meeting's shared guest link (one link for many guests, each waits in the lobby), made on
  /// first use. Hosts only.
  pub async fn shared_guest_link(&self, meeting: MeetingId) -> Result<GuestLinkResponse, MeetingError> {
    self
      .call(
        ApiMethod::Put,
        &format!("/api/v1/meetings/{meeting}/shared-guest-link"),
        Some("{}".to_owned()),
      )
      .await
  }

  /// Revokes the shared guest link: nobody new gets in with it.
  pub async fn revoke_shared_guest_link(&self, meeting: MeetingId) -> Result<(), MeetingError> {
    self
      .send(
        ApiMethod::Delete,
        &format!("/api/v1/meetings/{meeting}/shared-guest-link"),
        None,
      )
      .await
      .map(|_| ())
  }

  /// Asks the hosts to let the caller into a meeting they opened by its link.
  pub async fn knock(&self, meeting: MeetingId) -> Result<LobbyState, MeetingError> {
    let response: LobbyStateResponse = self
      .call(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/lobby/me"),
        None,
      )
      .await?;
    Ok(response.state)
  }

  /// Where the caller stands after knocking.
  pub async fn my_lobby_state(&self, meeting: MeetingId) -> Result<LobbyState, MeetingError> {
    let response: LobbyStateResponse = self
      .call(
        ApiMethod::Get,
        &format!("/api/v1/meetings/{meeting}/lobby/me"),
        None,
      )
      .await?;
    Ok(response.state)
  }

  pub async fn lobby(&self, meeting: MeetingId) -> Result<Vec<LobbyEntry>, MeetingError> {
    let response: ListLobbyResponse = self
      .call(ApiMethod::Get, &format!("/api/v1/meetings/{meeting}/lobby"), None)
      .await?;
    Ok(response.entries)
  }

  pub async fn admit(&self, meeting: MeetingId, guest: UserId) -> Result<(), MeetingError> {
    self
      .send(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/lobby/{guest}/admit"),
        None,
      )
      .await
      .map(|_| ())
  }

  pub async fn deny(&self, meeting: MeetingId, guest: UserId) -> Result<(), MeetingError> {
    self
      .send(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/lobby/{guest}/deny"),
        None,
      )
      .await
      .map(|_| ())
  }

  async fn call<T: serde::de::DeserializeOwned>(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<T, MeetingError> {
    let response = self.send(method, path, maybe_body).await?;
    serde_json::from_str(&response.body).map_err(|error| MeetingError::Unreadable(error.to_string()))
  }

  async fn send(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> Result<ApiResponse, MeetingError> {
    let response = self.auth.api_request(method, path, maybe_body).await?;
    check(response)
  }
}

/// Signed-in meeting state shared by the meetings screens. Provided as context after sign-in.
#[derive(Clone, Copy)]
pub struct MeetingsHub {
  client: StoredValue<MeetingsClient, LocalStorage>,
  pub meetings: RwSignal<Vec<MeetingResponse>>,
  /// Guests waiting for admission, per meeting (only meetings the user hosts are asked).
  pub lobbies: RwSignal<Vec<(MeetingId, Vec<LobbyEntry>)>>,
  /// Why the last call failed, for a visible message.
  pub maybe_error: RwSignal<Option<String>>,
  pub is_loaded: RwSignal<bool>,
  /// The meeting just scheduled or started, for the dialog that hands out its link.
  pub maybe_created: RwSignal<Option<MeetingResponse>>,
  /// The server's clock as of the last list (meeting reminders run on it).
  pub clock: RwSignal<ServerClock>,
}

impl MeetingsHub {
  pub fn new(client: MeetingsClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
      meetings: RwSignal::new(Vec::new()),
      lobbies: RwSignal::new(Vec::new()),
      maybe_error: RwSignal::new(None),
      is_loaded: RwSignal::new(false),
      maybe_created: RwSignal::new(None),
      clock: RwSignal::new(ServerClock::default()),
    }
  }

  pub fn client(&self) -> MeetingsClient {
    self.client.get_value()
  }

  /// The meeting whose chat is `conversation`, if `me` organizes it. Reactive.
  pub fn hosted_meeting_for(&self, conversation: ConversationId, me: UserId) -> Option<MeetingId> {
    self.meetings.with(|all| {
      all
        .iter()
        .find(|meeting| meeting.conversation_id == conversation && meeting.organizer_user_id == me)
        .map(|meeting| meeting.id)
    })
  }

  /// Reloads the list, then the lobbies of the meetings `me` hosts that use one.
  pub async fn refresh(&self, me: UserId, now: TimestampMs) {
    match self.client().list(now).await {
      Ok(MeetingList {
        meetings,
        maybe_clock,
      }) => {
        if let Some(clock) = maybe_clock {
          self.clock.set_if_alive(clock);
        }
        self.maybe_error.set_if_alive(None);
        self.meetings.set_if_alive(meetings.clone());
        self.is_loaded.set_if_alive(true);
        let mut lobbies = Vec::new();
        for meeting in meetings.iter().filter(|meeting| hosts_lobby(meeting, me)) {
          if let Ok(entries) = self.client().lobby(meeting.id).await
            && !entries.is_empty()
          {
            lobbies.push((meeting.id, entries));
          }
        }
        self.lobbies.set_if_alive(lobbies);
      },
      Err(error) => {
        tracing::warn!(%error, "could not load the meetings");
        self.maybe_error.set_if_alive(Some(error.to_string()));
      },
    }
  }

  /// Runs `action`, shows its failure, and refreshes on success.
  pub async fn run<T>(
    &self,
    me: UserId,
    now: TimestampMs,
    action: impl std::future::Future<Output = Result<T, MeetingError>>,
  ) -> Option<T> {
    match action.await {
      Ok(value) => {
        self.refresh(me, now).await;
        Some(value)
      },
      Err(error) => {
        tracing::warn!(%error, "a meeting action failed");
        self.maybe_error.set_if_alive(Some(error.to_string()));
        None
      },
    }
  }
}

/// A live meeting where `me` is the host, so guests and members with its link may wait in its
/// lobby. Also without `has_lobby`: guests of the shared link and members always wait there.
pub fn hosts_lobby(meeting: &MeetingResponse, me: UserId) -> bool {
  meeting.organizer_user_id == me && meeting.status == teamcraft_proto::MeetingStatus::Scheduled
}

/// Whether the meeting has ended or was cancelled (shown dimmed, no join).
pub fn is_over(meeting: &MeetingResponse, now: TimestampMs) -> bool {
  meeting.status != teamcraft_proto::MeetingStatus::Scheduled || meeting.ends_at.0 <= now
}

/// The caller's own answer to the invitation, when they are an attendee.
pub fn my_rsvp(meeting: &MeetingResponse, me: UserId) -> Option<RsvpStatus> {
  meeting
    .attendees
    .iter()
    .find(|attendee| attendee.maybe_user_id == Some(me))
    .map(|attendee| attendee.rsvp.clone())
}

// ---- The schedule form --------------------------------------------------------------------------

/// How a meeting repeats, as the dialog offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
  Never,
  Daily,
  Weekly,
  Monthly,
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

  fn frequency(self) -> Option<RecurrenceFrequency> {
    match self {
      Self::Never => None,
      Self::Daily => Some(RecurrenceFrequency::Daily),
      Self::Weekly => Some(RecurrenceFrequency::Weekly),
      Self::Monthly => Some(RecurrenceFrequency::Monthly),
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalInvitee {
  pub email: String,
  pub name: String,
}

/// What the schedule dialog collects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleForm {
  pub title: String,
  pub description: String,
  /// Value of a `datetime-local` input, local time.
  pub starts_at: String,
  pub duration_minutes: u32,
  pub members: Vec<UserId>,
  pub externals: Vec<ExternalInvitee>,
  pub repeat: Repeat,
  /// Every n-th day, week or month.
  pub interval: u32,
  /// Total number of occurrences; `None` repeats until cancelled.
  pub maybe_count: Option<u32>,
  pub has_lobby: bool,
  /// Made when the dialog opens and kept for every submit of it, so a retry after a lost answer
  /// (or a second click) schedules the meeting only once.
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

impl ScheduleForm {
  pub fn new(starts_at: String) -> Self {
    Self {
      title: String::new(),
      description: String::new(),
      starts_at,
      duration_minutes: 30,
      members: Vec::new(),
      externals: Vec::new(),
      repeat: Repeat::Never,
      interval: 1,
      maybe_count: None,
      has_lobby: true,
      maybe_idempotency_key: Some(IdempotencyKey::random()),
    }
  }

  pub fn to_request(&self, now: TimestampMs) -> Result<CreateMeetingRequest, FormError> {
    let title = self.title.trim();
    if title.is_empty() {
      return Err(FormError::MissingTitle);
    }
    let starts_at = parse_local_datetime(&self.starts_at).ok_or(FormError::InvalidStart)?;
    if starts_at < now - MS_PER_MINUTE {
      return Err(FormError::StartInThePast);
    }
    if self.duration_minutes == 0 || self.duration_minutes > MAX_DURATION_MINUTES {
      return Err(FormError::InvalidDuration);
    }
    if self.repeat != Repeat::Never && self.interval == 0 {
      return Err(FormError::InvalidInterval);
    }
    let mut invitees: Vec<MeetingInvitee> = self
      .members
      .iter()
      .map(|user_id| MeetingInvitee::User { user_id: *user_id })
      .collect();
    for external in &self.externals {
      let email = external.email.trim();
      if !is_plausible_email(email) {
        return Err(FormError::InvalidEmail(email.to_owned()));
      }
      invitees.push(MeetingInvitee::External {
        email: email.to_owned(),
        maybe_name: Some(external.name.trim().to_owned()).filter(|name| !name.is_empty()),
      });
    }
    Ok(CreateMeetingRequest {
      title: title.to_owned(),
      maybe_description: Some(self.description.trim().to_owned()).filter(|text| !text.is_empty()),
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + i64::from(self.duration_minutes) * MS_PER_MINUTE),
      invitees,
      maybe_recurrence: self.repeat.frequency().map(|frequency| MeetingRecurrence {
        frequency,
        interval: self.interval,
        maybe_count: self.maybe_count.filter(|count| *count > 0),
      }),
      has_lobby: self.has_lobby,
      maybe_idempotency_key: self.maybe_idempotency_key,
    })
  }
}

/// Why a form cannot be sent; the dialog maps each to a translated message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormError {
  MissingTitle,
  InvalidStart,
  StartInThePast,
  InvalidDuration,
  InvalidInterval,
  InvalidEmail(String),
}

impl FormError {
  /// Fluent key of the message.
  pub fn key(&self) -> &'static str {
    match self {
      Self::MissingTitle => "meeting-error-title",
      Self::InvalidStart => "meeting-error-start",
      Self::StartInThePast => "meeting-error-past",
      Self::InvalidDuration => "meeting-error-duration",
      Self::InvalidInterval => "meeting-error-interval",
      Self::InvalidEmail(_) => "meeting-error-email",
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

// ---- The guest side -----------------------------------------------------------------------------

/// The meeting and token of a personal link (`/meeting/<id>#invite=<token>`), if the page is one.
pub fn parse_guest_link(path: &str, fragment: &str) -> Option<(MeetingId, String)> {
  let id_text = path.strip_prefix(MEETING_PATH_PREFIX)?.trim_end_matches('/');
  let meeting = id_text.parse::<MeetingId>().ok()?;
  let token = fragment
    .trim_start_matches('#')
    .split('&')
    .find_map(|part| part.strip_prefix(INVITE_FRAGMENT_KEY))
    .filter(|token| !token.is_empty())?;
  Some((meeting, token.to_owned()))
}

/// Requests of a guest. They carry no member account: the link's token is traded for a session.
#[derive(Clone)]
pub struct GuestClient {
  config: AppConfig,
}

impl GuestClient {
  pub fn new(config: AppConfig) -> Self {
    Self { config }
  }

  pub async fn join(
    &self,
    invite_token: &str,
    display_name: &str,
  ) -> Result<JoinMeetingResponse, MeetingError> {
    let body = to_json(&JoinMeetingRequest {
      invite_token: invite_token.to_owned(),
      display_name: display_name.trim().to_owned(),
    })?;
    let response = self
      .request("POST", "/api/v1/meetings/join", None, Some(body))
      .await?;
    serde_json::from_str(&response.body).map_err(|error| MeetingError::Unreadable(error.to_string()))
  }

  /// Where the guest stands: waiting, admitted or denied.
  pub async fn lobby_state(&self, meeting: MeetingId, guest_token: &str) -> Result<LobbyState, MeetingError> {
    let path = format!("/api/v1/meetings/{meeting}/lobby/me");
    let response = self.request("GET", &path, Some(guest_token), None).await?;
    let state: LobbyStateResponse =
      serde_json::from_str(&response.body).map_err(|error| MeetingError::Unreadable(error.to_string()))?;
    Ok(state.state)
  }

  async fn request(
    &self,
    method: &str,
    path: &str,
    maybe_token: Option<&str>,
    maybe_body: Option<String>,
  ) -> Result<ApiResponse, MeetingError> {
    let mut headers = vec![("Accept".to_owned(), "application/json".to_owned())];
    if let Some(token) = maybe_token {
      headers.push(("Authorization".to_owned(), format!("Bearer {token}")));
    }
    if maybe_body.is_some() {
      headers.push(("Content-Type".to_owned(), "application/json".to_owned()));
    }
    let response = fetch(
      method,
      &self.config.api_url(path),
      &headers,
      maybe_body.map(String::into_bytes),
    )
    .await
    .map_err(|error| MeetingError::Auth(AuthError::Network(error)))?;
    check(ApiResponse {
      status: response.status,
      body: String::from_utf8_lossy(&response.body).into_owned(),
    })
  }
}

fn check(response: ApiResponse) -> Result<ApiResponse, MeetingError> {
  if response.status < 300 {
    return Ok(response);
  }
  let code = serde_json::from_str::<ErrorBody>(&response.body)
    .map(|body| body.error_code.as_str().to_owned())
    .unwrap_or_else(|_| "unknown".to_owned());
  Err(MeetingError::Refused {
    status: response.status,
    code,
  })
}

impl MeetingError {
  /// The failure an error toast words (`feedback::report_failure`).
  pub fn to_failure(&self) -> ActionFailure {
    match self {
      Self::Refused { code, .. } => ActionFailure::new(
        FailureKind::Server(ErrorCode::from(code.clone())),
        self.to_string(),
      ),
      Self::Auth(AuthError::Network(_)) => ActionFailure::new(FailureKind::Network, self.to_string()),
      Self::Auth(_) | Self::Unreadable(_) => ActionFailure::other(self.to_string()),
    }
  }

  /// Whether the server refused with this HTTP status.
  pub fn is_status(&self, expected: u16) -> bool {
    matches!(self, Self::Refused { status, .. } if *status == expected)
  }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, MeetingError> {
  serde_json::to_string(value).map_err(|error| MeetingError::Unreadable(error.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  const NOW: TimestampMs = 1_792_540_800_000;

  #[test]
  fn a_valid_form_becomes_a_request() {
    let mut form = filled_form();
    form.members = vec![id_from_raw(5)];
    form.externals = vec![ExternalInvitee {
      email: " guest@example.com ".into(),
      name: "Guest".into(),
    }];
    form.repeat = Repeat::Weekly;
    form.maybe_count = Some(4);
    let request = form.to_request(NOW).expect("valid");
    assert_eq!(request.title, "Planning");
    assert_eq!(request.ends_at.0 - request.starts_at.0, 45 * MS_PER_MINUTE);
    assert_eq!(request.invitees.len(), 2);
    assert_eq!(
      request.invitees[1],
      MeetingInvitee::External {
        email: "guest@example.com".into(),
        maybe_name: Some("Guest".into())
      }
    );
    let recurrence = request.maybe_recurrence.expect("recurrence");
    assert_eq!(
      (recurrence.frequency, recurrence.interval, recurrence.maybe_count),
      (RecurrenceFrequency::Weekly, 1, Some(4))
    );
    assert!(request.has_lobby);
  }

  #[test]
  fn the_form_refuses_what_the_server_would() {
    type Edit = fn(&mut ScheduleForm);
    let cases: [(Edit, FormError); 6] = [
      (|form| form.title = "  ".into(), FormError::MissingTitle),
      (|form| form.starts_at = "tomorrow".into(), FormError::InvalidStart),
      (
        |form| form.starts_at = "2020-01-01T10:00".into(),
        FormError::StartInThePast,
      ),
      (|form| form.duration_minutes = 0, FormError::InvalidDuration),
      (
        |form| {
          form.repeat = Repeat::Daily;
          form.interval = 0;
        },
        FormError::InvalidInterval,
      ),
      (
        |form| {
          form.externals = vec![ExternalInvitee {
            email: "nobody".into(),
            name: String::new(),
          }]
        },
        FormError::InvalidEmail("nobody".into()),
      ),
    ];
    for (edit, expected) in cases {
      let mut form = filled_form();
      edit(&mut form);
      assert_eq!(form.to_request(NOW), Err(expected));
    }
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
  fn guest_links_are_read_from_the_path_and_the_fragment() {
    let (meeting, token) = parse_guest_link("/meeting/123456789", "#invite=abc.def").expect("link");
    assert_eq!(meeting.to_string(), "123456789");
    assert_eq!(token, "abc.def");
    assert!(parse_guest_link("/meeting/123456789", "").is_none());
    assert!(parse_guest_link("/meeting/123456789", "#invite=").is_none());
    assert!(parse_guest_link("/meeting/not-a-number", "#invite=abc").is_none());
    assert!(parse_guest_link("/threads", "#invite=abc").is_none());
    assert!(parse_guest_link("/meeting/123456789/", "#x=1&invite=abc").is_some());
  }

  #[test]
  fn refusals_carry_the_stable_error_code() {
    let response = ApiResponse {
      status: 404,
      body: r#"{"error_code":"meeting_not_found","message":"no such meeting"}"#.into(),
    };
    assert_eq!(
      check(response),
      Err(MeetingError::Refused {
        status: 404,
        code: "meeting_not_found".into()
      })
    );
    let odd = ApiResponse {
      status: 502,
      body: "<html>".into(),
    };
    assert_eq!(
      check(odd),
      Err(MeetingError::Refused {
        status: 502,
        code: "unknown".into()
      })
    );
  }

  #[test]
  fn refusals_become_failures_worded_by_their_code_and_keep_their_status() {
    let not_found = MeetingError::Refused {
      status: STATUS_NOT_FOUND,
      code: "meeting_not_found".into(),
    };
    let offline = MeetingError::Auth(AuthError::Network("down".into()));

    assert_eq!(
      not_found.to_failure().kind,
      FailureKind::Server(ErrorCode::MeetingNotFound)
    );
    assert!(not_found.is_status(STATUS_NOT_FOUND));
    assert!(!not_found.is_status(STATUS_CONFLICT));
    assert_eq!(offline.to_failure().kind, FailureKind::Network);
    assert!(!offline.is_status(STATUS_NOT_FOUND));
    assert_eq!(
      MeetingError::Unreadable("x".into()).to_failure().kind,
      FailureKind::Other
    );
  }

  #[test]
  fn hosts_see_the_lobby_of_their_live_meetings_also_without_has_lobby() {
    let organizer: UserId = id_from_raw(1);
    let mut meeting = MeetingResponse {
      id: id_from_raw(7),
      title: "Sync".into(),
      maybe_description: None,
      starts_at: UnixMillis(NOW),
      ends_at: UnixMillis(NOW + 3_600_000),
      kind: teamcraft_proto::MeetingKind::Instant,
      status: teamcraft_proto::MeetingStatus::Scheduled,
      has_lobby: false,
      organizer_user_id: organizer,
      conversation_id: id_from_raw(9),
      join_url: String::new(),
      maybe_recurrence: None,
      attendees: Vec::new(),
      undelivered_invitations: Vec::new(),
    };
    assert!(
      hosts_lobby(&meeting, organizer),
      "shared-link guests and members always wait"
    );
    assert!(!hosts_lobby(&meeting, id_from_raw(2)));
    meeting.status = teamcraft_proto::MeetingStatus::Cancelled;
    assert!(!hosts_lobby(&meeting, organizer));
  }

  fn filled_form() -> ScheduleForm {
    // The test clock is "now" in UTC, so a start two hours later is in the future everywhere.
    let starts = crate::clock::format_local_datetime_input(NOW + 2 * 3_600_000);
    let mut form = ScheduleForm::new(starts);
    form.title = "Planning".into();
    form.duration_minutes = 45;
    form
  }
}
