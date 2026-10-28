use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_proto::{CustomStatus, DeviceActivity, DeviceId, GatewayEvent, Id, PresenceState, UserId};
use time::OffsetDateTime;
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tracing::{debug, warn};

/// Frames queued per connection before it is considered too slow and dropped (it can resume).
const CHANNEL_CAPACITY: usize = 1024;
/// The sequence number of events that are neither numbered nor buffered (typing): they only matter
/// to whoever is connected right now. Clients keep the highest number they saw, so `0` never moves
/// their resume position (docs/gateway.md, "Resume").
pub const EPHEMERAL_SEQ: u64 = 0;
/// Locks that serialize presence refreshes: users map onto them by ID, so memory stays fixed
/// however many users there are, and two users rarely share one.
const PRESENCE_REFRESH_LOCKS: usize = 64;

/// What was last broadcast about a user's presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceSnapshot {
  pub state: PresenceState,
  pub maybe_status: Option<CustomStatus>,
}

impl Default for PresenceSnapshot {
  fn default() -> Self {
    Self {
      state: PresenceState::Offline,
      maybe_status: None,
    }
  }
}

/// Registry of gateway sessions and their event buffers.
///
/// A session outlives its connection for `resume_window`: events keep being numbered and buffered,
/// so a client that reconnects with its last seen sequence number receives exactly what it missed.
/// State is in memory and per server process; running several instances needs a shared bus
/// (Postgres `LISTEN/NOTIFY`), which is not part of M1.
///
/// Memory: each event is serialized once and shared ([`SharedEvent`]) by every session's buffer
/// and queue; a buffer holds at most `buffer_events` events and `buffer_bytes` of them, and a
/// connection that falls behind its buffer is dropped (issue #156). All buffers together reference
/// at most `max_total_buffer_bytes`; beyond it the slowest sessions go first (see
/// [`Hub::enforce_total_budget`]).
pub struct Hub {
  options: HubOptions,
  inner: Mutex<Inner>,
  /// Per user with held phone pushes: woken when a desktop or web session of that user stops being
  /// attentive (the held pushes go out then). Only that user's waiter wakes. One entry per user who
  /// ever had a held push since the start, so bounded by the users.
  attention_watchers: Mutex<HashMap<UserId, Arc<Notify>>>,
  /// One presence refresh per user at a time (see [`Hub::presence_refresh_lock`]).
  presence_refresh_locks: Vec<tokio::sync::Mutex<()>>,
}

#[derive(Debug, Clone, Copy)]
pub struct HubOptions {
  pub resume_window: Duration,
  pub buffer_events: usize,
  /// Bytes of events one session's resume buffer may reference. Events are shared, so this bounds
  /// what a single stalled or disconnected session keeps alive, not a copy per session.
  pub buffer_bytes: usize,
  /// Bytes all resume buffers of the process may reference together (counted per session, so a
  /// shared event counts once for every session that holds it: the real memory is at most this).
  pub max_total_buffer_bytes: usize,
  /// Sessions one device may have; opening more drops the oldest (waiting ones first).
  pub max_sessions_per_device: usize,
  pub max_sessions_per_user: usize,
  /// How long an "attentive" report counts without being repeated.
  pub attention_lease: Duration,
  /// How long the user's own activity on a desktop or web session (input it reported, a message
  /// sent from it) holds phone pushes back without an attentive report (notifications.md,
  /// "Attention"). `ZERO`: only attention counts.
  pub desktop_activity_window: Duration,
}

/// A connection attached to a session.
pub struct Attached {
  pub session_id: String,
  /// Which connection of the session this is: [`Hub::detach`] only detaches the current one, so a
  /// late detach of a replaced (half-open) connection leaves a resumed session alone.
  pub connection_id: u64,
  /// Buffered events the client missed, to be sent before anything from [`Self::next_outgoing`].
  pub replay: Vec<Outgoing>,
  receiver: mpsc::Receiver<Outgoing>,
  link: Arc<ConnectionLink>,
}

/// An event frame on its way to one connection.
#[derive(Debug, Clone)]
pub struct Outgoing {
  /// [`EPHEMERAL_SEQ`] for events that are not buffered.
  pub seq: u64,
  pub event: Arc<SharedEvent>,
}

/// A gateway event serialized once, shared by every session that gets it.
#[derive(Debug, PartialEq, Eq)]
pub struct SharedEvent {
  json: Box<str>,
}

/// What the hub and a connection task both see of one connection.
#[derive(Debug)]
struct ConnectionLink {
  /// The highest sequence number the peer has taken.
  delivered_seq: AtomicU64,
  /// The hub dropped the connection (too slow, replaced, revoked): stop at the next frame.
  is_dropped: AtomicBool,
}

/// The hub's side of a live connection. Dropping it (the session was removed, the connection was
/// replaced or was too slow) tells the connection task to stop, which frees its queue.
struct Connection {
  id: u64,
  sender: mpsc::Sender<Outgoing>,
  link: Arc<ConnectionLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResumeError {
  #[error("unknown or expired session")]
  Unknown,
  #[error("session belongs to someone else")]
  NotOwner,
  #[error("events were dropped from the buffer")]
  Gap,
}

/// Which kinds of device of a user are connected and in use.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActiveDevices {
  /// A phone or tablet has the app in the foreground.
  pub is_mobile_active: bool,
  /// The user is at a desktop or web session right now: looking at it, or using it (notifications.md,
  /// "Attention").
  pub is_desktop_attentive: bool,
}

/// What a desktop or web session said about the user's attention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attention {
  /// An older client that never reports attention: its activity stands in for it.
  NeverReported,
  /// Attentive until the lease runs out.
  Attentive { until: Instant },
  /// Not looking at the session since then; activity after that still counts (the user works at
  /// the computer in another window, or the webview reports focus wrongly).
  Away { since: Instant },
}

/// What the hub knows about a session's activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionActivity {
  Active,
  /// The device said it is idle: only the device itself ends this.
  Idle,
  /// The device said it is active but showed no sign of life for the workspace's away timeout. Any
  /// sign of life (input, typing, an attentive report) ends this at once.
  TimedOut,
}

/// What a user's devices currently report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceAggregate {
  /// At least one device is active.
  Active,
  /// Devices are connected, none is active.
  Idle,
  NoDevice,
}

#[derive(Default)]
struct Inner {
  sessions: HashMap<String, Session>,
  by_user: HashMap<UserId, HashSet<String>>,
  last_presence: HashMap<UserId, PresenceSnapshot>,
  /// What the user's devices reported at the last presence refresh.
  last_aggregate: HashMap<UserId, DeviceAggregate>,
  next_created_seq: u64,
  next_connection_id: u64,
  /// Sum of every session's `buffer_bytes`.
  total_buffer_bytes: usize,
  /// Busy periods from the users' calendars, as last polled.
  busy: HashMap<UserId, Vec<(OffsetDateTime, OffsetDateTime)>>,
}

struct Session {
  /// Order of creation, to drop the oldest first.
  created_seq: u64,
  user: UserId,
  device: DeviceId,
  activity: SessionActivity,
  /// When the device last said it is active (or did something that shows it is).
  last_active_at: Instant,
  attention: Attention,
  next_seq: u64,
  buffer: VecDeque<(u64, Arc<SharedEvent>)>,
  /// Bytes of the events in `buffer`.
  buffer_bytes: usize,
  maybe_connection: Option<Connection>,
  maybe_detached_at: Option<Instant>,
  /// The identity provider's session the connection authenticated with.
  maybe_oidc_session: Option<String>,
}

impl Hub {
  pub fn new(options: HubOptions) -> Self {
    Self {
      options,
      inner: Mutex::new(Inner::default()),
      attention_watchers: Mutex::default(),
      presence_refresh_locks: (0..PRESENCE_REFRESH_LOCKS)
        .map(|_| tokio::sync::Mutex::new(()))
        .collect(),
    }
  }

  pub fn open_session(&self, user: UserId, device: DeviceId, activity: DeviceActivity) -> Attached {
    let session_id = new_session_id();
    let mut inner = self.lock();
    let (connection, attached) = new_connection(&mut inner.next_connection_id, &session_id, Vec::new(), 0);
    inner.next_created_seq += 1;
    let session = Session {
      created_seq: inner.next_created_seq,
      user,
      device,
      activity: SessionActivity::reported(&activity),
      last_active_at: Instant::now(),
      attention: Attention::NeverReported,
      next_seq: 1,
      buffer: VecDeque::new(),
      buffer_bytes: 0,
      maybe_connection: Some(connection),
      maybe_detached_at: None,
      maybe_oidc_session: None,
    };
    inner.by_user.entry(user).or_default().insert(session_id.clone());
    inner.sessions.insert(session_id, session);
    self.evict_excess_sessions(&mut inner, user, device);
    attached
  }

  /// Attaches a new connection to an existing session and returns what it missed after `last_seq`.
  pub fn resume_session(
    &self,
    session_id: &str,
    user: UserId,
    device: DeviceId,
    last_seq: u64,
  ) -> Result<Attached, ResumeError> {
    let mut inner = self.lock();
    let inner = &mut *inner;
    let session = inner.sessions.get_mut(session_id).ok_or(ResumeError::Unknown)?;
    if session.user != user || session.device != device {
      return Err(ResumeError::NotOwner);
    }
    if session
      .maybe_detached_at
      .is_some_and(|at| at.elapsed() > self.options.resume_window)
    {
      return Err(ResumeError::Unknown);
    }
    // Sequence numbers start at 1; the oldest buffered event must directly follow `last_seq`.
    let oldest_available = session.buffer.front().map_or(session.next_seq, |(seq, _)| *seq);
    if last_seq + 1 < oldest_available || last_seq >= session.next_seq {
      return Err(ResumeError::Gap);
    }

    let replay: Vec<Outgoing> = session
      .buffer
      .iter()
      .filter(|(seq, _)| *seq > last_seq)
      .map(|(seq, event)| Outgoing {
        seq: *seq,
        event: event.clone(),
      })
      .collect();
    let (connection, attached) = new_connection(&mut inner.next_connection_id, session_id, replay, last_seq);
    // The previous connection (often a half-open one the client gave up on) is replaced: its
    // task stops at its next frame.
    session.maybe_connection = Some(connection);
    session.maybe_detached_at = None;
    Ok(attached)
  }

  /// The connection `connection_id` is gone; the session stays resumable for the resume window.
  /// Nothing happens if the session has another connection by now (the client resumed on a new
  /// one before the old task noticed its socket was dead).
  pub fn detach(&self, session_id: &str, connection_id: u64) {
    let maybe_user = {
      let mut inner = self.lock();
      let Some(session) = inner.sessions.get_mut(session_id) else {
        return;
      };
      if session
        .maybe_connection
        .as_ref()
        .is_some_and(|connection| connection.id != connection_id)
      {
        debug!("a replaced gateway connection ended; the session stays attached");
        return;
      }
      let now = Instant::now();
      session.maybe_connection = None;
      if session.maybe_detached_at.is_none() {
        session.maybe_detached_at = Some(now);
      }
      session.attention = Attention::Away { since: now };
      Some(session.user)
    };
    if let Some(user) = maybe_user {
      self.announce_attention_lost(user);
    }
  }

  /// Keeps the number of sessions of the device and of the user within the options by dropping
  /// the oldest, those waiting to be resumed first. Dropping a session closes its connection.
  fn evict_excess_sessions(&self, inner: &mut Inner, user: UserId, device: DeviceId) {
    for (limit, only_device) in [
      (self.options.max_sessions_per_device, Some(device)),
      (self.options.max_sessions_per_user, None),
    ] {
      loop {
        let mut candidates: Vec<(bool, u64, String)> = inner
          .by_user
          .get(&user)
          .into_iter()
          .flatten()
          .filter_map(|id| inner.sessions.get(id).map(|session| (id, session)))
          .filter(|(_, session)| only_device.is_none_or(|device| session.device == device))
          .map(|(id, session)| {
            (
              session.maybe_connection.is_some(),
              session.created_seq,
              id.clone(),
            )
          })
          .collect();
        if candidates.len() <= limit.max(1) {
          break;
        }
        // Waiting (detached) sessions go first, then the oldest.
        candidates.sort();
        let victim = candidates.remove(0).2;
        inner.forget_session(&victim);
        if let Some(set) = inner.by_user.get_mut(&user) {
          set.remove(&victim);
        }
      }
    }
  }

  /// Records the device's own activity report; a repeated "active" counts for the away timeout.
  /// Returns whether the session's activity changed (only then the presence needs a refresh).
  /// A device going idle no longer holds phone pushes back: its user's held pushes go out now.
  pub fn set_activity(&self, session_id: &str, activity: DeviceActivity) -> bool {
    let (has_changed, maybe_gone_idle) = {
      let mut inner = self.lock();
      let Some(session) = inner.sessions.get_mut(session_id) else {
        return false;
      };
      if activity == DeviceActivity::Active {
        session.last_active_at = Instant::now();
      }
      let reported = SessionActivity::reported(&activity);
      let previous = std::mem::replace(&mut session.activity, reported);
      let is_gone_idle = previous == SessionActivity::Active && reported != SessionActivity::Active;
      (previous != reported, is_gone_idle.then_some(session.user))
    };
    if let Some(user) = maybe_gone_idle {
      self.announce_attention_lost(user);
    }
    has_changed
  }

  /// Whether this report would make an idle or timed-out session active. The gateway lets such a
  /// report through its throttle, so activity after an away shows at once.
  pub fn is_waking(&self, session_id: &str, activity: &DeviceActivity) -> bool {
    *activity == DeviceActivity::Active
      && self
        .lock()
        .sessions
        .get(session_id)
        .is_some_and(|session| session.activity != SessionActivity::Active)
  }

  /// Records what a desktop or web session says about the user's attention. Being attentive also
  /// counts as being active for the away timeout and ends a timed-out session's away. Returns the
  /// user if their devices' activity changed (the caller refreshes the presence).
  pub fn set_attention(&self, session_id: &str, is_attentive: bool) -> Option<UserId> {
    let now = Instant::now();
    let (maybe_lost, maybe_woken) = {
      let mut inner = self.lock();
      let session = inner.sessions.get_mut(session_id)?;
      if is_attentive {
        session.attention = Attention::Attentive {
          until: now + self.options.attention_lease,
        };
        (None, session.show_sign_of_life(now))
      } else {
        session.attention = Attention::Away { since: now };
        (Some(session.user), None)
      }
    };
    if let Some(user) = maybe_lost {
      self.announce_attention_lost(user);
    }
    maybe_woken
  }

  /// A heartbeat of the session: an attentive report stays valid for another lease. Sessions don't
  /// repeat their report, so the server learns transitions only. Returns the user if a timed-out
  /// session became active again (the caller refreshes the presence).
  pub fn renew_attention(&self, session_id: &str) -> Option<UserId> {
    let now = Instant::now();
    let mut inner = self.lock();
    let session = inner.sessions.get_mut(session_id)?;
    let Attention::Attentive { .. } = session.attention else {
      return None;
    };
    session.attention = Attention::Attentive {
      until: now + self.options.attention_lease,
    };
    session.show_sign_of_life(now)
  }

  /// What wakes when a desktop or web session of `user` stops being attentive. A loss that happens
  /// while nobody waits is kept as one permit, so a waiter that comes late still wakes.
  pub fn watch_attention(&self, user: UserId) -> Arc<Notify> {
    self
      .attention_watchers
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
      .entry(user)
      .or_default()
      .clone()
  }

  fn announce_attention_lost(&self, user: UserId) {
    let maybe_watcher = self
      .attention_watchers
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
      .get(&user)
      .cloned();
    // Nobody watching (no held pushes) is fine.
    if let Some(watcher) = maybe_watcher {
      watcher.notify_one();
    }
  }

  /// Notes that the device shows signs of life (it typed, it asked for something). Returns the user
  /// if a timed-out session became active again (the caller refreshes the presence).
  pub fn touch_activity(&self, session_id: &str) -> Option<UserId> {
    self
      .lock()
      .sessions
      .get_mut(session_id)?
      .show_sign_of_life(Instant::now())
  }

  /// The user did something on `device` themselves (sent a message): every connected session of
  /// that device is active from now on, also one that said it is idle or that the away timeout
  /// made away. The device's own idle report may be stale (a screen lock that was not reported
  /// over, an idle time that is wrong); a message the user just sent is not. Returns the user if
  /// a session changed (the caller refreshes the presence).
  pub fn note_user_action(&self, user: UserId, device: DeviceId) -> Option<UserId> {
    let now = Instant::now();
    let mut inner = self.lock();
    let inner = &mut *inner;
    let mut has_changed = false;
    for session_id in inner.by_user.get(&user).into_iter().flatten() {
      let Some(session) = inner.sessions.get_mut(session_id) else {
        continue;
      };
      if session.device != device || session.maybe_connection.is_none() {
        continue;
      }
      session.last_active_at = now;
      has_changed |=
        std::mem::replace(&mut session.activity, SessionActivity::Active) != SessionActivity::Active;
    }
    has_changed.then_some(user)
  }

  /// Held while a user's presence is recomputed and announced. Without it two refreshes that
  /// overlap (a device report, the periodic refresh, a disconnect) could announce in the wrong
  /// order: the one that read the devices first would win and leave the user "away" while a
  /// device is in use, until something else changes.
  pub fn presence_refresh_lock(&self, user: UserId) -> &tokio::sync::Mutex<()> {
    let stripe = user.snowflake().get().unsigned_abs() % PRESENCE_REFRESH_LOCKS as u64;
    // Invariant: the remainder is below the number of locks, which fits a usize.
    &self.presence_refresh_locks[usize::try_from(stripe).unwrap_or(0)]
  }

  /// Turns devices that reported active but have not shown a sign since `cutoff` into timed-out
  /// ones (the workspace's away timeout). Returns the users whose devices changed.
  pub fn demote_idle(&self, cutoff: Instant) -> Vec<UserId> {
    let mut users = HashSet::new();
    for session in self.lock().sessions.values_mut() {
      if session.maybe_connection.is_some()
        && session.activity == SessionActivity::Active
        && session.last_active_at < cutoff
      {
        session.activity = SessionActivity::TimedOut;
        users.insert(session.user);
      }
    }
    users.into_iter().collect()
  }

  /// Users with at least one live connection.
  pub fn online_users(&self) -> Vec<UserId> {
    let inner = self.lock();
    inner
      .by_user
      .iter()
      .filter(|(_, ids)| {
        ids.iter().any(|id| {
          inner
            .sessions
            .get(id)
            .is_some_and(|session| session.maybe_connection.is_some())
        })
      })
      .map(|(user, _)| *user)
      .collect()
  }

  /// Which kinds of device of the user are in use. `is_mobile` tells phones from the rest. A desktop
  /// or web session is in use while its attentive report lasts, and also for
  /// [`HubOptions::desktop_activity_window`] after the user's own activity on it (reported input, a
  /// sent message) that came after its last "away" report: the user is at the computer, if not in
  /// this window (notifications.md, "Attention"). Without `uses_attention` (the user or the
  /// instance turned it off) a desktop counts by its activity, as presence does.
  pub fn active_devices(
    &self,
    user: UserId,
    is_mobile: impl Fn(DeviceId) -> bool,
    uses_attention: bool,
  ) -> ActiveDevices {
    let now = Instant::now();
    let inner = self.lock();
    let mut active = ActiveDevices::default();
    for session_id in inner.by_user.get(&user).into_iter().flatten() {
      let Some(session) = inner.sessions.get(session_id) else {
        continue;
      };
      if session.maybe_connection.is_none() {
        continue;
      }
      if is_mobile(session.device) {
        active.is_mobile_active |= session.activity == SessionActivity::Active;
      } else {
        active.is_desktop_attentive |= match session.attention {
          _ if !uses_attention => session.activity == SessionActivity::Active,
          Attention::NeverReported => session.activity == SessionActivity::Active,
          Attention::Attentive { until } => until > now,
          Attention::Away { since } => {
            session.has_acted_since(since, now, self.options.desktop_activity_window)
          },
        };
      }
    }
    active
  }

  /// Remembers when the user's calendar says they are busy (replaces the earlier answer).
  pub fn set_busy_periods(&self, user: UserId, periods: Vec<(OffsetDateTime, OffsetDateTime)>) {
    let mut inner = self.lock();
    if periods.is_empty() {
      inner.busy.remove(&user);
    } else {
      inner.busy.insert(user, periods);
    }
  }

  /// The end of the busy period the user is in at `now`, if any.
  pub fn busy_until(&self, user: UserId, now: OffsetDateTime) -> Option<OffsetDateTime> {
    self
      .lock()
      .busy
      .get(&user)?
      .iter()
      .filter(|(start, end)| *start <= now && now < *end)
      .map(|(_, end)| *end)
      .max()
  }

  /// Numbers the event per session, buffers it and delivers it to connected sessions of `users`.
  /// Typing goes out unnumbered and unbuffered ([`EPHEMERAL_SEQ`]).
  pub fn publish(&self, users: impl IntoIterator<Item = UserId>, event: &GatewayEvent) {
    let Some(shared) = SharedEvent::new(event) else {
      return;
    };
    if is_ephemeral(event) {
      self.deliver_ephemeral(users, &shared);
      return;
    }
    let users: HashSet<UserId> = users.into_iter().collect();
    let mut inner = self.lock();
    let inner = &mut *inner;
    for user in users {
      let Some(ids) = inner.by_user.get(&user) else {
        continue;
      };
      for session_id in ids {
        if let Some(session) = inner.sessions.get_mut(session_id) {
          let before = session.buffer_bytes;
          session.push(&shared, &self.options);
          inner.total_buffer_bytes = (inner.total_buffer_bytes + session.buffer_bytes).saturating_sub(before);
        }
      }
    }
    self.enforce_total_budget(inner);
  }

  /// Keeps all buffers together within `max_total_buffer_bytes` (issue #156): many sessions that
  /// each stay within their own budget must not add up to more than the process can hold. The
  /// slowest go first, so the healthy majority keeps its resume buffers:
  ///
  /// 1. sessions waiting to be resumed, the longest-waiting first (they get `session_invalid` and
  ///    sync afresh);
  /// 2. connected sessions that are behind, the most bytes behind first (dropped the same way);
  /// 3. events connected sessions already took, the largest buffers first (they only matter to a
  ///    resume of a connection that breaks right now, which then syncs afresh).
  fn enforce_total_budget(&self, inner: &mut Inner) {
    let limit = self.options.max_total_buffer_bytes;
    if inner.total_buffer_bytes <= limit {
      return;
    }
    let before = inner.total_buffer_bytes;
    let mut dropped_sessions = 0_usize;
    for victim in inner.sessions_slowest_first() {
      if inner.total_buffer_bytes <= limit {
        break;
      }
      if let Some(session) = inner.forget_session(&victim) {
        if let Some(ids) = inner.by_user.get_mut(&session.user) {
          ids.remove(&victim);
          if ids.is_empty() {
            inner.by_user.remove(&session.user);
          }
        }
        dropped_sessions += 1;
      }
    }
    let mut largest: Vec<(usize, String)> = inner
      .sessions
      .iter()
      .map(|(id, session)| (session.buffer_bytes, id.clone()))
      .collect();
    largest.sort_by(|left, right| right.cmp(left));
    for (_, id) in largest {
      if inner.total_buffer_bytes <= limit {
        break;
      }
      if let Some(session) = inner.sessions.get_mut(&id) {
        let freed = session.trim_delivered();
        inner.total_buffer_bytes = inner.total_buffer_bytes.saturating_sub(freed);
      }
    }
    warn!(
      before_bytes = before,
      after_bytes = inner.total_buffer_bytes,
      limit_bytes = limit,
      dropped_sessions,
      "gateway buffers over their process-wide budget; the slowest sessions were dropped"
    );
  }

  /// Delivers the event to the live connections of `users` only: no sequence number, nothing to
  /// resume. For what is stale a moment later anyway; it never fills a resume buffer.
  fn deliver_ephemeral(&self, users: impl IntoIterator<Item = UserId>, shared: &Arc<SharedEvent>) {
    let users: HashSet<UserId> = users.into_iter().collect();
    let inner = self.lock();
    for user in users {
      for session_id in inner.by_user.get(&user).into_iter().flatten() {
        let Some(connection) = inner
          .sessions
          .get(session_id)
          .and_then(|session| session.maybe_connection.as_ref())
        else {
          continue;
        };
        // A full queue loses the event: the next one replaces it, and the queue's owner is
        // dropped by the next numbered event if it stays behind.
        let _ = connection.sender.try_send(Outgoing {
          seq: EPHEMERAL_SEQ,
          event: shared.clone(),
        });
      }
    }
  }

  /// Delivers the event to every known session (workspace-wide announcements).
  pub fn publish_all(&self, event: &GatewayEvent) {
    let users: Vec<UserId> = self.lock().by_user.keys().copied().collect();
    self.publish(users, event);
  }

  /// Ends all sessions of a user (deactivation, logout): connections are dropped and cannot resume.
  pub fn revoke_user(&self, user: UserId) {
    let mut inner = self.lock();
    if let Some(ids) = inner.by_user.remove(&user) {
      for id in ids {
        // Dropping the sender closes the connection's receiver, which ends the connection.
        inner.forget_session(&id);
      }
    }
    inner.last_presence.remove(&user);
    inner.last_aggregate.remove(&user);
  }

  /// `true` if the device has a live gateway connection (it gets events directly, no push needed).
  pub fn is_device_connected(&self, user: UserId, device: DeviceId) -> bool {
    let inner = self.lock();
    inner.by_user.get(&user).into_iter().flatten().any(|id| {
      inner
        .sessions
        .get(id)
        .is_some_and(|session| session.device == device && session.maybe_connection.is_some())
    })
  }

  /// Remembers which identity provider session a gateway session belongs to.
  pub fn set_oidc_session(&self, session_id: &str, oidc_session: &str) {
    if let Some(session) = self.lock().sessions.get_mut(session_id) {
      session.maybe_oidc_session = Some(oidc_session.to_owned());
    }
  }

  /// Ends the gateway sessions that authenticated with this identity provider session, and those of
  /// `maybe_user` that authenticated without a provider session ID (they may belong to it).
  pub fn revoke_oidc_session(&self, oidc_session: &str, maybe_user: Option<UserId>) {
    let mut inner = self.lock();
    let ids: Vec<(String, UserId)> = inner
      .sessions
      .iter()
      .filter(|(_, session)| match session.maybe_oidc_session.as_deref() {
        Some(other) => other == oidc_session,
        // Signed in with a token without a session ID: it may be the ended session.
        None => maybe_user == Some(session.user),
      })
      .map(|(id, session)| (id.clone(), session.user))
      .collect();
    for (id, user) in ids {
      inner.forget_session(&id);
      if let Some(set) = inner.by_user.get_mut(&user) {
        set.remove(&id);
      }
    }
  }

  /// Ends the sessions of one device.
  pub fn revoke_device(&self, user: UserId, device: DeviceId) {
    let mut inner = self.lock();
    let ids: Vec<String> = inner
      .by_user
      .get(&user)
      .into_iter()
      .flatten()
      .filter(|id| {
        inner
          .sessions
          .get(*id)
          .is_some_and(|session| session.device == device)
      })
      .cloned()
      .collect();
    for id in ids {
      inner.forget_session(&id);
      if let Some(set) = inner.by_user.get_mut(&user) {
        set.remove(&id);
      }
    }
  }

  /// Aggregates the activity of the user's connected devices.
  pub fn device_aggregate(&self, user: UserId) -> DeviceAggregate {
    let inner = self.lock();
    let mut aggregate = DeviceAggregate::NoDevice;
    for session_id in inner.by_user.get(&user).into_iter().flatten() {
      let Some(session) = inner.sessions.get(session_id) else {
        continue;
      };
      if session.maybe_connection.is_none() {
        continue;
      }
      match session.activity {
        SessionActivity::Active => return DeviceAggregate::Active,
        SessionActivity::Idle | SessionActivity::TimedOut => aggregate = DeviceAggregate::Idle,
      }
    }
    aggregate
  }

  /// Remembers what the user's devices report and returns what they reported before (`None`: this
  /// is the first refresh since the server started).
  pub fn swap_aggregate(&self, user: UserId, aggregate: DeviceAggregate) -> Option<DeviceAggregate> {
    self.lock().last_aggregate.insert(user, aggregate)
  }

  /// Remembers the broadcast presence of a user; `true` if it differs from the previous one.
  pub fn record_presence(&self, user: UserId, snapshot: PresenceSnapshot) -> bool {
    self.lock().last_presence.insert(user, snapshot.clone()) != Some(snapshot)
  }

  /// The last presence broadcast for the user (offline without status if none).
  pub fn last_presence(&self, user: UserId) -> PresenceSnapshot {
    self.lock().last_presence.get(&user).cloned().unwrap_or_default()
  }

  /// Drops sessions that stayed disconnected longer than the resume window; returns their users.
  pub fn sweep_expired(&self) -> Vec<UserId> {
    let mut inner = self.lock();
    let window = self.options.resume_window;
    let expired: Vec<(String, UserId)> = inner
      .sessions
      .iter()
      .filter(|(_, session)| session.maybe_detached_at.is_some_and(|at| at.elapsed() > window))
      .map(|(id, session)| (id.clone(), session.user))
      .collect();
    for (id, user) in &expired {
      inner.forget_session(id);
      if let Some(ids) = inner.by_user.get_mut(user) {
        ids.remove(id);
        if ids.is_empty() {
          inner.by_user.remove(user);
        }
      }
    }
    expired.into_iter().map(|(_, user)| user).collect()
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    // The state is plain maps; a panic in another thread can't leave it half-updated in a way that
    // matters more than dropping the whole gateway.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Inner {
  /// Removes a session and its buffer from the byte count. The caller updates `by_user`.
  fn forget_session(&mut self, id: &str) -> Option<Session> {
    let session = self.sessions.remove(id)?;
    self.total_buffer_bytes = self.total_buffer_bytes.saturating_sub(session.buffer_bytes);
    Some(session)
  }

  /// Sessions that may be dropped for the process-wide budget, slowest first: waiting ones by how
  /// long they wait, then connected ones that are behind by how many bytes. Connected sessions
  /// that took everything but the newest event are not listed.
  fn sessions_slowest_first(&self) -> Vec<String> {
    let mut waiting: Vec<(Instant, &String)> = Vec::new();
    let mut behind: Vec<(usize, &String)> = Vec::new();
    for (id, session) in &self.sessions {
      match (&session.maybe_connection, session.maybe_detached_at) {
        (None, maybe_detached_at) => waiting.push((maybe_detached_at.unwrap_or_else(Instant::now), id)),
        (Some(connection), _) => {
          let delivered = connection.link.delivered_seq.load(Ordering::Acquire);
          let undelivered = session.bytes_behind(delivered);
          if undelivered > 0 {
            behind.push((undelivered, id));
          }
        },
      }
    }
    waiting.sort();
    behind.sort_by(|left, right| right.cmp(left));
    waiting
      .into_iter()
      .map(|(_, id)| id.clone())
      .chain(behind.into_iter().map(|(_, id)| id.clone()))
      .collect()
  }
}

impl Session {
  /// Numbers, buffers and queues one event. A connection that fell behind its buffer (an event it
  /// has not taken was evicted) can never catch up from it and only holds memory: it is dropped,
  /// and its client resumes or, past the gap, syncs afresh.
  fn push(&mut self, event: &Arc<SharedEvent>, options: &HubOptions) {
    let seq = self.next_seq;
    self.next_seq += 1;
    self.buffer.push_back((seq, event.clone()));
    self.buffer_bytes += event.byte_len();
    // The newest event stays even if it alone is over the byte limit.
    while self.buffer.len() > options.buffer_events.max(1)
      || (self.buffer_bytes > options.buffer_bytes && self.buffer.len() > 1)
    {
      if let Some((_, evicted)) = self.buffer.pop_front() {
        self.buffer_bytes -= evicted.byte_len();
      }
    }
    let Some(connection) = &self.maybe_connection else {
      return;
    };
    let oldest = self.buffer.front().map_or(seq, |(oldest, _)| *oldest);
    let delivered = connection.link.delivered_seq.load(Ordering::Acquire);
    let is_behind = delivered.saturating_add(1) < oldest;
    let outgoing = Outgoing {
      seq,
      event: event.clone(),
    };
    if is_behind || connection.sender.try_send(outgoing).is_err() {
      // Too slow or already closed: drop the connection; the client resumes from its last seq.
      debug!(seq, delivered, "gateway connection fell behind, dropped");
      self.maybe_connection = None;
      self.maybe_detached_at = Some(Instant::now());
    }
  }
}

impl Session {
  /// Bytes of the buffered events after `delivered` that the connection has not taken, not
  /// counting the newest (it is on its way right now; a healthy connection is never behind by it).
  fn bytes_behind(&self, delivered: u64) -> usize {
    let newest = self.next_seq.saturating_sub(1);
    self
      .buffer
      .iter()
      .filter(|(seq, _)| *seq > delivered && *seq < newest)
      .map(|(_, event)| event.byte_len())
      .sum()
  }

  /// Drops the buffered events the connection already took; returns the bytes freed. Nothing for a
  /// session without a connection (all of its buffer is still to be replayed).
  fn trim_delivered(&mut self) -> usize {
    let Some(connection) = &self.maybe_connection else {
      return 0;
    };
    let delivered = connection.link.delivered_seq.load(Ordering::Acquire);
    let mut freed = 0;
    while self.buffer.front().is_some_and(|(seq, _)| *seq <= delivered) {
      if let Some((_, event)) = self.buffer.pop_front() {
        freed += event.byte_len();
      }
    }
    self.buffer_bytes = self.buffer_bytes.saturating_sub(freed);
    freed
  }

  /// The session showed a sign of life: it counts for the away timeout, and a session the timeout
  /// made away is active again. A device that said it is idle stays idle until it says otherwise.
  /// Returns the user if the activity changed.
  fn show_sign_of_life(&mut self, now: Instant) -> Option<UserId> {
    match self.activity {
      SessionActivity::Active => {
        self.last_active_at = now;
        None
      },
      SessionActivity::TimedOut => {
        self.last_active_at = now;
        self.activity = SessionActivity::Active;
        Some(self.user)
      },
      SessionActivity::Idle => None,
    }
  }

  /// The device is active and showed it after `since`, within `window` of `now`. Any report or
  /// action that renews the activity counts (gateway.md, "Presence"): input the device reported, a
  /// message sent from it, typing.
  fn has_acted_since(&self, since: Instant, now: Instant, window: Duration) -> bool {
    self.activity == SessionActivity::Active
      && self.last_active_at > since
      && now.saturating_duration_since(self.last_active_at) < window
  }
}

impl SessionActivity {
  /// What a device's own report means; a value this version does not know counts as idle.
  fn reported(activity: &DeviceActivity) -> Self {
    match activity {
      DeviceActivity::Active => Self::Active,
      DeviceActivity::Idle | DeviceActivity::Unknown(_) => Self::Idle,
    }
  }
}

impl Attached {
  /// The next event for the connection; `None` once the hub dropped the connection (too slow,
  /// replaced by a resume, session ended), which ends the connection task and frees its queue.
  pub async fn next_outgoing(&mut self) -> Option<Outgoing> {
    let outgoing = self.receiver.recv().await?;
    (!self.link.is_dropped.load(Ordering::Acquire)).then_some(outgoing)
  }

  /// The peer took the event numbered `seq`.
  pub fn delivered(&self, seq: u64) {
    if seq != EPHEMERAL_SEQ {
      self.link.delivered_seq.fetch_max(seq, Ordering::AcqRel);
    }
  }
}

impl Outgoing {
  /// The `event` frame as sent, `{"type":"event","seq":…,"event":…}`, without serializing the event
  /// again for every recipient.
  pub fn to_frame(&self) -> String {
    format!(
      r#"{{"type":"event","seq":{},"event":{}}}"#,
      self.seq, self.event.json
    )
  }
}

impl SharedEvent {
  /// `None` (logged) if the event can't be serialized, which plain data never fails to.
  fn new(event: &GatewayEvent) -> Option<Arc<Self>> {
    match serde_json::to_string(event) {
      Ok(json) => Some(Arc::new(Self {
        json: json.into_boxed_str(),
      })),
      Err(error) => {
        warn!(%error, "a gateway event could not be serialized");
        None
      },
    }
  }

  pub fn byte_len(&self) -> usize {
    self.json.len()
  }
}

impl Drop for Connection {
  fn drop(&mut self) {
    self.link.is_dropped.store(true, Ordering::Release);
  }
}

/// A connection for a session: the hub's end and the connection task's end.
fn new_connection(
  next_id: &mut u64,
  session_id: &str,
  replay: Vec<Outgoing>,
  delivered_seq: u64,
) -> (Connection, Attached) {
  *next_id += 1;
  let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
  let link = Arc::new(ConnectionLink {
    delivered_seq: AtomicU64::new(delivered_seq),
    is_dropped: AtomicBool::new(false),
  });
  let connection = Connection {
    id: *next_id,
    sender,
    link: link.clone(),
  };
  let attached = Attached {
    session_id: session_id.to_owned(),
    connection_id: *next_id,
    replay,
    receiver,
    link,
  };
  (connection, attached)
}

/// Events that only matter to whoever is connected at this moment.
fn is_ephemeral(event: &GatewayEvent) -> bool {
  // The link request carries a key that must not sit in the replay buffer, and a request that is
  // replayed after its time is worse than none: devices fetch the open ones on every (re)connect.
  matches!(
    event,
    GatewayEvent::TypingStart { .. }
      | GatewayEvent::DeviceLinkRequested { .. }
      | GatewayEvent::DeviceLinkClosed { .. }
  )
}

fn new_session_id() -> String {
  let mut bytes = [0u8; 16];
  // Invariant: the system random source doesn't fail; without it nothing else would be safe either.
  #[allow(clippy::expect_used)]
  aws_lc_rs::rand::fill(&mut bytes).expect("system random source");
  bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationId, Id, Snowflake};

  use super::*;

  const OPTIONS: HubOptions = HubOptions {
    resume_window: Duration::from_secs(60),
    buffer_events: 3,
    buffer_bytes: 1024 * 1024,
    max_total_buffer_bytes: 1024 * 1024 * 1024,
    max_sessions_per_device: 100,
    max_sessions_per_user: 100,
    attention_lease: Duration::from_secs(75),
    desktop_activity_window: Duration::from_secs(120),
  };

  #[tokio::test(start_paused = true)]
  async fn events_get_consecutive_sequence_numbers_per_session() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(2), device(2), DeviceActivity::Active);

    hub.publish([user(1), user(2)], &event(1));
    hub.publish([user(1)], &event(2));

    assert_eq!(seqs(&mut a), vec![1, 2]);
    assert_eq!(seqs(&mut b), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn events_only_reach_the_addressed_users() {
    let hub = Hub::new(OPTIONS);
    let mut other = hub.open_session(user(3), device(3), DeviceActivity::Active);
    hub.publish([user(1)], &event(1));
    assert!(seqs(&mut other).is_empty());
  }

  #[tokio::test(start_paused = true)]
  async fn resume_replays_exactly_the_missed_events() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let session_id = first.session_id.clone();
    hub.publish([user(1)], &event(1));
    hub.detach(&session_id, first.connection_id);
    hub.publish([user(1)], &event(2));
    hub.publish([user(1)], &event(3));

    let resumed = hub.resume_session(&session_id, user(1), device(1), 1).unwrap();
    let replayed: Vec<u64> = resumed.replay.iter().map(|outgoing| outgoing.seq).collect();
    assert_eq!(replayed, vec![2, 3]);
  }

  #[tokio::test(start_paused = true)]
  async fn events_after_resume_continue_the_numbering() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = first.session_id.clone();
    hub.publish([user(1)], &event(1));
    hub.detach(&id, first.connection_id);

    let mut resumed = hub.resume_session(&id, user(1), device(1), 1).unwrap();
    hub.publish([user(1)], &event(2));
    assert_eq!(seqs(&mut resumed), vec![2]);
  }

  #[tokio::test(start_paused = true)]
  async fn resume_fails_when_events_fell_out_of_the_buffer() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = first.session_id.clone();
    hub.detach(&id, first.connection_id);
    for n in 1..=5 {
      hub.publish([user(1)], &event(n));
    }
    // Buffer holds seq 3..=5; the client saw nothing (0), so seq 1 and 2 are lost.
    assert_eq!(
      hub.resume_session(&id, user(1), device(1), 0).err(),
      Some(ResumeError::Gap)
    );
    assert!(hub.resume_session(&id, user(1), device(1), 2).is_ok());
  }

  #[tokio::test(start_paused = true)]
  async fn resume_rejects_foreign_expired_and_future_sequences() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = first.session_id.clone();
    hub.detach(&id, first.connection_id);

    assert_eq!(
      hub.resume_session(&id, user(2), device(1), 0).err(),
      Some(ResumeError::NotOwner)
    );
    assert_eq!(
      hub.resume_session(&id, user(1), device(9), 0).err(),
      Some(ResumeError::NotOwner)
    );
    assert_eq!(
      hub.resume_session("nope", user(1), device(1), 0).err(),
      Some(ResumeError::Unknown)
    );
    assert_eq!(
      hub.resume_session(&id, user(1), device(1), 7).err(),
      Some(ResumeError::Gap)
    );

    tokio::time::advance(Duration::from_secs(61)).await;
    assert_eq!(
      hub.resume_session(&id, user(1), device(1), 0).err(),
      Some(ResumeError::Unknown)
    );
  }

  mod session_caps {
    use super::*;

    fn capped(per_device: usize, per_user: usize) -> Hub {
      Hub::new(HubOptions {
        max_sessions_per_device: per_device,
        max_sessions_per_user: per_user,
        ..OPTIONS
      })
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_keeps_its_newest_sessions_and_waiting_ones_go_first() {
      let hub = capped(2, 100);
      let mut first = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let second = hub.open_session(user(1), device(1), DeviceActivity::Active);
      // The older one is live, the newer one waits to be resumed: the waiting one is dropped first.
      hub.detach(&second.session_id, second.connection_id);
      let third = hub.open_session(user(1), device(1), DeviceActivity::Active);
      assert!(
        hub
          .resume_session(&second.session_id, user(1), device(1), 0)
          .is_err()
      );
      hub.publish([user(1)], &event(1));
      assert_eq!(seqs(&mut first), vec![1]);
      drop(third);

      // With all live, the oldest goes and its connection ends.
      let fourth = hub.open_session(user(1), device(1), DeviceActivity::Active);
      assert!(
        first.next_outgoing().await.is_none(),
        "the oldest connection was closed"
      );
      drop(fourth);
    }

    #[tokio::test(start_paused = true)]
    async fn the_cap_per_user_counts_all_devices_and_other_users_are_untouched() {
      let hub = capped(10, 3);
      let mut oldest = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let _b = hub.open_session(user(1), device(2), DeviceActivity::Active);
      let _c = hub.open_session(user(1), device(3), DeviceActivity::Active);
      let mut other = hub.open_session(user(2), device(9), DeviceActivity::Active);
      let _d = hub.open_session(user(1), device(4), DeviceActivity::Active);
      assert!(oldest.next_outgoing().await.is_none());
      hub.publish([user(2)], &event(1));
      assert_eq!(seqs(&mut other), vec![1]);
    }
  }

  #[tokio::test(start_paused = true)]
  async fn sweep_removes_only_expired_detached_sessions() {
    let hub = Hub::new(OPTIONS);
    let gone = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let _kept = hub.open_session(user(2), device(2), DeviceActivity::Active);
    hub.detach(&gone.session_id, gone.connection_id);

    assert!(hub.sweep_expired().is_empty());
    tokio::time::advance(Duration::from_secs(61)).await;
    assert_eq!(hub.sweep_expired(), vec![user(1)]);
    assert_eq!(hub.device_aggregate(user(2)), DeviceAggregate::Active);
  }

  #[tokio::test(start_paused = true)]
  async fn activity_is_aggregated_over_connected_devices() {
    let hub = Hub::new(OPTIONS);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);

    let desktop = hub.open_session(user(1), device(1), DeviceActivity::Idle);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
    let phone = hub.open_session(user(1), device(2), DeviceActivity::Active);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);

    hub.set_activity(&phone.session_id, DeviceActivity::Idle);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
    hub.detach(&phone.session_id, phone.connection_id);
    hub.detach(&desktop.session_id, desktop.connection_id);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
  }

  mod several_devices {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(300);

    #[tokio::test(start_paused = true)]
    async fn the_device_in_use_wins_over_idle_and_timed_out_ones() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let phone = hub.open_session(user(1), device(2), DeviceActivity::Active);
      let laptop = hub.open_session(user(1), device(3), DeviceActivity::Active);
      // The phone lies untouched until the away timeout, the laptop went idle by itself.
      tokio::time::advance(TIMEOUT - Duration::from_secs(10)).await;
      hub.set_activity(&desktop.session_id, DeviceActivity::Active);
      hub.set_activity(&laptop.session_id, DeviceActivity::Idle);
      tokio::time::advance(Duration::from_secs(20)).await;
      assert_eq!(hub.demote_idle(Instant::now() - TIMEOUT), vec![user(1)]);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);

      // Only when the desktop stops too is the user away; gone when the last device is.
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
      for session in [&desktop, &phone, &laptop] {
        hub.detach(&session.session_id, session.connection_id);
      }
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
    }

    #[tokio::test(start_paused = true)]
    async fn sending_from_a_device_makes_it_active_whatever_it_reported() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let phone = hub.open_session(user(1), device(2), DeviceActivity::Idle);
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);

      assert_eq!(hub.note_user_action(user(1), device(1)), Some(user(1)));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
      // Already active: nobody needs to hear about it again, but it counts for the timeout.
      tokio::time::advance(TIMEOUT - Duration::from_secs(1)).await;
      assert_eq!(hub.note_user_action(user(1), device(1)), None);
      tokio::time::advance(Duration::from_secs(2)).await;
      hub.set_activity(&phone.session_id, DeviceActivity::Idle);
      assert!(hub.demote_idle(Instant::now() - TIMEOUT).is_empty());
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);

      // A timed-out device wakes up the same way.
      tokio::time::advance(TIMEOUT + Duration::from_secs(1)).await;
      assert_eq!(hub.demote_idle(Instant::now() - TIMEOUT), vec![user(1)]);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
      assert_eq!(hub.note_user_action(user(1), device(1)), Some(user(1)));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
    }

    #[tokio::test(start_paused = true)]
    async fn sending_touches_only_the_connected_sessions_of_that_device() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Idle);
      let _other_user = hub.open_session(user(2), device(1), DeviceActivity::Idle);
      // Sent from a device without a gateway connection: nothing to change.
      assert_eq!(hub.note_user_action(user(1), device(2)), None);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
      hub.detach(&desktop.session_id, desktop.connection_id);
      assert_eq!(hub.note_user_action(user(1), device(1)), None);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
      assert_eq!(hub.device_aggregate(user(2)), DeviceAggregate::Idle);
    }

    #[tokio::test]
    async fn refreshes_of_one_user_share_a_lock() {
      let hub = Hub::new(OPTIONS);
      let held = hub.presence_refresh_lock(user(1)).lock().await;
      assert!(hub.presence_refresh_lock(user(1)).try_lock().is_err());
      assert!(hub.presence_refresh_lock(user(2)).try_lock().is_ok());
      drop(held);
      assert!(hub.presence_refresh_lock(user(1)).try_lock().is_ok());
    }
  }

  mod away_timeout {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(300);

    /// Lets the away timeout pass for a session that does nothing and applies it.
    async fn time_out(hub: &Hub) -> Vec<UserId> {
      tokio::time::advance(TIMEOUT + Duration::from_secs(1)).await;
      hub.demote_idle(Instant::now() - TIMEOUT)
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_device_times_out_and_any_sign_of_life_brings_it_back() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);

      assert_eq!(time_out(&hub).await, vec![user(1)]);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
      assert_eq!(hub.touch_activity(&web.session_id), Some(user(1)));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
      // Already active: nothing changes, nobody needs to hear about it.
      assert_eq!(hub.touch_activity(&web.session_id), None);

      assert_eq!(time_out(&hub).await, vec![user(1)]);
      assert_eq!(hub.set_attention(&web.session_id, true), Some(user(1)));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);

      assert_eq!(time_out(&hub).await, vec![user(1)]);
      assert_eq!(hub.renew_attention(&web.session_id), Some(user(1)));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
    }

    #[tokio::test(start_paused = true)]
    async fn signs_of_life_keep_a_device_from_timing_out() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      tokio::time::advance(TIMEOUT - Duration::from_secs(1)).await;
      hub.touch_activity(&web.session_id);
      tokio::time::advance(Duration::from_secs(2)).await;
      assert!(hub.demote_idle(Instant::now() - TIMEOUT).is_empty());
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
    }

    #[tokio::test(start_paused = true)]
    async fn a_device_that_said_it_is_idle_stays_idle_until_it_says_otherwise() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&web.session_id, true);
      hub.set_activity(&web.session_id, DeviceActivity::Idle);

      assert_eq!(hub.renew_attention(&web.session_id), None);
      assert_eq!(hub.set_attention(&web.session_id, true), None);
      assert_eq!(hub.touch_activity(&web.session_id), None);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
      // An idle device is no candidate for the timeout either.
      assert!(time_out(&hub).await.is_empty());

      hub.set_activity(&web.session_id, DeviceActivity::Active);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_report_that_ends_an_away_is_waking() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      assert!(!hub.is_waking(&web.session_id, &DeviceActivity::Active));
      assert!(!hub.is_waking(&web.session_id, &DeviceActivity::Idle));

      time_out(&hub).await;
      assert!(hub.is_waking(&web.session_id, &DeviceActivity::Active));
      assert!(!hub.is_waking(&web.session_id, &DeviceActivity::Idle));

      hub.set_activity(&web.session_id, DeviceActivity::Idle);
      assert!(hub.is_waking(&web.session_id, &DeviceActivity::Active));
      assert!(!hub.is_waking("unknown", &DeviceActivity::Active));
    }

    #[tokio::test(start_paused = true)]
    async fn only_a_changing_report_asks_for_a_presence_refresh() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      assert!(!hub.set_activity(&web.session_id, DeviceActivity::Active));
      assert!(hub.set_activity(&web.session_id, DeviceActivity::Idle));
      assert!(!hub.set_activity(&web.session_id, DeviceActivity::Idle));
      assert!(hub.set_activity(&web.session_id, DeviceActivity::Active));
      time_out(&hub).await;
      assert!(hub.set_activity(&web.session_id, DeviceActivity::Active));
      assert!(!hub.set_activity("unknown", DeviceActivity::Active));
    }

    #[tokio::test(start_paused = true)]
    async fn a_repeated_active_report_keeps_a_device_from_timing_out() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      tokio::time::advance(TIMEOUT - Duration::from_secs(1)).await;
      hub.set_activity(&web.session_id, DeviceActivity::Active);
      tokio::time::advance(Duration::from_secs(2)).await;
      assert!(hub.demote_idle(Instant::now() - TIMEOUT).is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unknown_activity_counts_as_idle() {
      let hub = Hub::new(OPTIONS);
      let web = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_activity(&web.session_id, DeviceActivity::Unknown("dozing".into()));
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Idle);
    }
  }

  mod attention {
    use super::*;

    fn desktop_only(_: DeviceId) -> bool {
      false
    }

    #[tokio::test(start_paused = true)]
    async fn an_attentive_report_expires_after_the_lease() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, true);
      assert!(
        hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
      tokio::time::advance(Duration::from_secs(74)).await;
      assert!(
        hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
      tokio::time::advance(Duration::from_secs(2)).await;
      assert!(
        !hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
      // Repeating the report renews it.
      hub.set_attention(&desktop.session_id, true);
      assert!(
        hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeats_renew_an_attentive_report_but_never_make_one() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, true);
      for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(30)).await;
        hub.renew_attention(&desktop.session_id);
      }
      assert!(
        hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
      hub.set_attention(&desktop.session_id, false);
      hub.renew_attention(&desktop.session_id);
      assert!(
        !hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
    }

    #[tokio::test(start_paused = true)]
    async fn without_attention_a_desktop_counts_by_its_activity() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, false);
      assert!(
        hub
          .active_devices(user(1), desktop_only, false)
          .is_desktop_attentive
      );
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert!(
        !hub
          .active_devices(user(1), desktop_only, false)
          .is_desktop_attentive
      );
    }

    #[tokio::test(start_paused = true)]
    async fn a_desktop_that_reported_away_is_not_attentive_even_if_active() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, false);
      assert!(
        !hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
    }

    #[tokio::test(start_paused = true)]
    async fn using_the_computer_holds_pushes_back_while_the_window_is_not_in_front() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, false);
      assert!(!is_in_use(&hub));
      // Input anywhere on the computer (or in a window the webview thinks is unfocused) renews the
      // device's activity: the user is at the desktop.
      tokio::time::advance(Duration::from_secs(1)).await;
      hub.set_activity(&desktop.session_id, DeviceActivity::Active);
      assert!(is_in_use(&hub));
      tokio::time::advance(Duration::from_secs(119)).await;
      assert!(is_in_use(&hub));
      // Nothing for the window's length: the user left.
      tokio::time::advance(Duration::from_secs(1)).await;
      assert!(!is_in_use(&hub));
      // Typing shows it as well.
      hub.touch_activity(&desktop.session_id);
      assert!(is_in_use(&hub));
    }

    #[tokio::test(start_paused = true)]
    async fn an_away_report_after_the_last_activity_wins() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_activity(&desktop.session_id, DeviceActivity::Active);
      tokio::time::advance(Duration::from_secs(1)).await;
      // The tab was hidden or closed after the input: not at this session any more.
      hub.set_attention(&desktop.session_id, false);
      assert!(!is_in_use(&hub));
    }

    #[tokio::test(start_paused = true)]
    async fn going_idle_ends_it_and_releases_the_held_pushes() {
      let hub = Hub::new(OPTIONS);
      let watcher = hub.watch_attention(user(1));
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, false);
      assert!(is_woken(&watcher).await);
      tokio::time::advance(Duration::from_secs(1)).await;
      hub.set_activity(&desktop.session_id, DeviceActivity::Active);
      assert!(is_in_use(&hub));
      assert!(!is_woken(&watcher).await);
      // The screen locks: the device says idle, the phone gets its pushes now.
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert!(!is_in_use(&hub));
      assert!(is_woken(&watcher).await);
      // Idle again changes nothing and wakes nobody.
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert!(!is_woken(&watcher).await);
    }

    #[tokio::test(start_paused = true)]
    async fn without_an_activity_window_only_attention_counts() {
      let hub = Hub::new(HubOptions {
        desktop_activity_window: Duration::ZERO,
        ..OPTIONS
      });
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, false);
      tokio::time::advance(Duration::from_secs(1)).await;
      hub.set_activity(&desktop.session_id, DeviceActivity::Active);
      assert!(!is_in_use(&hub));
    }

    #[tokio::test(start_paused = true)]
    async fn clients_that_never_report_attention_fall_back_to_their_activity() {
      let hub = Hub::new(OPTIONS);
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      assert!(
        hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
      hub.set_activity(&desktop.session_id, DeviceActivity::Idle);
      assert!(
        !hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
    }

    #[tokio::test(start_paused = true)]
    async fn phones_count_by_activity_not_attention() {
      let hub = Hub::new(OPTIONS);
      let _phone = hub.open_session(user(1), device(2), DeviceActivity::Active);
      let active = hub.active_devices(user(1), |device| device == super::device(2), true);
      assert!(active.is_mobile_active);
      assert!(!active.is_desktop_attentive);
    }

    #[tokio::test(start_paused = true)]
    async fn losing_attention_or_the_connection_wakes_only_that_users_watcher() {
      let hub = Hub::new(OPTIONS);
      let watcher = hub.watch_attention(user(1));
      let other = hub.watch_attention(user(2));
      let desktop = hub.open_session(user(1), device(1), DeviceActivity::Active);
      hub.set_attention(&desktop.session_id, true);
      assert!(!is_woken(&watcher).await);
      hub.set_attention(&desktop.session_id, false);
      assert!(is_woken(&watcher).await);
      hub.set_attention(&desktop.session_id, true);
      hub.detach(&desktop.session_id, desktop.connection_id);
      assert!(is_woken(&watcher).await);
      assert!(!is_woken(&other).await, "other users' waiters stay asleep");
      assert!(
        !hub
          .active_devices(user(1), desktop_only, true)
          .is_desktop_attentive
      );
    }

    fn is_in_use(hub: &Hub) -> bool {
      hub
        .active_devices(user(1), desktop_only, true)
        .is_desktop_attentive
    }

    async fn is_woken(watcher: &Notify) -> bool {
      tokio::time::timeout(Duration::from_millis(10), watcher.notified())
        .await
        .is_ok()
    }
  }

  mod memory {
    use super::*;

    /// Bytes of one [`big_event`] as serialized.
    fn big_event_bytes() -> usize {
      SharedEvent::new(&big_event(1)).unwrap().byte_len()
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_consumer_keeps_no_more_than_its_byte_budget() {
      let budget = 10 * big_event_bytes();
      let hub = Hub::new(HubOptions {
        buffer_events: 1000,
        buffer_bytes: budget,
        ..OPTIONS
      });
      let mut stalled = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let mut healthy = hub.open_session(user(2), device(2), DeviceActivity::Active);
      for n in 1..=100 {
        hub.publish([user(1), user(2)], &big_event(n));
        // The healthy connection takes every event as it comes.
        while let Ok(outgoing) = healthy.receiver.try_recv() {
          healthy.delivered(outgoing.seq);
        }
      }

      // The stalled one was dropped once an event it never took left its buffer, long before its
      // queue (1024 frames) was full: what it holds is bounded by the budget, not by the traffic.
      let queued = seqs(&mut stalled);
      assert!(queued.len() <= 11, "{} frames queued", queued.len());
      assert!(stalled.next_outgoing().await.is_none(), "its task ends");
      let inner = hub.lock();
      for session in inner.sessions.values() {
        assert!(
          session.buffer_bytes <= budget,
          "{} bytes buffered",
          session.buffer_bytes
        );
      }
      drop(inner);
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
      assert_eq!(hub.device_aggregate(user(2)), DeviceAggregate::Active);
      // It can't resume across the gap: its client syncs afresh.
      assert_eq!(
        hub
          .resume_session(&stalled.session_id, user(1), device(1), 0)
          .err(),
        Some(ResumeError::Gap)
      );
    }

    #[tokio::test(start_paused = true)]
    async fn an_event_is_serialized_once_and_shared_by_every_session() {
      let hub = Hub::new(OPTIONS);
      let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let mut b = hub.open_session(user(2), device(2), DeviceActivity::Active);
      hub.publish([user(1), user(2)], &big_event(1));
      let from_a = a.receiver.try_recv().unwrap();
      let from_b = b.receiver.try_recv().unwrap();
      assert!(Arc::ptr_eq(&from_a.event, &from_b.event));
      let inner = hub.lock();
      let buffered = &inner.sessions[&a.session_id].buffer[0].1;
      assert!(Arc::ptr_eq(buffered, &from_a.event), "the buffer shares it too");
    }

    #[test]
    fn the_shared_frame_is_the_protocol_s_event_frame() {
      let event = big_event(7);
      let outgoing = Outgoing {
        seq: 42,
        event: SharedEvent::new(&event).unwrap(),
      };
      let parsed: teamcraft_proto::GatewayServerMessage = serde_json::from_str(&outgoing.to_frame()).unwrap();
      assert_eq!(
        parsed,
        teamcraft_proto::GatewayServerMessage::Event { seq: 42, event }
      );
    }

    #[tokio::test(start_paused = true)]
    async fn typing_is_neither_numbered_nor_buffered() {
      let hub = Hub::new(OPTIONS);
      let mut live = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let typing = GatewayEvent::TypingStart {
        conversation_id: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
        user_id: user(2),
        maybe_thread_id: None,
      };
      for _ in 0..10 {
        hub.publish([user(1)], &typing);
      }
      hub.publish([user(1)], &event(1));
      assert_eq!(seqs(&mut live), [vec![EPHEMERAL_SEQ; 10], vec![1]].concat());

      // A waiting session gets no typing at all, and its buffer holds only the real event.
      hub.detach(&live.session_id, live.connection_id);
      hub.publish([user(1)], &typing);
      let resumed = hub
        .resume_session(&live.session_id, user(1), device(1), 0)
        .unwrap();
      let replayed: Vec<u64> = resumed.replay.iter().map(|outgoing| outgoing.seq).collect();
      assert_eq!(replayed, vec![1]);
    }

    /// A 16 KiB event, like a message near a moderate size limit.
    #[tokio::test(start_paused = true)]
    async fn the_process_budget_drops_waiting_sessions_first_then_those_most_behind() {
      let event_bytes = big_event_bytes();
      let budget = 25 * event_bytes + event_bytes / 2;
      let hub = Hub::new(HubOptions {
        buffer_events: 1000,
        buffer_bytes: 20 * event_bytes,
        max_total_buffer_bytes: budget,
        ..OPTIONS
      });
      let mut healthy = hub.open_session(user(1), device(1), DeviceActivity::Active);
      let waiting = hub.open_session(user(2), device(2), DeviceActivity::Active);
      hub.detach(&waiting.session_id, waiting.connection_id);
      let mut stalled = hub.open_session(user(3), device(3), DeviceActivity::Active);
      let everyone = [user(1), user(2), user(3)];

      // 3 × 9 events are over the budget: the waiting session goes, the others stay.
      for n in 11..=19 {
        hub.publish(everyone, &big_event(n));
        take_all(&mut healthy);
        assert_within_budget(&hub, budget);
      }
      assert_eq!(
        hub
          .resume_session(&waiting.session_id, user(2), device(2), 0)
          .err(),
        Some(ResumeError::Unknown),
        "the waiting session was dropped: its client syncs afresh"
      );
      assert_eq!(hub.device_aggregate(user(3)), DeviceAggregate::Active);

      // Two buffers of 9 grow to 13 each: over the budget again, and the stalled one goes.
      for n in 21..=24 {
        hub.publish(everyone, &big_event(n));
        take_all(&mut healthy);
        assert_within_budget(&hub, budget);
      }
      assert_eq!(hub.device_aggregate(user(3)), DeviceAggregate::NoDevice);
      seqs(&mut stalled);
      assert!(stalled.next_outgoing().await.is_none(), "its task ends");
      // The healthy one kept its connection and its buffer for a resume.
      assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
      hub.publish([user(1)], &big_event(25));
      assert_eq!(take_all(&mut healthy), vec![14]);
    }

    #[tokio::test(start_paused = true)]
    async fn with_only_healthy_sessions_the_events_they_took_are_trimmed() {
      let event_bytes = big_event_bytes();
      let budget = 10 * event_bytes + event_bytes / 2;
      let hub = Hub::new(HubOptions {
        buffer_events: 1000,
        buffer_bytes: 100 * event_bytes,
        max_total_buffer_bytes: budget,
        ..OPTIONS
      });
      let mut sessions: Vec<Attached> = (1..=3)
        .map(|n| hub.open_session(user(n), device(n), DeviceActivity::Active))
        .collect();
      for n in 11..=30 {
        hub.publish([user(1), user(2), user(3)], &big_event(n));
        for attached in &mut sessions {
          take_all(attached);
        }
        assert_within_budget(&hub, budget);
      }
      for n in 1..=3 {
        assert_eq!(hub.device_aggregate(user(n)), DeviceAggregate::Active);
      }
    }

    fn take_all(attached: &mut Attached) -> Vec<u64> {
      let seqs = seqs(attached);
      if let Some(last) = seqs.last() {
        attached.delivered(*last);
      }
      seqs
    }

    fn assert_within_budget(hub: &Hub, budget: usize) {
      let inner = hub.lock();
      let sum: usize = inner.sessions.values().map(|session| session.buffer_bytes).sum();
      assert_eq!(
        inner.total_buffer_bytes, sum,
        "the running total matches the buffers"
      );
      assert!(sum <= budget, "{sum} bytes buffered, budget {budget}");
    }

    fn big_event(n: i64) -> GatewayEvent {
      GatewayEvent::MembershipAdd {
        conversation_id: ConversationId::from_snowflake(Snowflake::new(n).unwrap()),
        user: teamcraft_proto::UserSummary {
          id: user(n),
          maybe_username: None,
          display_name: "x".repeat(16 * 1024),
          kind: teamcraft_proto::UserKind::Member,
          maybe_email: None,
          maybe_organization: None,
          maybe_has_devices: None,
          is_deactivated: false,
          is_deleted: false,
        },
      }
    }
  }

  #[tokio::test(start_paused = true)]
  async fn a_late_detach_of_a_replaced_connection_keeps_the_resumed_session() {
    let hub = Hub::new(OPTIONS);
    let old = hub.open_session(user(1), device(1), DeviceActivity::Active);
    hub.publish([user(1)], &event(1));
    // The client gave up on a half-open socket and resumed on a new one before the server noticed.
    let mut resumed = hub
      .resume_session(&old.session_id, user(1), device(1), 1)
      .unwrap();
    hub.detach(&old.session_id, old.connection_id);

    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::Active);
    hub.publish([user(1)], &event(2));
    assert_eq!(seqs(&mut resumed), vec![2], "the resumed connection stays open");
    // The current connection's own detach still works.
    hub.detach(&resumed.session_id, resumed.connection_id);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
  }

  #[tokio::test(start_paused = true)]
  async fn a_replaced_connection_stops_at_its_next_frame() {
    let hub = Hub::new(OPTIONS);
    let mut old = hub.open_session(user(1), device(1), DeviceActivity::Active);
    hub.publish([user(1)], &event(1));
    let _resumed = hub
      .resume_session(&old.session_id, user(1), device(1), 0)
      .unwrap();
    assert!(old.next_outgoing().await.is_none());
  }

  #[tokio::test(start_paused = true)]
  async fn slow_consumers_are_detached_and_can_resume() {
    let hub = Hub::new(HubOptions {
      resume_window: Duration::from_secs(60),
      buffer_events: 5000,
      ..OPTIONS
    });
    let attached = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = attached.session_id.clone();
    for n in 0..(CHANNEL_CAPACITY as i64 + 10) {
      hub.publish([user(1)], &event(n));
    }
    // The queue overflowed: the connection counts as gone, the session is still resumable.
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
    let resumed = hub
      .resume_session(&id, user(1), device(1), CHANNEL_CAPACITY as u64)
      .unwrap();
    assert_eq!(resumed.replay.len(), 10);
  }

  #[tokio::test(start_paused = true)]
  async fn revoking_a_user_closes_connections_and_forbids_resuming() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let b = hub.open_session(user(2), device(2), DeviceActivity::Active);
    hub.revoke_user(user(1));
    assert!(
      a.next_outgoing().await.is_none(),
      "the connection's channel is closed"
    );
    assert_eq!(
      hub.resume_session(&a.session_id, user(1), device(1), 0).err(),
      Some(ResumeError::Unknown)
    );
    assert_eq!(hub.device_aggregate(user(2)), DeviceAggregate::Active);
    drop(b);
  }

  #[tokio::test(start_paused = true)]
  async fn revoking_a_device_keeps_the_other_devices_of_the_user() {
    let hub = Hub::new(OPTIONS);
    let mut phone = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut laptop = hub.open_session(user(1), device(2), DeviceActivity::Active);
    hub.revoke_device(user(1), device(1));
    assert!(phone.next_outgoing().await.is_none());
    hub.publish([user(1)], &event(1));
    assert_eq!(seqs(&mut laptop), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn revoking_a_provider_session_ends_only_its_connections() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(1), device(2), DeviceActivity::Active);
    hub.set_oidc_session(&a.session_id, "sid-a");
    hub.set_oidc_session(&b.session_id, "sid-b");
    let mut sessionless = hub.open_session(user(1), device(3), DeviceActivity::Active);
    let mut other_user = hub.open_session(user(2), device(4), DeviceActivity::Active);
    hub.revoke_oidc_session("sid-a", Some(user(1)));
    assert!(a.next_outgoing().await.is_none());
    assert!(
      sessionless.next_outgoing().await.is_none(),
      "a session without a provider session ID may be the ended one"
    );
    hub.publish([user(1), user(2)], &event(1));
    assert_eq!(seqs(&mut b), vec![1]);
    assert_eq!(seqs(&mut other_user), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn publish_all_reaches_every_user() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(2), device(2), DeviceActivity::Active);
    hub.publish_all(&event(1));
    assert_eq!((seqs(&mut a), seqs(&mut b)), (vec![1], vec![1]));
  }

  #[test]
  fn presence_changes_are_reported_once() {
    let hub = Hub::new(OPTIONS);
    let snapshot = |state, status: Option<&str>| PresenceSnapshot {
      state,
      maybe_status: status.map(|text| CustomStatus {
        maybe_emoji: None,
        maybe_text: Some(text.into()),
        maybe_expires_at: None,
        maybe_activity: None,
      }),
    };
    assert!(hub.record_presence(user(1), snapshot(PresenceState::Online, None)));
    assert!(!hub.record_presence(user(1), snapshot(PresenceState::Online, None)));
    assert!(hub.record_presence(user(1), snapshot(PresenceState::Away, None)));
    // A status change alone counts as a change.
    assert!(hub.record_presence(user(1), snapshot(PresenceState::Away, Some("lunch"))));
    assert!(!hub.record_presence(user(1), snapshot(PresenceState::Away, Some("lunch"))));
    assert_eq!(
      hub
        .last_presence(user(1))
        .maybe_status
        .unwrap()
        .maybe_text
        .as_deref(),
      Some("lunch")
    );
    assert_eq!(hub.last_presence(user(9)), PresenceSnapshot::default());
  }

  #[test]
  fn session_ids_are_unique_and_long() {
    let a = new_session_id();
    assert_eq!(a.len(), 32);
    assert_ne!(a, new_session_id());
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn event(n: i64) -> GatewayEvent {
    GatewayEvent::ConversationUpdate {
      conversation_id: ConversationId::from_snowflake(Snowflake::new(n).unwrap()),
    }
  }

  fn seqs(attached: &mut Attached) -> Vec<u64> {
    let mut seqs = Vec::new();
    while let Ok(outgoing) = attached.receiver.try_recv() {
      seqs.push(outgoing.seq);
    }
    seqs
  }
}
