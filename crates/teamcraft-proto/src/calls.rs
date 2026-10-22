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

use crate::{
  Base64Bytes, CallId, DeviceId, ErrorBody, MediaCapabilities, TransportFeedback, UserId, VideoCodec,
  VideoLayerInfo, VideoTrack, VideoTrackKind,
};

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
  Join {
    token: String,
    /// Older clients omit it and are treated as audio-only.
    #[serde(default)]
    capabilities: MediaCapabilities,
  },
  /// The client's DAVE key package, sent after [`CallEvent::DaveExternalSender`].
  DaveKeyPackage {
    key_package: Base64Bytes,
    /// The device's signed call key binding for this key package's DAVE key (e2ee.md §8).
    #[serde(default)]
    maybe_key_binding: Option<Base64Bytes>,
  },
  /// The client's commit for the proposals of `epoch`; `maybe_welcome` when members were added.
  DaveCommit {
    epoch: u64,
    commit: Base64Bytes,
    maybe_welcome: Option<Base64Bytes>,
  },
  /// Starts sending a video track; the SFU answers with [`CallEvent::VideoPublished`].
  PublishVideo {
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  },
  UnpublishVideo {
    stream_id: MediaStreamId,
  },
  /// Arrival times of the datagrams the SFU sent to this client (downlink estimation).
  TransportFeedback {
    feedback: TransportFeedback,
  },
  /// Receive a video track up to `max_layer`; `None` stops receiving it.
  Subscribe {
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
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
    #[serde(default)]
    video_tracks: Vec<VideoTrack>,
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
    /// Call key bindings of the devices these proposals add.
    #[serde(default)]
    key_bindings: Vec<DaveKeyBinding>,
  },
  /// The selected commit for `epoch`; members move to `epoch + 1`.
  DaveAnnounceCommit {
    epoch: u64,
    commit: Base64Bytes,
  },
  DaveWelcome {
    welcome: Base64Bytes,
    /// Call key bindings of the group's other members.
    #[serde(default)]
    key_bindings: Vec<DaveKeyBinding>,
  },
  /// A video track was published (also sent to its publisher, which learns the stream id).
  VideoPublished {
    track: VideoTrack,
  },
  VideoUnpublished {
    stream_id: MediaStreamId,
  },
  /// To the publisher: send a keyframe on this layer soon (a receiver switches to it).
  KeyframeRequested {
    stream_id: MediaStreamId,
    layer: u8,
  },
  /// To the publisher: the layers someone receives; others may be paused to save uplink.
  LayersNeeded {
    stream_id: MediaStreamId,
    layers: Vec<u8>,
  },
  /// Arrival times of the datagrams this client sent (uplink estimation, done by the client).
  TransportFeedback {
    feedback: TransportFeedback,
  },
  /// The meeting host muted everyone; clients mute their microphone (and may unmute).
  MutedByHost {
    by_user_id: UserId,
  },
  /// The meeting host removed this user; the SFU closes the connection right after.
  RemovedByHost {
    by_user_id: UserId,
  },
  /// The meeting host ended the call for everyone; the SFU closes the connection right after.
  EndedByHost {
    by_user_id: UserId,
  },
  #[serde(other)]
  Unknown,
}

/// A device's signed call key binding (`teamcraft-crypto::call_binding`), relayed by the SFU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaveKeyBinding {
  pub device_id: DeviceId,
  pub binding: Base64Bytes,
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
      Self::Join { capabilities, .. } => f
        .debug_struct("Join")
        .field("token", &"[redacted]")
        .field("capabilities", capabilities)
        .finish(),
      Self::DaveKeyPackage {
        key_package,
        maybe_key_binding,
      } => f
        .debug_struct("DaveKeyPackage")
        .field("key_package", key_package)
        .field("maybe_key_binding", maybe_key_binding)
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
      Self::PublishVideo { kind, codec, layers } => f
        .debug_struct("PublishVideo")
        .field("kind", kind)
        .field("codec", codec)
        .field("layers", layers)
        .finish(),
      Self::TransportFeedback { feedback } => f
        .debug_struct("TransportFeedback")
        .field("feedback", feedback)
        .finish(),
      Self::UnpublishVideo { stream_id } => f
        .debug_struct("UnpublishVideo")
        .field("stream_id", stream_id)
        .finish(),
      Self::Subscribe {
        stream_id,
        maybe_max_layer,
      } => f
        .debug_struct("Subscribe")
        .field("stream_id", stream_id)
        .field("maybe_max_layer", maybe_max_layer)
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
    // Older clients send no capabilities: audio only.
    let join: CallClientMessage = serde_json::from_str(r#"{"type":"join","token":"t"}"#).unwrap();
    assert_eq!(
      join,
      CallClientMessage::Join {
        token: "t".into(),
        capabilities: MediaCapabilities::default(),
      }
    );
    let subscribe = CallClientMessage::Subscribe {
      stream_id: MediaStreamId(4),
      maybe_max_layer: Some(1),
    };
    assert_eq!(
      serde_json::to_string(&subscribe).unwrap(),
      r#"{"type":"subscribe","stream_id":4,"maybe_max_layer":1}"#
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
  fn video_events_wire_format() {
    let event = CallEvent::KeyframeRequested {
      stream_id: MediaStreamId(7),
      layer: 2,
    };
    assert_eq!(
      serde_json::to_string(&event).unwrap(),
      r#"{"type":"keyframe_requested","stream_id":7,"layer":2}"#
    );
    let feedback = CallEvent::TransportFeedback {
      feedback: TransportFeedback {
        base_seq: 65535,
        arrivals_us: vec![Some(10), None],
      },
    };
    assert_eq!(
      serde_json::to_string(&feedback).unwrap(),
      r#"{"type":"transport_feedback","feedback":{"base_seq":65535,"arrivals_us":[10,null]}}"#
    );
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
      capabilities: MediaCapabilities::default(),
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
