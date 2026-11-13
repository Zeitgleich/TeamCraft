//! Framing of the call signaling stream and the per-session event sequence.
//!
//! Message types live in `teamcraft_proto` (`CallClientMessage`, `CallServerMessage`); the wire is
//! a 4-byte big-endian length followed by one JSON message.
//!
//! A session's queue and the numbering of its events belong together ([`SignalQueue`]): when the
//! session moves to another connection (#1309), the old connection's writer sends what was queued
//! before the move and then hands the queue on to the new connection's writer, which numbers on.
//! So the client sees one sequence without a gap, whichever connection carried which part.

use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{CallEvent, CallServerMessage, ErrorBody};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use wtransport::{RecvStream, SendStream};

/// What the SFU queues for one session; the writer turns it into a [`CallServerMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
  Event(CallEvent),
  Error(ErrorBody),
  /// The answer to a client's ping of `id`, read at `received_us` on the SFU's clock (#1890).
  Pong {
    id: u64,
    received_us: u64,
  },
  /// The session moved to another connection (#1309): what was queued before this goes out on the
  /// current one, then its writer hands the queue on ([`SignalQueue::hand_over`]) and closes it.
  HandOver,
}

/// Numbers events per session, starting at 1 (api-conventions.md).
#[derive(Debug, Default)]
pub struct EventSequence {
  last_seq: u64,
}

/// A session's signaling on its way out: its queue and the numbering of its events, owned by the
/// writer of the connection that carries the session.
#[derive(Debug)]
pub struct SignalQueue {
  outgoing: mpsc::Receiver<Outgoing>,
  sequence: EventSequence,
  handover: Arc<Handover>,
}

/// Where a session's [`SignalQueue`] goes when the session moves to another connection: set by the
/// move, before it queues [`Outgoing::HandOver`].
#[derive(Debug, Default)]
pub struct Handover {
  maybe_to: Mutex<Option<oneshot::Sender<SignalQueue>>>,
}

/// What comes next out of a [`SignalQueue`].
#[derive(Debug)]
pub enum Dequeued {
  /// To write on the connection, numbered if it is an event. Boxed: a message is large next to
  /// the hand-over marker.
  Message(Box<CallServerMessage>),
  /// The session moved: nothing more goes out on this connection.
  HandOver,
}

#[derive(Debug, Error)]
pub enum SignalingError {
  #[error("stream read failed: {0}")]
  Read(#[from] wtransport::error::StreamReadExactError),
  #[error("stream write failed: {0}")]
  Write(#[from] wtransport::error::StreamWriteError),
  #[error("message of {actual_bytes} bytes exceeds the limit of {max_bytes} bytes")]
  TooLarge { actual_bytes: usize, max_bytes: usize },
  #[error("invalid JSON: {0}")]
  Json(#[from] serde_json::Error),
}

impl EventSequence {
  /// The message for `outgoing`; `None` for [`Outgoing::HandOver`], which isn't one.
  pub fn frame(&mut self, outgoing: Outgoing) -> Option<CallServerMessage> {
    Some(match outgoing {
      Outgoing::Event(event) => {
        self.last_seq += 1;
        CallServerMessage::Event {
          seq: self.last_seq,
          event,
        }
      },
      Outgoing::Error(error) => CallServerMessage::Error { error },
      Outgoing::Pong { id, received_us } => CallServerMessage::Pong {
        id,
        maybe_received_us: Some(received_us),
      },
      Outgoing::HandOver => return None,
    })
  }
}

impl SignalQueue {
  /// A fresh session's queue: its events are numbered from 1.
  pub fn new(outgoing: mpsc::Receiver<Outgoing>, handover: Arc<Handover>) -> Self {
    Self {
      outgoing,
      sequence: EventSequence::default(),
      handover,
    }
  }

  /// The next thing to do; `None` when nothing more can come (every sender is gone). Cancel-safe.
  pub async fn next(&mut self) -> Option<Dequeued> {
    let outgoing = self.outgoing.recv().await?;
    Some(match self.sequence.frame(outgoing) {
      Some(message) => Dequeued::Message(Box::new(message)),
      None => Dequeued::HandOver,
    })
  }

  /// Passes the queue on to the connection the session moved to, after [`Dequeued::HandOver`].
  /// `false` if that one is gone already (its own end then decides about the slot).
  pub fn hand_over(self) -> bool {
    let Some(to) = self.handover.take() else {
      return false;
    };
    to.send(self).is_ok()
  }
}

impl Handover {
  /// The session is about to move: the queue will come out of the returned receiver.
  pub fn prepare(&self) -> oneshot::Receiver<SignalQueue> {
    let (to, from) = oneshot::channel();
    *lock(&self.maybe_to) = Some(to);
    from
  }

  /// The move didn't happen after all.
  pub fn cancel(&self) {
    lock(&self.maybe_to).take();
  }

  fn take(&self) -> Option<oneshot::Sender<SignalQueue>> {
    lock(&self.maybe_to).take()
  }
}

/// Reads one length-prefixed JSON message.
pub async fn read_message<T>(stream: &mut RecvStream, max_bytes: usize) -> Result<T, SignalingError>
where
  T: for<'de> Deserialize<'de>,
{
  let mut length = [0u8; 4];
  stream.read_exact(&mut length).await?;
  let actual_bytes = u32::from_be_bytes(length) as usize;
  if actual_bytes > max_bytes {
    return Err(SignalingError::TooLarge {
      actual_bytes,
      max_bytes,
    });
  }
  let mut body = vec![0u8; actual_bytes];
  stream.read_exact(&mut body).await?;
  Ok(serde_json::from_slice(&body)?)
}

/// Writes one length-prefixed JSON message.
pub async fn write_message<T: Serialize>(stream: &mut SendStream, message: &T) -> Result<(), SignalingError> {
  stream.write_all(&encode_message(message)?).await?;
  Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  // Nothing spans a panic point while it is held.
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Length-prefixed JSON encoding of one message.
pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>, SignalingError> {
  let body = serde_json::to_vec(message)?;
  let length = u32::try_from(body.len()).map_err(|_| SignalingError::TooLarge {
    actual_bytes: body.len(),
    max_bytes: u32::MAX as usize,
  })?;
  let mut framed = Vec::with_capacity(4 + body.len());
  framed.extend_from_slice(&length.to_be_bytes());
  framed.extend_from_slice(&body);
  Ok(framed)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallClientMessage, ErrorCode};

  use super::*;

  #[test]
  fn events_are_numbered_from_one_and_errors_and_pongs_are_not() {
    let mut sequence = EventSequence::default();
    let event = || {
      Outgoing::Event(CallEvent::ActiveSpeakers {
        device_ids: Vec::new(),
      })
    };
    assert!(matches!(
      sequence.frame(event()),
      Some(CallServerMessage::Event { seq: 1, .. })
    ));
    let error = sequence.frame(Outgoing::Error(ErrorBody::new(ErrorCode::ProtocolViolation, "x")));
    assert!(matches!(error, Some(CallServerMessage::Error { .. })));
    assert_eq!(
      sequence.frame(Outgoing::Pong {
        id: 9,
        received_us: 5
      }),
      Some(CallServerMessage::Pong {
        id: 9,
        maybe_received_us: Some(5)
      })
    );
    assert!(matches!(
      sequence.frame(event()),
      Some(CallServerMessage::Event { seq: 2, .. })
    ));
    assert_eq!(sequence.frame(Outgoing::HandOver), None);
  }

  /// A session that moves to another connection (#1309): what was queued before the move goes out
  /// on the old one, and the new one numbers on from there, also what was queued after the move.
  #[tokio::test]
  async fn a_handed_over_queue_numbers_on_without_a_gap() {
    let (signal_tx, signal_rx) = mpsc::channel(8);
    let handover = Arc::new(Handover::default());
    let mut old = SignalQueue::new(signal_rx, Arc::clone(&handover));
    signal_tx.try_send(event()).unwrap();
    signal_tx.try_send(event()).unwrap();
    let arrives = handover.prepare();
    signal_tx.try_send(Outgoing::HandOver).unwrap();
    signal_tx.try_send(event()).unwrap();

    assert!(matches!(
      old.next().await,
      Some(Dequeued::Message(message)) if matches!(*message, CallServerMessage::Event { seq: 1, .. })
    ));
    assert!(matches!(
      old.next().await,
      Some(Dequeued::Message(message)) if matches!(*message, CallServerMessage::Event { seq: 2, .. })
    ));
    assert!(matches!(old.next().await, Some(Dequeued::HandOver)));
    assert!(old.hand_over());
    let mut new = arrives.await.unwrap();
    assert!(matches!(
      new.next().await,
      Some(Dequeued::Message(message)) if matches!(*message, CallServerMessage::Event { seq: 3, .. })
    ));
  }

  #[tokio::test]
  async fn a_queue_for_a_connection_that_went_away_is_not_handed_over() {
    let (_signal_tx, signal_rx) = mpsc::channel(8);
    let handover = Arc::new(Handover::default());
    let queue = SignalQueue::new(signal_rx, Arc::clone(&handover));
    drop(handover.prepare());
    assert!(!queue.hand_over());
    let (_signal_tx, signal_rx) = mpsc::channel(8);
    let handover = Arc::new(Handover::default());
    drop(handover.prepare());
    handover.cancel();
    assert!(
      !SignalQueue::new(signal_rx, handover).hand_over(),
      "nobody asked for it"
    );
  }

  fn event() -> Outgoing {
    Outgoing::Event(CallEvent::ActiveSpeakers {
      device_ids: Vec::new(),
    })
  }

  #[test]
  fn framing_prefixes_length() {
    let framed = encode_message(&CallClientMessage::Unknown).unwrap();
    let body = &framed[4..];
    assert_eq!(
      u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize,
      body.len()
    );
  }
}
