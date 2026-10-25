//! Calls: which call a conversation has, and who may join it (ARCHITECTURE.md §6.3).
//!
//! The server decides access and hands out short-lived join tokens (EdDSA-signed JWTs with
//! [`CallJoinClaims`]); the SFU verifies them with the server's public key and never asks the
//! server. Active calls are tracked in memory: a conversation keeps its call id while tokens are
//! issued, and gets a new one after `call_idle_secs` without any.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::ExposeSecret;
use subtle::ConstantTimeEq;
use teamcraft_proto::{
  CALL_JOIN_TOKEN_AUDIENCE, CallId, CallJoinClaims, CallJoinTokenResponse, CallRosterEntry,
  CallStateResponse, ConversationId, ConversationKind, CreateCallJoinTokenRequest, DeviceId, GatewayEvent,
  SfuControlAction, SfuControlRequest, SfuParticipantReport, UnixMillis, UserId,
};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::time::sleep;
use tracing::{info, warn};

use super::access::load_conversation;
use crate::auth::{AuthError, AuthenticatedUser};
use crate::config::CallsConfig;
use crate::error::ApiError;
use crate::state::AppState;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// Signs join tokens and tracks the active call of each conversation.
pub struct CallService {
  signing_key: EncodingKey,
  issuer: String,
  sfu_url: String,
  /// The SFU certificates' SHA-256s (lowercase hex) for browsers, when they are self-signed.
  sfu_certificate_sha256: Vec<String>,
  token_ttl: Duration,
  call_idle_timeout: Duration,
  ring_timeout: Duration,
  /// Token the SFU reports participants with; `None` disables the reports.
  maybe_sfu_token: Option<Vec<u8>>,
  active: Mutex<HashMap<ConversationId, ActiveCall>>,
  /// Meetings the host locked, and people the host removed, until the call ends.
  controls: Mutex<HostControls>,
  /// Where host controls go.
  maybe_control: Option<ControlTarget>,
  /// When the SFU last presented its registration token (Unix milliseconds, 0: never).
  last_sfu_contact_ms: AtomicI64,
}

#[derive(Default)]
struct HostControls {
  locked: HashSet<ConversationId>,
  removed: HashSet<(ConversationId, UserId)>,
}

struct ControlTarget {
  url: String,
  client: reqwest::Client,
}

#[derive(Debug, Clone)]
struct ActiveCall {
  call_id: CallId,
  last_token_at: Instant,
  /// Set when the first participant joined (reported by the SFU).
  maybe_started: Option<Started>,
  participants: Vec<CallRosterEntry>,
  is_ringing: bool,
}

#[derive(Debug, Clone, Copy)]
struct Started {
  by: UserId,
  at: UnixMillis,
}

/// The roster of a running call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSnapshot {
  pub conversation_id: ConversationId,
  pub call_id: CallId,
  pub started_by: UserId,
  pub started_at: UnixMillis,
  pub is_ringing: bool,
  pub participants: Vec<CallRosterEntry>,
}

/// What a join report changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinChange {
  /// Already in the roster; nothing to tell anybody.
  Unchanged,
  /// The first participant joined: the call starts (and rings if `snapshot.is_ringing`).
  Started(CallSnapshot),
  Updated(CallSnapshot),
}

/// What a leave report changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaveChange {
  Unchanged,
  Updated(CallSnapshot),
  Ended {
    conversation_id: ConversationId,
    call_id: CallId,
  },
}

#[derive(Debug, Error)]
pub enum CallsSetupError {
  #[error("reading the join token signing key {path}: {source}")]
  ReadKey { path: String, source: std::io::Error },
  #[error("the join token signing key is not an Ed25519 PEM private key: {0}")]
  InvalidKey(#[from] jsonwebtoken::errors::Error),
  #[error("setting up the SFU control client failed: {0}")]
  Control(String),
  #[error("sfu_certificate_sha256 must be 32 bytes as hex: {0:?}")]
  InvalidCertificateHash(String),
}

impl ActiveCall {
  fn snapshot(
    &self,
    conversation_id: ConversationId,
    started_by: UserId,
    started_at: UnixMillis,
  ) -> CallSnapshot {
    CallSnapshot {
      conversation_id,
      call_id: self.call_id,
      started_by,
      started_at,
      is_ringing: self.is_ringing,
      participants: self.participants.clone(),
    }
  }
}

impl CallService {
  pub fn from_config(config: &CallsConfig) -> Result<Self, CallsSetupError> {
    let pem = std::fs::read(&config.signing_key_path).map_err(|source| CallsSetupError::ReadKey {
      path: config.signing_key_path.display().to_string(),
      source,
    })?;
    Self::new(config, &pem)
  }

  pub fn new(config: &CallsConfig, signing_key_pem: &[u8]) -> Result<Self, CallsSetupError> {
    Ok(Self {
      signing_key: EncodingKey::from_ed_pem(signing_key_pem)?,
      issuer: config.issuer.clone(),
      sfu_url: config.sfu_url.clone(),
      sfu_certificate_sha256: config
        .maybe_sfu_certificate_sha256
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|hash| !hash.is_empty())
        .map(normalized_sha256)
        .collect::<Result<_, _>>()?,
      token_ttl: Duration::from_secs(config.join_token_ttl_secs),
      call_idle_timeout: Duration::from_secs(config.call_idle_secs),
      ring_timeout: Duration::from_secs(config.ring_timeout_secs),
      maybe_sfu_token: config
        .maybe_sfu_registration_token
        .as_ref()
        .map(|t| t.expose_secret().as_bytes().to_vec()),
      active: Mutex::new(HashMap::new()),
      controls: Mutex::new(HostControls::default()),
      last_sfu_contact_ms: AtomicI64::new(0),
      maybe_control: config
        .maybe_sfu_control_url
        .as_ref()
        .map(|url| {
          Ok::<_, reqwest::Error>(ControlTarget {
            url: format!("{}/internal/v1/calls/control", url.trim_end_matches('/')),
            client: reqwest::Client::builder().timeout(CONTROL_TIMEOUT).build()?,
          })
        })
        .transpose()
        .map_err(|error| CallsSetupError::Control(error.to_string()))?,
    })
  }

  /// The call id of the conversation's running call.
  pub fn running_call(&self, conversation_id: ConversationId) -> Option<CallId> {
    self.snapshot(conversation_id).map(|snapshot| snapshot.call_id)
  }

  pub fn sfu_url(&self) -> &str {
    &self.sfu_url
  }

  pub fn is_reporting_enabled(&self) -> bool {
    self.maybe_sfu_token.is_some()
  }

  /// When the SFU last reported to the server.
  pub fn last_sfu_contact(&self) -> Option<UnixMillis> {
    match self.last_sfu_contact_ms.load(Ordering::Relaxed) {
      0 => None,
      millis => Some(UnixMillis(millis)),
    }
  }

  pub fn active_call_count(&self) -> usize {
    self.active.lock().unwrap_or_else(PoisonError::into_inner).len()
  }

  /// Whether host controls can reach the SFU.
  pub fn has_controls(&self) -> bool {
    self.maybe_control.is_some()
  }

  pub fn is_locked(&self, conversation_id: ConversationId) -> bool {
    self.controls().locked.contains(&conversation_id)
  }

  pub fn is_removed(&self, conversation_id: ConversationId, user: UserId) -> bool {
    self.controls().removed.contains(&(conversation_id, user))
  }

  pub fn set_locked(&self, conversation_id: ConversationId, is_locked: bool) {
    let mut controls = self.controls();
    if is_locked {
      controls.locked.insert(conversation_id);
    } else {
      controls.locked.remove(&conversation_id);
    }
  }

  pub fn mark_removed(&self, conversation_id: ConversationId, user: UserId) {
    self.controls().removed.insert((conversation_id, user));
  }

  fn forget_controls(&self, conversation_id: ConversationId) {
    let mut controls = self.controls();
    controls.locked.remove(&conversation_id);
    controls
      .removed
      .retain(|(conversation, _)| *conversation != conversation_id);
  }

  fn controls(&self) -> std::sync::MutexGuard<'_, HostControls> {
    self.controls.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Tells the SFU what the host wants (wire format: `POST /internal/v1/calls/control`).
  pub async fn send_control(&self, request: &SfuControlRequest) -> Result<(), ApiError> {
    let target = self
      .maybe_control
      .as_ref()
      .ok_or_else(|| ApiError::Unavailable("host controls are not configured".into()))?;
    let token = self
      .maybe_sfu_token
      .as_deref()
      .ok_or_else(|| ApiError::Unavailable("host controls need the SFU registration token".into()))?;
    let response = target
      .client
      .post(&target.url)
      .bearer_auth(String::from_utf8_lossy(token))
      .json(request)
      .send()
      .await
      .map_err(|error| ApiError::Unavailable(format!("the SFU is not reachable: {error}")))?;
    match response.status() {
      status if status.is_success() => Ok(()),
      reqwest::StatusCode::NOT_FOUND => Err(ApiError::call_not_found()),
      status => Err(ApiError::Unavailable(format!(
        "the SFU answered with status {status}"
      ))),
    }
  }

  /// The conversation's current call, or a new one from `new_call_id`.
  fn call_for(
    &self,
    conversation_id: ConversationId,
    now: Instant,
    new_call_id: impl FnOnce() -> Result<CallId, ApiError>,
  ) -> Result<CallId, ApiError> {
    let mut active = self.lock();
    // Calls with participants are alive whatever the token age says.
    active.retain(|_, call| {
      !call.participants.is_empty()
        || now.saturating_duration_since(call.last_token_at) < self.call_idle_timeout
    });
    if let Some(call) = active.get_mut(&conversation_id) {
      call.last_token_at = now;
      return Ok(call.call_id);
    }
    let call_id = new_call_id()?;
    active.insert(
      conversation_id,
      ActiveCall {
        call_id,
        last_token_at: now,
        maybe_started: None,
        participants: Vec::new(),
        is_ringing: false,
      },
    );
    Ok(call_id)
  }

  /// Whether `presented` is the SFU's registration token (constant time).
  fn is_sfu_token(&self, presented: &[u8]) -> Result<bool, ApiError> {
    let expected = self
      .maybe_sfu_token
      .as_deref()
      .ok_or_else(|| ApiError::Unavailable("SFU reports are not configured".into()))?;
    Ok(bool::from(expected.ct_eq(presented)))
  }

  fn conversation_of(&self, call_id: CallId) -> Option<ConversationId> {
    let active = self.lock();
    active
      .iter()
      .find(|(_, call)| call.call_id == call_id)
      .map(|(conversation_id, _)| *conversation_id)
  }

  /// Records a participant. The first one starts the call (ringing if `rings`); a joiner other
  /// than the starter stops the ringing.
  fn participant_joined(
    &self,
    conversation_id: ConversationId,
    entry: CallRosterEntry,
    rings: bool,
    now: UnixMillis,
  ) -> JoinChange {
    let mut active = self.lock();
    let Some(call) = active.get_mut(&conversation_id) else {
      return JoinChange::Unchanged;
    };
    if call.participants.contains(&entry) {
      return JoinChange::Unchanged;
    }
    call.participants.push(entry);
    match call.maybe_started {
      None => {
        call.maybe_started = Some(Started {
          by: entry.user_id,
          at: now,
        });
        call.is_ringing = rings;
        JoinChange::Started(call.snapshot(conversation_id, entry.user_id, now))
      },
      Some(started) => {
        if call.is_ringing && entry.user_id != started.by {
          call.is_ringing = false;
        }
        JoinChange::Updated(call.snapshot(conversation_id, started.by, started.at))
      },
    }
  }

  /// Removes a participant; the last one leaving ends the call.
  fn participant_left(&self, conversation_id: ConversationId, entry: CallRosterEntry) -> LeaveChange {
    let mut active = self.lock();
    let Some(call) = active.get_mut(&conversation_id) else {
      return LeaveChange::Unchanged;
    };
    let Some(position) = call.participants.iter().position(|known| *known == entry) else {
      return LeaveChange::Unchanged;
    };
    call.participants.remove(position);
    if call.participants.is_empty() {
      let call_id = call.call_id;
      active.remove(&conversation_id);
      drop(active);
      self.forget_controls(conversation_id);
      return LeaveChange::Ended {
        conversation_id,
        call_id,
      };
    }
    match call.maybe_started {
      Some(started) => LeaveChange::Updated(call.snapshot(conversation_id, started.by, started.at)),
      None => LeaveChange::Unchanged,
    }
  }

  /// Ends the ringing of `call_id` if it still rings.
  fn stop_ringing(&self, conversation_id: ConversationId, call_id: CallId) -> Option<CallSnapshot> {
    let mut active = self.lock();
    let call = active.get_mut(&conversation_id)?;
    if call.call_id != call_id || !call.is_ringing {
      return None;
    }
    call.is_ringing = false;
    let started = call.maybe_started?;
    Some(call.snapshot(conversation_id, started.by, started.at))
  }

  /// Whether `call_id` is the conversation's call and still rings. Calls live in memory, so after
  /// a restart nothing rings any more.
  pub fn is_ringing(&self, conversation_id: ConversationId, call_id: CallId) -> bool {
    self
      .lock()
      .get(&conversation_id)
      .is_some_and(|call| call.call_id == call_id && call.is_ringing)
  }

  /// Whether the device is in the roster of the conversation's call. The SFU reports a device
  /// whose connection was lost as gone only after its reconnect grace period, so this holds while
  /// it rejoins.
  pub fn is_device_in_call(&self, conversation_id: ConversationId, device_id: DeviceId) -> bool {
    self
      .lock()
      .get(&conversation_id)
      .is_some_and(|call| call.participants.iter().any(|entry| entry.device_id == device_id))
  }

  /// Whether a device of the user is in the roster of a running call.
  pub fn is_in_call(&self, user: UserId) -> bool {
    self
      .lock()
      .values()
      .any(|call| call.participants.iter().any(|entry| entry.user_id == user))
  }

  /// The running call of the conversation: one with at least one participant.
  fn snapshot(&self, conversation_id: ConversationId) -> Option<CallSnapshot> {
    let active = self.lock();
    let call = active.get(&conversation_id)?;
    let started = call.maybe_started?;
    Some(call.snapshot(conversation_id, started.by, started.at))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConversationId, ActiveCall>> {
    // No invariant spans a panic point, so a poisoned map is still consistent.
    self.active.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Claims for one device, valid from `now_unix_secs` for the configured lifetime.
  fn claims(
    &self,
    call_id: CallId,
    user_id: UserId,
    device_id: DeviceId,
    now_unix_secs: u64,
  ) -> CallJoinClaims {
    CallJoinClaims {
      iss: self.issuer.clone(),
      aud: CALL_JOIN_TOKEN_AUDIENCE.to_owned(),
      iat: now_unix_secs,
      exp: now_unix_secs + self.token_ttl.as_secs(),
      call_id,
      user_id,
      device_id,
    }
  }

  fn sign(&self, claims: &CallJoinClaims) -> Result<String, ApiError> {
    jsonwebtoken::encode(&Header::new(Algorithm::EdDSA), claims, &self.signing_key)
      .map_err(|error| ApiError::Internal(format!("signing a join token failed: {error}")))
  }
}

/// Issues a join token for the conversation's call. Only members may join, with one of their own
/// active devices.
pub async fn create_join_token(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
  request: CreateCallJoinTokenRequest,
) -> Result<CallJoinTokenResponse, ApiError> {
  let calls = state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))?;
  let access = load_conversation(state, caller, conversation_id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can join the call".into()));
  }
  if access.view.conversation.is_archived {
    return Err(ApiError::Forbidden("the conversation is archived".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }

  if access.view.conversation.kind == ConversationKind::Meeting {
    check_meeting_admission(state, calls, caller, conversation_id, request.device_id).await?;
  }
  let call_id = calls.call_for(conversation_id, Instant::now(), || state.next_id())?;
  let claims = calls.claims(call_id, caller.user.id, request.device_id, unix_now_secs()?);
  Ok(CallJoinTokenResponse {
    call_id,
    token: calls.sign(&claims)?,
    sfu_url: calls.sfu_url.clone(),
    expires_at: UnixMillis(i64::try_from(claims.exp).unwrap_or(i64::MAX).saturating_mul(1000)),
    sfu_certificate_sha256: calls.sfu_certificate_sha256.clone(),
  })
}

/// A SHA-256 written as hex, with or without colons, as 64 lowercase hex digits.
fn normalized_sha256(text: &str) -> Result<String, CallsSetupError> {
  let hex: String = text
    .chars()
    .filter(|c| *c != ':')
    .map(|c| c.to_ascii_lowercase())
    .collect();
  if hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
    Ok(hex)
  } else {
    Err(CallsSetupError::InvalidCertificateHash(text.to_owned()))
  }
}

/// A meeting that is over, a lock and removals by the host keep people out of its call. A lock
/// doesn't keep out a device that is in the call already: it rejoins after a lost connection.
async fn check_meeting_admission(
  state: &AppState,
  calls: &CallService,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
  device_id: DeviceId,
) -> Result<(), ApiError> {
  let Some(meeting) = state
    .database
    .get_meeting_by_conversation(conversation_id)
    .await?
  else {
    return Ok(());
  };
  if meeting.status != teamcraft_proto::MeetingStatus::Scheduled.as_str() {
    return Err(ApiError::Conflict("the meeting is over".into()));
  }
  if calls.is_removed(conversation_id, caller.user.id) {
    return Err(ApiError::Forbidden(
      "the host removed you from the meeting".into(),
    ));
  }
  if calls.is_locked(conversation_id)
    && !calls.is_device_in_call(conversation_id, device_id)
    && !state
      .database
      .meeting_host_ids(&meeting)
      .await?
      .contains(&caller.user.id)
  {
    return Err(ApiError::Forbidden("the meeting is locked".into()));
  }
  Ok(())
}

/// The call running in the conversation, for members.
pub async fn get_call(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
) -> Result<CallStateResponse, ApiError> {
  let calls = calls_of(state)?;
  let access = load_conversation(state, caller, conversation_id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can see the call".into()));
  }
  calls
    .snapshot(conversation_id)
    .map(state_response)
    .ok_or_else(ApiError::call_not_found)
}

/// Checks the SFU's bearer token for the internal endpoints.
pub fn authenticate_sfu(state: &AppState, presented: Option<&str>) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let presented = presented.ok_or(AuthError::MissingToken)?;
  if calls.is_sfu_token(presented.as_bytes())? {
    let now = OffsetDateTime::now_utc().unix_timestamp().saturating_mul(1000);
    calls.last_sfu_contact_ms.store(now, Ordering::Relaxed);
    Ok(())
  } else {
    Err(AuthError::InvalidToken("not the SFU registration token".into()).into())
  }
}

/// The SFU reports that a device joined. Starts the call (and rings DMs) for the first one.
pub async fn report_joined(state: &AppState, report: SfuParticipantReport) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let conversation_id = calls
    .conversation_of(report.call_id)
    .ok_or_else(ApiError::call_not_found)?;
  let conversation = state
    .database
    .get_conversation(conversation_id, report.user_id)
    .await?
    .ok_or_else(ApiError::call_not_found)?;
  let rings = matches!(
    conversation.conversation.kind,
    ConversationKind::Dm | ConversationKind::Group
  );
  let entry = CallRosterEntry {
    user_id: report.user_id,
    device_id: report.device_id,
  };
  let now = UnixMillis(
    unix_now_secs()?
      .saturating_mul(1000)
      .try_into()
      .unwrap_or(i64::MAX),
  );
  let change = calls.participant_joined(conversation_id, entry, rings, now);
  let members = state.database.conversation_member_ids(conversation_id).await?;
  record_join(state, &change, report.user_id).await;
  match change {
    JoinChange::Unchanged => {},
    JoinChange::Started(snapshot) => {
      info!(conversation_id = %conversation_id, call_id = %snapshot.call_id, "call started");
      state.hub.publish(
        members.iter().copied(),
        &GatewayEvent::CallStart {
          conversation_id,
          call_id: snapshot.call_id,
          started_by: snapshot.started_by,
          started_at: snapshot.started_at,
          is_ringing: snapshot.is_ringing,
          participants: snapshot.participants.clone(),
        },
      );
      if snapshot.is_ringing {
        state.hub.publish(
          members
            .iter()
            .copied()
            .filter(|member| *member != snapshot.started_by),
          &GatewayEvent::CallRing {
            conversation_id,
            call_id: snapshot.call_id,
            from_user_id: snapshot.started_by,
          },
        );
        tokio::spawn(crate::push::routing::notify_ring(
          state.clone(),
          conversation_id,
          snapshot.call_id,
          snapshot.started_by,
        ));
        spawn_ring_timeout(
          state.clone(),
          conversation_id,
          snapshot.call_id,
          calls.ring_timeout,
        );
      }
    },
    JoinChange::Updated(snapshot) => {
      if !snapshot.is_ringing {
        // Somebody answered: the other phones stop ringing.
        tokio::spawn(crate::push::routing::end_ring(state.clone(), snapshot.call_id));
      }
      publish_update(state, members, &snapshot);
    },
  }
  refresh_caller_presence(state, report.user_id).await;
  Ok(())
}

/// The SFU reports that a device left. The last one leaving ends the call.
pub async fn report_left(state: &AppState, report: SfuParticipantReport) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let conversation_id = calls
    .conversation_of(report.call_id)
    .ok_or_else(ApiError::call_not_found)?;
  let entry = CallRosterEntry {
    user_id: report.user_id,
    device_id: report.device_id,
  };
  let change = calls.participant_left(conversation_id, entry);
  if matches!(change, LeaveChange::Unchanged) {
    return Ok(());
  }
  if let LeaveChange::Ended { call_id, .. } = &change {
    tokio::spawn(crate::push::routing::end_ring(state.clone(), *call_id));
    finish_history(state, *call_id).await;
  }
  let members = state.database.conversation_member_ids(conversation_id).await?;
  match change {
    LeaveChange::Unchanged => {},
    LeaveChange::Updated(snapshot) => publish_update(state, members, &snapshot),
    LeaveChange::Ended {
      conversation_id,
      call_id,
    } => {
      info!(conversation_id = %conversation_id, call_id = %call_id, "call ended");
      state.hub.publish(
        members,
        &GatewayEvent::CallEnd {
          conversation_id,
          call_id,
        },
      );
    },
  }
  refresh_caller_presence(state, report.user_id).await;
  Ok(())
}

/// Being in a call is part of a user's presence ("in a call"); tell the people who see it.
async fn refresh_caller_presence(state: &AppState, user: UserId) {
  if let Err(error) = super::presence::refresh_presence(state, user).await {
    warn!(%error, "refreshing presence after a call report failed");
  }
}

/// Drops `user` from the conversation's running call, if any. Called when the user loses access
/// to the conversation: a join token alone can't be recalled, so the SFU is told to close the
/// session. Best effort; the SFU's token lifetime bounds a failure (security-review C9).
pub async fn evict_from_call(state: &AppState, conversation_id: ConversationId, user: UserId) {
  let Some(calls) = state.maybe_calls.as_ref() else {
    return;
  };
  let Some(call_id) = calls.running_call(conversation_id) else {
    return;
  };
  if !calls.has_controls() {
    return;
  }
  let request = SfuControlRequest {
    call_id,
    by_user_id: user,
    action: SfuControlAction::RemoveParticipant { user_id: user },
  };
  if let Err(error) = calls.send_control(&request).await {
    warn!(%conversation_id, %error, "evicting a removed member from the call failed");
  }
}

fn calls_of(state: &AppState) -> Result<&Arc<CallService>, ApiError> {
  state
    .maybe_calls
    .as_ref()
    .ok_or_else(|| ApiError::Unavailable("calls are not configured".into()))
}

fn publish_update(state: &AppState, members: Vec<UserId>, snapshot: &CallSnapshot) {
  state.hub.publish(
    members,
    &GatewayEvent::CallUpdate {
      conversation_id: snapshot.conversation_id,
      call_id: snapshot.call_id,
      is_ringing: snapshot.is_ringing,
      participants: snapshot.participants.clone(),
    },
  );
}

/// Writes the call into the history the first time we hear of it, and who took part.
async fn record_join(state: &AppState, change: &JoinChange, user: UserId) {
  let snapshot = match change {
    JoinChange::Unchanged => return,
    JoinChange::Started(snapshot) => {
      let started_at =
        super::convert::offset_date_time(snapshot.started_at).unwrap_or_else(OffsetDateTime::now_utc);
      if let Err(error) = state
        .database
        .start_call_record(
          snapshot.call_id,
          snapshot.conversation_id,
          snapshot.started_by,
          started_at,
          snapshot.is_ringing,
        )
        .await
      {
        warn!(%error, "recording the call failed");
        return;
      }
      snapshot
    },
    JoinChange::Updated(snapshot) => snapshot,
  };
  if let Err(error) = state
    .database
    .record_call_participant(snapshot.call_id, user)
    .await
  {
    warn!(%error, "recording a call participant failed");
  }
}

/// Closes the history entry; a ringing call that nobody answered is a missed call.
async fn finish_history(state: &AppState, call_id: CallId) {
  if let Err(error) = state
    .database
    .finish_call_record(call_id, OffsetDateTime::now_utc())
    .await
  {
    warn!(%error, "closing the call record failed");
  }
  report_missed(state, call_id).await;
}

/// Tells the people who were rung and did not join, once per call.
async fn report_missed(state: &AppState, call_id: CallId) {
  let missed = match state.database.claim_missed_call(call_id).await {
    Ok(Some(missed)) => missed,
    Ok(None) => return,
    Err(error) => {
      warn!(%error, "looking for a missed call failed");
      return;
    },
  };
  let members = match state.database.conversation_member_ids(missed.conversation).await {
    Ok(members) => members,
    Err(error) => {
      warn!(%error, "listing the members of a missed call failed");
      return;
    },
  };
  for member in members.into_iter().filter(|member| *member != missed.started_by) {
    super::system_events::emit(
      state,
      missed.conversation,
      teamcraft_proto::SystemEventKind::MissedCall,
      Some(missed.started_by),
      Some(member),
    )
    .await;
  }
}

/// Stops the ringing after the timeout unless somebody joined (or the call ended) meanwhile.
fn spawn_ring_timeout(state: AppState, conversation_id: ConversationId, call_id: CallId, timeout: Duration) {
  tokio::spawn(async move {
    sleep(timeout).await;
    let Ok(calls) = calls_of(&state) else { return };
    let Some(snapshot) = calls.stop_ringing(conversation_id, call_id) else {
      return;
    };
    crate::push::routing::end_ring(state.clone(), call_id).await;
    report_missed(&state, call_id).await;
    match state.database.conversation_member_ids(conversation_id).await {
      Ok(members) => publish_update(&state, members, &snapshot),
      Err(error) => warn!(%error, "could not publish the end of ringing"),
    }
  });
}

fn state_response(snapshot: CallSnapshot) -> CallStateResponse {
  CallStateResponse {
    call_id: snapshot.call_id,
    conversation_id: snapshot.conversation_id,
    started_by: snapshot.started_by,
    started_at: snapshot.started_at,
    is_ringing: snapshot.is_ringing,
    participants: snapshot.participants,
  }
}

fn unix_now_secs() -> Result<u64, ApiError> {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs())
    .map_err(|_| ApiError::Internal("system clock is before 1970".into()))
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use jsonwebtoken::{DecodingKey, Validation};
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  /// Throwaway Ed25519 key pair used only by these tests. It protects nothing.
  const TEST_SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
  const TEST_PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
  const NOW_UNIX_SECS: u64 = 1_800_000_000;

  #[test]
  fn the_sfu_certificate_hash_is_normalized_or_refused() {
    let dotted = vec!["AB"; 32].join(":");
    let mut config = config();
    config.maybe_sfu_certificate_sha256 = Some(format!("{dotted}, {}", "cd".repeat(32)));
    let service = CallService::new(&config, TEST_SIGNING_KEY_PEM.as_bytes()).unwrap();
    assert_eq!(
      service.sfu_certificate_sha256,
      vec!["ab".repeat(32), "cd".repeat(32)]
    );
    config.maybe_sfu_certificate_sha256 = Some("abc".into());
    assert!(matches!(
      CallService::new(&config, TEST_SIGNING_KEY_PEM.as_bytes()),
      Err(CallsSetupError::InvalidCertificateHash(_))
    ));
  }

  mod tokens {
    use super::*;

    #[test]
    fn signed_token_verifies_with_the_public_key() {
      let service = service();
      let claims = service.claims(id(7), id(4), id(5), NOW_UNIX_SECS);
      let token = service.sign(&claims).unwrap();

      let mut validation = Validation::new(Algorithm::EdDSA);
      validation.set_audience(&[CALL_JOIN_TOKEN_AUDIENCE]);
      validation.set_issuer(&["https://chat.example.com"]);
      validation.validate_exp = false;
      let decoded = jsonwebtoken::decode::<CallJoinClaims>(
        &token,
        &DecodingKey::from_ed_pem(TEST_PUBLIC_KEY_PEM.as_bytes()).unwrap(),
        &validation,
      )
      .unwrap();
      assert_eq!(decoded.claims, claims);
      assert_eq!(decoded.claims.exp - decoded.claims.iat, 60);
    }

    #[test]
    fn invalid_signing_key_is_rejected() {
      assert!(matches!(
        CallService::new(&config(), b"not a key"),
        Err(CallsSetupError::InvalidKey(_))
      ));
    }
  }

  mod active_calls {
    use super::*;

    #[test]
    fn conversation_keeps_its_call_while_tokens_are_issued() {
      let service = service();
      let start = Instant::now();
      let first = service.call_for(id(1), start, || Ok(id(100))).unwrap();
      let later = service
        .call_for(id(1), start + Duration::from_secs(3000), || Ok(id(101)))
        .unwrap();
      assert_eq!((first, later), (id(100), id(100)));
    }

    #[test]
    fn device_in_the_roster_counts_as_in_the_call() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();
      let entry = CallRosterEntry {
        user_id: id(4),
        device_id: id(5),
      };
      assert!(!service.is_device_in_call(id(1), id(5)));
      service.participant_joined(id(1), entry, false, UnixMillis(0));
      assert!(service.is_device_in_call(id(1), id(5)));
      assert!(!service.is_device_in_call(id(1), id(6)));
      assert!(!service.is_device_in_call(id(2), id(5)));
    }

    #[test]
    fn idle_call_is_replaced() {
      let service = service();
      let start = Instant::now();
      service.call_for(id(1), start, || Ok(id(100))).unwrap();
      let after_idle = service
        .call_for(id(1), start + Duration::from_secs(3600), || Ok(id(101)))
        .unwrap();
      assert_eq!(after_idle, id(101));
    }

    #[test]
    fn conversations_have_separate_calls() {
      let service = service();
      let now = Instant::now();
      let a = service.call_for(id(1), now, || Ok(id(100))).unwrap();
      let b = service.call_for(id(2), now, || Ok(id(200))).unwrap();
      assert_ne!(a, b);
    }
  }

  fn service() -> CallService {
    CallService::new(&config(), TEST_SIGNING_KEY_PEM.as_bytes()).unwrap()
  }

  fn config() -> CallsConfig {
    CallsConfig {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      issuer: "https://chat.example.com".into(),
      signing_key_path: PathBuf::from("unused.pem"),
      join_token_ttl_secs: 60,
      call_idle_secs: 3600,
      maybe_sfu_registration_token: None,
      ring_timeout_secs: 30,
      maybe_sfu_control_url: None,
      maybe_sfu_certificate_sha256: None,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
