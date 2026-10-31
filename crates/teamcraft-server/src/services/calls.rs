//! Calls: which call a conversation has, and who may join it (ARCHITECTURE.md §6.3).
//!
//! The server decides access and hands out short-lived join tokens (EdDSA-signed JWTs with
//! [`CallJoinClaims`]); the SFU verifies them with the server's public key and never asks the
//! server. Active calls are tracked in memory: a conversation keeps its call id while tokens are
//! issued, and gets a new one after `call_idle_secs` without any.
//!
//! The roster follows the SFU's join and leave reports, and is reconciled with the whole roster the
//! SFU reports at start and periodically (#684): a device the SFU no longer has is dropped (after
//! `roster_grace_secs` while the SFU is fresh from a restart and its devices rejoin), a call left
//! with nobody ends. When the SFU that sent rosters falls silent for `roster_timeout_secs`, every
//! call ends: nothing confirms anybody is still in one.
//!
//! A 1:1 direct message call ends for both as soon as one side leaves (issue #997, the instance
//! setting `one_to_one_call_ends_for_both`): the SFU reports a device as gone only after its
//! reconnect grace period, so a short drop doesn't end it. The other side's devices hear
//! `call_end` with [`CallEndCause::OneSideLeft`] and leave by themselves; a device that doesn't
//! (an older app, one that missed the event) is disconnected by the SFU a little later.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, EncodingKey, Header};
use secrecy::ExposeSecret;
use subtle::ConstantTimeEq;
use teamcraft_proto::{
  CALL_JOIN_TOKEN_AUDIENCE, CallEndCause, CallId, CallJoinClaims, CallJoinTokenResponse, CallRosterEntry,
  CallStateResponse, ChannelRole, ConversationId, ConversationKind, CreateCallJoinTokenRequest,
  DependencyName, DeviceId, GatewayEvent, ListRunningCallsResponse, MoveCallRequest, SfuControlAction,
  SfuControlRequest, SfuParticipantReport, SfuRosterReport, UnixMillis, UserId,
};
use thiserror::Error;
use time::OffsetDateTime;
use tokio::time::sleep;
use tracing::{debug, info, warn};

use super::access::load_conversation;
use crate::auth::{AuthError, AuthenticatedUser};
use crate::config::CallsConfig;
use crate::dependencies::{DependencyGuard, Fault, Retries};
use crate::error::ApiError;
use crate::state::AppState;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a join token waits for the SFU's roster when the conversation has no call here.
const PRE_TOKEN_ROSTER_TIMEOUT: Duration = Duration::from_secs(1);
/// How long the devices left behind in a 1:1 call that ended for both get to leave by themselves
/// (with the hang-up tone) before the SFU is told to disconnect them.
const LEFT_BEHIND_DISCONNECT_DELAY: Duration = Duration::from_secs(5);
/// Pauses before the background retries of a failed roster read at start. They stop early once a
/// roster arrived; the SFU's periodic roster covers anything later.
const STARTUP_ROSTER_RETRIES: [Duration; 6] = [
  Duration::from_secs(1),
  Duration::from_secs(2),
  Duration::from_secs(4),
  Duration::from_secs(8),
  Duration::from_secs(15),
  Duration::from_secs(30),
];

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
  max_devices_per_user: usize,
  /// Token the SFU reports participants with; `None` disables the reports.
  maybe_sfu_token: Option<Vec<u8>>,
  active: Mutex<HashMap<ConversationId, ActiveCall>>,
  /// Meetings the host locked, and people the host removed, until the call ends.
  controls: Mutex<HostControls>,
  /// Where host controls go.
  maybe_control: Option<ControlTarget>,
  /// When the SFU last presented its registration token (Unix milliseconds, 0: never).
  last_sfu_contact_ms: AtomicI64,
  /// How long a device may be missing from the rosters of an SFU fresh from a restart.
  roster_grace: Duration,
  /// Without a roster for this long, the SFU is taken to be gone with its calls.
  roster_timeout: Duration,
  /// When the last roster arrived; `None` until the first (an SFU that sends none keeps the roster
  /// on its join and leave reports alone) and after the calls expired.
  last_roster_at: Mutex<Option<Instant>>,
  /// Calls that ended here lately, and when: a roster sent while the SFU was still closing them
  /// must not bring them back. Forgotten after `roster_grace`.
  recently_ended: Mutex<HashMap<CallId, Instant>>,
}

#[derive(Default)]
struct HostControls {
  locked: HashSet<ConversationId>,
  removed: HashSet<(ConversationId, UserId)>,
}

struct ControlTarget {
  url: String,
  /// Where a starting server reads the SFU's roster (`GET`).
  roster_url: String,
  client: reqwest::Client,
  /// The SFU's breaker (issue #743): host controls fail fast while it restarts.
  guard: Arc<DependencyGuard>,
}

#[derive(Debug, Clone)]
struct ActiveCall {
  call_id: CallId,
  last_token_at: Instant,
  /// Set when the first participant joined (reported by the SFU).
  maybe_started: Option<Started>,
  participants: Vec<CallRosterEntry>,
  /// Participants missing from the SFU's rosters, and since when (only while the SFU is fresh from
  /// a restart; otherwise a missing device is dropped at once).
  missing_since: HashMap<CallRosterEntry, Instant>,
  is_ringing: bool,
  /// Set when a DM call moved into this meeting chat: the people added there ring when the call
  /// starts (or at once if it runs), and only they.
  maybe_added_ring: Option<AddedRing>,
  /// Set when this DM call moved into a meeting chat: a second move goes nowhere else.
  maybe_moved_to: Option<ConversationId>,
  /// The conversation's kind, once known (a join token or join report names it; a call taken over
  /// from the SFU learns it from the database).
  maybe_kind: Option<ConversationKind>,
  /// Two people were in the call at the same time: one of them leaving a 1:1 call ends it.
  has_had_both_sides: bool,
}

/// The people added to a call that moved into a meeting (`move_call`), or the invitees of a
/// meeting whose call starts, rung on behalf of `by`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddedRing {
  pub by: UserId,
  pub users: HashSet<UserId>,
  pub ends: RingEnd,
}

/// What ends the ringing of an [`AddedRing`] besides the timeout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingEnd {
  /// One of the rung people answering (a call moved into a meeting: the added people).
  FirstAnswer,
  /// Only the timeout: everybody invited to a meeting is told it started, whoever joins first.
  Timeout,
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
  /// One side of a 1:1 call left: the call ended for both, and `left_behind` (the other side's
  /// devices) leave it by themselves.
  EndedForBoth {
    conversation_id: ConversationId,
    call_id: CallId,
    left_behind: Vec<CallRosterEntry>,
  },
}

/// What dropping participants (a roster, an expiry, leaving on all devices) changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RosterChanges {
  /// One per call that changed: [`LeaveChange::Updated`] or [`LeaveChange::Ended`].
  pub changes: Vec<LeaveChange>,
  /// Whose devices were dropped (their presence changes).
  pub users: HashSet<UserId>,
  /// Devices the SFU has in a call the server knows, without the server having heard of their
  /// join: handled like a join report.
  pub missing_joins: Vec<SfuParticipantReport>,
  /// Calls taken over from the SFU, with who started them: their conversation's kind is looked up.
  pub adopted: Vec<(ConversationId, UserId)>,
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
  /// One side of a 1:1 call is gone: it was a direct message call with both sides in it, nobody
  /// moved it into a meeting, and only one person is left in it.
  fn is_down_to_one_side(&self) -> bool {
    self.maybe_kind == Some(ConversationKind::Dm)
      && self.maybe_moved_to.is_none()
      && self.has_had_both_sides
      && people_in(&self.participants) == 1
  }

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
      max_devices_per_user: config.max_devices_per_user_per_call,
      maybe_sfu_token: config
        .maybe_sfu_registration_token
        .as_ref()
        .map(|t| t.expose_secret().as_bytes().to_vec()),
      active: Mutex::new(HashMap::new()),
      controls: Mutex::new(HostControls::default()),
      last_sfu_contact_ms: AtomicI64::new(0),
      roster_grace: Duration::from_secs(config.roster_grace_secs),
      roster_timeout: Duration::from_secs(config.roster_timeout_secs),
      last_roster_at: Mutex::new(None),
      recently_ended: Mutex::new(HashMap::new()),
      maybe_control: config
        .maybe_sfu_control_url
        .as_ref()
        .map(|url| {
          Ok::<_, reqwest::Error>(ControlTarget {
            url: format!("{}/internal/v1/calls/control", url.trim_end_matches('/')),
            roster_url: format!("{}/internal/v1/calls/roster", url.trim_end_matches('/')),
            client: reqwest::Client::builder().timeout(CONTROL_TIMEOUT).build()?,
            guard: DependencyGuard::standalone(DependencyName::Sfu),
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
    // Every host control is idempotent (mute all, lock, unlock, end, remove): safe to repeat
    // while the SFU restarts.
    target
      .guard
      .call(Retries::Idempotent, control_fault, || async {
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
      })
      .await
  }

  /// Host controls go through `guard`, the registry's breaker of the SFU.
  pub fn with_dependency_guard(mut self, guard: Arc<DependencyGuard>) -> Self {
    if let Some(target) = &mut self.maybe_control {
      target.guard = guard;
    }
    self
  }

  /// Reads the SFU's roster (`GET /internal/v1/calls/roster`); a starting server takes over the
  /// calls that kept running with it. `None` without host controls configured.
  async fn fetch_sfu_roster(&self) -> Result<Option<SfuRosterReport>, ApiError> {
    let (Some(target), Some(token)) = (self.maybe_control.as_ref(), self.maybe_sfu_token.as_deref()) else {
      return Ok(None);
    };
    let response = target
      .client
      .get(&target.roster_url)
      .bearer_auth(String::from_utf8_lossy(token))
      .send()
      .await
      .map_err(|error| ApiError::Unavailable(format!("the SFU is not reachable: {error}")))?;
    if !response.status().is_success() {
      return Err(ApiError::Unavailable(format!(
        "the SFU answered with status {}",
        response.status()
      )));
    }
    response
      .json()
      .await
      .map(Some)
      .map_err(|error| ApiError::Unavailable(format!("the SFU's roster is unreadable: {error}")))
  }

  /// Reads the SFU's roster within `timeout` and takes over the calls it runs that the server
  /// doesn't know (only that: a pulled snapshot is not in the order of the SFU's pushed reports, so
  /// it neither drops anybody nor counts as the SFU's roster). `None` without host controls.
  async fn pull_and_adopt(&self, timeout: Duration) -> Result<Option<RosterChanges>, ApiError> {
    let report = tokio::time::timeout(timeout, self.fetch_sfu_roster())
      .await
      .map_err(|_| ApiError::Unavailable("the SFU's roster did not come in time".into()))??;
    let Some(report) = report else {
      return Ok(None);
    };
    Ok(Some(self.adopt_calls(
      &report,
      Instant::now(),
      unix_now_millis()?,
    )))
  }

  /// [`Self::pull_and_adopt`] again after each of `pauses`, until it worked or a roster arrived
  /// from the SFU by itself (which takes the calls over too).
  async fn retry_adoption(&self, pauses: &[Duration]) -> Option<RosterChanges> {
    for pause in pauses {
      sleep(*pause).await;
      if self.last_roster().is_some() {
        return None;
      }
      match self.pull_and_adopt(CONTROL_TIMEOUT).await {
        Ok(maybe_changes) => return maybe_changes,
        Err(error) => debug!(%error, "reading the SFU's roster failed again"),
      }
    }
    warn!("the SFU's roster could not be read; its next roster takes the calls over");
    None
  }

  /// Whether the conversation has a call here (running, or opened by a join token).
  fn has_call(&self, conversation_id: ConversationId) -> bool {
    self.lock().contains_key(&conversation_id)
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
        missing_since: HashMap::new(),
        is_ringing: false,
        maybe_added_ring: None,
        maybe_moved_to: None,
        maybe_kind: None,
        has_had_both_sides: false,
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

  /// Records a participant. The first one starts the call (ringing if `rings` or people were added
  /// to it, [`Self::ring_added`]); a joiner other than the starter stops the ringing, or for added
  /// people one of them.
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
    // The device is back (after an SFU restart): no longer missing.
    call.missing_since.remove(&entry);
    if call.participants.contains(&entry) {
      return JoinChange::Unchanged;
    }
    call.participants.push(entry);
    call.has_had_both_sides |= people_in(&call.participants) >= 2;
    match call.maybe_started {
      None => {
        call.maybe_started = Some(Started {
          by: entry.user_id,
          at: now,
        });
        call.is_ringing = match &call.maybe_added_ring {
          Some(ring) => !ring.users.is_empty(),
          None => rings,
        };
        JoinChange::Started(call.snapshot(conversation_id, entry.user_id, now))
      },
      Some(started) => {
        let is_answer = match &call.maybe_added_ring {
          Some(ring) => ring.ends == RingEnd::FirstAnswer && ring.users.contains(&entry.user_id),
          None => entry.user_id != started.by,
        };
        if call.is_ringing && is_answer {
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
    call.missing_since.remove(&entry);
    if call.participants.is_empty() {
      let call_id = call.call_id;
      active.remove(&conversation_id);
      drop(active);
      self.forget_controls(conversation_id);
      self.note_ended(call_id, Instant::now());
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

  /// Reconciles the roster with the SFU's whole roster (#684). The SFU sends it in the same ordered
  /// queue as its join and leave reports, so it reflects everything reported before it. A device
  /// it lacks is gone: at once once the SFU has run for `roster_grace` (it would have rejoined by
  /// then), while it is fresh from a restart only after missing for `roster_grace`. Devices it has
  /// that the server never heard of come back as [`RosterChanges::missing_joins`]. Calls it runs
  /// that the server doesn't know (it restarted) are taken over ([`Self::adopt_calls`]); `now_ms`
  /// is their start when the SFU names none.
  fn reconcile(&self, report: &SfuRosterReport, now: Instant, now_ms: UnixMillis) -> RosterChanges {
    *self.last_roster() = Some(now);
    let adopted = self.adopt_calls(report, now, now_ms);
    let on_sfu: HashMap<CallId, HashSet<CallRosterEntry>> = report
      .calls
      .iter()
      .map(|call| (call.call_id, call.participants.iter().copied().collect()))
      .collect();
    let is_settled = Duration::from_secs(report.uptime_secs) >= self.roster_grace;
    let grace = self.roster_grace;
    let nobody = HashSet::new();
    let mut changes = self.drop_participants(|_, call| {
      let present = on_sfu.get(&call.call_id).unwrap_or(&nobody);
      let mut gone = Vec::new();
      for entry in &call.participants {
        if present.contains(entry) {
          call.missing_since.remove(entry);
          continue;
        }
        let since = *call.missing_since.entry(*entry).or_insert(now);
        if is_settled || now.saturating_duration_since(since) >= grace {
          gone.push(*entry);
        }
      }
      gone
    });
    let active = self.lock();
    for call in active.values() {
      let Some(present) = on_sfu.get(&call.call_id) else {
        continue;
      };
      changes.missing_joins.extend(
        present
          .iter()
          .filter(|entry| !call.participants.contains(entry))
          .map(|entry| SfuParticipantReport {
            call_id: call.call_id,
            user_id: entry.user_id,
            device_id: entry.device_id,
          }),
      );
    }
    drop(active);
    changes.changes.extend(adopted.changes);
    changes.users.extend(adopted.users);
    changes.adopted.extend(adopted.adopted);
    changes
  }

  /// Takes over the calls the SFU runs that the server lost (it restarted; calls live in memory):
  /// the SFU names each call's conversation from the server-signed join tokens, and its devices
  /// joined with tokens this server signed. Nothing rings again, and the history entry written
  /// when the call started is the one closed when it ends. Not taken over: calls without a
  /// conversation (tokens of an older server), calls that ended here moments ago (a roster from
  /// while the SFU was closing them), and a second call in a conversation whose call has people in
  /// it.
  fn adopt_calls(&self, report: &SfuRosterReport, now: Instant, now_ms: UnixMillis) -> RosterChanges {
    let mut result = RosterChanges::default();
    let mut recently_ended = self.recently_ended();
    let grace = self.roster_grace;
    recently_ended.retain(|_, at| now.saturating_duration_since(*at) < grace);
    let mut active = self.lock();
    for call in &report.calls {
      let Some(conversation_id) = call.maybe_conversation_id else {
        continue;
      };
      let Some(started_by) = call
        .maybe_started_by
        .or_else(|| call.participants.first().map(|entry| entry.user_id))
      else {
        continue;
      };
      if recently_ended.contains_key(&call.call_id)
        || active.values().any(|known| known.call_id == call.call_id)
      {
        continue;
      }
      if active
        .get(&conversation_id)
        .is_some_and(|existing| !existing.participants.is_empty())
      {
        warn!(%conversation_id, call_id = %call.call_id, "the SFU runs a second call in a conversation");
        continue;
      }
      let mut participants: Vec<CallRosterEntry> = Vec::new();
      for entry in &call.participants {
        if !participants.contains(entry) {
          participants.push(*entry);
        }
      }
      let started = Started {
        by: started_by,
        at: call.maybe_started_at.unwrap_or(now_ms),
      };
      let has_had_both_sides = people_in(&participants) >= 2;
      let adopted = ActiveCall {
        call_id: call.call_id,
        last_token_at: now,
        maybe_started: Some(started),
        participants,
        missing_since: HashMap::new(),
        // Whoever was rung had their chance before the restart.
        is_ringing: false,
        maybe_added_ring: None,
        maybe_moved_to: None,
        // Looked up by the caller (`RosterChanges::adopted`).
        maybe_kind: None,
        has_had_both_sides,
      };
      info!(%conversation_id, call_id = %call.call_id, "call taken over from the SFU's roster");
      result
        .users
        .extend(adopted.participants.iter().map(|entry| entry.user_id));
      result.changes.push(LeaveChange::Updated(adopted.snapshot(
        conversation_id,
        started.by,
        started.at,
      )));
      result.adopted.push((conversation_id, started.by));
      // A join token handed out since the restart opened an empty call there: the running one wins.
      active.insert(conversation_id, adopted);
    }
    result
  }

  /// Remembers that the call ended here, so a late roster doesn't take it over again.
  fn note_ended(&self, call_id: CallId, now: Instant) {
    let grace = self.roster_grace;
    let mut recently_ended = self.recently_ended();
    // Bounded by the calls that end within one grace period.
    recently_ended.retain(|_, at| now.saturating_duration_since(*at) < grace);
    recently_ended.insert(call_id, now);
  }

  /// Notes the kind of the conversation of `call_id` (a 1:1 call ends when one side leaves).
  fn note_kind(&self, conversation_id: ConversationId, call_id: CallId, kind: ConversationKind) {
    let mut active = self.lock();
    if let Some(call) = active.get_mut(&conversation_id)
      && call.call_id == call_id
    {
      call.maybe_kind = Some(kind);
    }
  }

  /// Ends `call_id` for both when one side of the 1:1 call is gone
  /// ([`ActiveCall::is_down_to_one_side`]): the devices still in it are `left_behind`. `None` when
  /// the call goes on, or ended already (a repeat changes nothing).
  fn end_if_one_side_left(&self, conversation_id: ConversationId, call_id: CallId) -> Option<LeaveChange> {
    let mut active = self.lock();
    let call = active.get(&conversation_id)?;
    if call.call_id != call_id || !call.is_down_to_one_side() {
      return None;
    }
    let left_behind = active.remove(&conversation_id)?.participants;
    drop(active);
    self.forget_controls(conversation_id);
    self.note_ended(call_id, Instant::now());
    Some(LeaveChange::EndedForBoth {
      conversation_id,
      call_id,
      left_behind,
    })
  }

  fn recently_ended(&self) -> std::sync::MutexGuard<'_, HashMap<CallId, Instant>> {
    self.recently_ended.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Ends every call when the SFU that sent rosters fell silent for `roster_timeout` (stopped, or
  /// cut off from the server): nothing confirms anybody is still in a call. An SFU that never sent
  /// a roster (an older one) is left alone.
  fn expire_if_silent(&self, now: Instant) -> RosterChanges {
    {
      let mut last = self.last_roster();
      match *last {
        Some(at) if now.saturating_duration_since(at) >= self.roster_timeout => *last = None,
        _ => return RosterChanges::default(),
      }
    }
    let changes = self.drop_participants(|_, call| call.participants.clone());
    if !changes.changes.is_empty() {
      warn!(
        calls = changes.changes.len(),
        "no call roster from the SFU in time: its calls end"
      );
    }
    changes
  }

  /// Drops every device of `user` from the conversation's call ("Leave on all devices").
  fn drop_user(&self, conversation_id: ConversationId, user: UserId) -> RosterChanges {
    self.drop_participants(|conversation, call| {
      if conversation != conversation_id {
        return Vec::new();
      }
      call
        .participants
        .iter()
        .filter(|entry| entry.user_id == user)
        .copied()
        .collect()
    })
  }

  /// Drops the participants `gone` picks from each running call; a call left with nobody ends.
  fn drop_participants(
    &self,
    mut gone: impl FnMut(ConversationId, &mut ActiveCall) -> Vec<CallRosterEntry>,
  ) -> RosterChanges {
    let mut result = RosterChanges::default();
    let mut ended = Vec::new();
    let mut active = self.lock();
    for (conversation_id, call) in active.iter_mut() {
      let Some(started) = call.maybe_started else {
        continue;
      };
      let dropped = gone(*conversation_id, call);
      if dropped.is_empty() {
        continue;
      }
      call.participants.retain(|entry| !dropped.contains(entry));
      for entry in &dropped {
        call.missing_since.remove(entry);
        result.users.insert(entry.user_id);
      }
      info!(
        conversation_id = %conversation_id,
        call_id = %call.call_id,
        dropped = dropped.len(),
        "participants dropped from the call roster"
      );
      if call.participants.is_empty() {
        ended.push((*conversation_id, call.call_id));
      } else {
        result.changes.push(LeaveChange::Updated(call.snapshot(
          *conversation_id,
          started.by,
          started.at,
        )));
      }
    }
    for (conversation_id, _) in &ended {
      active.remove(conversation_id);
    }
    drop(active);
    for (conversation_id, call_id) in ended {
      self.forget_controls(conversation_id);
      self.note_ended(call_id, Instant::now());
      result.changes.push(LeaveChange::Ended {
        conversation_id,
        call_id,
      });
    }
    result
  }

  /// How often to check for a silent SFU: a few times per `roster_timeout`.
  pub fn roster_sweep_interval(&self) -> Duration {
    (self.roster_timeout / 4).max(Duration::from_secs(1))
  }

  fn last_roster(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
    self.last_roster_at.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Notes the people added to a call that moves into this meeting chat: they ring when its call
  /// starts, or now if it runs already (the returned snapshot then rings). Who else joins doesn't
  /// end their ringing, only one of them answering or the timeout. A new call keeps the entry for
  /// `call_idle_secs`, like one a join token opened.
  fn ring_added(
    &self,
    conversation_id: ConversationId,
    ring: AddedRing,
    now: Instant,
    new_call_id: impl FnOnce() -> Result<CallId, ApiError>,
  ) -> Result<Option<CallSnapshot>, ApiError> {
    self.call_for(conversation_id, now, new_call_id)?;
    let mut active = self.lock();
    let Some(call) = active.get_mut(&conversation_id) else {
      return Ok(None);
    };
    let is_anybody_rung = !ring.users.is_empty();
    call.maybe_added_ring = Some(ring);
    let Some(started) = call.maybe_started else {
      return Ok(None);
    };
    if !is_anybody_rung {
      return Ok(None);
    }
    call.is_ringing = true;
    Ok(Some(call.snapshot(conversation_id, started.by, started.at)))
  }

  /// Whom the conversation's call rings for, if somebody moved a call into it.
  fn added_ring(&self, conversation_id: ConversationId) -> Option<AddedRing> {
    self
      .lock()
      .get(&conversation_id)
      .and_then(|call| call.maybe_added_ring.clone())
  }

  /// Notes that the conversation's running call moves into `to`. Two people adding at the same time
  /// would split the call into two rooms: the second move into another room is refused with the
  /// first one's chat; the same move again (a retry) is fine.
  fn mark_moved(&self, conversation_id: ConversationId, to: ConversationId) -> Result<(), ConversationId> {
    let mut active = self.lock();
    let Some(call) = active.get_mut(&conversation_id) else {
      return Ok(());
    };
    match call.maybe_moved_to {
      Some(existing) if existing != to => Err(existing),
      _ => {
        call.maybe_moved_to = Some(to);
        Ok(())
      },
    }
  }

  /// Whether a device of the user is in the conversation's call.
  pub fn is_user_in_call(&self, conversation_id: ConversationId, user: UserId) -> bool {
    self
      .lock()
      .get(&conversation_id)
      .is_some_and(|call| call.participants.iter().any(|entry| entry.user_id == user))
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

  /// Whether the device may join the conversation's call: it is in the roster already (it
  /// rejoins), or the account has fewer than `max_devices_per_user_per_call` devices there.
  pub fn has_room_for_device(
    &self,
    conversation_id: ConversationId,
    user: UserId,
    device_id: DeviceId,
  ) -> bool {
    let active = self.lock();
    let Some(call) = active.get(&conversation_id) else {
      return true;
    };
    let mut devices_of_user = 0;
    for entry in &call.participants {
      if entry.device_id == device_id {
        return true;
      }
      if entry.user_id == user {
        devices_of_user += 1;
      }
    }
    devices_of_user < self.max_devices_per_user
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

  /// Every running call (one with at least one participant), by conversation.
  fn running_snapshots(&self) -> Vec<CallSnapshot> {
    let active = self.lock();
    let mut snapshots: Vec<CallSnapshot> = active
      .iter()
      .filter_map(|(conversation_id, call)| {
        let started = call.maybe_started?;
        Some(call.snapshot(*conversation_id, started.by, started.at))
      })
      .collect();
    snapshots.sort_by_key(|snapshot| snapshot.conversation_id);
    snapshots
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<ConversationId, ActiveCall>> {
    // No invariant spans a panic point, so a poisoned map is still consistent.
    self.active.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Claims for one device, valid from `now_unix_secs` for the configured lifetime.
  fn claims(
    &self,
    conversation_id: ConversationId,
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
      maybe_conversation_id: Some(conversation_id),
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
  if !calls.has_room_for_device(conversation_id, caller.user.id, request.device_id) {
    return Err(ApiError::CallDeviceLimit {
      max_devices: calls.max_devices_per_user,
    });
  }
  adopt_before_first_token(state, calls, conversation_id).await;
  let call_id = calls.call_for(conversation_id, Instant::now(), || state.next_id())?;
  let kind = access.view.conversation.kind;
  let ends_when_one_side_leaves = kind == ConversationKind::Dm && ends_one_to_one_calls(state);
  calls.note_kind(conversation_id, call_id, kind);
  let claims = calls.claims(
    conversation_id,
    call_id,
    caller.user.id,
    request.device_id,
    unix_now_secs()?,
  );
  Ok(CallJoinTokenResponse {
    call_id,
    token: calls.sign(&claims)?,
    sfu_url: calls.sfu_url.clone(),
    expires_at: UnixMillis(i64::try_from(claims.exp).unwrap_or(i64::MAX).saturating_mul(1000)),
    sfu_certificate_sha256: calls.sfu_certificate_sha256.clone(),
    ends_when_one_side_leaves,
  })
}

/// Whether a 1:1 call ends for both when one side leaves (the instance setting, issue #997).
fn ends_one_to_one_calls(state: &AppState) -> bool {
  state.settings.get().policy.one_to_one_call_ends_for_both
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

/// The calls running in the caller's conversations (issue #976): a client compares its roster with
/// this after every gateway connection and when the user comes back to it, so a call whose end it
/// missed doesn't stay on screen. Without any call running (or without calls configured) nothing
/// is read from the database.
pub async fn list_running_calls(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListRunningCallsResponse, ApiError> {
  let Some(calls) = state.maybe_calls.as_ref() else {
    return Ok(ListRunningCallsResponse::default());
  };
  let running = calls.running_snapshots();
  if running.is_empty() {
    return Ok(ListRunningCallsResponse::default());
  }
  let member_of: HashSet<ConversationId> = state
    .database
    .conversation_ids_of_user(caller.user.id)
    .await?
    .into_iter()
    .collect();
  Ok(ListRunningCallsResponse {
    calls: running
      .into_iter()
      .filter(|snapshot| member_of.contains(&snapshot.conversation_id))
      .map(state_response)
      .collect(),
  })
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
  let is_meeting = conversation.conversation.kind == ConversationKind::Meeting;
  calls.note_kind(conversation_id, report.call_id, conversation.conversation.kind);
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
        start_ringing(state, calls, &snapshot, &members);
      } else if is_meeting {
        ring_meeting_invitees(state, calls, conversation_id, snapshot.started_by, &members).await;
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

/// Rings the call's members: in a DM or group DM everybody but the starter, in a meeting a call
/// moved into the people added to it.
fn start_ringing(state: &AppState, calls: &CallService, snapshot: &CallSnapshot, members: &[UserId]) {
  let conversation_id = snapshot.conversation_id;
  let (caller, maybe_only) = match calls.added_ring(conversation_id) {
    Some(ring) => (ring.by, Some(ring.users)),
    None => (snapshot.started_by, None),
  };
  let rung: Vec<UserId> = members
    .iter()
    .copied()
    .filter(|member| match &maybe_only {
      Some(only) => only.contains(member),
      None => *member != caller,
    })
    .collect();
  state.hub.publish(
    rung,
    &GatewayEvent::CallRing {
      conversation_id,
      call_id: snapshot.call_id,
      from_user_id: caller,
    },
  );
  tokio::spawn(crate::push::routing::notify_ring(
    state.clone(),
    conversation_id,
    snapshot.call_id,
    caller,
    maybe_only,
  ));
  spawn_ring_timeout(
    state.clone(),
    conversation_id,
    snapshot.call_id,
    calls.ring_timeout,
  );
}

/// A call that starts in a scheduled meeting's chat around one of its occurrences rings the invitees
/// who haven't declined, also when the organizer starts it early (issue #1006). Their ringing ends
/// with the timeout, not when the first of them joins. Failures only cost the ring.
async fn ring_meeting_invitees(
  state: &AppState,
  calls: &CallService,
  conversation_id: ConversationId,
  starter: UserId,
  members: &[UserId],
) {
  let users = match super::meetings::invitees_to_ring(state, conversation_id, starter).await {
    Ok(users) if !users.is_empty() => users,
    Ok(_) => return,
    Err(error) => {
      warn!(%error, conversation_id = %conversation_id, "finding the invitees to ring failed");
      return;
    },
  };
  let ring = AddedRing {
    by: starter,
    users,
    ends: RingEnd::Timeout,
  };
  match calls.ring_added(conversation_id, ring, Instant::now(), || state.next_id()) {
    Ok(Some(snapshot)) => {
      publish_update(state, members.to_vec(), &snapshot);
      start_ringing(state, calls, &snapshot, members);
    },
    Ok(None) => {},
    Err(error) => warn!(%error, conversation_id = %conversation_id, "ringing the invitees failed"),
  }
}

/// Somebody in a DM or group DM call added people: the call moves into the meeting chat the caller
/// just started for it (docs/meetings.md, "A call becomes a meeting"). Everybody in the call is told
/// to switch over, the added people ring in the meeting. The DM itself doesn't change. A repeat
/// tells them again and rings anew, which is what a retry after a lost answer needs.
pub async fn move_call(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
  request: MoveCallRequest,
) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let access = load_conversation(state, caller, conversation_id).await?;
  if !access.is_member() {
    return Err(ApiError::Forbidden("only members can move the call".into()));
  }
  if !matches!(
    access.view.conversation.kind,
    ConversationKind::Dm | ConversationKind::Group
  ) {
    return Err(ApiError::InvalidRequest(
      "only DM and group DM calls move into a meeting".into(),
    ));
  }
  let Some(call_id) = calls.running_call(conversation_id) else {
    return Err(ApiError::call_not_found());
  };
  if !calls.is_user_in_call(conversation_id, caller.user.id) {
    return Err(ApiError::Forbidden("only people in the call can move it".into()));
  }
  let target = load_conversation(state, caller, request.to_conversation_id).await?;
  let organizes =
    target.view.conversation.kind == ConversationKind::Meeting && target.role() == Some(&ChannelRole::Admin);
  if !organizes {
    return Err(ApiError::Forbidden(
      "a call moves only into a meeting the caller organizes".into(),
    ));
  }
  let max_rung = state.settings.get().meeting_mail.max_invitees;
  if request.ring_user_ids.len() > max_rung {
    return Err(ApiError::InvalidRequest(format!(
      "at most {max_rung} people to ring"
    )));
  }
  if let Err(existing) = calls.mark_moved(conversation_id, request.to_conversation_id) {
    return Err(ApiError::Conflict(format!(
      "the call moved into {existing} already"
    )));
  }
  let target_members = state
    .database
    .conversation_member_ids(request.to_conversation_id)
    .await?;
  let ring = AddedRing {
    by: caller.user.id,
    users: request
      .ring_user_ids
      .iter()
      .copied()
      .filter(|user| *user != caller.user.id && target_members.contains(user))
      .collect(),
    ends: RingEnd::FirstAnswer,
  };
  let maybe_ringing = calls.ring_added(request.to_conversation_id, ring, Instant::now(), || {
    state.next_id()
  })?;
  info!(
    conversation_id = %conversation_id,
    to_conversation_id = %request.to_conversation_id,
    "call moves into a meeting"
  );
  let members = state.database.conversation_member_ids(conversation_id).await?;
  state.hub.publish(
    members,
    &GatewayEvent::CallMove {
      conversation_id,
      call_id,
      to_conversation_id: request.to_conversation_id,
      moved_by: caller.user.id,
    },
  );
  if let Some(snapshot) = maybe_ringing {
    publish_update(state, target_members.clone(), &snapshot);
    start_ringing(state, calls, &snapshot, &target_members);
  }
  Ok(())
}

/// The SFU reports that a device left. The last one leaving ends the call, and so does one side
/// of a 1:1 call leaving.
pub async fn report_left(state: &AppState, report: SfuParticipantReport) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let conversation_id = calls
    .conversation_of(report.call_id)
    .ok_or_else(ApiError::call_not_found)?;
  let entry = CallRosterEntry {
    user_id: report.user_id,
    device_id: report.device_id,
  };
  let change = settle_one_to_one(state, calls, calls.participant_left(conversation_id, entry));
  if matches!(change, LeaveChange::Unchanged) {
    return Ok(());
  }
  publish_leave(state, conversation_id, change).await?;
  refresh_caller_presence(state, report.user_id).await;
  Ok(())
}

/// The SFU reports its whole roster: devices it no longer has leave the server's roster, calls left
/// with nobody end, and devices whose join the server missed join (#684).
pub async fn report_roster(state: &AppState, report: SfuRosterReport) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let changes = calls.reconcile(&report, Instant::now(), unix_now_millis()?);
  publish_roster_changes(state, changes).await;
  Ok(())
}

/// At start: reads the SFU's roster and takes over the calls that kept running while this server
/// restarted, before people ask for join tokens (which would open a second call next to them). An
/// SFU that can't be asked (no control URL, unreachable) delivers the same with its next roster.
pub async fn adopt_running_calls(state: &AppState) {
  let Some(calls) = state.maybe_calls.clone() else {
    return;
  };
  match calls.pull_and_adopt(CONTROL_TIMEOUT).await {
    Ok(Some(changes)) => publish_adopted(state, changes).await,
    Ok(None) => {},
    Err(error) => {
      warn!(%error, "reading the SFU's roster at start failed; trying again in the background");
      let state = state.clone();
      tokio::spawn(async move {
        if let Some(changes) = calls.retry_adoption(&STARTUP_ROSTER_RETRIES).await {
          publish_adopted(&state, changes).await;
        }
      });
    },
  }
}

async fn publish_adopted(state: &AppState, changes: RosterChanges) {
  if !changes.changes.is_empty() {
    info!(calls = changes.changes.len(), "calls taken over from the SFU");
  }
  publish_roster_changes(state, changes).await;
}

/// Before the first join token of a conversation without a call: the SFU may run one the server
/// lost (it restarted and couldn't read the roster yet). Asking first, for at most
/// [`PRE_TOKEN_ROSTER_TIMEOUT`], keeps the token on the running call; without an answer the
/// token opens a new call as before (and the SFU still puts the device into the running room).
async fn adopt_before_first_token(state: &AppState, calls: &CallService, conversation_id: ConversationId) {
  if calls.has_call(conversation_id) {
    return;
  }
  match calls.pull_and_adopt(PRE_TOKEN_ROSTER_TIMEOUT).await {
    Ok(Some(changes)) => publish_adopted(state, changes).await,
    Ok(None) => {},
    Err(error) => debug!(%conversation_id, %error, "no SFU roster before the first join token"),
  }
}

/// Ends the calls of an SFU that stopped sending rosters ([`CallService::expire_if_silent`]).
pub async fn expire_silent_sfu(state: &AppState) {
  let Some(calls) = state.maybe_calls.as_ref() else {
    return;
  };
  let changes = calls.expire_if_silent(Instant::now());
  publish_roster_changes(state, changes).await;
}

/// "Leave on all devices": the caller's devices leave the conversation's call. The SFU is told to
/// disconnect them (they don't rejoin); the server's roster drops them whatever the SFU says, so an
/// entry only the server still had (a leave it never heard of) goes too. Idempotent: without a
/// call, or with none of the caller's devices in it, nothing happens.
pub async fn leave_call_everywhere(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation_id: ConversationId,
) -> Result<(), ApiError> {
  let calls = calls_of(state)?;
  let user = caller.user.id;
  let Some(call_id) = calls.running_call(conversation_id) else {
    return Ok(());
  };
  if !calls.is_user_in_call(conversation_id, user) {
    return Ok(());
  }
  if calls.has_controls() {
    let request = SfuControlRequest {
      call_id,
      by_user_id: user,
      action: SfuControlAction::DisconnectUser { user_id: user },
    };
    // Not on the SFU (the stuck entry this is for) or the SFU unreachable: the roster below still
    // drops the devices, and a device the SFU does have comes back with its next roster.
    if let Err(error) = calls.send_control(&request).await {
      info!(%conversation_id, %error, "the SFU did not disconnect the devices of a user leaving everywhere");
    }
  }
  info!(%conversation_id, %call_id, "a user leaves the call on all devices");
  publish_roster_changes(state, calls.drop_user(conversation_id, user)).await;
  Ok(())
}

/// Tells the members what dropping participants changed, joins the devices the SFU had that the
/// server missed, and refreshes the presence of those who left.
async fn publish_roster_changes(state: &AppState, changes: RosterChanges) {
  let Some(calls) = state.maybe_calls.as_ref() else {
    return;
  };
  note_adopted_kinds(state, calls, &changes.adopted).await;
  for change in changes.changes {
    let change = settle_one_to_one(state, calls, change);
    let conversation_id = match &change {
      LeaveChange::Unchanged => continue,
      LeaveChange::Updated(snapshot) => snapshot.conversation_id,
      LeaveChange::Ended { conversation_id, .. } | LeaveChange::EndedForBoth { conversation_id, .. } => {
        *conversation_id
      },
    };
    if let Err(error) = publish_leave(state, conversation_id, change).await {
      warn!(%conversation_id, %error, "publishing a call roster change failed");
    }
  }
  for join in changes.missing_joins {
    debug!(call_id = %join.call_id, "the SFU has a device the server missed the join of");
    if let Err(error) = report_joined(state, join).await {
      debug!(%error, "a join from the SFU's roster was not taken");
    }
  }
  for user in changes.users {
    refresh_caller_presence(state, user).await;
  }
}

/// Tells the conversation's members about a leave; an ended call also stops ringing and closes its
/// history entry.
async fn publish_leave(
  state: &AppState,
  conversation_id: ConversationId,
  change: LeaveChange,
) -> Result<(), ApiError> {
  if let LeaveChange::Ended { call_id, .. } | LeaveChange::EndedForBoth { call_id, .. } = &change {
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
          cause: CallEndCause::LastLeft,
        },
      );
    },
    LeaveChange::EndedForBoth {
      conversation_id,
      call_id,
      left_behind,
    } => {
      info!(
        conversation_id = %conversation_id,
        call_id = %call_id,
        left_behind = left_behind.len(),
        "one side left the 1:1 call: it ended for both"
      );
      state.hub.publish(
        members,
        &GatewayEvent::CallEnd {
          conversation_id,
          call_id,
          cause: CallEndCause::OneSideLeft,
        },
      );
      let users: HashSet<UserId> = left_behind.iter().map(|entry| entry.user_id).collect();
      for user in &users {
        refresh_caller_presence(state, *user).await;
      }
      spawn_disconnect_left_behind(state.clone(), call_id, users);
    },
  }
  Ok(())
}

/// Ends a 1:1 call for both once one side of it is gone (issue #997), when the instance setting
/// asks for it: an [`LeaveChange::Updated`] that leaves one person in a 1:1 call becomes
/// [`LeaveChange::EndedForBoth`]. Every other change stays as it is.
fn settle_one_to_one(state: &AppState, calls: &CallService, change: LeaveChange) -> LeaveChange {
  let LeaveChange::Updated(snapshot) = &change else {
    return change;
  };
  if !ends_one_to_one_calls(state) {
    return change;
  }
  calls
    .end_if_one_side_left(snapshot.conversation_id, snapshot.call_id)
    .unwrap_or(change)
}

/// Looks up the conversation kind of calls taken over from the SFU after a restart: a 1:1 call
/// among them still ends when one side leaves.
async fn note_adopted_kinds(state: &AppState, calls: &CallService, adopted: &[(ConversationId, UserId)]) {
  for (conversation_id, started_by) in adopted {
    let Some(call_id) = calls.running_call(*conversation_id) else {
      continue;
    };
    match state
      .database
      .get_conversation(*conversation_id, *started_by)
      .await
    {
      Ok(Some(found)) => calls.note_kind(*conversation_id, call_id, found.conversation.kind),
      Ok(None) => {},
      Err(error) => warn!(%conversation_id, %error, "looking up the kind of a call taken over failed"),
    }
  }
}

/// The devices left behind in a 1:1 call that ended for both leave by themselves when they hear
/// `call_end`; one that doesn't (an older app, a device that missed the event while its media kept
/// running) would stay alone on the SFU, and its roster would bring the call back. After
/// [`LEFT_BEHIND_DISCONNECT_DELAY`] the SFU disconnects whatever is still there. Best effort:
/// without host controls nothing is sent, and a call the SFU no longer has is fine.
fn spawn_disconnect_left_behind(state: AppState, call_id: CallId, users: HashSet<UserId>) {
  if users.is_empty() {
    return;
  }
  tokio::spawn(async move {
    sleep(LEFT_BEHIND_DISCONNECT_DELAY).await;
    let Ok(calls) = calls_of(&state) else { return };
    if !calls.has_controls() {
      return;
    }
    for user_id in users {
      let request = SfuControlRequest {
        call_id,
        by_user_id: user_id,
        action: SfuControlAction::DisconnectUser { user_id },
      };
      match calls.send_control(&request).await {
        Ok(()) => debug!(%call_id, "disconnected a device left behind in a 1:1 call"),
        Err(error) => debug!(%call_id, %error, "nothing left behind to disconnect"),
      }
    }
  });
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

/// How many people the roster's devices belong to.
fn people_in(participants: &[CallRosterEntry]) -> usize {
  participants
    .iter()
    .map(|entry| entry.user_id)
    .collect::<HashSet<_>>()
    .len()
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

fn unix_now_millis() -> Result<UnixMillis, ApiError> {
  Ok(UnixMillis(
    unix_now_secs()?
      .saturating_mul(1000)
      .try_into()
      .unwrap_or(i64::MAX),
  ))
}

/// An unreachable SFU or a 5xx is an outage; "no such call" is an answer.
fn control_fault(error: &ApiError) -> Fault {
  match error {
    ApiError::Unavailable(_) => Fault::Outage,
    _ => Fault::Refusal,
  }
}

#[cfg(test)]
mod tests {
  use std::path::PathBuf;

  use jsonwebtoken::{DecodingKey, Validation};
  use teamcraft_proto::{Id, SfuCallRoster, Snowflake};

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
      let claims = service.claims(id(9), id(7), id(4), id(5), NOW_UNIX_SECS);
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
    fn an_account_gets_no_device_in_a_call_beyond_the_limit() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();
      assert!(
        service.has_room_for_device(id(1), id(4), id(50)),
        "a call without participants"
      );
      for device in 50..53 {
        service.participant_joined(id(1), entry(4, device), false, UnixMillis(0));
      }
      service.participant_joined(id(1), entry(5, 60), false, UnixMillis(0));

      assert!(
        !service.has_room_for_device(id(1), id(4), id(53)),
        "a fourth device"
      );
      assert!(
        service.has_room_for_device(id(1), id(4), id(51)),
        "a device in the call rejoins"
      );
      assert!(
        service.has_room_for_device(id(1), id(5), id(61)),
        "the limit is per account"
      );
      assert!(
        service.has_room_for_device(id(2), id(4), id(53)),
        "another conversation"
      );
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
    fn added_people_ring_when_the_call_starts_and_only_they_answer() {
      let service = service();
      let ring = AddedRing {
        by: id(4),
        users: HashSet::from([id(6)]),
        ends: RingEnd::FirstAnswer,
      };
      let ringing_now = service
        .ring_added(id(1), ring, Instant::now(), || Ok(id(100)))
        .unwrap();
      assert!(ringing_now.is_none(), "nothing rings before the call starts");

      let started = service.participant_joined(id(1), entry(5, 50), false, UnixMillis(0));
      assert!(matches!(started, JoinChange::Started(snapshot) if snapshot.is_ringing));
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(1));
      assert!(
        service.is_ringing(id(1), id(100)),
        "the mover joining doesn't answer"
      );
      service.participant_joined(id(1), entry(6, 60), false, UnixMillis(2));
      assert!(!service.is_ringing(id(1), id(100)));
    }

    #[test]
    fn invitees_of_a_starting_meeting_ring_until_the_timeout() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(0));
      let ring = AddedRing {
        by: id(4),
        users: HashSet::from([id(6), id(7)]),
        ends: RingEnd::Timeout,
      };
      service
        .ring_added(id(1), ring, Instant::now(), || Ok(id(101)))
        .unwrap();
      service.participant_joined(id(1), entry(6, 60), false, UnixMillis(1));
      assert!(
        service.is_ringing(id(1), id(100)),
        "the first invitee joining doesn't end the others' ringing"
      );
      assert!(service.stop_ringing(id(1), id(100)).is_some());
    }

    #[test]
    fn people_added_to_a_running_call_ring_at_once() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(0));
      let ring = AddedRing {
        by: id(4),
        users: HashSet::from([id(6)]),
        ends: RingEnd::FirstAnswer,
      };

      let ringing_now = service
        .ring_added(id(1), ring, Instant::now(), || Ok(id(101)))
        .unwrap();

      assert_eq!(ringing_now.map(|snapshot| snapshot.call_id), Some(id(100)));
      assert!(service.is_ringing(id(1), id(100)));
    }

    #[test]
    fn a_move_without_anybody_added_rings_nobody() {
      let service = service();
      let ring = AddedRing {
        by: id(4),
        users: HashSet::new(),
        ends: RingEnd::FirstAnswer,
      };
      service
        .ring_added(id(1), ring, Instant::now(), || Ok(id(100)))
        .unwrap();
      let started = service.participant_joined(id(1), entry(4, 40), true, UnixMillis(0));
      assert!(matches!(started, JoinChange::Started(snapshot) if !snapshot.is_ringing));
    }

    #[test]
    fn a_call_moves_into_one_room_only() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(0));
      assert_eq!(service.mark_moved(id(1), id(2)), Ok(()));
      assert_eq!(service.mark_moved(id(1), id(2)), Ok(()), "a retry");
      assert_eq!(service.mark_moved(id(1), id(3)), Err(id(2)));
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

  /// Issue #976: the roster is keyed by device, a leave removes exactly that device, and the last
  /// one leaving ends the call, whoever leaves first.
  mod leaves {
    use super::*;

    #[test]
    fn a_leave_removes_only_the_device_that_left() {
      let service = service();
      running_call(
        &service,
        id(1),
        id(100),
        &[entry(4, 40), entry(4, 41), entry(5, 50)],
      );

      let change = service.participant_left(id(1), entry(4, 40));

      assert!(matches!(
        &change,
        LeaveChange::Updated(snapshot) if snapshot.participants == vec![entry(4, 41), entry(5, 50)]
      ));
      assert!(!service.is_device_in_call(id(1), id(40)));
      assert!(
        service.is_device_in_call(id(1), id(41)),
        "the user's other device stays"
      );
    }

    #[test]
    fn the_last_leave_ends_the_call_in_either_order() {
      for (first, second) in [(entry(4, 40), entry(5, 50)), (entry(5, 50), entry(4, 40))] {
        let service = service();
        running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);

        assert!(matches!(
          service.participant_left(id(1), first),
          LeaveChange::Updated(snapshot) if snapshot.participants == vec![second]
        ));
        assert_eq!(
          service.participant_left(id(1), second),
          LeaveChange::Ended {
            conversation_id: id(1),
            call_id: id(100),
          }
        );
        assert!(service.running_snapshots().is_empty(), "{first:?} first");
        assert_eq!(
          service.participant_left(id(1), second),
          LeaveChange::Unchanged,
          "a repeated report"
        );
      }
    }

    #[test]
    fn a_leave_of_a_device_not_in_the_call_changes_nothing() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40)]);

      assert_eq!(
        service.participant_left(id(1), entry(4, 41)),
        LeaveChange::Unchanged
      );
      assert_eq!(service.running_snapshots().len(), 1);
    }

    #[test]
    fn only_calls_with_somebody_in_them_are_listed() {
      let service = service();
      running_call(&service, id(2), id(200), &[entry(4, 40)]);
      running_call(&service, id(1), id(100), &[entry(5, 50)]);
      // A join token alone opens no running call.
      service.call_for(id(3), Instant::now(), || Ok(id(300))).unwrap();

      let listed: Vec<(ConversationId, CallId)> = service
        .running_snapshots()
        .into_iter()
        .map(|snapshot| (snapshot.conversation_id, snapshot.call_id))
        .collect();
      assert_eq!(listed, vec![(id(1), id(100)), (id(2), id(200))]);
    }
  }

  mod reconciliation {
    use super::*;

    /// Longer than the test config's `roster_grace_secs`: the SFU has settled.
    const SETTLED: u64 = 600;

    #[test]
    fn a_device_the_sfu_no_longer_has_is_dropped() {
      let service = service();
      let now = Instant::now();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);

      let changes = reconcile(&service, &roster(SETTLED, &[(100, &[entry(5, 50)])]), now);

      assert_eq!(changes.changes.len(), 1);
      assert!(matches!(
        &changes.changes[0],
        LeaveChange::Updated(snapshot) if snapshot.participants == vec![entry(5, 50)]
      ));
      assert_eq!(changes.users, HashSet::from([id(4)]));
      assert!(changes.missing_joins.is_empty());
      assert!(!service.is_device_in_call(id(1), id(40)));
    }

    #[test]
    fn a_call_the_sfu_no_longer_has_ends() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);

      let changes = reconcile(&service, &roster(SETTLED, &[]), Instant::now());

      assert_eq!(
        changes.changes,
        vec![LeaveChange::Ended {
          conversation_id: id(1),
          call_id: id(100),
        }]
      );
      assert_eq!(service.running_call(id(1)), None);
      assert_eq!(service.active_call_count(), 0);
    }

    #[test]
    fn a_call_nobody_joined_yet_is_left_alone() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(100))).unwrap();

      let changes = reconcile(&service, &roster(SETTLED, &[]), Instant::now());

      assert_eq!(changes, RosterChanges::default());
      assert_eq!(service.active_call_count(), 1);
    }

    #[test]
    fn a_restarted_sfu_gives_its_devices_time_to_rejoin() {
      let service = service();
      let start = Instant::now();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);

      // Fresh from a restart: nobody has rejoined yet.
      let first = reconcile(&service, &roster(2, &[]), start);
      assert_eq!(first.changes, Vec::new());
      // One of them is back; the other one is still given time.
      service.participant_joined(id(1), entry(5, 50), false, UnixMillis(1));
      let meanwhile = reconcile(
        &service,
        &roster(32, &[(100, &[entry(5, 50)])]),
        start + Duration::from_secs(30),
      );
      assert_eq!(meanwhile.changes, Vec::new());
      assert!(service.is_device_in_call(id(1), id(40)));
      let later = start + Duration::from_secs(61);
      let second = reconcile(&service, &roster(63, &[(100, &[entry(5, 50)])]), later);

      assert!(matches!(
        &second.changes[..],
        [LeaveChange::Updated(snapshot)] if snapshot.participants == vec![entry(5, 50)]
      ));
    }

    #[test]
    fn a_call_nobody_rejoins_after_an_sfu_restart_ends_after_the_grace() {
      let service = service();
      let start = Instant::now();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);

      // Issue #427: the SFU went away without reporting leaves, and nobody comes back.
      let first = reconcile(&service, &roster(0, &[]), start);
      let within = reconcile(&service, &roster(59, &[]), start + Duration::from_secs(59));
      assert_eq!((first.changes, within.changes), (Vec::new(), Vec::new()));
      assert_eq!(service.running_call(id(1)), Some(id(100)));
      let after = reconcile(&service, &roster(60, &[]), start + Duration::from_secs(60));

      assert_eq!(
        after.changes,
        vec![LeaveChange::Ended {
          conversation_id: id(1),
          call_id: id(100),
        }]
      );
      assert_eq!(
        after.users,
        HashSet::from([id(4), id(5)]),
        "their presence is updated"
      );
      assert_eq!(service.running_call(id(1)), None);
    }

    #[test]
    fn devices_the_server_missed_come_back_as_joins() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40)]);

      let changes = reconcile(
        &service,
        &roster(
          SETTLED,
          &[(100, &[entry(4, 40), entry(6, 60)]), (999, &[entry(7, 70)])],
        ),
        Instant::now(),
      );

      assert!(changes.changes.is_empty());
      assert_eq!(
        changes.missing_joins,
        vec![SfuParticipantReport {
          call_id: id(100),
          user_id: id(6),
          device_id: id(60),
        }],
        "calls the server doesn't know are not made up"
      );
    }

    #[test]
    fn a_leave_that_overtook_its_join_is_repaired_by_the_next_roster() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(5, 50)]);
      // A quick rejoin whose leave arrived first: the leave finds nothing, the join stays.
      let leave = service.participant_left(id(1), entry(4, 40));
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(1));
      assert_eq!(leave, LeaveChange::Unchanged);
      assert!(service.is_device_in_call(id(1), id(40)));

      reconcile(
        &service,
        &roster(SETTLED, &[(100, &[entry(5, 50)])]),
        Instant::now(),
      );

      assert!(!service.is_device_in_call(id(1), id(40)));
      assert!(service.is_device_in_call(id(1), id(50)));
    }

    #[test]
    fn a_silent_sfu_ends_its_calls_after_the_timeout() {
      let service = service();
      let start = Instant::now();
      running_call(&service, id(1), id(100), &[entry(4, 40)]);
      assert_eq!(
        service.expire_if_silent(start + Duration::from_secs(3600)),
        RosterChanges::default(),
        "an SFU that never sent a roster keeps the old behaviour"
      );

      reconcile(&service, &roster(SETTLED, &[(100, &[entry(4, 40)])]), start);
      let before = service.expire_if_silent(start + Duration::from_secs(119));
      let after = service.expire_if_silent(start + Duration::from_secs(120));

      assert_eq!(before, RosterChanges::default());
      assert_eq!(
        after.changes,
        vec![LeaveChange::Ended {
          conversation_id: id(1),
          call_id: id(100),
        }]
      );
      assert_eq!(after.users, HashSet::from([id(4)]));
      assert_eq!(
        service.expire_if_silent(start + Duration::from_secs(500)),
        RosterChanges::default(),
        "once"
      );
    }

    #[test]
    fn leaving_on_all_devices_drops_only_that_users_devices() {
      let service = service();
      running_call(
        &service,
        id(1),
        id(100),
        &[entry(4, 40), entry(4, 41), entry(5, 50)],
      );
      running_call(&service, id(2), id(200), &[entry(4, 42)]);

      let changes = service.drop_user(id(1), id(4));

      assert!(matches!(
        &changes.changes[..],
        [LeaveChange::Updated(snapshot)] if snapshot.participants == vec![entry(5, 50)]
      ));
      assert!(!service.is_user_in_call(id(1), id(4)));
      assert!(service.is_user_in_call(id(2), id(4)), "other calls stay");
    }

    #[test]
    fn leaving_on_all_devices_ends_a_call_with_nobody_else() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(4, 41)]);

      let changes = service.drop_user(id(1), id(4));

      assert_eq!(
        changes.changes,
        vec![LeaveChange::Ended {
          conversation_id: id(1),
          call_id: id(100),
        }]
      );
      assert_eq!(
        service.drop_user(id(1), id(4)),
        RosterChanges::default(),
        "a retry"
      );
    }

    fn reconcile(service: &CallService, report: &SfuRosterReport, now: Instant) -> RosterChanges {
      service.reconcile(report, now, UnixMillis(5_000))
    }
  }

  mod roster_reads {
    use std::sync::atomic::AtomicUsize;

    use axum::Json;
    use axum::extract::State;
    use axum::http::StatusCode;
    use axum::routing::get;

    use super::*;

    #[tokio::test]
    async fn a_failed_read_at_start_is_retried_and_tokens_name_the_running_call() {
      let sfu = FakeSfu::start(1).await;
      let service = service_with_sfu(&sfu.url);

      // The read at start finds the SFU busy; the background retry gets the roster.
      assert!(service.pull_and_adopt(Duration::from_secs(5)).await.is_err());
      let adopted = service.retry_adoption(&[Duration::ZERO]).await;

      assert!(matches!(&adopted, Some(changes) if changes.changes.len() == 1));
      let call = service.call_for(id(1), Instant::now(), || Ok(id(999))).unwrap();
      assert_eq!(call, id(100), "the join token names the running call");
    }

    #[tokio::test]
    async fn the_first_join_token_of_a_conversation_asks_the_sfu_first() {
      let sfu = FakeSfu::start(1).await;
      let service = service_with_sfu(&sfu.url);
      assert!(service.pull_and_adopt(Duration::from_secs(5)).await.is_err());

      // What `create_join_token` does for a conversation without a call.
      assert!(!service.has_call(id(1)));
      let adopted = service
        .pull_and_adopt(PRE_TOKEN_ROSTER_TIMEOUT)
        .await
        .unwrap()
        .unwrap();
      let call = service.call_for(id(1), Instant::now(), || Ok(id(999))).unwrap();

      assert_eq!(adopted.changes.len(), 1);
      assert_eq!(call, id(100));
      assert_eq!(sfu.reads(), 2);
    }

    #[tokio::test]
    async fn retries_stop_once_the_sfu_sent_a_roster_by_itself() {
      let sfu = FakeSfu::start(0).await;
      let service = service_with_sfu(&sfu.url);
      service.reconcile(
        &named_roster(600, 1, 100, &[entry(4, 40)]),
        Instant::now(),
        UnixMillis(5_000),
      );

      let adopted = service.retry_adoption(&[Duration::ZERO, Duration::ZERO]).await;

      assert_eq!(adopted, None);
      assert_eq!(sfu.reads(), 0);
    }

    /// The SFU's roster endpoint: refuses the first `failures` reads, then answers with call 100
    /// of conversation 1.
    struct FakeSfu {
      url: String,
      reads: Arc<AtomicUsize>,
    }

    impl FakeSfu {
      async fn start(failures: usize) -> Self {
        async fn roster(
          State((reads, failures)): State<(Arc<AtomicUsize>, usize)>,
        ) -> Result<Json<SfuRosterReport>, StatusCode> {
          if reads.fetch_add(1, Ordering::SeqCst) < failures {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
          }
          Ok(Json(named_roster(600, 1, 100, &[entry(4, 40), entry(5, 50)])))
        }
        let reads = Arc::new(AtomicUsize::new(0));
        let router = axum::Router::new()
          .route("/internal/v1/calls/roster", get(roster))
          .with_state((Arc::clone(&reads), failures));
        let listener = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0))
          .await
          .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self { url, reads }
      }

      fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
      }
    }

    fn service_with_sfu(url: &str) -> CallService {
      let mut config = config();
      config.maybe_sfu_control_url = Some(url.to_owned());
      config.maybe_sfu_registration_token = Some("sfu-token-for-tests".to_owned().into());
      CallService::new(&config, TEST_SIGNING_KEY_PEM.as_bytes()).unwrap()
    }
  }

  /// Issue #997: a 1:1 call ends for both when one side leaves.
  mod one_to_one {
    use super::*;

    /// Longer than the test config's `roster_grace_secs`: the SFU has settled.
    const SETTLED: u64 = 600;

    #[test]
    fn one_side_leaving_ends_the_call_for_both() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(5, 50), entry(5, 51)]);

      let change = service.participant_left(id(1), entry(4, 40));
      let LeaveChange::Updated(snapshot) = &change else {
        panic!("{change:?}");
      };

      assert_eq!(
        service.end_if_one_side_left(snapshot.conversation_id, snapshot.call_id),
        Some(LeaveChange::EndedForBoth {
          conversation_id: id(1),
          call_id: id(100),
          left_behind: vec![entry(5, 50), entry(5, 51)],
        })
      );
      assert_eq!(service.running_call(id(1)), None);
      assert!(!service.is_in_call(id(5)), "the other side is out of the roster");
      // Idempotent: the end is told once, and a late leave of the other side changes nothing.
      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
      assert_eq!(
        service.participant_left(id(1), entry(5, 50)),
        LeaveChange::Unchanged
      );
      // A roster from while the devices were leaving doesn't bring the call back.
      let late = named_roster(SETTLED, 1, 100, &[entry(5, 50)]);
      assert!(
        service
          .reconcile(&late, Instant::now(), UnixMillis(5_000))
          .changes
          .is_empty()
      );
      assert_eq!(service.running_call(id(1)), None);
    }

    #[test]
    fn a_device_of_a_side_that_is_still_in_the_call_leaving_ends_nothing() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(4, 41), entry(5, 50)]);

      service.participant_left(id(1), entry(4, 40));

      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
      assert!(service.is_device_in_call(id(1), id(50)));
    }

    #[test]
    fn a_group_call_of_three_goes_on_when_one_leaves() {
      let service = service();
      running_call(
        &service,
        id(1),
        id(100),
        &[entry(4, 40), entry(5, 50), entry(6, 60)],
      );
      service.note_kind(id(1), id(100), ConversationKind::Group);
      service.participant_left(id(1), entry(4, 40));
      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
      service.participant_left(id(1), entry(5, 50));
      assert_eq!(
        service.end_if_one_side_left(id(1), id(100)),
        None,
        "not a 1:1 call, even with one person left"
      );
      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    #[test]
    fn a_call_nobody_answered_ends_only_with_its_caller() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(4, 41)]);

      service.participant_left(id(1), entry(4, 40));

      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    #[test]
    fn a_call_that_moved_into_a_meeting_is_not_ended_for_both() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(5, 50)]);
      service.mark_moved(id(1), id(2)).unwrap();

      service.participant_left(id(1), entry(4, 40));

      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
    }

    #[test]
    fn another_call_of_the_conversation_is_not_ended() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(5, 50)]);
      service.participant_left(id(1), entry(4, 40));

      assert_eq!(service.end_if_one_side_left(id(1), id(999)), None);
      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    /// The SFU keeps a lost device's slot through its reconnect grace period and only then reports
    /// it gone; while it is fresh from a restart, the server gives devices the same time.
    #[test]
    fn a_short_drop_that_reconnects_in_time_ends_nothing() {
      let service = service();
      let start = Instant::now();
      direct_call(&service, &[entry(4, 40), entry(5, 50)]);

      let meanwhile = service.reconcile(&roster(2, &[(100, &[entry(5, 50)])]), start, UnixMillis(5_000));
      assert!(meanwhile.changes.is_empty());
      service.participant_joined(id(1), entry(4, 40), false, UnixMillis(2));
      let back = service.reconcile(
        &roster(32, &[(100, &[entry(4, 40), entry(5, 50)])]),
        start + Duration::from_secs(30),
        UnixMillis(35_000),
      );

      assert!(back.changes.is_empty());
      assert_eq!(service.end_if_one_side_left(id(1), id(100)), None);
      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    /// The roster expiry drops a device the SFU no longer has; one side gone ends the call.
    #[test]
    fn a_side_the_sfu_dropped_ends_the_call() {
      let service = service();
      direct_call(&service, &[entry(4, 40), entry(5, 50)]);

      let changes = service.reconcile(
        &roster(SETTLED, &[(100, &[entry(5, 50)])]),
        Instant::now(),
        UnixMillis(5_000),
      );

      assert!(matches!(&changes.changes[..], [LeaveChange::Updated(_)]));
      assert!(matches!(
        service.end_if_one_side_left(id(1), id(100)),
        Some(LeaveChange::EndedForBoth { left_behind, .. }) if left_behind == vec![entry(5, 50)]
      ));
    }

    /// Calls live in memory: a restarted server takes a 1:1 call over from the SFU, learns its
    /// kind from the database, and still ends it when one side leaves.
    #[test]
    fn a_call_taken_over_after_a_restart_still_ends_for_both() {
      let service = service();
      let changes = service.reconcile(
        &named_roster(SETTLED, 1, 100, &[entry(4, 40), entry(5, 50)]),
        Instant::now(),
        UnixMillis(5_000),
      );
      assert_eq!(changes.adopted, vec![(id(1), id(4))]);
      service.participant_left(id(1), entry(5, 50));
      assert_eq!(
        service.end_if_one_side_left(id(1), id(100)),
        None,
        "the kind is not known yet"
      );
      service.note_kind(id(1), id(100), ConversationKind::Dm);

      assert!(matches!(
        service.end_if_one_side_left(id(1), id(100)),
        Some(LeaveChange::EndedForBoth { left_behind, .. }) if left_behind == vec![entry(4, 40)]
      ));
    }

    /// A running direct message call `100` of conversation `1` with `entries` in it.
    fn direct_call(service: &CallService, entries: &[CallRosterEntry]) {
      running_call(service, id(1), id(100), entries);
      service.note_kind(id(1), id(100), ConversationKind::Dm);
    }
  }

  mod restarts {
    use super::*;

    #[test]
    fn a_restarted_server_takes_over_the_calls_the_sfu_runs() {
      // A fresh server: it lost every call.
      let service = service();
      let report = named_roster(600, 1, 100, &[entry(4, 40), entry(5, 50)]);

      let changes = service.reconcile(&report, Instant::now(), UnixMillis(5_000));

      let [LeaveChange::Updated(snapshot)] = &changes.changes[..] else {
        panic!("{changes:?}");
      };
      assert_eq!(snapshot.call_id, id(100));
      assert_eq!(snapshot.participants, vec![entry(4, 40), entry(5, 50)]);
      assert_eq!(
        (snapshot.started_by, snapshot.started_at),
        (id(4), UnixMillis(1234))
      );
      assert!(!snapshot.is_ringing, "nothing rings again");
      assert_eq!(changes.users, HashSet::from([id(4), id(5)]));
      assert!(changes.missing_joins.is_empty());
      assert_eq!(service.running_call(id(1)), Some(id(100)));
      // New joins get the running call, not a second one next to it.
      let call = service.call_for(id(1), Instant::now(), || Ok(id(999))).unwrap();
      assert_eq!(call, id(100));
      // The next roster changes nothing.
      let again = service.reconcile(&report, Instant::now(), UnixMillis(6_000));
      assert_eq!(again, RosterChanges::default());
    }

    #[test]
    fn a_join_token_handed_out_before_the_first_roster_gives_way_to_the_running_call() {
      let service = service();
      service.call_for(id(1), Instant::now(), || Ok(id(300))).unwrap();

      service.reconcile(
        &named_roster(600, 1, 100, &[entry(4, 40)]),
        Instant::now(),
        UnixMillis(5_000),
      );

      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    #[test]
    fn a_call_that_just_ended_is_not_taken_over_again() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40)]);
      let ended = service.participant_left(id(1), entry(4, 40));
      assert!(matches!(ended, LeaveChange::Ended { .. }));

      // Sent while the SFU was still closing the last connection.
      let changes = service.reconcile(
        &named_roster(600, 1, 100, &[entry(4, 40)]),
        Instant::now(),
        UnixMillis(5_000),
      );

      assert_eq!(changes, RosterChanges::default());
      assert_eq!(service.running_call(id(1)), None);
    }

    #[test]
    fn a_call_with_people_in_it_is_not_replaced() {
      let service = service();
      running_call(&service, id(1), id(100), &[entry(4, 40)]);
      let mut report = named_roster(600, 1, 200, &[entry(5, 50)]);
      report.calls.push(SfuCallRoster {
        call_id: id(100),
        participants: vec![entry(4, 40)],
        maybe_conversation_id: Some(id(1)),
        maybe_started_by: Some(id(4)),
        maybe_started_at: None,
      });

      let changes = service.reconcile(&report, Instant::now(), UnixMillis(5_000));

      assert_eq!(changes.changes, Vec::new());
      assert_eq!(service.running_call(id(1)), Some(id(100)));
    }

    #[test]
    fn an_sfu_restart_keeps_the_call_while_everybody_rejoins() {
      let service = service();
      let start = Instant::now();
      running_call(&service, id(1), id(100), &[entry(4, 40), entry(5, 50)]);
      let mut all = Vec::new();

      // The restarted SFU's first roster: its rooms are gone, nobody has rejoined yet.
      all.extend(service.reconcile(&roster(0, &[]), start, UnixMillis(0)).changes);
      // Both rejoin within seconds; the server already has them.
      let first = service.participant_joined(id(1), entry(4, 40), false, UnixMillis(3));
      let second = service.participant_joined(id(1), entry(5, 50), false, UnixMillis(4));
      for later in [30, 61, 120] {
        let report = named_roster(later, 1, 100, &[entry(4, 40), entry(5, 50)]);
        all.extend(
          service
            .reconcile(&report, start + Duration::from_secs(later), UnixMillis(0))
            .changes,
        );
      }

      assert_eq!((first, second), (JoinChange::Unchanged, JoinChange::Unchanged));
      assert_eq!(all, Vec::new(), "no update, no call_end");
      assert_eq!(service.running_call(id(1)), Some(id(100)));
      assert!(service.is_device_in_call(id(1), id(40)) && service.is_device_in_call(id(1), id(50)));
    }
  }

  /// A running call of `conversation` with `entries` in it.
  fn running_call(
    service: &CallService,
    conversation: ConversationId,
    call: CallId,
    entries: &[CallRosterEntry],
  ) {
    service
      .call_for(conversation, Instant::now(), || Ok(call))
      .unwrap();
    for (at, entry) in entries.iter().enumerate() {
      service.participant_joined(conversation, *entry, false, UnixMillis(at as i64));
    }
  }

  /// An SFU roster of calls without the details only newer join tokens carry.
  fn roster(uptime_secs: u64, calls: &[(i64, &[CallRosterEntry])]) -> SfuRosterReport {
    SfuRosterReport {
      uptime_secs,
      calls: calls
        .iter()
        .map(|(call_id, participants)| SfuCallRoster {
          call_id: id(*call_id),
          participants: participants.to_vec(),
          maybe_conversation_id: None,
          maybe_started_by: None,
          maybe_started_at: None,
        })
        .collect(),
    }
  }

  /// A roster of the call `call` of `conversation`, with what the SFU knows from the join tokens.
  fn named_roster(
    uptime_secs: u64,
    conversation: i64,
    call: i64,
    entries: &[CallRosterEntry],
  ) -> SfuRosterReport {
    SfuRosterReport {
      uptime_secs,
      calls: vec![SfuCallRoster {
        call_id: id(call),
        participants: entries.to_vec(),
        maybe_conversation_id: Some(id(conversation)),
        maybe_started_by: Some(id(4)),
        maybe_started_at: Some(UnixMillis(1234)),
      }],
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
      roster_grace_secs: 60,
      roster_timeout_secs: 120,
      max_devices_per_user_per_call: 3,
    }
  }

  fn entry(user: i64, device: i64) -> CallRosterEntry {
    CallRosterEntry {
      user_id: id(user),
      device_id: id(device),
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
