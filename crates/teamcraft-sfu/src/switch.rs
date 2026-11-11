//! Moving a session to another connection without a rejoin (#1309): a call that had to take the
//! WebSocket fallback goes back to QUIC once UDP gets through again, with its slot, its DAVE group
//! and its event sequence, and without a word to the others.
//!
//! Every `joined` carries a secret of its session ([`SwitchSecret`]); it goes out only over that
//! session's own connection (TLS), so showing it proves the new connection belongs to the same
//! client. A connection that starts with `switch` instead of `join` is held to the same limits
//! before it is in the call as one that joins, but its token isn't checked again and it takes no
//! turn of the join pace: nobody joins, and nothing re-keys.
//!
//! The secrets of the sessions that run are kept here, at most one per session, and each goes when
//! its session ends or once it was used. The first [`SELECTOR_BYTES`] of a secret find its entry;
//! the whole secret is then compared in constant time, so the time a lookup takes tells nothing
//! about the part that matters.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rand_core::{OsRng, RngCore};
use subtle::ConstantTimeEq;
use teamcraft_proto::{Base64Bytes, SWITCH_SECRET_BYTES};
use tracing::debug;

use crate::auth::JoinGrant;
use crate::room::{Room, SessionId};
use crate::signaling::Handover;

/// The bytes of a secret that pick its entry: no secret in themselves.
const SELECTOR_BYTES: usize = 8;

/// The secret one session hands its client with `joined`. Not `Clone`, no `Debug` of its bytes.
pub(crate) struct SwitchSecret([u8; SWITCH_SECRET_BYTES]);

/// The secrets of the sessions that may move.
#[derive(Default)]
pub(crate) struct SwitchSecrets {
  entries: Mutex<HashMap<[u8; SELECTOR_BYTES], Entry>>,
  next_registration: AtomicU64,
}

struct Entry {
  secret: [u8; SWITCH_SECRET_BYTES],
  target: SwitchTarget,
  registration: u64,
}

/// The session a secret moves.
#[derive(Clone)]
pub(crate) struct SwitchTarget {
  pub(crate) room: Arc<Room>,
  pub(crate) grant: JoinGrant,
  pub(crate) session_id: SessionId,
  /// Where the session's signaling queue goes when it moves.
  pub(crate) handover: Arc<Handover>,
}

/// A session's secret while the session runs: dropping it withdraws the secret, unless it was used.
pub(crate) struct SwitchRegistration<'a> {
  secrets: &'a SwitchSecrets,
  selector: [u8; SELECTOR_BYTES],
  registration: u64,
}

impl SwitchSecret {
  /// A fresh secret from the system's generator; `None` if it fails, and the session can't move.
  pub(crate) fn random() -> Option<Self> {
    let mut bytes = [0u8; SWITCH_SECRET_BYTES];
    match OsRng.try_fill_bytes(&mut bytes) {
      Ok(()) => Some(Self(bytes)),
      Err(error) => {
        debug!(%error, "no switch secret: the system's random generator failed");
        None
      },
    }
  }

  /// As `joined` carries it.
  pub(crate) fn to_wire(&self) -> Base64Bytes {
    Base64Bytes::new(self.0.to_vec())
  }

  #[cfg(test)]
  pub(crate) fn from_bytes(bytes: [u8; SWITCH_SECRET_BYTES]) -> Self {
    Self(bytes)
  }
}

impl SwitchSecrets {
  /// Keeps `secret` for `target` until the returned registration is dropped. `None` in the unlikely
  /// case that another session's secret starts the same: this one can't move then.
  pub(crate) fn register(
    &self,
    secret: SwitchSecret,
    target: SwitchTarget,
  ) -> Option<SwitchRegistration<'_>> {
    let selector = selector_of(&secret.0);
    let registration = self.next_registration.fetch_add(1, Ordering::Relaxed);
    let mut entries = lock(&self.entries);
    if entries.contains_key(&selector) {
      debug!("two switch secrets start the same: this session can't move");
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
    Some(SwitchRegistration {
      secrets: self,
      selector,
      registration,
    })
  }

  /// The session `secret` moves, if it is one of a session that runs: once, the secret is used up
  /// by this.
  pub(crate) fn take(&self, secret: &[u8]) -> Option<SwitchTarget> {
    let secret: &[u8; SWITCH_SECRET_BYTES] = secret.try_into().ok()?;
    let selector = selector_of(secret);
    let mut entries = lock(&self.entries);
    let entry = entries.get(&selector)?;
    if !bool::from(entry.secret.ct_eq(secret)) {
      return None;
    }
    entries.remove(&selector).map(|entry| entry.target)
  }

  /// Secrets kept now.
  #[cfg(test)]
  pub(crate) fn len(&self) -> usize {
    lock(&self.entries).len()
  }
}

impl Drop for SwitchRegistration<'_> {
  fn drop(&mut self) {
    let mut entries = lock(&self.secrets.entries);
    // Only its own: once used, the selector may belong to another session's secret.
    if entries
      .get(&self.selector)
      .is_some_and(|entry| entry.registration == self.registration)
    {
      entries.remove(&self.selector);
    }
  }
}

fn selector_of(secret: &[u8; SWITCH_SECRET_BYTES]) -> [u8; SELECTOR_BYTES] {
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
  use teamcraft_proto::{Id, MediaCapabilities, Snowflake};
  use tokio::sync::mpsc;

  use super::*;
  use crate::config::SfuConfig;
  use crate::room::{ParticipantHandle, ParticipantLink, Rooms};

  #[test]
  fn a_secret_finds_its_session_once() {
    let secrets = SwitchSecrets::default();
    let (target, _rx) = target();
    let session_id = target.session_id;
    let _registration = secrets
      .register(SwitchSecret::from_bytes([5; 32]), target)
      .unwrap();

    let found = secrets.take(&[5; 32]).expect("the secret of a running session");
    assert_eq!(found.session_id, session_id);
    assert!(secrets.take(&[5; 32]).is_none(), "used up");
  }

  #[test]
  fn an_unknown_or_malformed_secret_finds_nothing() {
    let secrets = SwitchSecrets::default();
    let (target, _rx) = target();
    let _registration = secrets
      .register(SwitchSecret::from_bytes([5; 32]), target)
      .unwrap();
    let mut close = [5; 32];
    close[31] = 6;
    assert!(
      secrets.take(&close).is_none(),
      "the same selector, another secret"
    );
    assert!(secrets.take(&[7; 32]).is_none());
    assert!(secrets.take(&[5; 16]).is_none());
    assert!(secrets.take(&[]).is_none());
    assert!(
      secrets.take(&[5; 32]).is_some(),
      "a wrong guess doesn't use it up"
    );
  }

  #[test]
  fn a_session_that_ends_takes_its_secret_along() {
    let secrets = SwitchSecrets::default();
    let (target, _rx) = target();
    let registration = secrets
      .register(SwitchSecret::from_bytes([5; 32]), target)
      .unwrap();
    assert_eq!(secrets.len(), 1);
    drop(registration);
    assert_eq!(secrets.len(), 0);
    assert!(secrets.take(&[5; 32]).is_none());
  }

  #[test]
  fn a_used_secrets_end_leaves_a_later_one_alone() {
    let secrets = SwitchSecrets::default();
    let (first, _rx) = target();
    let first = secrets
      .register(SwitchSecret::from_bytes([5; 32]), first)
      .unwrap();
    assert!(secrets.take(&[5; 32]).is_some());
    // Another session's secret that happens to start the same.
    let (second, _rx) = target();
    let mut later = [5; 32];
    later[31] = 1;
    let _second = secrets.register(SwitchSecret::from_bytes(later), second).unwrap();
    drop(first);
    assert!(secrets.take(&later).is_some());
  }

  #[test]
  fn two_secrets_that_start_the_same_are_not_both_kept() {
    let secrets = SwitchSecrets::default();
    let (first, _rx) = target();
    let (second, _rx) = target();
    let _first = secrets
      .register(SwitchSecret::from_bytes([5; 32]), first)
      .unwrap();
    let mut other = [5; 32];
    other[31] = 1;
    assert!(
      secrets
        .register(SwitchSecret::from_bytes(other), second)
        .is_none()
    );
  }

  #[test]
  fn fresh_secrets_differ() {
    let first = SwitchSecret::random().unwrap();
    let second = SwitchSecret::random().unwrap();
    assert_ne!(first.0, second.0);
    assert_eq!(first.to_wire().len(), SWITCH_SECRET_BYTES);
  }

  /// A session in a room of its own, and its signaling queue.
  fn target() -> (SwitchTarget, mpsc::Receiver<crate::signaling::Outgoing>) {
    let rooms = Rooms::new(SfuConfig::default());
    let (signal_tx, signal_rx) = mpsc::channel(64);
    let handle = ParticipantHandle {
      link: Arc::new(QuietLink),
      signal_tx,
    };
    let grant = JoinGrant {
      call_id: id(9),
      user_id: id(1),
      device_id: id(10),
      maybe_conversation_id: None,
    };
    let (room, session_id, _) = rooms
      .join(
        grant,
        MediaCapabilities::default(),
        teamcraft_proto::DaveTransitionMode::Coordinated,
        None,
        handle,
        None,
      )
      .unwrap();
    let target = SwitchTarget {
      room,
      grant,
      session_id,
      handover: Arc::default(),
    };
    (target, signal_rx)
  }

  struct QuietLink;

  impl ParticipantLink for QuietLink {
    fn send_datagram(&self, _bytes: &[u8]) {}
    fn close(&self, _code: u32, _reason: &[u8]) {}
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
