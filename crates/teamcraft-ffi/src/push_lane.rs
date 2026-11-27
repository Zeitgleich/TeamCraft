//! The app's pushes beside the engine (issue #2441). A wake-up has the push service's few seconds
//! (FCM about 10 s; the Android app hands over to expedited work at 8 s), while a job on the engine
//! can take seconds or, on a bad day, minutes: a push queued behind one showed late or as a
//! placeholder, and a push that waited there for its token and its fetch held up `start` in turn.
//!
//! So the app's push is opened on a lane of its own: the store through a read connection of its
//! own, the token and the server on the HTTP runtime ([`run_request`]). The engine is asked only for
//! what needs the app's client (catching the conversation up, a late look's read position), on a
//! queue it takes before its other jobs, and only if it starts on it within
//! [`PUSH_ENGINE_START_WAIT`]: else the push goes on without it and the app tries again later, as
//! after any timeout. Its writes (the diagnostics, what was shown) wait for the writer at most
//! [`PUSH_WRITE_WAIT`]. Same rule as #2401: no push waits on the engine longer than that bound.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_client_api::notify::{MentionRules, is_mentioned};
use teamcraft_core::api::AccessTokenSource as _;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::notified;
use teamcraft_core::notify_rules::load_mention_preferences;
use teamcraft_core::people_names::shown_name;
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{PushPreview, is_read, push_preview};
use teamcraft_core::records;
use teamcraft_core::store::Table;
use teamcraft_proto::{PushWakeUp, UserSummary};
use tokio::sync::mpsc;

use crate::engine::{
  Api, ForeignTokens, Job, PUSH_FETCH_BUDGET, PUSH_PREVIEW_BUDGET, PushAccess, PushFetch, PushOpening,
  PushReader, PushTimes, Store, timed_out,
};
use crate::error::TeamCraftError;
use crate::http_fetch::run_request;
use crate::logging::LIFECYCLE;
use crate::server_pool::ServerPool;
use crate::store_threads::StoreHold;
use crate::types::PushNotification;

/// How long a push waits for the engine to start on its part. The engine takes pushes before its
/// other jobs, so this is the wait for the job that runs: past it the push goes on without the
/// catch-up and still has the fetch's time ([`PUSH_FETCH_BUDGET`]) within the app's 8 s.
pub(crate) const PUSH_ENGINE_START_WAIT: Duration = Duration::from_millis(1500);
/// How long a push waits for the store's writer (the diagnostics, what was shown): a group of
/// writes on the engine holds it. Past it the notification shows, and the writes follow.
const PUSH_WRITE_WAIT: Duration = Duration::from_secs(1);
/// What a push gives an engine part that started past its deadline, before it goes on without it:
/// the catch-up is cut at the deadline, a thread that doesn't answer then is stuck.
const PUSH_ENGINE_GRACE: Duration = Duration::from_secs(1);

/// The app's pushes: a view of the store with a read connection of its own, the app's tokens and
/// the HTTP runtime's connections to the server.
#[derive(Clone)]
pub(crate) struct PushLane {
  store: Store,
  tokens: ForeignTokens,
  http: Arc<ServerPool>,
  sync: SyncRunning,
  /// One push's writes at a time: whether a message was shown before is read, then written.
  writes: Arc<Mutex<()>>,
  /// Suspending waits for a push that still reads or writes (#2179). Last: let go after the store.
  _hold: StoreHold,
}

/// Whether the engine runs a sync loop, readable off the engine thread.
#[derive(Clone, Default)]
pub(crate) struct SyncRunning(Arc<AtomicBool>);

/// Held by a running sync loop: dropped when it ends, however it ends.
pub(crate) struct SyncRunningMark(Arc<AtomicBool>);

/// What a push asks of the engine ([`crate::engine::Engine::push_on_engine`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PushEnginePart {
  pub(crate) catches_up: bool,
  pub(crate) refreshes_read_position: bool,
}

/// What the engine's part of a push gave.
pub(crate) struct PushEngineAnswer {
  pub(crate) preview: PushPreview,
  pub(crate) catch_up: Duration,
}

/// How a push fared with the engine, for the `push opened` line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PushEngineWait {
  /// The push needed nothing of the engine.
  NotAsked,
  /// All of the push ran on the engine (the notification extension).
  OnEngine,
  /// The engine did its part in time.
  Answered,
  /// The engine didn't start on it in time: the push went on without it.
  Busy,
  /// The engine started but didn't answer within the push's time.
  Stuck,
  /// The engine stopped or its part failed.
  Failed,
}

/// Whether the engine started a push's part or the push gave up waiting first; whichever is first
/// wins, so a part the push went on without never runs.
#[derive(Clone, Default)]
struct EngineTurn(Arc<AtomicU8>);

impl PushLane {
  pub(crate) fn new(
    store: Store,
    tokens: ForeignTokens,
    http: Arc<ServerPool>,
    sync: SyncRunning,
    hold: StoreHold,
  ) -> Self {
    Self {
      store,
      tokens,
      http,
      sync,
      writes: Arc::default(),
      _hold: hold,
    }
  }

  /// The notification of the app for a push payload, opened on this lane, on the HTTP runtime: the
  /// caller's thread and the engine wait for none of it.
  pub(crate) async fn notification(
    &self,
    push_jobs: mpsc::UnboundedSender<Job>,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
    opening: PushOpening,
  ) -> Result<PushNotification, TeamCraftError> {
    let lane = self.clone();
    run_request(async move {
      let (notification, _engine) = lane.open(push_jobs, payload, utc_offset_minutes, opening).await?;
      Ok(notification)
    })
    .await
    .map_err(TeamCraftError::Core)?
  }

  /// The notification for a push, and how it fared with the engine.
  pub(crate) async fn open(
    self,
    push_jobs: mpsc::UnboundedSender<Job>,
    payload: Vec<u8>,
    utc_offset_minutes: i16,
    opening: PushOpening,
  ) -> Result<(PushNotification, PushEngineWait), TeamCraftError> {
    let _step = teamcraft_core::timing::step("push_notification");
    let opened_at = Instant::now();
    let started = tokio::time::Instant::now();
    let mut times = PushTimes::default();
    let (mut preview, access) = if opening == PushOpening::StoreOnly {
      (
        push_preview::<Api>(&self.store, None, &payload).await?,
        PushAccess::NotAsked,
      )
    } else {
      store_or_server_preview(
        &self.store,
        &self.http,
        &self.tokens,
        &payload,
        started,
        self.sync.is_running(),
        &mut times,
      )
      .await?
    };
    let mut engine_wait = PushEngineWait::NotAsked;
    if let Some(part) = PushEnginePart::of(&preview, access, opening, self.sync.is_running()) {
      let deadline = started + PUSH_PREVIEW_BUDGET;
      (preview, engine_wait) =
        push_part_on_engine(&push_jobs, payload, preview, part, deadline, &mut times).await;
    }
    if opening != PushOpening::StoreOnly {
      log_push_opened(
        opening,
        &preview,
        opened_at,
        &times,
        engine_wait,
        !self.sync.is_running(),
      );
    }
    let is_ring_answered =
      is_ring_answered(&self.http.api(), &preview.wake_up, opening, PushReader::App).await;
    let now_ms = SystemClock.now_ms();
    let is_duplicate = self.record(&preview, opening, now_ms).await;
    let notification = notification_of(
      &self.store,
      preview,
      is_duplicate,
      is_ring_answered,
      utc_offset_minutes,
      now_ms,
    )?;
    Ok((notification, engine_wait))
  }

  /// [`record_push`] off the runtime's threads, waiting for it at most [`PUSH_WRITE_WAIT`]. A
  /// push whose writes wait longer shows as new: once twice is better than a missed message.
  async fn record(&self, preview: &PushPreview, opening: PushOpening, now_ms: i64) -> bool {
    let (lane, preview) = (self.clone(), preview.clone());
    let recording = tokio::task::spawn_blocking(move || {
      // Plain data behind it: a panic elsewhere while it was held leaves nothing half-changed.
      let _turn = lane.writes.lock().unwrap_or_else(PoisonError::into_inner);
      record_push(&lane.store, &preview, opening, now_ms)
    });
    match tokio::time::timeout(PUSH_WRITE_WAIT, recording).await {
      Ok(Ok(Ok(is_duplicate))) => is_duplicate,
      Ok(Ok(Err(error))) => {
        tracing::debug!(%error, "push not recorded");
        false
      },
      Ok(Err(_ended)) => false,
      Err(_elapsed) => {
        tracing::info!(target: LIFECYCLE, "push writes wait for the store");
        false
      },
    }
  }
}

/// The engine's part of a push, if it starts within [`PUSH_ENGINE_START_WAIT`] and answers within
/// the push's time; `preview` as it was otherwise.
async fn push_part_on_engine(
  push_jobs: &mpsc::UnboundedSender<Job>,
  payload: Vec<u8>,
  preview: PushPreview,
  part: PushEnginePart,
  deadline: tokio::time::Instant,
  times: &mut PushTimes,
) -> (PushPreview, PushEngineWait) {
  let turn = EngineTurn::default();
  let engine_turn = turn.clone();
  let engine_preview = preview.clone();
  let (job, mut answer) = crate::engine::timed_job(move |engine| {
    Box::pin(async move {
      if !engine_turn.start() {
        // The push went on without it.
        return Ok(None);
      }
      let answer = engine
        .push_on_engine(&payload, engine_preview, part, deadline)
        .await?;
      Ok(Some(answer))
    })
  });
  if push_jobs.send(job).is_err() {
    return (preview, PushEngineWait::Failed);
  }
  let answered = match tokio::time::timeout(PUSH_ENGINE_START_WAIT, &mut answer).await {
    Ok(answered) => answered,
    Err(_elapsed) if turn.withdraw() => return (preview, PushEngineWait::Busy),
    Err(_elapsed) => match tokio::time::timeout_at(deadline + PUSH_ENGINE_GRACE, answer).await {
      Ok(answered) => answered,
      Err(_elapsed) => return (preview, PushEngineWait::Stuck),
    },
  };
  match answered {
    Ok(Ok(Some(answer))) => {
      times.catch_up = answer.catch_up;
      (answer.preview, PushEngineWait::Answered)
    },
    Ok(Ok(None) | Err(_)) | Err(_) => (preview, PushEngineWait::Failed),
  }
}

/// The store's preview of a wake-up, or the server's when the store doesn't have the message: a
/// message the sync already brought needs no token and no connection, which after a pause costs
/// the radio's wake-up, DNS, TCP and TLS (issue #1662). The token and the fetch share
/// [`PUSH_FETCH_BUDGET`] from `started`: a slow token refresh leaves less time for the fetch
/// instead of adding to it. Without a token in time the store's preview stays, and the app tries
/// again later.
pub(crate) async fn store_or_server_preview(
  store: &Store,
  http: &ServerPool,
  tokens: &ForeignTokens,
  payload: &[u8],
  started: tokio::time::Instant,
  is_sync_running: bool,
  times: &mut PushTimes,
) -> Result<(PushPreview, PushAccess), TeamCraftError> {
  let stored = push_preview::<Api>(store, None, payload).await?;
  match PushFetch::of(stored.needs_server(), is_sync_running) {
    PushFetch::NotNeeded => return Ok((stored, PushAccess::NotAsked)),
    PushFetch::LeftToSync => {
      tracing::info!(target: LIFECYCLE, "push left to the running sync");
      return Ok((stored, PushAccess::NotAsked));
    },
    PushFetch::Fetch => {},
  }
  let fetch_deadline = started + PUSH_FETCH_BUDGET;
  let auth_started = tokio::time::Instant::now();
  let access = match tokio::time::timeout_at(fetch_deadline, tokens.fresh_token()).await {
    Ok(Ok(_)) => PushAccess::Supplied,
    Ok(Err(_)) => PushAccess::Missing,
    Err(_elapsed) => PushAccess::TimedOut,
  };
  times.auth_wait = auth_started.elapsed();
  if access.is_missing() {
    // Either way the store's preview stays: a refresh that failed for now (offline, Doze, the
    // identity provider slow) is retried later, and the app signs out itself when its session ends.
    tracing::info!(target: LIFECYCLE, ?access, "no access token for the push");
    let preview = if access == PushAccess::TimedOut {
      timed_out(stored)
    } else {
      stored
    };
    return Ok((preview, access));
  }
  let api = http.api();
  // Cancel-safe: the preview only reads the store.
  let fetch_started = tokio::time::Instant::now();
  let fetched = tokio::time::timeout_at(fetch_deadline, push_preview(store, Some(&api), payload)).await;
  times.fetch = fetch_started.elapsed();
  let preview = match fetched {
    Ok(result) => result?,
    Err(_elapsed) => timed_out(stored),
  };
  Ok((preview, access))
}

/// Whether a ring looked at late was answered on another device, as the server's call history says
/// (issue #1257). Only a late look of the app asks: a ring opened as it arrives rings, and the
/// answer costs up to [`PUSH_FETCH_BUDGET`]. Best effort: `false`, a missed call, when the server
/// doesn't answer in time.
pub(crate) async fn is_ring_answered(
  api: &Api,
  wake_up: &PushWakeUp,
  opening: PushOpening,
  reader: PushReader,
) -> bool {
  let Some(call) = wake_up.maybe_call_id else {
    return false;
  };
  if !opening.asks_how_a_ring_ended(reader, &wake_up.kind) {
    return false;
  }
  let answered = tokio::time::timeout(PUSH_FETCH_BUDGET, push_guard::is_ring_answered(api, call)).await;
  if !matches!(answered, Ok(Ok(_))) {
    tracing::debug!("call history not read for a late ring");
  }
  matches!(answered, Ok(Ok(true)))
}

/// What a push leaves in the store: that it came (not for a retry, which is the same push again
/// and would skew the received count and the delay), that its message was shown, and what it
/// showed, for opening the conversation from it (issue #1065). Whether the message was shown
/// before.
pub(crate) fn record_push(
  store: &Store,
  preview: &PushPreview,
  opening: PushOpening,
  now_ms: i64,
) -> Result<bool, TeamCraftError> {
  if opening == PushOpening::Received {
    push_guard::record_received(store, &preview.wake_up, now_ms);
  }
  let is_duplicate = push_guard::is_duplicate(store, &preview.wake_up)?;
  if let Err(error) = notified::remember(store, preview) {
    tracing::debug!(%error, "notified message not kept");
  }
  Ok(is_duplicate)
}

/// The notification for `preview`, checked against what the user read, mutes and DND, and the
/// user's mention rules.
pub(crate) fn notification_of(
  store: &Store,
  preview: PushPreview,
  is_duplicate: bool,
  is_ring_answered: bool,
  utc_offset_minutes: i16,
  now_ms: i64,
) -> Result<PushNotification, TeamCraftError> {
  let is_read = is_read(store, &preview.wake_up);
  let maybe_suppression = push_guard::local_suppression(store, &preview.wake_up, now_ms, utc_offset_minutes)?;
  let mut notification: PushNotification = preview.into();
  notification.is_duplicate = is_duplicate;
  notification.is_read = is_read;
  notification.maybe_suppression = maybe_suppression.map(Into::into);
  notification.is_ring_answered = is_ring_answered;
  if !notification.is_mention
    && let (Some(text), Some(me)) = (&notification.maybe_text, own_names(store))
  {
    // Read from the store, where the actions' thread keeps them (issue #2225); a broken record
    // only costs the preferences.
    let preferences = load_mention_preferences(store).unwrap_or_default();
    notification.is_mention = is_mentioned(
      text,
      &MentionRules {
        username: &me.username,
        display_name: &me.display_name,
        first_name_is_mention: preferences.first_name_is_mention,
        channel_mentions_notify: preferences.channel_mentions_notify,
        keywords: &preferences.keywords,
      },
    );
  }
  Ok(notification)
}

/// The names a mention of the user would use, as the store knows them; `None` before enrollment.
fn own_names(store: &Store) -> Option<OwnNames> {
  let account = records::load_account(store).ok().flatten()?;
  let maybe_summary: Option<UserSummary> =
    records::get_json(store, Table::Users, &account.user_id.to_string())
      .ok()
      .flatten();
  // As the chat backend names the user: empty where the store doesn't know them yet.
  Some(maybe_summary.map_or_else(OwnNames::default, |summary| OwnNames {
    username: summary.maybe_username.clone().unwrap_or_default(),
    display_name: shown_name(&summary),
  }))
}

/// The user's own names.
#[derive(Default)]
struct OwnNames {
  username: String,
  display_name: String,
}

/// Why a notification said less, and how long it took: IDs and reasons only (security.md), so an
/// exported diagnostics log shows how often each path is taken.
pub(crate) fn log_push_opened(
  opening: PushOpening,
  preview: &PushPreview,
  opened_at: Instant,
  times: &PushTimes,
  engine: PushEngineWait,
  is_client_idle: bool,
) {
  tracing::info!(
    target: LIFECYCLE,
    ?opening,
    gap = ?preview.maybe_gap,
    ran_ms = opened_at.elapsed().as_millis(),
    auth_wait_ms = times.auth_wait.as_millis(),
    fetch_ms = times.fetch.as_millis(),
    catch_up_ms = times.catch_up.as_millis(),
    probe_ms = times.probe.as_millis(),
    ?engine,
    is_client_idle,
    "push opened"
  );
}

impl SyncRunning {
  pub(crate) fn is_running(&self) -> bool {
    self.0.load(Ordering::Acquire)
  }

  /// Marks a sync loop as running until the mark goes.
  pub(crate) fn mark(&self) -> SyncRunningMark {
    self.0.store(true, Ordering::Release);
    SyncRunningMark(self.0.clone())
  }
}

impl Drop for SyncRunningMark {
  fn drop(&mut self) {
    self.0.store(false, Ordering::Release);
  }
}

impl PushEnginePart {
  /// What `preview` needs of the engine: a catch-up of a message it couldn't read (not without a
  /// token: every request would ask the app for one again and fail), the read position of a late
  /// look. Nothing while a sync loop runs: it brings the message itself and keeps the read position
  /// fresh, and the client isn't free.
  fn of(
    preview: &PushPreview,
    access: PushAccess,
    opening: PushOpening,
    is_sync_running: bool,
  ) -> Option<Self> {
    if is_sync_running || opening == PushOpening::StoreOnly {
      return None;
    }
    let part = Self {
      catches_up: PushReader::App.catches_up(preview.needs_catch_up(), true) && !access.is_missing(),
      refreshes_read_position: opening.refreshes_read_position(PushReader::App, true),
    };
    (part.catches_up || part.refreshes_read_position).then_some(part)
  }
}

impl EngineTurn {
  const QUEUED: u8 = 0;
  const STARTED: u8 = 1;
  const WITHDRAWN: u8 = 2;

  /// The engine starts on the part: `false` if the push went on without it.
  fn start(&self) -> bool {
    self.take(Self::STARTED)
  }

  /// The push goes on without the engine: `false` if the engine started on it first.
  fn withdraw(&self) -> bool {
    self.take(Self::WITHDRAWN)
  }

  fn take(&self, state: u8) -> bool {
    self
      .0
      .compare_exchange(Self::QUEUED, state, Ordering::AcqRel, Ordering::Acquire)
      .is_ok()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_first_of_start_and_withdraw_wins() {
    let turn = EngineTurn::default();
    assert!(turn.withdraw());
    assert!(!turn.start(), "a withdrawn part never runs");

    let turn = EngineTurn::default();
    assert!(turn.start());
    assert!(!turn.withdraw(), "a started part is waited for");
  }

  #[test]
  fn a_sync_loop_counts_as_running_until_its_mark_goes() {
    let sync = SyncRunning::default();
    assert!(!sync.is_running());
    let mark = sync.mark();
    assert!(sync.is_running());
    drop(mark);
    assert!(!sync.is_running());
  }
}
