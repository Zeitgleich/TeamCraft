//! A second connection for a session's own audio.
//!
//! On the WebSocket fallback (#1979): on one TCP connection a lost segment holds up everything
//! behind it, and a voice behind a camera's video was late by up to a second at every loss. On a
//! connection of its own the voice waits only for its own losses.
//!
//! On QUIC (#2141): a connection that carries nothing but voice can be marked DSCP Expedited
//! Forwarding as a whole, so the first hop's voice queue serves it; the session's connection
//! carries video too and stays unmarked (RFC 8837). quinn has no per-datagram mark.
//!
//! Every session gets a random secret right after its join
//! ([`teamcraft_proto::CallServerMessage::AudioSocket`]), over its own connection (TLS) only. A
//! second connection of the same transport that starts with `attach_audio` and that secret carries
//! the session's media datagrams to the SFU from then on; the SFU sends nothing on it but its close.
//! It is no session of its own: no slot, no DAVE leaf, no events. A fallback one holds a connection
//! slot of the call's fallback connections; a QUIC one holds nothing once attached (one per session
//! at most, see below). It ends when its session ends or another connection attaches in its place.
//!
//! As with the switch secrets ([`crate::switch`]), the first [`SELECTOR_BYTES`] of a secret find its
//! entry and the whole secret is then compared in constant time. Unlike them, a secret stays good for
//! as long as its session runs on its connection: a client whose audio connection broke attaches
//! again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;
use teamcraft_proto::{AUDIO_SOCKET_SECRET_BYTES, Base64Bytes};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Bytes;
use tracing::debug;

use crate::auth::JoinGrant;

/// The bytes of a secret that pick its entry: no secret in themselves.
const SELECTOR_BYTES: usize = 8;
/// Datagrams of a session's audio connection that wait for its session's task: a second of a voice
/// in 20 ms packets. More waiting is a session that stalled, and what comes then is dropped.
pub(crate) const AUDIO_SOCKET_QUEUE: usize = 64;
/// How often an audio connection looks whether it is still the one its session uses.
pub(crate) const AUDIO_SOCKET_CHECK_INTERVAL: Duration = Duration::from_secs(1);

/// The secret one fallback session hands its client. Not `Clone`, no `Debug` of its bytes.
pub(crate) struct AudioSocketSecret([u8; AUDIO_SOCKET_SECRET_BYTES]);

/// The secrets of the fallback sessions that take a second connection for their audio.
#[derive(Default)]
pub(crate) struct AudioSockets {
  entries: Mutex<HashMap<[u8; SELECTOR_BYTES], Entry>>,
  next_registration: AtomicU64,
}

struct Entry {
  secret: [u8; AUDIO_SOCKET_SECRET_BYTES],
  target: AudioTarget,
  registration: u64,
}

/// Where an audio connection's datagrams go: the session's own connection task, which hands them
/// to the session as its own.
#[derive(Clone)]
pub(crate) struct AudioTarget {
  /// Whose session it is: the audio connection's upgrade token, if it showed one, must be the
  /// same device's in the same call.
  pub(crate) grant: JoinGrant,
  pub(crate) datagrams: mpsc::Sender<Bytes>,
  /// The number of the audio connection attached last: an earlier one stops.
  attached: Arc<AtomicU64>,
}

/// A session's secret while the session runs on its connection: dropping it withdraws the secret.
pub(crate) struct AudioSocketRegistration<'a> {
  sockets: &'a AudioSockets,
  selector: [u8; SELECTOR_BYTES],
  registration: u64,
}

impl AudioSocketSecret {
  /// A fresh secret from the system's generator; `None` if it fails, and the session's audio stays
  /// on its connection.
  pub(crate) fn random() -> Option<Self> {
    let mut bytes = [0u8; AUDIO_SOCKET_SECRET_BYTES];
    match OsRng.try_fill_bytes(&mut bytes) {
      Ok(()) => Some(Self(bytes)),
      Err(error) => {
        debug!(%error, "no audio socket secret: the system's random generator failed");
        None
      },
    }
  }

  /// As [`teamcraft_proto::CallServerMessage::AudioSocket`] carries it.
  pub(crate) fn to_wire(&self) -> Base64Bytes {
    Base64Bytes::new(self.0.to_vec())
  }

  #[cfg(test)]
  pub(crate) fn from_bytes(bytes: [u8; AUDIO_SOCKET_SECRET_BYTES]) -> Self {
    Self(bytes)
  }
}

impl AudioTarget {
  pub(crate) fn new(grant: JoinGrant, datagrams: mpsc::Sender<Bytes>) -> Self {
    Self {
      grant,
      datagrams,
      attached: Arc::default(),
    }
  }

  /// A new audio connection attaches: the number it goes by, which ends any earlier one.
  pub(crate) fn attach(&self) -> u64 {
    self.attached.fetch_add(1, Ordering::AcqRel) + 1
  }

  /// Whether the audio connection `number` is still the one attached last.
  pub(crate) fn is_current(&self, number: u64) -> bool {
    self.attached.load(Ordering::Acquire) == number
  }
}

impl AudioSockets {
  /// Keeps `secret` for `target` until the returned registration is dropped. `None` in the unlikely
  /// case that another session's secret starts the same: this one's audio stays on its connection.
  pub(crate) fn register(
    &self,
    secret: &AudioSocketSecret,
    target: AudioTarget,
  ) -> Option<AudioSocketRegistration<'_>> {
    let selector = selector_of(&secret.0);
    let registration = self.next_registration.fetch_add(1, Ordering::Relaxed);
    let mut entries = lock(&self.entries);
    if entries.contains_key(&selector) {
      debug!("two audio socket secrets start the same: this session's audio stays on its connection");
      return None;
    }
    entries.insert(
      selector,
      Entry {
        secret: secret.0,
        target,
        registration,
      },
    );
    Some(AudioSocketRegistration {
      sockets: self,
      selector,
      registration,
    })
  }

  /// The session `secret` belongs to, if it is one of a session that runs on its connection.
  pub(crate) fn find(&self, secret: &[u8]) -> Option<AudioTarget> {
    let secret: &[u8; AUDIO_SOCKET_SECRET_BYTES] = secret.try_into().ok()?;
    let entries = lock(&self.entries);
    let entry = entries.get(&selector_of(secret))?;
    bool::from(entry.secret.ct_eq(secret)).then(|| entry.target.clone())
  }

  /// Secrets kept now.
  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    lock(&self.entries).len()
  }
}

impl Drop for AudioSocketRegistration<'_> {
  fn drop(&mut self) {
    let mut entries = lock(&self.sockets.entries);
    if entries
      .get(&self.selector)
      .is_some_and(|entry| entry.registration == self.registration)
    {
      entries.remove(&self.selector);
    }
  }
}

fn selector_of(secret: &[u8; AUDIO_SOCKET_SECRET_BYTES]) -> [u8; SELECTOR_BYTES] {
  let mut selector = [0u8; SELECTOR_BYTES];
  selector.copy_from_slice(&secret[..SELECTOR_BYTES]);
  selector
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // Nothing spans a panic point while it is held.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_secret_finds_its_session_as_long_as_it_runs() {
    let sockets = AudioSockets::default();
    let (target, _rx) = target();
    let registration = sockets
      .register(&AudioSocketSecret::from_bytes([5; 32]), target)
      .unwrap();
    let found = sockets.find(&[5; 32]).expect("the secret of a running session");
    assert_eq!(found.grant.device_id, id(10));
    assert!(
      sockets.find(&[5; 32]).is_some(),
      "not used up: a broken audio connection attaches again"
    );
    drop(registration);
    assert_eq!(sockets.len(), 0);
    assert!(sockets.find(&[5; 32]).is_none(), "gone with its session");
  }

  #[test]
  fn an_unknown_or_malformed_secret_finds_nothing() {
    let sockets = AudioSockets::default();
    let (target, _rx) = target();
    let _registration = sockets
      .register(&AudioSocketSecret::from_bytes([5; 32]), target)
      .unwrap();
    let mut close = [5; 32];
    close[31] = 6;
    assert!(
      sockets.find(&close).is_none(),
      "the same selector, another secret"
    );
    assert!(sockets.find(&[7; 32]).is_none());
    assert!(sockets.find(&[5; 16]).is_none());
    assert!(sockets.find(&[]).is_none());
  }

  #[test]
  fn two_secrets_that_start_the_same_are_not_both_kept() {
    let sockets = AudioSockets::default();
    let (first, _rx) = target();
    let (second, _rx) = target();
    let _first = sockets
      .register(&AudioSocketSecret::from_bytes([5; 32]), first)
      .unwrap();
    let mut other = [5; 32];
    other[31] = 1;
    assert!(
      sockets
        .register(&AudioSocketSecret::from_bytes(other), second)
        .is_none()
    );
  }

  #[test]
  fn an_ended_sessions_registration_leaves_a_later_one_alone() {
    let sockets = AudioSockets::default();
    let (first, _rx) = target();
    let first = sockets
      .register(&AudioSocketSecret::from_bytes([5; 32]), first)
      .unwrap();
    drop(first);
    let (second, _rx) = target();
    let _second = sockets
      .register(&AudioSocketSecret::from_bytes([5; 32]), second)
      .unwrap();
    assert!(sockets.find(&[5; 32]).is_some());
  }

  #[test]
  fn a_later_audio_connection_ends_the_earlier_one() {
    let (target, _rx) = target();
    let first = target.attach();
    assert!(target.is_current(first));
    let second = target.clone().attach();
    assert!(!target.is_current(first));
    assert!(target.is_current(second));
  }

  #[test]
  fn fresh_secrets_differ() {
    let first = AudioSocketSecret::random().unwrap();
    let second = AudioSocketSecret::random().unwrap();
    assert_ne!(first.0, second.0);
    assert_eq!(first.to_wire().len(), AUDIO_SOCKET_SECRET_BYTES);
  }

  fn target() -> (AudioTarget, mpsc::Receiver<Bytes>) {
    let (tx, rx) = mpsc::channel(8);
    let grant = JoinGrant {
      call_id: id(9),
      user_id: id(1),
      device_id: id(10),
      maybe_conversation_id: None,
    };
    (AudioTarget::new(grant, tx), rx)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
