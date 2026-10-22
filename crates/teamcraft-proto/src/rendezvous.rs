//! Device-linking relay (`/api/v1/rendezvous`, e2ee.md §5).
//!
//! Two devices of the same user connect with the same random `rendezvous_id` and the server forwards
//! **binary** frames between them without looking inside (the devices run their own authenticated
//! key exchange on top). Text frames are control frames defined here and never relayed.

use serde::{Deserialize, Serialize};

use crate::ErrorBody;

/// Control frames sent by a device. The first frame must be `join`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RendezvousClientMessage {
  /// `token` is the OIDC access token, `rendezvous_id` 32 random hex characters chosen by the
  /// device that shows the QR code (or the approval prompt).
  Join { token: String, rendezvous_id: String },
  #[serde(other)]
  Unknown,
}

/// Control frames sent by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RendezvousServerMessage {
  Joined {
    peer_present: bool,
  },
  PeerJoined,
  /// The other device left; the relay is closed.
  PeerLeft,
  /// The relay's lifetime ended.
  Expired,
  Error {
    error: ErrorBody,
  },
  #[serde(other)]
  Unknown,
}

impl std::fmt::Debug for RendezvousClientMessage {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::Join { rendezvous_id, .. } => f
        .debug_struct("Join")
        .field("token", &"[redacted]")
        .field("rendezvous_id", rendezvous_id)
        .finish(),
      Self::Unknown => f.write_str("Unknown"),
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wire_format_is_stable() {
    let join: RendezvousClientMessage =
      serde_json::from_str(r#"{"type":"join","token":"t","rendezvous_id":"ab"}"#).unwrap();
    assert!(matches!(join, RendezvousClientMessage::Join { .. }));
    assert_eq!(
      serde_json::to_string(&RendezvousServerMessage::PeerLeft).unwrap(),
      r#"{"type":"peer_left"}"#
    );
    assert_eq!(
      serde_json::to_string(&RendezvousServerMessage::Joined { peer_present: true }).unwrap(),
      r#"{"type":"joined","peer_present":true}"#
    );
  }

  #[test]
  fn unknown_frames_are_tolerated_and_tokens_redacted() {
    let unknown: RendezvousClientMessage = serde_json::from_str(r#"{"type":"wave"}"#).unwrap();
    assert!(matches!(unknown, RendezvousClientMessage::Unknown));
    let server: RendezvousServerMessage = serde_json::from_str(r#"{"type":"throttled"}"#).unwrap();
    assert_eq!(server, RendezvousServerMessage::Unknown);
    let join = RendezvousClientMessage::Join {
      token: "secret".into(),
      rendezvous_id: "ab".into(),
    };
    assert!(!format!("{join:?}").contains("secret"));
  }
}
