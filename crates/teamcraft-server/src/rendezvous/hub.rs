use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use teamcraft_proto::UserId;
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
}

#[derive(Default)]
struct Inner {
  channels: HashMap<String, Channel>,
  per_user: HashMap<UserId, usize>,
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

  /// A device is gone (closed, expired, errored): the relay ends and the peer is told.
  pub fn leave(&self, id: &str, slot: usize) {
    let mut inner = self.lock();
    let Some(channel) = inner.channels.remove(id) else {
      return;
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
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
}
