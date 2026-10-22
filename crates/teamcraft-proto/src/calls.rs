//! Call signaling between a client and the SFU (ARCHITECTURE.md §6).
//!
//! The client opens a WebTransport session to the SFU, opens one bidirectional stream and sends
//! [`CallClientMessage::Join`] with a join token from `teamcraft-server`. Messages on that stream
//! are framed as a 4-byte big-endian length followed by one JSON message. Every
//! [`CallServerMessage::Event`] carries a sequence number that increases by one per connection.
//! A dropped connection is not resumed: the client rejoins with a fresh token and gets a fresh
//! `joined` event (media state is ephemeral).
//!
//! Media flows as QUIC datagrams (`teamcraft-media::datagram`), not through these types.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{Base64Bytes, CallId, DeviceId, ErrorBody, UserId};

/// `aud` of call join tokens.
pub const CALL_JOIN_TOKEN_AUDIENCE: &str = "teamcraft-sfu";

/// Claims of a call join token: a JWT signed by `teamcraft-server` (EdDSA) and verified by the
/// SFU with the server's public key. Short-lived (about a minute); the SFU checks it once on join.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallJoinClaims {
  /// Issuer, the server's public base URL.
  pub iss: String,
  /// Always [`CALL_JOIN_TOKEN_AUDIENCE`].
  pub aud: String,
  /// Issued at, Unix seconds.
  pub iat: u64,
  /// Expiry, Unix seconds.
  pub exp: u64,
  pub call_id: CallId,
  pub user_id: UserId,
  pub device_id: DeviceId,
}

/// Messages from the client to the SFU.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallClientMessage {
  /// First message of every connection. `token` is a call join token from `teamcraft-server`.
  Join { token: String },
  /// The client's DAVE key package, sent after [`CallEvent::DaveExternalSender`].
  DaveKeyPackage { key_package: Base64Bytes },
  /// The client's commit for the proposals of `epoch`; `maybe_welcome` when members were added.
  DaveCommit {
    epoch: u64,
    commit: Base64Bytes,
    maybe_welcome: Option<Base64Bytes>,
  },
  #[serde(other)]
  Unknown,
}

/// Frames sent by the SFU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallServerMessage {
  Event {
    seq: u64,
    event: CallEvent,
  },
  /// The request failed; for `join` the SFU closes the connection afterwards.
  Error {
    error: ErrorBody,
  },
  #[serde(other)]
  Unknown,
}

/// Things that happened in the call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CallEvent {
  /// Answer to `join`: who we are in the call and who else is there.
  Joined {
    call_id: CallId,
    participant: CallParticipant,
    participants: Vec<CallParticipant>,
  },
  ParticipantJoined {
    participant: CallParticipant,
  },
  ParticipantLeft {
    device_id: DeviceId,
  },
  /// Devices whose audio the SFU currently forwards, loudest first.
  ActiveSpeakers {
    device_ids: Vec<DeviceId>,
  },
  /// Serialized MLS `ExternalSender` the client installs before creating its key package.
  DaveExternalSender {
    package: Base64Bytes,
  },
  /// Proposals for `epoch`. Every member commits; the SFU announces the first commit. Clients
  /// must check `expected_participants` against the roster they trust (e2ee.md §3).
  DaveProposals {
    epoch: u64,
    proposals: Base64Bytes,
    expected_participants: Vec<DeviceId>,
  },
  /// The selected commit for `epoch`; members move to `epoch + 1`.
  DaveAnnounceCommit {
    epoch: u64,
    commit: Base64Bytes,
  },
  DaveWelcome {
    welcome: Base64Bytes,
  },
  #[serde(other)]
  Unknown,
}

/// One device in a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallParticipant {
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// Stream id the device's audio datagrams carry (assigned by the SFU).
  pub audio_stream_id: MediaStreamId,
}

/// SFU-assigned id of one media stream, as in the datagram header (`teamcraft-media::datagram`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MediaStreamId(pub u32);

// Join tokens don't belong in logs.
impl fmt::Debug for CallClientMessage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Join { .. } => f.write_str("Join { token: [redacted] }"),
      Self::DaveKeyPackage { key_package } => f
        .debug_struct("DaveKeyPackage")
        .field("key_package", key_package)
        .finish(),
      Self::DaveCommit {
        epoch,
        commit,
        maybe_welcome,
      } => f
        .debug_struct("DaveCommit")
        .field("epoch", epoch)
        .field("commit", commit)
        .field("maybe_welcome", maybe_welcome)
        .finish(),
      Self::Unknown => f.write_str("Unknown"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{ErrorCode, Id, Snowflake};

  #[test]
  fn client_messages_wire_format() {
    let join = CallClientMessage::Join { token: "t".into() };
    assert_eq!(
      serde_json::to_string(&join).unwrap(),
      r#"{"type":"join","token":"t"}"#
    );
    let commit = CallClientMessage::DaveCommit {
      epoch: 3,
      commit: Base64Bytes::new(vec![1, 2]),
      maybe_welcome: None,
    };
    assert_eq!(
      serde_json::to_string(&commit).unwrap(),
      r#"{"type":"dave_commit","epoch":3,"commit":"AQI=","maybe_welcome":null}"#
    );
  }

  #[test]
  fn server_event_wire_format() {
    let message = CallServerMessage::Event {
      seq: 1,
      event: CallEvent::ParticipantJoined {
        participant: participant(),
      },
    };
    let json = serde_json::to_string(&message).unwrap();
    assert_eq!(
      json,
      r#"{"type":"event","seq":1,"event":{"type":"participant_joined","participant":{"user_id":"4","device_id":"5","audio_stream_id":9}}}"#
    );
    assert_eq!(serde_json::from_str::<CallServerMessage>(&json).unwrap(), message);
  }

  #[test]
  fn error_wire_format() {
    let message = CallServerMessage::Error {
      error: ErrorBody::new(ErrorCode::InvalidJoinToken, "expired"),
    };
    assert_eq!(
      serde_json::to_string(&message).unwrap(),
      r#"{"type":"error","error":{"error_code":"invalid_join_token","message":"expired"}}"#
    );
  }

  #[test]
  fn unknown_variants_are_tolerated() {
    let client: CallClientMessage = serde_json::from_str(r#"{"type":"raise_hand"}"#).unwrap();
    assert_eq!(client, CallClientMessage::Unknown);
    let event: CallServerMessage =
      serde_json::from_str(r#"{"type":"event","seq":2,"event":{"type":"reaction","emoji":"x"}}"#).unwrap();
    assert_eq!(
      event,
      CallServerMessage::Event {
        seq: 2,
        event: CallEvent::Unknown
      }
    );
    let frame: CallServerMessage = serde_json::from_str(r#"{"type":"goaway"}"#).unwrap();
    assert_eq!(frame, CallServerMessage::Unknown);
  }

  #[test]
  fn join_claims_wire_format() {
    let claims = CallJoinClaims {
      iss: "https://chat.example.com".into(),
      aud: CALL_JOIN_TOKEN_AUDIENCE.into(),
      iat: 100,
      exp: 160,
      call_id: CallId::from_snowflake(Snowflake::new(7).unwrap()),
      user_id: UserId::from_snowflake(Snowflake::new(4).unwrap()),
      device_id: DeviceId::from_snowflake(Snowflake::new(5).unwrap()),
    };
    assert_eq!(
      serde_json::to_string(&claims).unwrap(),
      r#"{"iss":"https://chat.example.com","aud":"teamcraft-sfu","iat":100,"exp":160,"call_id":"7","user_id":"4","device_id":"5"}"#
    );
  }

  #[test]
  fn join_token_is_redacted_in_debug_output() {
    let join = CallClientMessage::Join {
      token: "secret-token".into(),
    };
    assert!(!format!("{join:?}").contains("secret-token"));
  }

  fn participant() -> CallParticipant {
    CallParticipant {
      user_id: UserId::from_snowflake(Snowflake::new(4).unwrap()),
      device_id: DeviceId::from_snowflake(Snowflake::new(5).unwrap()),
      audio_stream_id: MediaStreamId(9),
    }
  }
}
