//! Media negotiation types of calls: codecs, capabilities and published video tracks.
//!
//! Audio is always Opus and every participant has exactly one audio stream (assigned on join).
//! Video tracks (camera, screen) are published explicitly, each with up to
//! [`MAX_SIMULCAST_LAYERS`] simulcast layers; receivers subscribe per track up to a layer, and the
//! SFU forwards one layer per receiver (ARCHITECTURE.md §6.3).

use serde::{Deserialize, Serialize};

use crate::wire_enum::wire_enum;
use crate::{DeviceId, MediaStreamId};

/// Highest number of simulcast layers per video track (the datagram header has 4 bits).
pub const MAX_SIMULCAST_LAYERS: u8 = 3;

wire_enum! {
  /// Audio codecs. Opus is the only one, but the list keeps room for others.
  pub enum AudioCodec {
    Opus => "opus",
  }
}

wire_enum! {
  /// Video codecs in order of preference when several are common (best compression first).
  pub enum VideoCodec {
    Av1 => "av1",
    Vp9 => "vp9",
    H264 => "h264",
  }
}

wire_enum! {
  /// What a video track shows.
  pub enum VideoTrackKind {
    Camera => "camera",
    Screen => "screen",
  }
}

/// What a client can encode and decode; sent with `join`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCapabilities {
  pub audio_codecs: Vec<AudioCodec>,
  /// Codecs the client can encode (hardware or software).
  pub video_encoders: Vec<VideoCodec>,
  /// Codecs the client can decode.
  pub video_decoders: Vec<VideoCodec>,
  /// Simulcast layers the client can send per video track.
  pub max_simulcast_layers: u8,
}

/// One simulcast layer of a published video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoLayerInfo {
  /// 0 is the lowest resolution.
  pub layer: u8,
  pub width: u16,
  pub height: u16,
  pub max_framerate: u8,
  pub max_bitrate_bps: u32,
}

/// A published video track as announced to all participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoTrack {
  pub stream_id: MediaStreamId,
  pub device_id: DeviceId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  pub layers: Vec<VideoLayerInfo>,
}

/// Arrival times of datagrams by transport sequence number, for bandwidth estimation (transport-wide
/// feedback). Datagram `base_seq + i` (wrapping) arrived at `arrivals_us[i]` microseconds on the
/// receiver's clock, or was lost (`None`). Only differences between arrival times matter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportFeedback {
  pub base_seq: u16,
  pub arrivals_us: Vec<Option<u64>>,
}

impl Default for MediaCapabilities {
  /// Audio only: what every client supports.
  fn default() -> Self {
    Self {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: Vec::new(),
      video_decoders: Vec::new(),
      max_simulcast_layers: 0,
    }
  }
}

impl VideoCodec {
  /// Preference rank: lower is better. Unknown codecs come last.
  pub fn preference(&self) -> u8 {
    match self {
      Self::Av1 => 0,
      Self::Vp9 => 1,
      Self::H264 => 2,
      Self::Unknown(_) => u8::MAX,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn capabilities_wire_format_and_default() {
    let capabilities = MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::Av1, VideoCodec::H264],
      max_simulcast_layers: 3,
    };
    assert_eq!(
      serde_json::to_string(&capabilities).unwrap(),
      r#"{"audio_codecs":["opus"],"video_encoders":["h264"],"video_decoders":["av1","h264"],"max_simulcast_layers":3}"#
    );
    assert!(MediaCapabilities::default().video_decoders.is_empty());
  }

  #[test]
  fn unknown_codec_is_kept_and_ranked_last() {
    let codec: VideoCodec = serde_json::from_str(r#""vvc""#).unwrap();
    assert_eq!(codec, VideoCodec::Unknown("vvc".into()));
    assert!(codec.preference() > VideoCodec::H264.preference());
    assert!(VideoCodec::Av1.preference() < VideoCodec::Vp9.preference());
  }
}
