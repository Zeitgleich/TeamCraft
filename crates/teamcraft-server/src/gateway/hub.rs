use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;
use std::time::Duration;

use teamcraft_proto::{DeviceActivity, DeviceId, GatewayEvent, GatewayServerMessage, PresenceState, UserId};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Frames queued per connection before it is considered too slow and dropped (it can resume).
const CHANNEL_CAPACITY: usize = 1024;

/// Registry of gateway sessions and their event buffers.
///
/// A session outlives its connection for `resume_window`: events keep being numbered and buffered,
/// so a client that reconnects with its last seen sequence number receives exactly what it missed.
/// State is in memory and per server process; running several instances needs a shared bus
/// (Postgres `LISTEN/NOTIFY`), which is not part of M1.
pub struct Hub {
  options: HubOptions,
  inner: Mutex<Inner>,
}

#[derive(Debug, Clone, Copy)]
pub struct HubOptions {
  pub resume_window: Duration,
  pub buffer_events: usize,
}

/// A connection attached to a session.
pub struct Attached {
  pub session_id: String,
  /// Buffered events the client missed, to be sent before anything from `receiver`.
  pub replay: Vec<GatewayServerMessage>,
  pub receiver: mpsc::Receiver<GatewayServerMessage>,
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
  last_presence: HashMap<UserId, PresenceState>,
}

struct Session {
  user: UserId,
  device: DeviceId,
  activity: DeviceActivity,
  next_seq: u64,
  buffer: VecDeque<(u64, GatewayEvent)>,
  maybe_sender: Option<mpsc::Sender<GatewayServerMessage>>,
  maybe_detached_at: Option<Instant>,
  /// The identity provider's session the connection authenticated with.
  maybe_oidc_session: Option<String>,
}

impl Hub {
  pub fn new(options: HubOptions) -> Self {
    Self {
      options,
      inner: Mutex::new(Inner::default()),
    }
  }

  pub fn open_session(&self, user: UserId, device: DeviceId, activity: DeviceActivity) -> Attached {
    let session_id = new_session_id();
    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    let session = Session {
      user,
      device,
      activity,
      next_seq: 1,
      buffer: VecDeque::new(),
      maybe_sender: Some(sender),
      maybe_detached_at: None,
      maybe_oidc_session: None,
    };
    let mut inner = self.lock();
    inner.by_user.entry(user).or_default().insert(session_id.clone());
    inner.sessions.insert(session_id.clone(), session);
    Attached {
      session_id,
      replay: Vec::new(),
      receiver,
    }
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

    let replay = session
      .buffer
      .iter()
      .filter(|(seq, _)| *seq > last_seq)
      .map(|(seq, event)| GatewayServerMessage::Event {
        seq: *seq,
        event: event.clone(),
      })
      .collect();
    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    session.maybe_sender = Some(sender);
    session.maybe_detached_at = None;
    Ok(Attached {
      session_id: session_id.to_owned(),
      replay,
      receiver,
    })
  }

  /// The connection is gone; the session stays resumable for the resume window.
  pub fn detach(&self, session_id: &str) {
    if let Some(session) = self.lock().sessions.get_mut(session_id) {
      session.maybe_sender = None;
      session.maybe_detached_at = Some(Instant::now());
    }
  }

  pub fn set_activity(&self, session_id: &str, activity: DeviceActivity) {
    if let Some(session) = self.lock().sessions.get_mut(session_id) {
      session.activity = activity;
    }
  }

  /// Numbers the event per session, buffers it and delivers it to connected sessions of `users`.
  pub fn publish(&self, users: impl IntoIterator<Item = UserId>, event: &GatewayEvent) {
    let mut inner = self.lock();
    let mut session_ids = Vec::new();
    for user in users.into_iter().collect::<HashSet<_>>() {
      if let Some(ids) = inner.by_user.get(&user) {
        session_ids.extend(ids.iter().cloned());
      }
    }
    for session_id in session_ids {
      let buffer_limit = self.options.buffer_events;
      let Some(session) = inner.sessions.get_mut(&session_id) else {
        continue;
      };
      let seq = session.next_seq;
      session.next_seq += 1;
      session.buffer.push_back((seq, event.clone()));
      while session.buffer.len() > buffer_limit {
        session.buffer.pop_front();
      }
      if let Some(sender) = &session.maybe_sender
        && sender
          .try_send(GatewayServerMessage::Event {
            seq,
            event: event.clone(),
          })
          .is_err()
      {
        // Too slow or already closed: drop the connection; the client resumes from its last seq.
        session.maybe_sender = None;
        session.maybe_detached_at = Some(Instant::now());
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
        inner.sessions.remove(&id);
      }
    }
    inner.last_presence.remove(&user);
  }

  /// `true` if the device has a live gateway connection (it gets events directly, no push needed).
  pub fn is_device_connected(&self, user: UserId, device: DeviceId) -> bool {
    let inner = self.lock();
    inner.by_user.get(&user).into_iter().flatten().any(|id| {
      inner
        .sessions
        .get(id)
        .is_some_and(|session| session.device == device && session.maybe_sender.is_some())
    })
  }

  /// Remembers which identity provider session a gateway session belongs to.
  pub fn set_oidc_session(&self, session_id: &str, oidc_session: &str) {
    if let Some(session) = self.lock().sessions.get_mut(session_id) {
      session.maybe_oidc_session = Some(oidc_session.to_owned());
    }
  }

  /// Ends the gateway sessions that authenticated with this identity provider session.
  pub fn revoke_oidc_session(&self, oidc_session: &str) {
    let mut inner = self.lock();
    let ids: Vec<(String, UserId)> = inner
      .sessions
      .iter()
      .filter(|(_, session)| session.maybe_oidc_session.as_deref() == Some(oidc_session))
      .map(|(id, session)| (id.clone(), session.user))
      .collect();
    for (id, user) in ids {
      inner.sessions.remove(&id);
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
      inner.sessions.remove(&id);
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
      if session.maybe_sender.is_none() {
        continue;
      }
      match session.activity {
        DeviceActivity::Active => return DeviceAggregate::Active,
        _ => aggregate = DeviceAggregate::Idle,
      }
    }
    aggregate
  }

  /// Remembers the broadcast presence of a user; `true` if it differs from the previous one.
  pub fn record_presence(&self, user: UserId, state: PresenceState) -> bool {
    self.lock().last_presence.insert(user, state.clone()) != Some(state)
  }

  /// The last presence broadcast for the user (`Offline` if none).
  pub fn last_presence(&self, user: UserId) -> PresenceState {
    self
      .lock()
      .last_presence
      .get(&user)
      .cloned()
      .unwrap_or(PresenceState::Offline)
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
      inner.sessions.remove(id);
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
  };

  #[tokio::test(start_paused = true)]
  async fn events_get_consecutive_sequence_numbers_per_session() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(2), device(2), DeviceActivity::Active);

    hub.publish([user(1), user(2)], &event(1));
    hub.publish([user(1)], &event(2));

    assert_eq!(seqs(&mut a.receiver), vec![1, 2]);
    assert_eq!(seqs(&mut b.receiver), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn events_only_reach_the_addressed_users() {
    let hub = Hub::new(OPTIONS);
    let mut other = hub.open_session(user(3), device(3), DeviceActivity::Active);
    hub.publish([user(1)], &event(1));
    assert!(seqs(&mut other.receiver).is_empty());
  }

  #[tokio::test(start_paused = true)]
  async fn resume_replays_exactly_the_missed_events() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let session_id = first.session_id.clone();
    hub.publish([user(1)], &event(1));
    hub.detach(&session_id);
    hub.publish([user(1)], &event(2));
    hub.publish([user(1)], &event(3));

    let resumed = hub.resume_session(&session_id, user(1), device(1), 1).unwrap();
    let replayed: Vec<u64> = resumed.replay.iter().map(seq_of).collect();
    assert_eq!(replayed, vec![2, 3]);
  }

  #[tokio::test(start_paused = true)]
  async fn events_after_resume_continue_the_numbering() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = first.session_id.clone();
    hub.publish([user(1)], &event(1));
    hub.detach(&id);

    let mut resumed = hub.resume_session(&id, user(1), device(1), 1).unwrap();
    hub.publish([user(1)], &event(2));
    assert_eq!(seqs(&mut resumed.receiver), vec![2]);
  }

  #[tokio::test(start_paused = true)]
  async fn resume_fails_when_events_fell_out_of_the_buffer() {
    let hub = Hub::new(OPTIONS);
    let first = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let id = first.session_id.clone();
    hub.detach(&id);
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
    hub.detach(&id);

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

  #[tokio::test(start_paused = true)]
  async fn sweep_removes_only_expired_detached_sessions() {
    let hub = Hub::new(OPTIONS);
    let gone = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let _kept = hub.open_session(user(2), device(2), DeviceActivity::Active);
    hub.detach(&gone.session_id);

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
    hub.detach(&phone.session_id);
    hub.detach(&desktop.session_id);
    assert_eq!(hub.device_aggregate(user(1)), DeviceAggregate::NoDevice);
  }

  #[tokio::test(start_paused = true)]
  async fn slow_consumers_are_detached_and_can_resume() {
    let hub = Hub::new(HubOptions {
      resume_window: Duration::from_secs(60),
      buffer_events: 5000,
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
      a.receiver.recv().await.is_none(),
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
    assert!(phone.receiver.recv().await.is_none());
    hub.publish([user(1)], &event(1));
    assert_eq!(seqs(&mut laptop.receiver), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn revoking_a_provider_session_ends_only_its_connections() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(1), device(2), DeviceActivity::Active);
    hub.set_oidc_session(&a.session_id, "sid-a");
    hub.set_oidc_session(&b.session_id, "sid-b");
    hub.revoke_oidc_session("sid-a");
    assert!(a.receiver.recv().await.is_none());
    hub.publish([user(1)], &event(1));
    assert_eq!(seqs(&mut b.receiver), vec![1]);
  }

  #[tokio::test(start_paused = true)]
  async fn publish_all_reaches_every_user() {
    let hub = Hub::new(OPTIONS);
    let mut a = hub.open_session(user(1), device(1), DeviceActivity::Active);
    let mut b = hub.open_session(user(2), device(2), DeviceActivity::Active);
    hub.publish_all(&event(1));
    assert_eq!((seqs(&mut a.receiver), seqs(&mut b.receiver)), (vec![1], vec![1]));
  }

  #[test]
  fn presence_changes_are_reported_once() {
    let hub = Hub::new(OPTIONS);
    assert!(hub.record_presence(user(1), PresenceState::Online));
    assert!(!hub.record_presence(user(1), PresenceState::Online));
    assert!(hub.record_presence(user(1), PresenceState::Away));
    assert_eq!(hub.last_presence(user(1)), PresenceState::Away);
    assert_eq!(hub.last_presence(user(9)), PresenceState::Offline);
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

  fn seq_of(message: &GatewayServerMessage) -> u64 {
    match message {
      GatewayServerMessage::Event { seq, .. } => *seq,
      other => panic!("not an event: {other:?}"),
    }
  }

  fn seqs(receiver: &mut mpsc::Receiver<GatewayServerMessage>) -> Vec<u64> {
    let mut seqs = Vec::new();
    while let Ok(message) = receiver.try_recv() {
      seqs.push(seq_of(&message));
    }
    seqs
  }
}
