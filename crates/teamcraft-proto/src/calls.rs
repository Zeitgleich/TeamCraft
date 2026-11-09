//! Call signaling between a client and the SFU (ARCHITECTURE.md §6).
//!
//! The client opens a WebTransport session to the SFU, opens one bidirectional stream and sends
//! [`CallClientMessage::Join`] with a join token from `teamcraft-server`. Messages on that stream
//! are framed as a 4-byte big-endian length followed by one JSON message. Every
//! [`CallServerMessage::Event`] carries a sequence number that increases by one per session (a
//! connection, and the one it moves to, below).
//! A QUIC connection survives a change of the client's address (connection migration). If it is
//! lost anyway, the SFU keeps the device's slot for a grace period ([`CallEvent::Joined`] says how
//! long) and tells the others it is reconnecting; the client joins again with a fresh token on a
//! new connection and takes its slot back with a fresh `joined` event (ARCHITECTURE.md §6.5).
//!
//! A call that runs on the WebSocket fallback moves back to QUIC without a rejoin when UDP gets
//! through again (#1309): `joined` carries a secret of the session, and a new QUIC connection that
//! starts with [`CallClientMessage::Switch`] instead of `join` takes the session over, with its
//! slot, its DAVE group and its event sequence. The SFU answers [`CallServerMessage::Switched`],
//! sends what was queued for the old connection on it and closes it with
//! [`SFU_SWITCHED_CLOSE_CODE`]; the events after that come on the new one, numbered on.
//!
//! Media flows as QUIC datagrams (`teamcraft-media::datagram`), not through these types.

use std::{fmt, num::NonZeroU16};

use serde::{Deserialize, Serialize};

use crate::{
  Base64Bytes, CallId, ConversationId, DeviceId, ErrorBody, MediaCapabilities, TransportFeedback, UserId,
  VideoCodec, VideoLayerInfo, VideoPublicationId, VideoTrack, VideoTrackKind,
};

/// `aud` of call join tokens.
pub const CALL_JOIN_TOKEN_AUDIENCE: &str = "teamcraft-sfu";

/// QUIC application close code of an SFU that shuts down (a restart, a rolling deploy). Unlike the
/// SFU's other closes it is not final: clients rejoin with a fresh join token, spread over a short
/// moment so the restarted SFU isn't hit by every client at once (ARCHITECTURE.md §6.5).
pub const SFU_GOING_AWAY_CLOSE_CODE: u32 = 0x4805;

/// Close code (QUIC application close, or the fallback's close frame) of a connection whose session
/// moved to another one ([`CallClientMessage::Switch`], #1309): every event queued for it went out
/// before the close, and the next one comes on the new connection. Not the end of the call.
pub const SFU_SWITCHED_CLOSE_CODE: u32 = 0x4806;

/// Bytes of the secret that lets a session move to another connection ([`CallEvent::Joined`]).
pub const SWITCH_SECRET_BYTES: usize = 32;

/// A DAVE transition: the switch of every member's encryption key to a new epoch (DAVE's
/// `transition_id`, e2ee.md §8). Members install the new epoch's decryption keys when they get the
/// commit or welcome that carries it, say so with [`CallClientMessage::DaveTransitionReady`], and
/// encrypt with the new key from [`CallEvent::DaveExecuteTransition`] on, so no receiver misses
/// frames at the switch. Never 0: DAVE's id 0 means "switch at once", which is a commit or welcome
/// without an id here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DaveTransitionId(pub NonZeroU16);

/// How a client applies DAVE transitions, announced with [`CallClientMessage::Join`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaveTransitionMode {
  /// Clients from before #1541: every commit and welcome switches the encryption key at once. The
  /// SFU sends them no transition ids and waits for nothing from them.
  #[default]
  AtOnce,
  /// The client takes [`DaveTransitionId`]s: it answers [`CallClientMessage::DaveTransitionReady`]
  /// and switches on [`CallEvent::DaveExecuteTransition`].
  Coordinated,
  /// A newer mode this SFU doesn't know; treated as [`Self::AtOnce`].
  #[serde(other)]
  Unknown,
}

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
  /// The conversation the call belongs to. The SFU reports it with its roster, so a restarted
  /// server takes over the calls that kept running (`SfuCallRoster`). Signed like the rest, so the
  /// SFU can't make it up; tokens of older servers lack it.
  #[serde(rename = "conversation_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_conversation_id: Option<ConversationId>,
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
    /// How the client applies DAVE transitions. Older clients omit it: [`DaveTransitionMode::AtOnce`].
    #[serde(default)]
    dave_transition_mode: DaveTransitionMode,
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
  /// The client processed the commit or welcome of `transition_id` and holds the new epoch's
  /// decryption keys (DAVE opcode 23). Sent only by clients that joined with
  /// [`DaveTransitionMode::Coordinated`]; older SFUs ignore it.
  DaveTransitionReady {
    transition_id: DaveTransitionId,
  },
  /// Starts sending a video track; the SFU answers with [`CallEvent::VideoPublished`], whose track
  /// echoes `maybe_publication_id` (#929). Older clients send none; older SFUs ignore it.
  PublishVideo {
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maybe_publication_id: Option<VideoPublicationId>,
  },
  UnpublishVideo {
    stream_id: MediaStreamId,
  },
  /// Starts sending the shared screen's sound (system audio) as a second Opus stream; the SFU
  /// answers with [`CallEvent::ScreenAudioPublished`]. One per device.
  PublishScreenAudio,
  UnpublishScreenAudio {
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
  /// A subscribed track can't be decoded until its next keyframe (a lost fragment, a frame that
  /// didn't decrypt, a decoder that failed): the SFU asks the publisher for one on the layer this
  /// device receives. Rate-limited on both sides; older SFUs ignore it.
  RequestKeyframe {
    stream_id: MediaStreamId,
  },
  /// The layers of an own video track this device's uplink carries now (the lowest ones; empty:
  /// none), whatever [`CallEvent::LayersNeeded`] asks for. The SFU forwards no higher layer of it,
  /// so receivers move to a layer that is actually sent instead of freezing on a paused one
  /// (#921). Sent when it changes and for every own track after a rejoin; until then the SFU
  /// takes every published layer as sent. Older SFUs ignore it.
  UplinkLayers {
    stream_id: MediaStreamId,
    layers: Vec<u8>,
  },
  /// This device muted or unmuted its microphone; the SFU tells the others
  /// ([`CallEvent::ParticipantMuted`]). Sent again after every (re)join. Metadata only: it changes
  /// nothing about what the SFU forwards.
  SetMuted {
    is_muted: bool,
  },
  /// Takes the call over to this device: the SFU closes the account's other devices in the call
  /// with [`CallEvent::ConnectionReplaced`] (#505). Only the account's own devices are affected.
  MoveHere,
  /// Asks the SFU to answer with [`CallServerMessage::Pong`] of the same `id`. A page can't see
  /// QUIC keep-alives (and Safari's `getStats()` counts nothing), so answered pings are what prove
  /// a quiet session's path alive in the browser (#841). Older SFUs ignore it.
  Ping {
    id: u64,
  },
  /// First message of a connection that takes over a session of this device instead of joining
  /// (#1309): `secret` is the session's [`CallEvent::Joined`] `switch_secret`, which went out only
  /// over that session's own connection. The SFU answers [`CallServerMessage::Switched`] or an
  /// error, after which it closes the connection. An SFU from before reads it as anything but
  /// `join`: it answers `protocol_violation` ("expected join") and closes.
  Switch {
    secret: Base64Bytes,
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
  /// Answer to [`CallClientMessage::Ping`]. Not an event: it takes no sequence number.
  Pong {
    id: u64,
  },
  /// Answer to [`CallClientMessage::Switch`]: this connection carries the session from now on. Not
  /// an event either: the events still queued for the old connection go out there, up to its close
  /// with [`SFU_SWITCHED_CLOSE_CODE`], and the next one comes here with the next number.
  Switched,
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
    #[serde(default)]
    screen_audio_tracks: Vec<ScreenAudioTrack>,
    /// How long the SFU keeps this device's slot after its connection was lost; rejoining within
    /// it resumes the call. Older SFUs omit it and drop the slot at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maybe_reconnect_grace_ms: Option<u64>,
    /// The most simulcast layers the SFU takes per video track (#978). SFUs from before omit it
    /// and take [`crate::LEGACY_SIMULCAST_LAYERS`]; a client publishes no more than that then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    maybe_max_simulcast_layers: Option<u8>,
    /// [`SWITCH_SECRET_BYTES`] random bytes that let this session move to another connection
    /// without a rejoin ([`CallClientMessage::Switch`], #1309); a fresh one with every `joined`.
    /// SFUs from before send none, and clients from before ignore it.
    #[serde(rename = "switch_secret", default, skip_serializing_if = "Option::is_none")]
    maybe_switch_secret: Option<Base64Bytes>,
  },
  ParticipantJoined {
    participant: CallParticipant,
  },
  ParticipantLeft {
    device_id: DeviceId,
  },
  /// A device's connection was lost; its slot is kept while it reconnects. Media from and to it
  /// pauses; `participant_left` follows if it doesn't come back within the grace period.
  ParticipantReconnecting {
    device_id: DeviceId,
  },
  /// A reconnecting device is back on a new connection (same slot and audio stream).
  ParticipantReconnected {
    device_id: DeviceId,
  },
  /// Another device muted or unmuted its microphone. After `joined` the SFU sends one for each
  /// device that is muted; a client forgets what it knew when it rejoins.
  ParticipantMuted {
    device_id: DeviceId,
    is_muted: bool,
  },
  /// Devices whose audio the SFU currently forwards, loudest first.
  ActiveSpeakers {
    device_ids: Vec<DeviceId>,
  },
  /// Serialized MLS `ExternalSender` the client installs before creating its key package. Sent
  /// again when the call's DAVE group starts over because nobody committed its proposals (#1555):
  /// the client drops its DAVE session, keys included, and answers with a fresh key package.
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
  /// The selected commit for `epoch`; members move to `epoch + 1` (DAVE opcode 29).
  DaveAnnounceCommit {
    epoch: u64,
    commit: Base64Bytes,
    /// The transition that switches the encryption key to the new epoch; the client answers
    /// [`CallClientMessage::DaveTransitionReady`] and switches on
    /// [`CallEvent::DaveExecuteTransition`]. Without one (older SFUs, clients that don't support
    /// transitions, the group's first epoch) the client switches at once.
    #[serde(rename = "transition_id", default, skip_serializing_if = "Option::is_none")]
    maybe_transition_id: Option<DaveTransitionId>,
  },
  /// DAVE opcode 30.
  DaveWelcome {
    welcome: Base64Bytes,
    /// Call key bindings of the group's other members.
    #[serde(default)]
    key_bindings: Vec<DaveKeyBinding>,
    /// As in [`CallEvent::DaveAnnounceCommit`].
    #[serde(rename = "transition_id", default, skip_serializing_if = "Option::is_none")]
    maybe_transition_id: Option<DaveTransitionId>,
  },
  /// Every member that got the transition's commit or welcome is ready (or gone): switch the
  /// encryption key now (DAVE opcode 22). A client that never hears it switches on its own after
  /// a while. TeamCraft has no counterpart of opcode 21: calls never downgrade to unencrypted
  /// media.
  DaveExecuteTransition {
    transition_id: DaveTransitionId,
  },
  /// This device's key packages are refused: proposals adding it stalled the call's group twice,
  /// nobody committed them (#1555). It stays in the call without media keys; a key package sent
  /// within `retry_after_secs`, also on a new connection, is refused again. Older clients read
  /// `Unknown` and notice by their own timeout.
  DaveRefused {
    retry_after_secs: u32,
  },
  /// A video track was published (also sent to its publisher, which learns the stream id).
  VideoPublished {
    track: VideoTrack,
  },
  VideoUnpublished {
    stream_id: MediaStreamId,
  },
  /// A device started sending its shared screen's sound (also sent to the publisher, which learns
  /// the stream id). Forwarded to everyone, independent of the speaker selection.
  ScreenAudioPublished {
    track: ScreenAudioTrack,
  },
  ScreenAudioUnpublished {
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
  /// The video codecs every other device in the call that receives video decodes, best first
  /// (#565); every known codec while no other device does. Sent after `joined` and whenever it
  /// changes (a device joins, leaves or rejoins with other capabilities). A publisher picks its
  /// screen share's codec from these and publishes the share anew when its codec drops out of
  /// them. Older SFUs send none: only H.264 is safe then.
  DecodableVideoCodecs {
    codecs: Vec<VideoCodec>,
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
  /// The same device joined the call on another connection (another window, or this one came
  /// back after the SFU gave it up for lost), or another device of the account took the call over
  /// ([`CallClientMessage::MoveHere`]); the SFU closes this connection right after. Not to be
  /// answered with a rejoin.
  ConnectionReplaced,
  #[serde(other)]
  Unknown,
}

/// What a screen share keeps when the link or the machine can't carry it all (WebRTC's
/// `contentHint`): `Detail` keeps the resolution (text, slides, code) and lets the frame rate drop;
/// `Motion` keeps the frame rate (video, games, scrolling) at a lower resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenContentMode {
  #[default]
  Detail,
  Motion,
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

/// A device's screen-share sound: Opus like its microphone, on its own stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenAudioTrack {
  pub device_id: DeviceId,
  pub stream_id: MediaStreamId,
}

/// SFU-assigned id of one media stream, as in the datagram header (`teamcraft-media::datagram`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MediaStreamId(pub u32);

// Join tokens don't belong in logs.
impl fmt::Debug for CallClientMessage {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Join {
        capabilities,
        dave_transition_mode,
        ..
      } => f
        .debug_struct("Join")
        .field("token", &"[redacted]")
        .field("capabilities", capabilities)
        .field("dave_transition_mode", dave_transition_mode)
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
      Self::DaveTransitionReady { transition_id } => f
        .debug_struct("DaveTransitionReady")
        .field("transition_id", transition_id)
        .finish(),
      Self::PublishVideo {
        kind,
        codec,
        layers,
        maybe_publication_id,
      } => f
        .debug_struct("PublishVideo")
        .field("kind", kind)
        .field("codec", codec)
        .field("layers", layers)
        .field("maybe_publication_id", maybe_publication_id)
        .finish(),
      Self::TransportFeedback { feedback } => f
        .debug_struct("TransportFeedback")
        .field("feedback", feedback)
        .finish(),
      Self::UnpublishVideo { stream_id } => f
        .debug_struct("UnpublishVideo")
        .field("stream_id", stream_id)
        .finish(),
      Self::PublishScreenAudio => f.write_str("PublishScreenAudio"),
      Self::UnpublishScreenAudio { stream_id } => f
        .debug_struct("UnpublishScreenAudio")
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
      Self::RequestKeyframe { stream_id } => f
        .debug_struct("RequestKeyframe")
        .field("stream_id", stream_id)
        .finish(),
      Self::UplinkLayers { stream_id, layers } => f
        .debug_struct("UplinkLayers")
        .field("stream_id", stream_id)
        .field("layers", layers)
        .finish(),
      Self::MoveHere => f.write_str("MoveHere"),
      Self::Ping { id } => f.debug_struct("Ping").field("id", id).finish(),
      Self::Switch { .. } => f.debug_struct("Switch").field("secret", &"[redacted]").finish(),
      Self::SetMuted { is_muted } => f.debug_struct("SetMuted").field("is_muted", is_muted).finish(),
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
        dave_transition_mode: DaveTransitionMode::AtOnce,
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

  /// DAVE transitions (#1541): the id is a number, absent without a transition, and older peers'
  /// messages still read.
  #[test]
  fn dave_refused_wire_format() {
    let refused = CallEvent::DaveRefused {
      retry_after_secs: 300,
    };
    let json = r#"{"type":"dave_refused","retry_after_secs":300}"#;
    assert_eq!(serde_json::to_string(&refused).unwrap(), json);
    assert_eq!(serde_json::from_str::<CallEvent>(json).unwrap(), refused);
  }

  #[test]
  fn dave_transition_wire_format() {
    let transition_id = DaveTransitionId(NonZeroU16::new(7).unwrap());
    let ready = CallClientMessage::DaveTransitionReady { transition_id };
    assert_eq!(
      serde_json::to_string(&ready).unwrap(),
      r#"{"type":"dave_transition_ready","transition_id":7}"#
    );
    let execute = CallEvent::DaveExecuteTransition { transition_id };
    assert_eq!(
      serde_json::to_string(&execute).unwrap(),
      r#"{"type":"dave_execute_transition","transition_id":7}"#
    );
    let announce = CallEvent::DaveAnnounceCommit {
      epoch: 2,
      commit: Base64Bytes::new(vec![1]),
      maybe_transition_id: Some(transition_id),
    };
    assert_eq!(
      serde_json::to_string(&announce).unwrap(),
      r#"{"type":"dave_announce_commit","epoch":2,"commit":"AQ==","transition_id":7}"#
    );
    // An older SFU's commit and welcome have no transition: apply them at once.
    let old: CallEvent =
      serde_json::from_str(r#"{"type":"dave_announce_commit","epoch":2,"commit":"AQ=="}"#).unwrap();
    assert!(matches!(
      old,
      CallEvent::DaveAnnounceCommit {
        maybe_transition_id: None,
        ..
      }
    ));
    let welcome = CallEvent::DaveWelcome {
      welcome: Base64Bytes::new(vec![1]),
      key_bindings: Vec::new(),
      maybe_transition_id: None,
    };
    assert_eq!(
      serde_json::to_string(&welcome).unwrap(),
      r#"{"type":"dave_welcome","welcome":"AQ==","key_bindings":[]}"#
    );
    // Id 0 means "at once" in DAVE; here that is an absent id, so 0 is malformed.
    assert!(
      serde_json::from_str::<CallEvent>(r#"{"type":"dave_execute_transition","transition_id":0}"#).is_err()
    );
    // Older clients don't say how they apply transitions: at once. A newer mode reads as unknown.
    let join: CallClientMessage = serde_json::from_str(r#"{"type":"join","token":"t"}"#).unwrap();
    assert!(matches!(
      join,
      CallClientMessage::Join {
        dave_transition_mode: DaveTransitionMode::AtOnce,
        ..
      }
    ));
    let join: CallClientMessage =
      serde_json::from_str(r#"{"type":"join","token":"t","dave_transition_mode":"batched"}"#).unwrap();
    assert!(matches!(
      join,
      CallClientMessage::Join {
        dave_transition_mode: DaveTransitionMode::Unknown,
        ..
      }
    ));
    // An older SFU or client skips the new messages.
    assert_eq!(
      serde_json::from_str::<CallClientMessage>(r#"{"type":"dave_transition_ready","transition_id":7}"#)
        .unwrap(),
      ready
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
  fn screen_audio_wire_format() {
    assert_eq!(
      serde_json::to_string(&CallClientMessage::PublishScreenAudio).unwrap(),
      r#"{"type":"publish_screen_audio"}"#
    );
    let event = CallEvent::ScreenAudioPublished {
      track: ScreenAudioTrack {
        device_id: DeviceId::from_snowflake(Snowflake::new(5).unwrap()),
        stream_id: MediaStreamId(11),
      },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"screen_audio_published","track":{"device_id":"5","stream_id":11}}"#
    );
    assert_eq!(serde_json::from_str::<CallEvent>(&json).unwrap(), event);
    // Older SFUs send no screen audio tracks with `joined`.
    let joined: CallEvent = serde_json::from_str(
      r#"{"type":"joined","call_id":"7","participant":{"user_id":"4","device_id":"5","audio_stream_id":9},"participants":[]}"#,
    )
    .unwrap();
    assert!(
      matches!(joined, CallEvent::Joined { screen_audio_tracks, .. } if screen_audio_tracks.is_empty())
    );
  }

  #[test]
  fn reconnect_wire_format() {
    let device_id = DeviceId::from_snowflake(Snowflake::new(5).unwrap());
    for (event, expected) in [
      (CallEvent::ConnectionReplaced, r#"{"type":"connection_replaced"}"#),
      (
        CallEvent::ParticipantReconnecting { device_id },
        r#"{"type":"participant_reconnecting","device_id":"5"}"#,
      ),
      (
        CallEvent::ParticipantReconnected { device_id },
        r#"{"type":"participant_reconnected","device_id":"5"}"#,
      ),
    ] {
      assert_eq!(serde_json::to_string(&event).unwrap(), expected);
      assert_eq!(serde_json::from_str::<CallEvent>(expected).unwrap(), event);
    }
    let joined = CallEvent::Joined {
      call_id: CallId::from_snowflake(Snowflake::new(7).unwrap()),
      participant: participant(),
      participants: Vec::new(),
      video_tracks: Vec::new(),
      screen_audio_tracks: Vec::new(),
      maybe_reconnect_grace_ms: Some(30_000),
      maybe_max_simulcast_layers: Some(4),
      maybe_switch_secret: None,
    };
    let json = serde_json::to_string(&joined).unwrap();
    assert!(json.ends_with(r#""maybe_reconnect_grace_ms":30000,"maybe_max_simulcast_layers":4}"#));
    assert_eq!(serde_json::from_str::<CallEvent>(&json).unwrap(), joined);
    // Older SFUs keep no slot and name no layer limit (#978: they take three).
    let old: CallEvent = serde_json::from_str(
      r#"{"type":"joined","call_id":"7","participant":{"user_id":"4","device_id":"5","audio_stream_id":9},"participants":[]}"#,
    )
    .unwrap();
    assert!(matches!(
      old,
      CallEvent::Joined {
        maybe_reconnect_grace_ms: None,
        maybe_max_simulcast_layers: None,
        ..
      }
    ));
    // Fields of newer SFUs are ignored, as clients from before #978 ignore the layer limit.
    let newer: CallEvent = serde_json::from_str(
      r#"{"type":"joined","call_id":"7","participant":{"user_id":"4","device_id":"5","audio_stream_id":9},"participants":[],"maybe_max_simulcast_layers":4,"hint":"x"}"#,
    )
    .unwrap();
    assert!(matches!(
      newer,
      CallEvent::Joined {
        maybe_max_simulcast_layers: Some(4),
        ..
      }
    ));
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
  fn mute_wire_format() {
    assert_eq!(
      serde_json::to_string(&CallClientMessage::SetMuted { is_muted: true }).unwrap(),
      r#"{"type":"set_muted","is_muted":true}"#
    );
    let event = CallEvent::ParticipantMuted {
      device_id: "7".parse().unwrap(),
      is_muted: false,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"participant_muted","device_id":"7","is_muted":false}"#
    );
    assert_eq!(serde_json::from_str::<CallEvent>(&json).unwrap(), event);
  }

  #[test]
  fn keyframe_request_wire_format() {
    let request = CallClientMessage::RequestKeyframe {
      stream_id: MediaStreamId(12),
    };
    let json = serde_json::to_string(&request).unwrap();
    assert_eq!(json, r#"{"type":"request_keyframe","stream_id":12}"#);
    assert_eq!(serde_json::from_str::<CallClientMessage>(&json).unwrap(), request);
  }

  /// Older clients read the event as `Unknown` and keep their codec (H.264).
  #[test]
  fn decodable_codecs_wire_format() {
    let event = CallEvent::DecodableVideoCodecs {
      codecs: vec![VideoCodec::Av1, VideoCodec::H264],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert_eq!(
      json,
      r#"{"type":"decodable_video_codecs","codecs":["av1","h264"]}"#
    );
    assert_eq!(serde_json::from_str::<CallEvent>(&json).unwrap(), event);
    let unknown: CallEvent = serde_json::from_str(r#"{"type":"some_later_event","codecs":[]}"#).unwrap();
    assert_eq!(unknown, CallEvent::Unknown);
  }

  #[test]
  fn move_here_wire_format() {
    assert_eq!(
      serde_json::to_string(&CallClientMessage::MoveHere).unwrap(),
      r#"{"type":"move_here"}"#
    );
  }

  #[test]
  fn ping_and_pong_wire_format() {
    let ping = CallClientMessage::Ping { id: 3 };
    let json = serde_json::to_string(&ping).unwrap();
    assert_eq!(json, r#"{"type":"ping","id":3}"#);
    assert_eq!(serde_json::from_str::<CallClientMessage>(&json).unwrap(), ping);
    let pong = CallServerMessage::Pong { id: 3 };
    let json = serde_json::to_string(&pong).unwrap();
    assert_eq!(json, r#"{"type":"pong","id":3}"#);
    assert_eq!(serde_json::from_str::<CallServerMessage>(&json).unwrap(), pong);
  }

  #[test]
  fn publish_video_wire_format_with_and_without_publication_id() {
    let publish = CallClientMessage::PublishVideo {
      kind: VideoTrackKind::Screen,
      codec: VideoCodec::H264,
      layers: Vec::new(),
      maybe_publication_id: Some(VideoPublicationId(4)),
    };
    let json = serde_json::to_string(&publish).unwrap();
    assert_eq!(
      json,
      r#"{"type":"publish_video","kind":"screen","codec":"h264","layers":[],"maybe_publication_id":4}"#
    );
    assert_eq!(serde_json::from_str::<CallClientMessage>(&json).unwrap(), publish);
    // Older clients send none: the SFU echoes none.
    let older = r#"{"type":"publish_video","kind":"screen","codec":"h264","layers":[]}"#;
    assert_eq!(
      serde_json::from_str::<CallClientMessage>(older).unwrap(),
      CallClientMessage::PublishVideo {
        kind: VideoTrackKind::Screen,
        codec: VideoCodec::H264,
        layers: Vec::new(),
        maybe_publication_id: None,
      }
    );
    // An SFU from before the field reads the request as before: unknown fields of a known message
    // are ignored, as a field newer than this one is here.
    let newer = r#"{"type":"publish_video","kind":"screen","codec":"h264","layers":[],"priority":1}"#;
    assert!(matches!(
      serde_json::from_str::<CallClientMessage>(newer).unwrap(),
      CallClientMessage::PublishVideo {
        maybe_publication_id: None,
        ..
      }
    ));
    // The answer echoes the id in its track.
    let answer: CallEvent = serde_json::from_str(
      r#"{"type":"video_published","track":{"stream_id":7,"device_id":"5","kind":"screen","codec":"h264","layers":[],"maybe_publication_id":4}}"#,
    )
    .unwrap();
    assert!(matches!(
      answer,
      CallEvent::VideoPublished { track } if track.maybe_publication_id == Some(VideoPublicationId(4))
    ));
  }

  #[test]
  fn uplink_layers_wire_format() {
    let message = CallClientMessage::UplinkLayers {
      stream_id: MediaStreamId(7),
      layers: vec![0, 1],
    };
    let json = serde_json::to_string(&message).unwrap();
    assert_eq!(json, r#"{"type":"uplink_layers","stream_id":7,"layers":[0,1]}"#);
    assert_eq!(serde_json::from_str::<CallClientMessage>(&json).unwrap(), message);
  }

  #[test]
  fn unknown_variants_are_tolerated() {
    let client: CallClientMessage = serde_json::from_str(r#"{"type":"raise_hand"}"#).unwrap();
    assert_eq!(client, CallClientMessage::Unknown);
    // With fields, as `uplink_layers` reaches an SFU from before it (#921): ignored as well.
    let client: CallClientMessage =
      serde_json::from_str(r#"{"type":"layers_paused","stream_id":7,"layers":[0]}"#).unwrap();
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

  /// A session moves to another connection (#1309): the secret comes with `joined`, the new
  /// connection names it first, and the SFU's answer is no event.
  #[test]
  fn switch_wire_format() {
    let joined: CallEvent = serde_json::from_str(
      r#"{"type":"joined","call_id":"7","participant":{"user_id":"4","device_id":"5","audio_stream_id":9},"participants":[],"switch_secret":"AQI="}"#,
    )
    .unwrap();
    assert!(matches!(
      joined,
      CallEvent::Joined { maybe_switch_secret: Some(secret), .. } if secret.as_slice() == [1, 2]
    ));
    let switch = CallClientMessage::Switch {
      secret: Base64Bytes::new(vec![1, 2]),
    };
    let json = serde_json::to_string(&switch).unwrap();
    assert_eq!(json, r#"{"type":"switch","secret":"AQI="}"#);
    assert_eq!(serde_json::from_str::<CallClientMessage>(&json).unwrap(), switch);
    assert!(!format!("{switch:?}").contains("AQI="), "the secret is redacted");
    let switched = serde_json::to_string(&CallServerMessage::Switched).unwrap();
    assert_eq!(switched, r#"{"type":"switched"}"#);
    assert_eq!(
      serde_json::from_str::<CallServerMessage>(&switched).unwrap(),
      CallServerMessage::Switched
    );
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
      maybe_conversation_id: Some(ConversationId::from_snowflake(Snowflake::new(9).unwrap())),
    };
    let json = serde_json::to_string(&claims).unwrap();
    assert_eq!(
      json,
      r#"{"iss":"https://chat.example.com","aud":"teamcraft-sfu","iat":100,"exp":160,"call_id":"7","user_id":"4","device_id":"5","conversation_id":"9"}"#
    );
    let older = json.replace(r#","conversation_id":"9""#, "");
    let parsed: CallJoinClaims = serde_json::from_str(&older).unwrap();
    assert_eq!(parsed.maybe_conversation_id, None, "tokens of older servers");
  }

  #[test]
  fn join_token_is_redacted_in_debug_output() {
    let join = CallClientMessage::Join {
      token: "secret-token".into(),
      capabilities: MediaCapabilities::default(),
      dave_transition_mode: DaveTransitionMode::Coordinated,
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
