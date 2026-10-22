//! Framing of the call signaling stream and the per-connection event sequence.
//!
//! Message types live in `teamcraft_proto` (`CallClientMessage`, `CallServerMessage`); the wire is
//! a 4-byte big-endian length followed by one JSON message.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{CallEvent, CallServerMessage, ErrorBody};
use thiserror::Error;
use wtransport::{RecvStream, SendStream};

/// What the SFU queues for one connection; the writer turns it into a [`CallServerMessage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outgoing {
  Event(CallEvent),
  Error(ErrorBody),
}

/// Numbers events per connection, starting at 1 (api-conventions.md).
#[derive(Debug, Default)]
pub struct EventSequence {
  last_seq: u64,
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
  pub fn frame(&mut self, outgoing: Outgoing) -> CallServerMessage {
    match outgoing {
      Outgoing::Event(event) => {
        self.last_seq += 1;
        CallServerMessage::Event {
          seq: self.last_seq,
          event,
        }
      },
      Outgoing::Error(error) => CallServerMessage::Error { error },
    }
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
  fn events_are_numbered_from_one_and_errors_are_not() {
    let mut sequence = EventSequence::default();
    let event = || {
      Outgoing::Event(CallEvent::ActiveSpeakers {
        device_ids: Vec::new(),
      })
    };
    assert!(matches!(
      sequence.frame(event()),
      CallServerMessage::Event { seq: 1, .. }
    ));
    let error = sequence.frame(Outgoing::Error(ErrorBody::new(ErrorCode::ProtocolViolation, "x")));
    assert!(matches!(error, CallServerMessage::Error { .. }));
    assert!(matches!(
      sequence.frame(event()),
      CallServerMessage::Event { seq: 2, .. }
    ));
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
