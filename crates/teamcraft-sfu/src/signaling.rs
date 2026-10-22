//! Signaling on the reliable bidirectional stream the client opens first.
//!
//! Framing: 4-byte big-endian length, then one JSON message. Binary DAVE payloads are JSON byte
//! arrays, which is simple for the browser page but wasteful; see SPIKE.md. These types move to
//! `teamcraft-proto` after the spike.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use wtransport::{RecvStream, SendStream};

use crate::ids::{CallId, DeviceId};

/// Messages from a client to the SFU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
  /// First message of every session.
  Join { token: String },
  /// The client's DAVE key package (after it received [`ServerMessage::DaveExternalSender`]).
  DaveKeyPackage { key_package: Vec<u8> },
  /// The client's commit for the proposals of `epoch`.
  DaveCommit {
    epoch: u64,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
  },
  #[serde(other)]
  Unknown,
}

/// Messages from the SFU to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
  Joined {
    call_id: CallId,
    participant_id: DeviceId,
    /// Stream id the client must put into its audio datagrams.
    audio_stream_id: u32,
    participants: Vec<ParticipantInfo>,
  },
  ParticipantJoined(ParticipantInfo),
  ParticipantLeft {
    participant_id: DeviceId,
  },
  /// Serialized MLS `ExternalSender` for this call.
  DaveExternalSender {
    package: Vec<u8>,
  },
  /// Proposals for `epoch`; every member commits, the SFU picks the first commit.
  DaveProposals {
    epoch: u64,
    proposals: Vec<u8>,
    expected_participants: Vec<DeviceId>,
  },
  /// The selected commit; members move to `epoch + 1`.
  DaveAnnounceCommit {
    epoch: u64,
    commit: Vec<u8>,
  },
  DaveWelcome {
    welcome: Vec<u8>,
  },
  Error {
    error_code: SignalingErrorCode,
    message: String,
  },
  #[serde(other)]
  Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipantInfo {
  pub participant_id: DeviceId,
  pub audio_stream_id: u32,
}

/// Stable error codes sent to clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalingErrorCode {
  InvalidToken,
  AlreadyJoined,
  ProtocolViolation,
  InvalidKeyPackage,
  #[serde(untagged)]
  Unknown(String),
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
  use super::*;

  #[test]
  fn client_message_wire_format() {
    let message = ClientMessage::Join {
      token: "dev:1:2".to_owned(),
    };
    assert_eq!(
      serde_json::to_string(&message).unwrap(),
      r#"{"type":"join","token":"dev:1:2"}"#
    );
    let commit = ClientMessage::DaveCommit {
      epoch: 3,
      commit: vec![1, 2],
      maybe_welcome: None,
    };
    assert_eq!(
      serde_json::to_string(&commit).unwrap(),
      r#"{"type":"dave_commit","epoch":3,"commit":[1,2],"maybe_welcome":null}"#
    );
  }

  #[test]
  fn server_message_wire_format() {
    let message = ServerMessage::ParticipantJoined(ParticipantInfo {
      participant_id: device(5),
      audio_stream_id: 9,
    });
    assert_eq!(
      serde_json::to_string(&message).unwrap(),
      r#"{"type":"participant_joined","participant_id":"5","audio_stream_id":9}"#
    );
    let error = ServerMessage::Error {
      error_code: SignalingErrorCode::InvalidToken,
      message: "no".to_owned(),
    };
    assert_eq!(
      serde_json::to_string(&error).unwrap(),
      r#"{"type":"error","error_code":"invalid_token","message":"no"}"#
    );
  }

  #[test]
  fn unknown_variants_are_tolerated() {
    let client: ClientMessage = serde_json::from_str(r#"{"type":"from_the_future","x":1}"#).unwrap();
    assert_eq!(client, ClientMessage::Unknown);
    let server: ServerMessage = serde_json::from_str(r#"{"type":"from_the_future"}"#).unwrap();
    assert_eq!(server, ServerMessage::Unknown);
    let code: SignalingErrorCode = serde_json::from_str(r#""rate_limited""#).unwrap();
    assert_eq!(code, SignalingErrorCode::Unknown("rate_limited".to_owned()));
  }

  #[test]
  fn framing_prefixes_length() {
    let framed = encode_message(&ClientMessage::Unknown).unwrap();
    let body = &framed[4..];
    assert_eq!(
      u32::from_be_bytes([framed[0], framed[1], framed[2], framed[3]]) as usize,
      body.len()
    );
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
