use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use teamcraft_proto::{Base64Bytes, DeviceLinkPlatform, DeviceLinkRequest, UserId};
use tokio::sync::mpsc;

use crate::config::RendezvousConfig;

const CHANNEL_CAPACITY: usize = 64;

/// What a connected device receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signal {
  /// An opaque frame from the peer.
  Frame(Vec<u8>),
  PeerJoined,
  /// The peer left; the relay is gone.
  PeerLeft,
  /// A device of the user denied the link request of the device waiting here.
  Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
  #[error("the rendezvous belongs to another user")]
  NotYours,
  #[error("the rendezvous already has two devices")]
  Full,
  #[error("too many open rendezvous")]
  TooMany,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
  #[error("frame too large")]
  FrameTooLarge,
  #[error("relay quota exceeded")]
  QuotaExceeded,
  #[error("the rendezvous is closed")]
  Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AnnounceError {
  /// No relay of the caller where only the new device waits: nothing to approve.
  #[error("no device is waiting at that rendezvous")]
  NotWaiting,
  #[error("the rendezvous already has a link request")]
  AlreadyAnnounced,
}

/// A device attached to a rendezvous.
pub struct Joined {
  pub slot: usize,
  pub peer_present: bool,
  pub receiver: mpsc::Receiver<Signal>,
}

/// Open relays. In memory, per server process (like the gateway hub).
pub struct RendezvousHub {
  limits: Limits,
  inner: Mutex<Inner>,
}

#[derive(Debug, Clone, Copy)]
struct Limits {
  max_frame_bytes: usize,
  max_total_bytes: usize,
  max_pending_frames: usize,
  max_per_user: usize,
  /// How long a link request stays open; the relay's lifetime.
  request_ttl: Duration,
}

#[derive(Default)]
struct Inner {
  channels: HashMap<String, Channel>,
  per_user: HashMap<UserId, usize>,
  /// Link requests of devices waiting in a relay (e2ee.md §5), by rendezvous ID. In memory like
  /// the relays: a restart ends both, and the new device starts over.
  requests: HashMap<String, PendingRequest>,
}

struct PendingRequest {
  user: UserId,
  key: Base64Bytes,
  device_name: String,
  platform: DeviceLinkPlatform,
  announced_at: Instant,
}

struct Channel {
  user: UserId,
  senders: [Option<mpsc::Sender<Signal>>; 2],
  /// Frames for a slot whose device has not joined yet.
  backlog: [VecDeque<Vec<u8>>; 2],
  relayed_bytes: usize,
}

impl RendezvousHub {
  pub fn new(config: &RendezvousConfig) -> Self {
    Self {
      limits: Limits {
        max_frame_bytes: config.max_frame_bytes,
        max_total_bytes: config.max_total_bytes,
        max_pending_frames: config.max_pending_frames,
        max_per_user: config.max_per_user,
        request_ttl: Duration::from_secs(config.ttl_secs),
      },
      inner: Mutex::new(Inner::default()),
    }
  }

  /// Attaches a device: the first one creates the relay, the second must be of the same user.
  pub fn join(&self, id: &str, user: UserId) -> Result<Joined, JoinError> {
    let mut inner = self.lock();
    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);

    if let Some(channel) = inner.channels.get_mut(id) {
      if channel.user != user {
        return Err(JoinError::NotYours);
      }
      if channel.senders[1].is_some() {
        return Err(JoinError::Full);
      }
      channel.senders[1] = Some(sender.clone());
      for frame in channel.backlog[1].drain(..) {
        // The receiver was just created and the backlog is smaller than its capacity.
        let _ = sender.try_send(Signal::Frame(frame));
      }
      if let Some(first) = &channel.senders[0] {
        let _ = first.try_send(Signal::PeerJoined);
      }
      return Ok(Joined {
        slot: 1,
        peer_present: true,
        receiver,
      });
    }

    let open = inner.per_user.get(&user).copied().unwrap_or(0);
    if open >= self.limits.max_per_user {
      return Err(JoinError::TooMany);
    }
    *inner.per_user.entry(user).or_insert(0) += 1;
    inner.channels.insert(
      id.to_owned(),
      Channel {
        user,
        senders: [Some(sender), None],
        backlog: [VecDeque::new(), VecDeque::new()],
        relayed_bytes: 0,
      },
    );
    Ok(Joined {
      slot: 0,
      peer_present: false,
      receiver,
    })
  }

  /// Forwards a frame from `slot` to the other device (or holds it until that device joins).
  pub fn relay(&self, id: &str, slot: usize, frame: Vec<u8>) -> Result<(), RelayError> {
    if frame.len() > self.limits.max_frame_bytes {
      return Err(RelayError::FrameTooLarge);
    }
    let mut inner = self.lock();
    let channel = inner.channels.get_mut(id).ok_or(RelayError::Closed)?;
    if channel.relayed_bytes + frame.len() > self.limits.max_total_bytes {
      return Err(RelayError::QuotaExceeded);
    }
    channel.relayed_bytes += frame.len();

    let peer = 1 - slot;
    match &channel.senders[peer] {
      Some(sender) => sender
        .try_send(Signal::Frame(frame))
        .map_err(|_| RelayError::Closed),
      None if channel.backlog[peer].len() < self.limits.max_pending_frames => {
        channel.backlog[peer].push_back(frame);
        Ok(())
      },
      None => Err(RelayError::QuotaExceeded),
    }
  }

  /// Announces the device waiting at `id` (slot 0, owned by `user`) to the user's other devices.
  pub fn announce(
    &self,
    id: &str,
    user: UserId,
    key: Base64Bytes,
    device_name: String,
    platform: DeviceLinkPlatform,
    now: Instant,
  ) -> Result<DeviceLinkRequest, AnnounceError> {
    let mut inner = self.lock();
    let is_waiting = inner.channels.get(id).is_some_and(|channel| {
      channel.user == user && channel.senders[0].is_some() && channel.senders[1].is_none()
    });
    if !is_waiting {
      return Err(AnnounceError::NotWaiting);
    }
    inner.prune_requests(now, self.limits.request_ttl);
    if inner.requests.contains_key(id) {
      return Err(AnnounceError::AlreadyAnnounced);
    }
    let request = DeviceLinkRequest {
      id: id.to_owned(),
      key: key.clone(),
      device_name: device_name.clone(),
      platform: platform.clone(),
      expires_in_secs: self.limits.request_ttl.as_secs(),
    };
    inner.requests.insert(
      id.to_owned(),
      PendingRequest {
        user,
        key,
        device_name,
        platform,
        announced_at: now,
      },
    );
    Ok(request)
  }

  /// The open link requests of `user`, with the time they have left.
  pub fn pending_requests(&self, user: UserId, now: Instant) -> Vec<DeviceLinkRequest> {
    let mut inner = self.lock();
    inner.prune_requests(now, self.limits.request_ttl);
    let mut requests: Vec<_> = inner
      .requests
      .iter()
      .filter(|(_, request)| request.user == user)
      .map(|(id, request)| {
        let left = self
          .limits
          .request_ttl
          .saturating_sub(now.saturating_duration_since(request.announced_at));
        (
          request.announced_at,
          DeviceLinkRequest {
            id: id.clone(),
            key: request.key.clone(),
            device_name: request.device_name.clone(),
            platform: request.platform.clone(),
            expires_in_secs: left.as_secs(),
          },
        )
      })
      .collect();
    requests.sort_by_key(|(announced_at, _)| *announced_at);
    requests.into_iter().map(|(_, request)| request).collect()
  }

  /// Drops the request of `id` (taken up, gone); returns its owner if there was one.
  pub fn close_request(&self, id: &str) -> Option<UserId> {
    self.lock().requests.remove(id).map(|request| request.user)
  }

  /// Drops the request of `id` if it is `user`'s; `false` if there is none of theirs.
  pub fn withdraw(&self, id: &str, user: UserId) -> bool {
    let mut inner = self.lock();
    if inner.requests.get(id).is_none_or(|request| request.user != user) {
      return false;
    }
    inner.requests.remove(id);
    true
  }

  /// A device of `user` denies the request: it is dropped and the waiting device is told. `false`
  /// if the request is not (or no longer) the user's.
  pub fn reject(&self, id: &str, user: UserId) -> bool {
    let mut inner = self.lock();
    if inner.requests.get(id).is_none_or(|request| request.user != user) {
      return false;
    }
    inner.requests.remove(id);
    if let Some(Some(waiting)) = inner.channels.get(id).map(|channel| &channel.senders[0]) {
      let _ = waiting.try_send(Signal::Rejected);
    }
    true
  }

  /// A device is gone (closed, expired, errored): the relay ends and the peer is told. Returns the
  /// owner of the link request that went with it, if any (its prompts have to go).
  pub fn leave(&self, id: &str, slot: usize) -> Option<UserId> {
    let mut inner = self.lock();
    let request_owner = inner.requests.remove(id).map(|request| request.user);
    let Some(channel) = inner.channels.remove(id) else {
      return request_owner;
    };
    if let Some(count) = inner.per_user.get_mut(&channel.user) {
      *count = count.saturating_sub(1);
      if *count == 0 {
        inner.per_user.remove(&channel.user);
      }
    }
    if let Some(peer) = &channel.senders[1 - slot] {
      let _ = peer.try_send(Signal::PeerLeft);
    }
    request_owner
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl Inner {
  fn prune_requests(&mut self, now: Instant, ttl: Duration) {
    self
      .requests
      .retain(|_, request| now.saturating_duration_since(request.announced_at) < ttl);
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  fn hub() -> RendezvousHub {
    RendezvousHub::new(&RendezvousConfig {
      max_frame_bytes: 8,
      max_total_bytes: 20,
      max_pending_frames: 2,
      max_per_user: 2,
      ..RendezvousConfig::default()
    })
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn drain(joined: &mut Joined) -> Vec<Signal> {
    let mut signals = Vec::new();
    while let Ok(signal) = joined.receiver.try_recv() {
      signals.push(signal);
    }
    signals
  }

  #[test]
  fn frames_flow_both_ways_in_order() {
    let hub = hub();
    let mut a = hub.join("r", user(1)).unwrap();
    assert!(!a.peer_present);
    let mut b = hub.join("r", user(1)).unwrap();
    assert!(b.peer_present);
    assert_eq!(drain(&mut a), vec![Signal::PeerJoined]);

    hub.relay("r", 0, vec![1]).unwrap();
    hub.relay("r", 0, vec![2]).unwrap();
    hub.relay("r", 1, vec![9]).unwrap();
    assert_eq!(
      drain(&mut b),
      vec![Signal::Frame(vec![1]), Signal::Frame(vec![2])]
    );
    assert_eq!(drain(&mut a), vec![Signal::Frame(vec![9])]);
  }

  #[test]
  fn frames_sent_before_the_peer_joined_are_delivered_first() {
    let hub = hub();
    let _a = hub.join("r", user(1)).unwrap();
    hub.relay("r", 0, vec![1]).unwrap();
    hub.relay("r", 0, vec![2]).unwrap();
    assert_eq!(
      hub.relay("r", 0, vec![3]),
      Err(RelayError::QuotaExceeded),
      "pending frames are limited"
    );
    let mut b = hub.join("r", user(1)).unwrap();
    assert_eq!(
      drain(&mut b),
      vec![Signal::Frame(vec![1]), Signal::Frame(vec![2])]
    );
  }

  #[test]
  fn only_the_owner_joins_and_only_two_devices() {
    let hub = hub();
    let _a = hub.join("r", user(1)).unwrap();
    assert_eq!(hub.join("r", user(2)).err(), Some(JoinError::NotYours));
    let _b = hub.join("r", user(1)).unwrap();
    assert_eq!(hub.join("r", user(1)).err(), Some(JoinError::Full));
  }

  #[test]
  fn frame_size_and_total_volume_are_limited() {
    let hub = hub();
    let _a = hub.join("r", user(1)).unwrap();
    let _b = hub.join("r", user(1)).unwrap();
    assert_eq!(hub.relay("r", 0, vec![0; 9]), Err(RelayError::FrameTooLarge));
    hub.relay("r", 0, vec![0; 8]).unwrap();
    hub.relay("r", 1, vec![0; 8]).unwrap();
    assert_eq!(hub.relay("r", 0, vec![0; 8]), Err(RelayError::QuotaExceeded));
  }

  #[test]
  fn leaving_ends_the_relay_and_frees_the_slot_of_the_user() {
    let hub = hub();
    let _a = hub.join("r1", user(1)).unwrap();
    let mut b = hub.join("r1", user(1)).unwrap();
    hub.leave("r1", 0);
    assert_eq!(drain(&mut b), vec![Signal::PeerLeft]);
    assert_eq!(hub.relay("r1", 1, vec![1]), Err(RelayError::Closed));
    // The same ID can be used again afterwards.
    assert!(hub.join("r1", user(2)).is_ok());
  }

  #[test]
  fn users_have_a_limit_of_open_relays() {
    let hub = hub();
    let _a = hub.join("r1", user(1)).unwrap();
    let _b = hub.join("r2", user(1)).unwrap();
    assert_eq!(hub.join("r3", user(1)).err(), Some(JoinError::TooMany));
    hub.leave("r1", 0);
    assert!(hub.join("r3", user(1)).is_ok());
    assert!(hub.join("r4", user(2)).is_ok(), "other users are unaffected");
  }

  mod link_requests {
    use super::*;

    fn announce(
      hub: &RendezvousHub,
      id: &str,
      owner: UserId,
      at: Instant,
    ) -> Result<DeviceLinkRequest, AnnounceError> {
      hub.announce(
        id,
        owner,
        Base64Bytes::new(vec![1; 32]),
        "Phone".into(),
        DeviceLinkPlatform::Android,
        at,
      )
    }

    #[test]
    fn only_a_device_that_waits_in_its_own_relay_can_be_announced() {
      let hub = hub();
      let now = Instant::now();
      assert_eq!(
        announce(&hub, "r", user(1), now).err(),
        Some(AnnounceError::NotWaiting)
      );
      let _new_device = hub.join("r", user(1)).unwrap();
      assert_eq!(
        announce(&hub, "r", user(2), now).err(),
        Some(AnnounceError::NotWaiting)
      );
      let request = announce(&hub, "r", user(1), now).unwrap();
      assert_eq!(request.expires_in_secs, 300);
      assert_eq!(
        announce(&hub, "r", user(1), now).err(),
        Some(AnnounceError::AlreadyAnnounced)
      );
      // Once an approving device sits in the relay there is nothing left to announce.
      let _approver = hub.join("r", user(1)).unwrap();
      hub.close_request("r");
      assert_eq!(
        announce(&hub, "r", user(1), now).err(),
        Some(AnnounceError::NotWaiting)
      );
    }

    #[test]
    fn requests_are_listed_for_their_owner_with_the_time_left_and_expire() {
      let hub = hub();
      let start = Instant::now();
      let _a = hub.join("r1", user(1)).unwrap();
      announce(&hub, "r1", user(1), start).unwrap();
      assert!(hub.pending_requests(user(2), start).is_empty());
      let listed = hub.pending_requests(user(1), start + Duration::from_secs(100));
      assert_eq!(listed.len(), 1);
      assert_eq!(listed[0].expires_in_secs, 200);
      assert!(
        hub
          .pending_requests(user(1), start + Duration::from_secs(300))
          .is_empty()
      );
    }

    #[test]
    fn open_requests_are_bounded_by_the_relays_of_the_user() {
      let hub = hub();
      let now = Instant::now();
      let _a = hub.join("r1", user(1)).unwrap();
      let _b = hub.join("r2", user(1)).unwrap();
      announce(&hub, "r1", user(1), now).unwrap();
      announce(&hub, "r2", user(1), now).unwrap();
      // A request needs its own relay, and a user has `max_per_user` of them.
      assert_eq!(hub.join("r3", user(1)).err(), Some(JoinError::TooMany));
      assert_eq!(
        announce(&hub, "r3", user(1), now).err(),
        Some(AnnounceError::NotWaiting)
      );
      assert_eq!(hub.pending_requests(user(1), now).len(), 2);
      hub.leave("r1", 0);
      assert_eq!(hub.pending_requests(user(1), now).len(), 1);
    }

    #[test]
    fn rejecting_tells_the_waiting_device_and_only_the_owner_may() {
      let hub = hub();
      let now = Instant::now();
      let mut waiting = hub.join("r", user(1)).unwrap();
      announce(&hub, "r", user(1), now).unwrap();
      assert!(!hub.reject("r", user(2)));
      assert!(hub.reject("r", user(1)));
      assert_eq!(drain(&mut waiting), vec![Signal::Rejected]);
      assert!(
        !hub.reject("r", user(1)),
        "a decided request can't be decided again"
      );
      assert!(hub.pending_requests(user(1), now).is_empty());
    }

    #[test]
    fn withdrawing_is_for_the_owner_only_and_leaving_takes_the_request_along() {
      let hub = hub();
      let now = Instant::now();
      let _waiting = hub.join("r", user(1)).unwrap();
      announce(&hub, "r", user(1), now).unwrap();
      assert!(!hub.withdraw("r", user(2)));
      assert_eq!(
        hub.pending_requests(user(1), now).len(),
        1,
        "somebody else's call changed nothing"
      );
      assert_eq!(hub.leave("r", 0), Some(user(1)));
      assert!(hub.pending_requests(user(1), now).is_empty());
      assert_eq!(hub.leave("r", 0), None);
    }

    #[test]
    fn a_request_that_was_taken_up_is_closed_once() {
      let hub = hub();
      let _waiting = hub.join("r", user(1)).unwrap();
      announce(&hub, "r", user(1), Instant::now()).unwrap();
      let _approver = hub.join("r", user(1)).unwrap();
      assert_eq!(hub.close_request("r"), Some(user(1)));
      assert_eq!(hub.close_request("r"), None);
      assert_eq!(
        hub.leave("r", 1),
        None,
        "a taken-up request needs no second announcement"
      );
    }
  }
}
