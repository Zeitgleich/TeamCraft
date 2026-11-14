//! Meetings: scheduling, the list, RSVP, guest links and the lobby, plus the guest side (a personal
//! link, a name, the lobby). The server owns all of it (docs/meetings.md); this module is the REST
//! client and the pure logic around it (form validation, link parsing), tested without a browser.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use leptos::prelude::*;
pub use teamcraft_client_api::meeting_edits::{
  MAX_DURATION_MINUTES, MS_PER_MINUTE, OwedChatChange, PendingEdit, invited_users, invitee_change, is_over,
  noting_pending_edit, plan_reconcile,
};
pub use teamcraft_client_api::meeting_invitees::{
  ChatInvitees, added_users, adding_invitee, adding_invitees, attending_users, saved_change,
};
use teamcraft_client_api::model::SpaceId;
use teamcraft_proto::{
  CancelScope, CreateGuestLinkRequest, CreateInstantMeetingRequest, CreateMeetingRequest, ErrorBody,
  ErrorCode, GuestLinkResponse, HolidayRegionResponse, IdempotencyKey, JoinMeetingRequest,
  JoinMeetingResponse, KnockByLinkResponse, ListLobbyResponse, ListMeetingsResponse, LobbyEntry, LobbyState,
  LobbyStateResponse, MeetingControlAction, MeetingControlRequest, MeetingId, MeetingInvitee,
  MeetingRecurrence, MeetingResponse, RecurrenceFrequency, RestoreMeetingRequest, RsvpStatus,
  SetHolidayRegionRequest, SetRsvpRequest, UnixMillis, UpdateMeetingRequest, Weekday,
};

use zeroize::Zeroizing;

use crate::auth::fetch::fetch;
use crate::auth::{ApiMethod, ApiResponse, AppConfig, AuthError, Authenticator};
use crate::clock::{format_local_datetime_input, local_day, now_ms, parse_local_datetime};
use crate::feedback::{ActionFailure, FailureKind};
use crate::meeting_reminders::ServerClock;
use crate::model::{ConversationId, TimestampMs, UserId};
use crate::reactive::SetIfAlive;
use crate::route::MeetingHandle;

/// HTTP status of a refused request for a meeting the caller is not in (or that doesn't exist).
pub const STATUS_NOT_FOUND: u16 = 404;
/// HTTP status of a refused request for a meeting that is over or cancelled.
pub const STATUS_CONFLICT: u16 = 409;
/// Meetings that ended less than this long ago are still listed.
const LIST_LOOKBACK_MS: i64 = 60 * 60_000;
const LIST_LIMIT: u32 = 100;
/// How far back the Past view of the meetings page reaches (#956).
const PAST_LOOKBACK_MS: i64 = 30 * 24 * 60 * 60_000;
/// The server's own maximum: a month of daily series still fits.
const PAST_LIST_LIMIT: u32 = 500;
/// Largest "every N days, weeks or months" and number of meetings of a series (the server's limits).
const MAX_INTERVAL: u32 = 100;
const MAX_COUNT: u32 = 1_000;
/// Prefix of the page of a meeting; the guest token follows in the fragment (`#invite=`).
const MEETING_PATH_PREFIX: &str = "/meeting/";
const INVITE_FRAGMENT_KEY: &str = "invite=";
/// The organizer's identity key fingerprint in a meeting link's fragment, and its name in links of
/// older apps.
const VERIFIER_FRAGMENT_KEY: &str = "v=";
const LEGACY_ORGANIZER_FRAGMENT_KEY: &str = "o=";

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

  /// The caller's meetings of the last [`PAST_LOOKBACK_MS`] up to `now`, for the Past view; those
  /// still running are in the list too (the view leaves them out).
  pub async fn list_past(&self, now: TimestampMs) -> Result<Vec<MeetingResponse>, MeetingError> {
    let path = format!(
      "/api/v1/meetings?from={}&to={now}&limit={PAST_LIST_LIMIT}",
      now - PAST_LOOKBACK_MS
    );
    let response = self
      .call::<ListMeetingsResponse>(ApiMethod::Get, &path, None)
      .await?;
    Ok(response.meetings)
  }

  /// Whose public holidays the caller's series skip (#1441).
  pub async fn holiday_region(&self) -> Result<HolidayRegionResponse, MeetingError> {
    self.call(ApiMethod::Get, "/api/v1/me/holiday-region", None).await
  }

  /// Sets the caller's own holiday region; `None` goes back to the workspace's.
  pub async fn set_holiday_region(
    &self,
    maybe_region: Option<String>,
  ) -> Result<HolidayRegionResponse, MeetingError> {
    let body = to_json(&SetHolidayRegionRequest { maybe_region })?;
    self
      .call(ApiMethod::Put, "/api/v1/me/holiday-region", Some(body))
      .await
  }

  pub async fn create(&self, request: &CreateMeetingRequest) -> Result<MeetingResponse, MeetingError> {
    self
      .call(ApiMethod::Post, "/api/v1/meetings", Some(to_json(request)?))
      .await
  }

  /// Saves a changed meeting (organizer only); invitees are replaced as a whole.
  pub async fn update(
    &self,
    meeting: MeetingId,
    request: &UpdateMeetingRequest,
  ) -> Result<MeetingResponse, MeetingError> {
    self
      .call(
        ApiMethod::Patch,
        &format!("/api/v1/meetings/{meeting}"),
        Some(to_json(request)?),
      )
      .await
  }

  /// "Meet now"; `invitees` become members of its chat right away (a call moving into it).
  pub async fn start_instant(
    &self,
    maybe_title: Option<String>,
    invitees: Vec<UserId>,
    maybe_space: Option<SpaceId>,
    maybe_idempotency_key: Option<IdempotencyKey>,
  ) -> Result<MeetingResponse, MeetingError> {
    let body = to_json(&CreateInstantMeetingRequest {
      maybe_title,
      invitees,
      maybe_space,
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

  /// Cancels the meeting, or of a repeating one the occurrence starting at `occurrence` (and with
  /// [`CancelScope::Following`] every one after it).
  pub async fn cancel(
    &self,
    meeting: MeetingId,
    scope: CancelScope,
    occurrence: UnixMillis,
  ) -> Result<(), MeetingError> {
    let path = match scope {
      CancelScope::Series => format!("/api/v1/meetings/{meeting}"),
      _ => format!(
        "/api/v1/meetings/{meeting}?scope={scope}&occurrence={}",
        occurrence.0
      ),
    };
    self.send(ApiMethod::Delete, &path, None).await.map(|_| ())
  }

  /// Takes a cancellation back: of the occurrence starting at `maybe_occurrence`, or of the meeting.
  pub async fn restore(
    &self,
    meeting: MeetingId,
    maybe_occurrence: Option<UnixMillis>,
  ) -> Result<(), MeetingError> {
    let body = to_json(&RestoreMeetingRequest { maybe_occurrence })?;
    self
      .send(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/restore"),
        Some(body),
      )
      .await
      .map(|_| ())
  }

  /// Takes a cancelled meeting off everybody's lists.
  pub async fn hide(&self, meeting: MeetingId) -> Result<(), MeetingError> {
    self
      .send(ApiMethod::Post, &format!("/api/v1/meetings/{meeting}/hide"), None)
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

  /// Makes the meeting's link anew (organizer only): the old one stops working, invitees get the
  /// new one.
  pub async fn renew_link(&self, meeting: MeetingId) -> Result<MeetingResponse, MeetingError> {
    self
      .call(
        ApiMethod::Post,
        &format!("/api/v1/meetings/{meeting}/link"),
        Some("{}".to_owned()),
      )
      .await
  }

  /// Tells the server this app lets guests in without a link secret (#987), so guests of the
  /// user's meetings aren't told the organizer's app needs an update.
  pub async fn mark_admitter(&self) -> Result<(), MeetingError> {
    self
      .send(ApiMethod::Put, "/api/v1/meetings/admitter", Some("{}".to_owned()))
      .await
      .map(|_| ())
  }

  /// A member opened a meeting link with a token: the meeting, and whether they are in or knocked.
  pub async fn knock_by_link(&self, token: &str) -> Result<KnockByLinkResponse, MeetingError> {
    self
      .call(
        ApiMethod::Post,
        &format!("/api/v1/meetings/link/{token}/knock"),
        None,
      )
      .await
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

  /// Who waits in a meeting's lobby, and whether the caller hosts it (decides on everybody) or is
  /// another member of a meeting without a lobby (sees only who came with the link).
  pub async fn lobby(&self, meeting: MeetingId) -> Result<ListLobbyResponse, MeetingError> {
    self
      .call(ApiMethod::Get, &format!("/api/v1/meetings/{meeting}/lobby"), None)
      .await
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
  /// Guests waiting for admission, per meeting (meetings the user hosts: [`hosts_lobby`], and those
  /// whose call the user is in: [`Self::in_call`]).
  pub lobbies: RwSignal<Vec<(MeetingId, Vec<LobbyEntry>)>>,
  /// Meetings of somebody else whose call the user is in: their lobby is asked for too, so a host
  /// decides there and an admitter the organizer named lets people of the link in (#1103).
  pub in_call: RwSignal<HashSet<MeetingId>>,
  /// Meetings of somebody else whose lobby the server let the user decide on (a host): on the
  /// meeting's card ([`hosts_lobby`]) and in its call.
  pub cohosted: RwSignal<HashSet<MeetingId>>,
  /// Why the last call failed, for a visible message.
  pub maybe_error: RwSignal<Option<String>>,
  pub is_loaded: RwSignal<bool>,
  /// The meetings of the Past view, loaded when it is first shown (`None` until then).
  pub maybe_past: RwSignal<Option<Vec<MeetingResponse>>>,
  /// The meeting just scheduled or started, for the dialog that hands out its link.
  pub maybe_created: RwSignal<Option<MeetingResponse>>,
  /// The server's clock as of the last list (meeting reminders run on it).
  pub clock: RwSignal<ServerClock>,
  /// The meeting link fragments this device made (`v=<pinned fingerprint>`), per meeting.
  pub verified_links: RwSignal<HashMap<MeetingId, String>>,
  /// Meetings whose fragment is being made, so each is asked for once.
  pub verifying: StoredValue<HashSet<MeetingId>, LocalStorage>,
  /// Key codes of waiting guests' keys as this device pinned them.
  pub key_codes: RwSignal<HashMap<UserId, String>>,
  /// Guests whose key code is being made.
  pub coding: StoredValue<HashSet<UserId>, LocalStorage>,
  /// A meeting guest's one chat, which is the meeting's (the guest can't list meetings).
  maybe_guest_chat: Option<ConversationId>,
}

impl MeetingsHub {
  pub fn new(client: MeetingsClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
      meetings: RwSignal::new(Vec::new()),
      lobbies: RwSignal::new(Vec::new()),
      in_call: RwSignal::new(HashSet::new()),
      cohosted: RwSignal::new(HashSet::new()),
      maybe_error: RwSignal::new(None),
      is_loaded: RwSignal::new(false),
      maybe_past: RwSignal::new(None),
      maybe_created: RwSignal::new(None),
      clock: RwSignal::new(ServerClock::default()),
      verified_links: RwSignal::new(HashMap::new()),
      verifying: StoredValue::new_local(HashSet::new()),
      key_codes: RwSignal::new(HashMap::new()),
      coding: StoredValue::new_local(HashSet::new()),
      maybe_guest_chat: None,
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

  /// The meeting of somebody else whose chat is `conversation`, if the server let the user host
  /// its lobby. Reactive.
  pub fn cohosted_meeting_for(&self, conversation: ConversationId) -> Option<MeetingId> {
    let cohosted = self.cohosted.get();
    self.meetings.with(|all| {
      all
        .iter()
        .find(|meeting| meeting.conversation_id == conversation && cohosted.contains(&meeting.id))
        .map(|meeting| meeting.id)
    })
  }

  /// The title of the meeting whose chat is `conversation`, if it is one of the user's. Reactive.
  /// The title of the meeting whose chat `conversation` is, as the call screens show it: from the
  /// meeting list, or for a meeting guest (who can't list meetings) the chat's own name, which is
  /// the meeting's title.
  pub fn heading_title(
    maybe_hub: Option<MeetingsHub>,
    conversation: ConversationId,
    name: &str,
  ) -> Option<String> {
    maybe_hub.and_then(|hub| {
      hub.title_for(conversation).or_else(|| {
        (hub.maybe_guest_chat == Some(conversation) && !name.trim().is_empty()).then(|| name.to_owned())
      })
    })
  }

  /// A meeting guest's hub: its one chat is the meeting's.
  pub fn for_guest(client: MeetingsClient, chat: ConversationId) -> Self {
    Self {
      maybe_guest_chat: Some(chat),
      ..Self::new(client)
    }
  }

  /// Whether this is a meeting guest's page (no account, only the meeting's chat and call).
  /// [`is_meeting_chat`] says whether a conversation is a meeting's.
  pub fn is_guest_page(&self) -> bool {
    self.maybe_guest_chat.is_some()
  }

  pub fn title_for(&self, conversation: ConversationId) -> Option<String> {
    self.meetings.with(|all| {
      all
        .iter()
        .find(|meeting| meeting.conversation_id == conversation)
        .map(|meeting| meeting.title.clone())
        .filter(|title| !title.trim().is_empty())
    })
  }

  /// Reloads the list, then the lobbies of the meetings `me` hosts ([`hosts_lobby`]: organizer, or
  /// chat admin as the list says) and of those whose call `me` is in.
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
        let mut cohosted = HashSet::new();
        let in_call = self.in_call.try_get_untracked().unwrap_or_default();
        for meeting in meetings
          .iter()
          .filter(|meeting| hosts_lobby(meeting, me) || in_call.contains(&meeting.id))
        {
          // A member of a meeting with a lobby who doesn't host it is refused: no lobby for them.
          let Ok(lobby) = self.client().lobby(meeting.id).await else {
            continue;
          };
          if lobby.is_host && meeting.organizer_user_id != me {
            cohosted.insert(meeting.id);
          }
          if !lobby.entries.is_empty() {
            lobbies.push((meeting.id, lobby.entries));
          }
        }
        self.cohosted.set_if_alive(cohosted);
        self.lobbies.set_if_alive(lobbies);
      },
      Err(error) => {
        tracing::warn!(%error, "could not load the meetings");
        self.maybe_error.set_if_alive(Some(error.to_string()));
      },
    }
  }

  /// Reloads the meetings of the Past view.
  pub async fn refresh_past(&self, now: TimestampMs) {
    match self.client().list_past(now).await {
      Ok(meetings) => {
        self.maybe_error.set_if_alive(None);
        self.maybe_past.set_if_alive(Some(meetings));
      },
      Err(error) => {
        tracing::warn!(%error, "could not load the past meetings");
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

// ---- Edits whose chat change is still owed ----------------------------------------------------

const PENDING_EDITS_KEY: &str = "meeting-edits-pending";

/// Notes the users the organizer is about to take off a meeting, before the save is sent
/// ([`PendingEdit`]).
pub fn note_pending_edit(meeting: MeetingId, removed: Vec<UserId>, now: TimestampMs) {
  if removed.is_empty() {
    return;
  }
  store_pending_edits(&noting_pending_edit(load_pending_edits(), meeting, removed, now));
}

/// The save failed or the core has taken over: nothing is owed for this meeting.
pub fn clear_pending_edit(meeting: MeetingId) {
  let mut edits = load_pending_edits();
  let before = edits.len();
  edits.retain(|edit| edit.meeting != meeting);
  if edits.len() != before {
    store_pending_edits(&edits);
  }
}

/// Finishes edits that were saved but not applied to the chat's group (the app was closed in
/// between), judging by the meeting list ([`plan_reconcile`]).
pub fn reconcile_pending_edits(
  now: TimestampMs,
  meetings: &[MeetingResponse],
  mut apply: impl FnMut(OwedChatChange),
) {
  let edits = load_pending_edits();
  let (kept, owed) = plan_reconcile(&edits, now, meetings);
  for change in owed {
    apply(change);
  }
  if kept.len() != edits.len() {
    store_pending_edits(&kept);
  }
}

fn load_pending_edits() -> Vec<PendingEdit> {
  crate::settings::load_string(PENDING_EDITS_KEY)
    .and_then(|text| serde_json::from_str(&text).ok())
    .unwrap_or_default()
}

fn store_pending_edits(edits: &[PendingEdit]) {
  if let Ok(text) = serde_json::to_string(edits) {
    crate::settings::store_string(PENDING_EDITS_KEY, &text);
  }
}

/// [`reconcile_pending_edits`] on the hub's current list, applied through the core.
pub fn finish_pending_edits(state: crate::state::AppState, hub: MeetingsHub) {
  let meetings = hub.meetings.get_untracked();
  reconcile_pending_edits(now_ms(), &meetings, |change| {
    // The server's list: only users the organizer's signed list names are added (#1726).
    let invitees = ChatInvitees::listed(change.attending);
    state.try_write(|backend| {
      backend.update_meeting_chat_members(change.conversation, change.meeting, invitees, change.removed)
    });
  });
}

/// A live meeting where `me` is the host, so guests and members with its link may wait in its
/// lobby. Also without `has_lobby`: guests of the shared link and members always wait there. The
/// host is the organizer, or a chat admin the meeting list marks `can_host_lobby` (#1305; older
/// servers don't, so there only the organizer's devices ask for the lobby outside the call).
pub fn hosts_lobby(meeting: &MeetingResponse, me: UserId) -> bool {
  (meeting.organizer_user_id == me || meeting.can_host_lobby)
    && meeting.status == teamcraft_proto::MeetingStatus::Scheduled
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

/// How often a meeting repeats, as the dialog offers it: the unit of "every N …".
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

/// How a series ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeriesEnd {
  Never,
  AfterCount,
  OnDate,
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
  /// Every n-th day, week or month; 0 when the field holds no number.
  pub interval: u32,
  /// The days of the week a daily or weekly series meets on, Monday first. All of them: every day.
  pub weekdays: [bool; 7],
  pub ends: SeriesEnd,
  /// Number of meetings of a series that ends after a count; 0 when the field holds no number.
  pub count: u32,
  /// Value of the `date` input of a series that ends on a date (`2026-12-31`), local time.
  pub until: String,
  /// The series leaves out public holidays (#1053).
  pub skips_holidays: bool,
  pub has_lobby: bool,
  /// The notetaker's account when "Notetaker joins the calls" is ticked (e2ee.md §8.2): invited like
  /// a member, so it is in the chat's group before any call.
  pub maybe_notetaker: Option<UserId>,
  /// Invited people who host with the organizer (#1720); signed by the organizer's device once the
  /// meeting is saved (`co_hosts_from_dialog`).
  pub co_hosts: Vec<UserId>,
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
      weekdays: [true; 7],
      ends: SeriesEnd::Never,
      count: 10,
      until: String::new(),
      skips_holidays: false,
      has_lobby: true,
      maybe_notetaker: None,
      co_hosts: Vec::new(),
      maybe_idempotency_key: Some(IdempotencyKey::random()),
    }
  }

  /// The form of the edit dialog: what the saved meeting says now (the organizer `me` is not an
  /// invitee in the list).
  pub fn from_meeting(meeting: &MeetingResponse, me: UserId) -> Self {
    let minutes = (meeting.ends_at.0 - meeting.starts_at.0) / MS_PER_MINUTE;
    let mut form = Self::new(format_local_datetime_input(meeting.starts_at.0));
    form.title = meeting.title.clone();
    form.description = meeting.maybe_description.clone().unwrap_or_default();
    form.duration_minutes = u32::try_from(minutes).unwrap_or(0);
    form.has_lobby = meeting.has_lobby;
    form.co_hosts = meeting.co_host_user_ids.clone();
    form.maybe_idempotency_key = None;
    if let Some(recurrence) = &meeting.maybe_recurrence {
      form.set_repeat(Repeat::of(&recurrence.frequency));
      form.interval = recurrence.interval;
      form.skips_holidays = recurrence.skips_holidays;
      if !recurrence.weekdays.is_empty() {
        form.weekdays = WEEKDAYS.map(|day| recurrence.weekdays.contains(&day));
      }
      if let Some(count) = recurrence.maybe_count {
        form.ends = SeriesEnd::AfterCount;
        form.count = count;
      }
      if let Some(until) = recurrence.maybe_until {
        form.ends = SeriesEnd::OnDate;
        form.until = date_input_of(until.0);
      }
    }
    for attendee in &meeting.attendees {
      match (attendee.maybe_user_id, &attendee.maybe_email) {
        (Some(user), _) if user != me => form.members.push(user),
        (None, Some(email)) => form.externals.push(ExternalInvitee {
          email: email.clone(),
          name: attendee.maybe_name.clone().unwrap_or_default(),
        }),
        _ => {},
      }
    }
    form
  }

  /// Picks how the meeting repeats, with the days that go with it: every day for a daily series, the
  /// start's day for a weekly one.
  pub fn set_repeat(&mut self, repeat: Repeat) {
    if repeat == self.repeat {
      return;
    }
    self.repeat = repeat;
    self.weekdays = match (repeat, self.start_weekday()) {
      (Repeat::Weekly, Some(day)) => {
        let mut only = [false; 7];
        only[day] = true;
        only
      },
      _ => [true; 7],
    };
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

  /// The change to save: the time only when it moved (a meeting already under way can still get a
  /// new invitee), the invitees as the whole list, the repeat rule when it changed.
  pub fn to_update_request(
    &self,
    now: TimestampMs,
    before: &MeetingResponse,
  ) -> Result<UpdateMeetingRequest, Vec<FormError>> {
    let create = self.to_request_inner(now, Some(before.starts_at.0))?;
    // The input has minute precision: seconds of the saved times are not a change.
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

  /// The request, or every reason it can't be sent, in the order of the fields.
  pub fn to_request(&self, now: TimestampMs) -> Result<CreateMeetingRequest, Vec<FormError>> {
    self.to_request_inner(now, None)
  }

  /// `maybe_unchanged_start` is a start that needs no future check (the saved start of a meeting being
  /// edited).
  fn to_request_inner(
    &self,
    now: TimestampMs,
    maybe_unchanged_start: Option<TimestampMs>,
  ) -> Result<CreateMeetingRequest, Vec<FormError>> {
    let mut errors = Vec::new();
    let title = self.title.trim();
    if title.is_empty() {
      errors.push(FormError::MissingTitle);
    }
    let maybe_start = parse_local_datetime(&self.starts_at);
    match maybe_start {
      None => errors.push(FormError::InvalidStart),
      Some(starts_at) => {
        // The input has minute precision: the saved start (with seconds) counts as unchanged.
        let is_unchanged =
          maybe_unchanged_start.is_some_and(|saved| (starts_at - saved).abs() < MS_PER_MINUTE);
        if starts_at < now - MS_PER_MINUTE && !is_unchanged {
          errors.push(FormError::StartInThePast);
        }
      },
    }
    if self.duration_minutes == 0 || self.duration_minutes > MAX_DURATION_MINUTES {
      errors.push(FormError::InvalidDuration);
    }
    let maybe_recurrence = self.recurrence(maybe_start, &mut errors);
    let mut invitees: Vec<MeetingInvitee> = self
      .members
      .iter()
      .map(|user_id| MeetingInvitee::User { user_id: *user_id })
      .collect();
    if let Some(bot) = self.maybe_notetaker.filter(|bot| !self.members.contains(bot)) {
      invitees.push(MeetingInvitee::User { user_id: bot });
    }
    for external in &self.externals {
      let email = external.email.trim();
      if !is_plausible_email(email) {
        errors.push(FormError::InvalidEmail(email.to_owned()));
        continue;
      }
      invitees.push(MeetingInvitee::External {
        email: email.to_owned(),
        maybe_name: Some(external.name.trim().to_owned()).filter(|name| !name.is_empty()),
      });
    }
    let Some(starts_at) = maybe_start.filter(|_| errors.is_empty()) else {
      return Err(errors);
    };
    Ok(CreateMeetingRequest {
      title: title.to_owned(),
      maybe_description: Some(self.description.trim().to_owned()).filter(|text| !text.is_empty()),
      starts_at: UnixMillis(starts_at),
      ends_at: UnixMillis(starts_at + i64::from(self.duration_minutes) * MS_PER_MINUTE),
      invitees,
      maybe_recurrence,
      has_lobby: self.has_lobby,
      // The dialog sets it: the form doesn't know where it is opened.
      maybe_space: None,
      maybe_idempotency_key: self.maybe_idempotency_key,
    })
  }

  /// The repeat rule of the form, noting what is wrong with it.
  fn recurrence(
    &self,
    maybe_start: Option<TimestampMs>,
    errors: &mut Vec<FormError>,
  ) -> Option<MeetingRecurrence> {
    let frequency = self.repeat.frequency()?;
    if self.interval == 0 || self.interval > MAX_INTERVAL {
      errors.push(FormError::InvalidInterval);
    }
    let only_start_day = self.start_weekday().map(|start| {
      let mut only = [false; 7];
      only[start] = true;
      only
    });
    // Every day, or a weekly series on its start's day: the rule needs no days.
    let is_plain = !self.weekdays.contains(&false)
      || (self.repeat == Repeat::Weekly && only_start_day == Some(self.weekdays));
    let weekdays: Vec<Weekday> = if self.repeat.has_weekdays() && !is_plain {
      WEEKDAYS
        .into_iter()
        .zip(self.weekdays)
        .filter_map(|(day, is_on)| is_on.then_some(day))
        .collect()
    } else {
      Vec::new()
    };
    if self.repeat.has_weekdays() && !self.weekdays.contains(&true) {
      errors.push(FormError::NoWeekday);
    } else if self.repeat.has_weekdays()
      && let Some(day) = self.start_weekday()
      && !self.weekdays[day]
    {
      // The first meeting is the start itself; on another day the series would surprise.
      errors.push(FormError::StartOffWeekdays);
    }
    let (mut maybe_count, mut maybe_until) = (None, None);
    match self.ends {
      SeriesEnd::Never => {},
      SeriesEnd::AfterCount if (1..=MAX_COUNT).contains(&self.count) => maybe_count = Some(self.count),
      SeriesEnd::AfterCount => errors.push(FormError::InvalidCount),
      SeriesEnd::OnDate => match parse_local_datetime(&format!("{}T23:59", self.until)) {
        None => errors.push(FormError::InvalidUntil),
        Some(day_end) if maybe_start.is_some_and(|start| day_end < start) => {
          errors.push(FormError::UntilBeforeStart);
        },
        // The whole last day counts.
        Some(day_end) => maybe_until = Some(UnixMillis(day_end + MS_PER_MINUTE - 1_000)),
      },
    }
    Some(MeetingRecurrence {
      frequency,
      interval: self.interval,
      weekdays,
      maybe_count,
      maybe_until,
      skips_holidays: self.skips_holidays,
    })
  }

  /// The local day of the week of the start, Monday first.
  fn start_weekday(&self) -> Option<usize> {
    let start = parse_local_datetime(&self.starts_at)?;
    // 1970-01-01 was a Thursday.
    usize::try_from((local_day(start) + 3).rem_euclid(7)).ok()
  }
}

/// The value of a `date` input for the local day of `ts`.
fn date_input_of(ts: TimestampMs) -> String {
  format_local_datetime_input(ts)
    .split_once('T')
    .map(|(date, _)| date.to_owned())
    .unwrap_or_default()
}

/// Why a form cannot be sent; the dialog shows each at its field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormError {
  MissingTitle,
  InvalidStart,
  StartInThePast,
  InvalidDuration,
  InvalidInterval,
  NoWeekday,
  StartOffWeekdays,
  InvalidCount,
  InvalidUntil,
  UntilBeforeStart,
  InvalidEmail(String),
}

/// The fields of the schedule dialog an error can belong to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormField {
  Title,
  Start,
  Duration,
  Interval,
  Weekdays,
  Count,
  Until,
  Externals,
}

impl FormField {
  /// The id of the field's element in the dialog.
  pub fn id(self) -> &'static str {
    match self {
      Self::Title => "meeting-title",
      Self::Start => "meeting-start",
      Self::Duration => "meeting-duration",
      Self::Interval => "meeting-interval",
      Self::Weekdays => "meeting-weekdays",
      Self::Count => "meeting-count",
      Self::Until => "meeting-until",
      Self::Externals => "meeting-externals",
    }
  }
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
      Self::NoWeekday => "meeting-error-no-weekday",
      Self::StartOffWeekdays => "meeting-error-start-weekday",
      Self::InvalidCount => "meeting-error-count",
      Self::InvalidUntil => "meeting-error-until",
      Self::UntilBeforeStart => "meeting-error-until-before",
      Self::InvalidEmail(_) => "meeting-error-email",
    }
  }

  pub fn field(&self) -> FormField {
    match self {
      Self::MissingTitle => FormField::Title,
      Self::InvalidStart | Self::StartInThePast => FormField::Start,
      Self::InvalidDuration => FormField::Duration,
      Self::InvalidInterval => FormField::Interval,
      Self::NoWeekday | Self::StartOffWeekdays => FormField::Weekdays,
      Self::InvalidCount => FormField::Count,
      Self::InvalidUntil | Self::UntilBeforeStart => FormField::Until,
      Self::InvalidEmail(_) => FormField::Externals,
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

/// What a meeting link holds for a guest's page (docs/meetings.md, "Links"): the path's handle
/// (the meeting link's token, or an old link's meeting ID), a personal link's one-time token, and
/// the organizer's key fingerprint `v` from the fragment, which never reaches a server. No secret.
#[derive(Clone, PartialEq, Eq)]
pub struct GuestLink {
  pub handle: MeetingHandle,
  pub maybe_invite_token: Option<String>,
  pub maybe_organizer_fingerprint: Option<String>,
  /// The key of the guest's session kept in this browser (`#r=`), on the address the page moved to
  /// after the guest joined, so a reload carries on (`crate::guest_resume`).
  pub maybe_resume_key: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for GuestLink {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("GuestLink")
      .field("handle", &self.handle)
      .field("has_invite_token", &self.maybe_invite_token.is_some())
      .field("maybe_organizer_fingerprint", &self.maybe_organizer_fingerprint)
      .field("has_resume_key", &self.maybe_resume_key.is_some())
      .finish()
  }
}

/// The guest link of a page, if it is one: `/meeting/<token>[#v=<fingerprint>]`, a personal link
/// with `#invite=<token>`, an old `/meeting/<id>#invite=<token>`, or a guest's own address after
/// joining (`#r=<key>`). An old link with the ID alone is a member's link, not a guest's.
pub fn parse_guest_link(path: &str, fragment: &str) -> Option<GuestLink> {
  let segment = path.strip_prefix(MEETING_PATH_PREFIX)?.trim_end_matches('/');
  let handle = MeetingHandle::parse(segment)?;
  let fragment = fragment.trim_start_matches('#');
  let value = |key: &str| {
    fragment
      .split('&')
      .find_map(|part| part.strip_prefix(key))
      .filter(|value| !value.is_empty())
      .map(str::to_owned)
  };
  let maybe_invite_token = value(INVITE_FRAGMENT_KEY);
  let maybe_resume_key = value(crate::guest_resume::RESUME_FRAGMENT_KEY).map(Zeroizing::new);
  if matches!(handle, MeetingHandle::Id(_)) && maybe_invite_token.is_none() && maybe_resume_key.is_none() {
    return None;
  }
  Some(GuestLink {
    handle,
    maybe_invite_token,
    maybe_organizer_fingerprint: value(VERIFIER_FRAGMENT_KEY)
      .or_else(|| value(LEGACY_ORGANIZER_FRAGMENT_KEY)),
    maybe_resume_key,
  })
}

/// `link` with `fragment` (`v=<fingerprint>`) as its verifier, replacing any `v` the server put
/// there: the copying device's pinned key wins over the server's word.
pub fn with_verifier(link: &str, fragment: &str) -> String {
  let (path, maybe_rest) = match link.split_once('#') {
    Some((path, rest)) => (path, Some(rest)),
    None => (link, None),
  };
  let mut parts: Vec<&str> = maybe_rest
    .into_iter()
    .flat_map(|rest| rest.split('&'))
    .filter(|part| {
      !part.is_empty()
        && !part.starts_with(VERIFIER_FRAGMENT_KEY)
        && !part.starts_with(LEGACY_ORGANIZER_FRAGMENT_KEY)
    })
    .collect();
  parts.push(fragment);
  format!("{path}#{}", parts.join("&"))
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
    link: &GuestLink,
    display_name: &str,
  ) -> Result<JoinMeetingResponse, MeetingError> {
    let maybe_link_token = match link.handle {
      MeetingHandle::Token(token) => Some(token.as_str().to_owned()),
      MeetingHandle::Id(_) => None,
    };
    let body = to_json(&JoinMeetingRequest {
      maybe_invite_token: link.maybe_invite_token.clone(),
      maybe_link_token,
      display_name: display_name.trim().to_owned(),
    })?;
    let response = self
      .request("POST", "/api/v1/meetings/join", None, Some(body))
      .await?;
    serde_json::from_str(&response.body).map_err(|error| MeetingError::Unreadable(error.to_string()))
  }

  /// Where the guest stands: waiting, admitted or denied.
  pub async fn lobby_state(
    &self,
    meeting: MeetingId,
    guest_token: &str,
  ) -> Result<LobbyStateResponse, MeetingError> {
    let path = format!("/api/v1/meetings/{meeting}/lobby/me");
    let response = self.request("GET", &path, Some(guest_token), None).await?;
    serde_json::from_str(&response.body).map_err(|error| MeetingError::Unreadable(error.to_string()))
  }

  /// Tells the hosts that the guest's device is set up, with its identity key fingerprint, which
  /// binds the key to this knock: an organizer's device admits only a key of this fingerprint.
  pub async fn device_ready(
    &self,
    meeting: MeetingId,
    guest_token: &str,
    fingerprint: &str,
  ) -> Result<(), MeetingError> {
    let body = to_json(&teamcraft_proto::GuestDeviceReadyRequest {
      fingerprint: fingerprint.to_owned(),
    })?;
    let path = format!("/api/v1/meetings/{meeting}/lobby/me/device");
    self.request("PUT", &path, Some(guest_token), Some(body)).await?;
    Ok(())
  }

  /// The guest leaves: the server ends their session.
  pub async fn leave(&self, meeting: MeetingId, guest_token: &str) -> Result<(), MeetingError> {
    let path = format!("/api/v1/meetings/{meeting}/leave");
    self
      .request("POST", &path, Some(guest_token), Some("{}".to_owned()))
      .await?;
    Ok(())
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

/// The i18n key that names a region of the holiday table (`settings-holiday-de-by`), or none
/// for the empty code.
pub fn holiday_region_key(code: &str) -> String {
  match code {
    "" => "settings-holiday-none".to_owned(),
    code => format!("settings-holiday-{}", code.to_lowercase()),
  }
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, MeetingError> {
  serde_json::to_string(value).map_err(|error| MeetingError::Unreadable(error.to_string()))
}

/// Whether `conversation` is a meeting's chat (#1932): the UI sees it as a channel outside every
/// space (the core's `view_kind`), and only meeting chats live outside spaces besides DMs and
/// group DMs (`Conversation::has_channel_settings`). Its call is the meeting's call, never a
/// huddle.
pub fn is_meeting_chat(conversation: &crate::model::Conversation) -> bool {
  use crate::model::ConversationKind;
  matches!(
    conversation.kind,
    ConversationKind::Public | ConversationKind::Private
  ) && conversation.maybe_space.is_none()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;
  use crate::route::LinkToken;

  const NOW: TimestampMs = 1_792_540_800_000;

  #[test]
  fn a_valid_form_becomes_a_request() {
    let mut form = filled_form();
    form.members = vec![id_from_raw(5)];
    form.externals = vec![ExternalInvitee {
      email: " guest@example.com ".into(),
      name: "Guest".into(),
    }];
    form.set_repeat(Repeat::Weekly);
    form.ends = SeriesEnd::AfterCount;
    form.count = 4;
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
    assert!(
      recurrence.weekdays.is_empty(),
      "a weekly series on its start's day names no days"
    );
    assert!(request.has_lobby);
  }

  mod repeat {
    use super::*;

    #[test]
    fn a_daily_series_on_work_days_names_them() {
      let mut form = monday_form();
      form.set_repeat(Repeat::Daily);
      form.weekdays = [true, true, true, true, true, false, false];
      let recurrence = form.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!(recurrence.weekdays, WEEKDAYS[..5].to_vec());
      form.weekdays = [true; 7];
      let every_day = form.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert!(every_day.weekdays.is_empty());
    }

    #[test]
    fn a_weekly_series_starts_on_its_start_day_and_can_add_others() {
      let mut form = monday_form();
      form.set_repeat(Repeat::Weekly);
      assert_eq!(form.weekdays, [true, false, false, false, false, false, false]);
      form.weekdays[3] = true;
      let recurrence = form.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!(recurrence.weekdays, vec![Weekday::Monday, Weekday::Thursday]);
    }

    #[test]
    fn a_series_ends_after_a_count_or_at_the_end_of_a_day() {
      let mut form = monday_form();
      form.set_repeat(Repeat::Daily);
      form.ends = SeriesEnd::OnDate;
      form.until = "2026-11-20".into();
      let until = form
        .to_request(NOW)
        .unwrap()
        .maybe_recurrence
        .unwrap()
        .maybe_until
        .unwrap();
      assert_eq!(
        until.0,
        parse_local_datetime("2026-11-20T23:59").unwrap() + 59_000
      );
      form.ends = SeriesEnd::AfterCount;
      form.count = 3;
      let counted = form.to_request(NOW).unwrap().maybe_recurrence.unwrap();
      assert_eq!((counted.maybe_count, counted.maybe_until), (Some(3), None));
    }

    #[test]
    fn a_saved_series_comes_back_into_the_form_and_is_not_sent_unchanged() {
      let mut form = monday_form();
      form.set_repeat(Repeat::Daily);
      form.weekdays = [true, true, true, true, true, false, false];
      form.ends = SeriesEnd::AfterCount;
      form.count = 20;
      let request = form.to_request(NOW).unwrap();
      let mut saved = fixture_meeting(id_from_raw(1));
      saved.starts_at = request.starts_at;
      saved.ends_at = request.ends_at;
      saved.maybe_recurrence = request.maybe_recurrence.clone();
      let again = ScheduleForm::from_meeting(&saved, id_from_raw(1));
      assert_eq!(
        (again.repeat, again.weekdays, again.ends, again.count),
        (form.repeat, form.weekdays, form.ends, form.count)
      );
      assert_eq!(
        again.to_update_request(NOW, &saved).unwrap().maybe_recurrence,
        None
      );
    }

    /// A Monday, 10:00 local time, well after `NOW`.
    fn monday_form() -> ScheduleForm {
      let mut form = filled_form();
      form.starts_at = "2026-11-09T10:00".into();
      form
    }
  }

  #[test]
  fn the_form_refuses_what_the_server_would() {
    type Edit = fn(&mut ScheduleForm);
    let cases: [(Edit, FormError); 11] = [
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
          form.set_repeat(Repeat::Daily);
          form.weekdays = [false; 7];
        },
        FormError::NoWeekday,
      ),
      (
        |form| {
          form.set_repeat(Repeat::Weekly);
          form.weekdays = [false; 7];
          form.weekdays[(form.start_weekday().unwrap() + 1) % 7] = true;
        },
        FormError::StartOffWeekdays,
      ),
      (
        |form| {
          form.set_repeat(Repeat::Monthly);
          form.ends = SeriesEnd::AfterCount;
          form.count = 0;
        },
        FormError::InvalidCount,
      ),
      (
        |form| {
          form.set_repeat(Repeat::Monthly);
          form.ends = SeriesEnd::OnDate;
        },
        FormError::InvalidUntil,
      ),
      (
        |form| {
          form.set_repeat(Repeat::Monthly);
          form.ends = SeriesEnd::OnDate;
          form.until = "2020-01-01".into();
        },
        FormError::UntilBeforeStart,
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
      assert_eq!(form.to_request(NOW), Err(vec![expected]));
    }
  }

  #[test]
  fn every_error_is_reported_at_once_in_field_order() {
    let mut form = filled_form();
    form.title = String::new();
    form.duration_minutes = 0;
    let errors = form.to_request(NOW).unwrap_err();
    assert_eq!(errors, vec![FormError::MissingTitle, FormError::InvalidDuration]);
    assert_eq!(errors[0].field().id(), "meeting-title");
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

  const TOKEN: &str = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";

  #[test]
  fn meeting_links_are_read_from_the_path_and_the_fragment() {
    let link = parse_guest_link(&format!("/meeting/{TOKEN}"), "#v=ab12").expect("link");
    assert_eq!(
      link.handle,
      MeetingHandle::Token(LinkToken::parse(TOKEN).unwrap())
    );
    assert_eq!(link.maybe_invite_token, None);
    assert_eq!(link.maybe_organizer_fingerprint.as_deref(), Some("ab12"));
    // Without a fragment it is still a guest link: the guest waits in the lobby.
    let bare = parse_guest_link(&format!("/meeting/{TOKEN}"), "").expect("link");
    assert_eq!(bare.maybe_organizer_fingerprint, None);
    let one_time = parse_guest_link(&format!("/meeting/{TOKEN}"), "#invite=abc.def&v=ab12").expect("link");
    assert_eq!(
      (
        one_time.maybe_invite_token.as_deref(),
        one_time.maybe_organizer_fingerprint.as_deref()
      ),
      (Some("abc.def"), Some("ab12"))
    );
  }

  #[test]
  fn old_links_with_the_id_are_guest_links_only_with_their_one_time_token() {
    let old = parse_guest_link("/meeting/123456789", "#invite=abc.def").expect("link");
    assert_eq!(old.handle, MeetingHandle::Id("123456789".parse().unwrap()));
    assert_eq!(old.maybe_organizer_fingerprint, None);
    // An older app's link: `o` is the fingerprint, the secret `k` is ignored.
    let older = parse_guest_link("/meeting/123456789", "#invite=abc.def&k=s3cret&o=ab12").expect("link");
    assert_eq!(older.maybe_organizer_fingerprint.as_deref(), Some("ab12"));
    assert!(parse_guest_link("/meeting/123456789", "").is_none());
    assert!(parse_guest_link("/meeting/123456789", "#invite=").is_none());
    assert!(parse_guest_link("/meeting/not-a-token", "#invite=abc").is_none());
    assert!(parse_guest_link("/threads", "#invite=abc").is_none());
    assert!(parse_guest_link("/meeting/123456789/", "#x=1&invite=abc").is_some());
  }

  #[test]
  fn a_guests_own_address_carries_the_resume_key() {
    let by_token = parse_guest_link(&format!("/meeting/{TOKEN}"), "#r=k3y").expect("link");
    assert_eq!(
      by_token.maybe_resume_key.as_deref().map(String::as_str),
      Some("k3y")
    );
    assert!(!format!("{by_token:?}").contains("k3y"));
    // A personal link's address after joining: the ID with the key, the one-time token gone.
    let by_id = parse_guest_link("/meeting/123456789", "#r=k3y").expect("link");
    assert_eq!(by_id.maybe_invite_token, None);
    assert_eq!(by_id.maybe_resume_key.as_deref().map(String::as_str), Some("k3y"));
    assert!(parse_guest_link("/meeting/123456789", "#r=").is_none());
  }

  #[test]
  fn the_copying_devices_fingerprint_replaces_the_servers() {
    let base = format!("https://chat.example.com/meeting/{TOKEN}");
    assert_eq!(with_verifier(&base, "v=11"), format!("{base}#v=11"));
    assert_eq!(
      with_verifier(&format!("{base}#v=22"), "v=11"),
      format!("{base}#v=11")
    );
    assert_eq!(
      with_verifier(&format!("{base}#invite=abc&v=22"), "v=11"),
      format!("{base}#invite=abc&v=11")
    );
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
      maybe_space: None,
      join_url: String::new(),
      maybe_recurrence: None,
      is_occurrence_cancelled: false,
      is_occurrence_holiday: false,
      can_host_lobby: false,
      co_host_user_ids: Vec::new(),
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

  #[test]
  fn a_chat_admin_the_list_marks_hosts_the_lobby_outside_the_call() {
    let admin: UserId = id_from_raw(2);
    let mut meeting = fixture_meeting(id_from_raw(1));
    assert!(!hosts_lobby(&meeting, admin), "older servers send no flag");
    meeting.can_host_lobby = true;
    assert!(hosts_lobby(&meeting, admin));
    meeting.status = teamcraft_proto::MeetingStatus::Cancelled;
    assert!(!hosts_lobby(&meeting, admin));
  }

  #[test]
  fn a_meeting_without_the_lobby_flag_reads_as_not_hosted() {
    let mut json = serde_json::to_value(fixture_meeting(id_from_raw(1))).unwrap();
    assert!(json.get("can_host_lobby").is_none(), "false is left out");
    json["can_host_lobby"] = serde_json::Value::Bool(true);
    let meeting: MeetingResponse = serde_json::from_value(json).unwrap();
    assert!(meeting.can_host_lobby);
  }

  #[test]
  fn an_edit_sends_the_whole_invitee_list_and_the_time_only_when_it_moved() {
    let organizer = id_from_raw(1);
    let mut before = fixture_meeting(organizer);
    before.attendees = vec![
      attendee(Some(id_from_raw(1)), None),
      attendee(Some(id_from_raw(2)), None),
      attendee(Some(id_from_raw(3)), None),
    ];
    let mut form = ScheduleForm::from_meeting(&before, organizer);
    assert_eq!(form.members, vec![id_from_raw(2), id_from_raw(3)]);
    form.members = vec![id_from_raw(3), id_from_raw(4)];
    let request = form.to_update_request(NOW, &before).expect("valid");
    assert_eq!(request.maybe_starts_at, None, "an unmoved time is not sent");
    let (invitees, removed) = invitee_change(&before, &request).expect("invitees sent");
    assert_eq!(invitees, vec![id_from_raw(3), id_from_raw(4)]);
    assert_eq!(removed, vec![id_from_raw(2)]);
    form.duration_minutes += 30;
    let request = form.to_update_request(NOW, &before).expect("valid");
    assert!(request.maybe_ends_at.is_some());
  }

  #[test]
  fn a_meeting_under_way_can_be_edited_but_not_moved_into_the_past() {
    let mut before = fixture_meeting(id_from_raw(1));
    before.starts_at = UnixMillis(NOW - 10 * MS_PER_MINUTE);
    before.ends_at = UnixMillis(NOW + 20 * MS_PER_MINUTE);
    let mut form = ScheduleForm::from_meeting(&before, id_from_raw(1));
    form.title = "Renamed".into();
    assert!(form.to_update_request(NOW, &before).is_ok());
    form.starts_at = crate::clock::format_local_datetime_input(NOW - 60 * MS_PER_MINUTE);
    assert_eq!(
      form.to_update_request(NOW, &before),
      Err(vec![FormError::StartInThePast])
    );
  }

  fn fixture_meeting(organizer: UserId) -> MeetingResponse {
    MeetingResponse {
      id: id_from_raw(7),
      title: "Planning".into(),
      maybe_description: None,
      starts_at: UnixMillis(NOW + 2 * 3_600_000 + 17_000),
      ends_at: UnixMillis(NOW + 2 * 3_600_000 + 17_000 + 30 * MS_PER_MINUTE),
      kind: teamcraft_proto::MeetingKind::Scheduled,
      status: teamcraft_proto::MeetingStatus::Scheduled,
      has_lobby: true,
      organizer_user_id: organizer,
      conversation_id: id_from_raw(9),
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

  fn attendee(
    maybe_user_id: Option<UserId>,
    maybe_email: Option<String>,
  ) -> teamcraft_proto::MeetingAttendeeResponse {
    teamcraft_proto::MeetingAttendeeResponse {
      maybe_user_id,
      maybe_email,
      maybe_name: None,
      rsvp: RsvpStatus::NeedsAction,
    }
  }

  mod notetaker {
    use teamcraft_proto::Id;

    use super::*;

    #[test]
    fn a_ticked_notetaker_is_invited_like_a_member() {
      let mut form = filled_form();
      let bot = UserId::from_snowflake(teamcraft_proto::Snowflake::new(99).unwrap());
      assert!(form.to_request(NOW).unwrap().invitees.is_empty());
      form.maybe_notetaker = Some(bot);
      let invitees = form.to_request(NOW).unwrap().invitees;
      assert_eq!(invitees, [MeetingInvitee::User { user_id: bot }]);
    }

    #[test]
    fn adding_the_notetaker_during_a_call_keeps_everybody_else_invited() {
      let me = UserId::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap());
      let alice = UserId::from_snowflake(teamcraft_proto::Snowflake::new(5).unwrap());
      let bot = UserId::from_snowflake(teamcraft_proto::Snowflake::new(99).unwrap());
      let mut meeting = fixture_meeting(me);
      let mut guest = attendee(None, Some("guest@example.com".into()));
      guest.maybe_name = Some("Guest".into());
      meeting.attendees = vec![attendee(Some(me), None), attendee(Some(alice), None), guest];
      let request = adding_invitee(&meeting, me, bot);
      assert_eq!(
        request.maybe_invitees,
        Some(vec![
          MeetingInvitee::User { user_id: alice },
          MeetingInvitee::User { user_id: bot },
          MeetingInvitee::External {
            email: "guest@example.com".into(),
            maybe_name: Some("Guest".into()),
          },
        ])
      );
      assert_eq!(request.maybe_title, None, "nothing else changes");
      assert_eq!(request.maybe_starts_at, None);
    }

    /// #1141: the organizer adds people during the meeting's call; those invited already, the
    /// organizer and a second pick of the same person are no additions.
    #[test]
    fn adding_people_during_a_call_keeps_the_list_and_adds_each_once() {
      let me = UserId::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap());
      let alice = UserId::from_snowflake(teamcraft_proto::Snowflake::new(5).unwrap());
      let bob = UserId::from_snowflake(teamcraft_proto::Snowflake::new(6).unwrap());
      let carol = UserId::from_snowflake(teamcraft_proto::Snowflake::new(7).unwrap());
      let mut meeting = fixture_meeting(me);
      meeting.attendees = vec![attendee(Some(me), None), attendee(Some(alice), None)];
      let request = adding_invitees(&meeting, me, &[bob, alice, me, carol, bob]);
      assert_eq!(
        request.maybe_invitees,
        Some(vec![
          MeetingInvitee::User { user_id: bob },
          MeetingInvitee::User { user_id: alice },
          MeetingInvitee::User { user_id: carol },
        ])
      );
      assert_eq!(request.maybe_title, None, "nothing else changes");
    }

    #[test]
    fn an_attending_notetaker_is_the_box_not_a_member() {
      let bot = UserId::from_snowflake(teamcraft_proto::Snowflake::new(99).unwrap());
      let alice = UserId::from_snowflake(teamcraft_proto::Snowflake::new(5).unwrap());
      let mut form = filled_form();
      form.members = vec![alice, bot];
      let form = form.with_notetaker(Some(bot));
      assert_eq!(
        (form.members.as_slice(), form.maybe_notetaker),
        (&[alice][..], Some(bot))
      );
      let request = form.to_request(NOW).unwrap();
      assert_eq!(request.invitees.len(), 2);
    }
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
