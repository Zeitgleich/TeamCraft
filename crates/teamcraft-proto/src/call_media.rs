//! Media negotiation types of calls: codecs, capabilities and published video tracks.
//!
//! Audio is always Opus and every participant has exactly one audio stream (assigned on join).
//! Video tracks (camera, screen) are published explicitly, each with up to
//! [`MAX_SIMULCAST_LAYERS`] simulcast layers; receivers subscribe per track up to a layer, and the
//! SFU forwards one layer per receiver (ARCHITECTURE.md §6.3).

use serde::{Deserialize, Serialize};

use crate::wire_enum::wire_enum;
use crate::{DeviceId, MediaStreamId};

/// Highest number of simulcast layers per video track (the datagram header has 4 bits). Four since
/// #978: a 1080p camera sends 1080p, 720p, 360p and 180p. An SFU says how many it takes in
/// [`crate::CallEvent::Joined`].
pub const MAX_SIMULCAST_LAYERS: u8 = 4;
/// The simulcast layers per video track SFUs from before #978 take, and say nothing about: a
/// client publishes at most this many where `joined` names no limit.
pub const LEGACY_SIMULCAST_LAYERS: u8 = 3;
/// The most fragments (RTP packets) one video frame may have: about 4.5 MB in 1100 byte
/// fragments, far above a keyframe of the largest picture sent (1440p), and a bound on what a
/// receiver holds for one frame.
pub const MAX_VIDEO_FRAME_FRAGMENTS: u16 = 4096;
/// What clients from before the fragment extension's long form reassemble (issue #941): 255
/// fragments, about 280 KB, less than a keyframe of a text-heavy screen share at 1440p.
pub const SHORT_VIDEO_FRAME_FRAGMENTS: u16 = 255;

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

/// Every video codec this version knows, best first: what a publisher may pick from when no other
/// device receives video ([`crate::CallEvent::DecodableVideoCodecs`]).
pub const KNOWN_VIDEO_CODECS: [VideoCodec; 3] = [VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264];

wire_enum! {
  /// The largest picture a screen share is sent at (#565). `Standard` is what every share had
  /// before: up to 1440p at 30 fps for detail, 1080p at 60 fps for motion, at most 8 Mbit/s. `Uhd`
  /// raises it to 4K at 30 fps (1440p at 60 fps for motion) and 16 Mbit/s: a choice per share, and
  /// only where the workspace allows it (`ScreenSharePolicy`).
  #[derive(Default)]
  pub enum ScreenShareResolution {
    #[default]
    Standard => "standard",
    Uhd => "uhd",
  }
}

wire_enum! {
  /// What a video track shows.
  pub enum VideoTrackKind {
    Camera => "camera",
    Screen => "screen",
  }
}

wire_enum! {
  /// What a client's call engine understands beyond the codecs it lists (#1733).
  pub enum VideoFeature {
    /// It takes the codec of each simulcast layer from [`VideoLayerInfo::maybe_codec`] and opens
    /// a decoder for another codec at a layer switch: the SFU may forward it layers of a track
    /// in other codecs than [`VideoTrack::codec`]. Clients without it get only the layers in the
    /// track's codec.
    LayerCodecs => "layer_codecs",
  }
}

wire_enum! {
  /// What a client's audio playout understands beyond the codecs it lists (#1893).
  pub enum AudioFeature {
    /// It plays Opus packets of any duration RFC 6716 allows (40 and 60 ms on a thin uplink), not
    /// only 20 ms ones: a sender may lengthen its voice's frames only while every other
    /// participant has it ([`crate::CallEvent::AudioFeatures`]).
    LongFrames => "long_frames",
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
  /// Simulcast layers the client can send per video track. Informational: the SFU takes up to
  /// [`MAX_SIMULCAST_LAYERS`] from anyone.
  pub max_simulcast_layers: u8,
  /// The most fragments of one video frame the client reassembles; the SFU forwards it no layer
  /// with larger frames. Clients that don't send it take [`SHORT_VIDEO_FRAME_FRAGMENTS`].
  #[serde(default = "short_video_frame_fragments")]
  pub max_frame_fragments: u16,
  /// What its call engine understands beyond the codecs (#1733). Clients from before send none.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub video_features: Vec<VideoFeature>,
  /// What its audio playout understands beyond the codecs (#1893). Clients from before send none.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub audio_features: Vec<AudioFeature>,
}

/// One simulcast layer of a published video track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoLayerInfo {
  /// 0 is the lowest resolution.
  pub layer: u8,
  pub width: u16,
  pub height: u16,
  pub max_framerate: u8,
  pub max_bitrate_bps: u32,
  /// The layer's codec where it isn't the track's (#1733): a screen share's large layers in AV1
  /// above an H.264 layer every device decodes. `None`: [`VideoTrack::codec`]. The SFU forwards
  /// such a layer only to receivers that decode its codec and know
  /// [`VideoFeature::LayerCodecs`].
  #[serde(rename = "codec", default, skip_serializing_if = "Option::is_none")]
  pub maybe_codec: Option<VideoCodec>,
}

/// A published video track as announced to all participants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoTrack {
  pub stream_id: MediaStreamId,
  pub device_id: DeviceId,
  pub kind: VideoTrackKind,
  pub codec: VideoCodec,
  pub layers: Vec<VideoLayerInfo>,
  /// The publisher's id of the request this track answers (`publish_video`), so it knows which of
  /// its requests the SFU answered (#929). Older clients send none, older SFUs echo none.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_publication_id: Option<VideoPublicationId>,
}

/// A client's own number for one of its video publications, unique per device within a call. The
/// SFU only echoes it in the track it announces ([`VideoTrack::maybe_publication_id`]): an answer
/// then belongs to exactly the request that asked for it, also when the client stopped the track
/// before the answer came, or asked again for the same kind meanwhile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VideoPublicationId(pub u32);

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
      max_frame_fragments: MAX_VIDEO_FRAME_FRAGMENTS,
      video_features: Vec::new(),
      audio_features: Vec::new(),
    }
  }
}

impl MediaCapabilities {
  /// Whether its playout takes voice packets longer than 20 ms.
  pub fn has_long_audio_frames(&self) -> bool {
    self.audio_features.contains(&AudioFeature::LongFrames)
  }

  /// Whether its engine takes layers in other codecs than their track's.
  pub fn has_layer_codecs(&self) -> bool {
    self.video_features.contains(&VideoFeature::LayerCodecs)
  }
}

impl VideoLayerInfo {
  /// The codec this layer is encoded in, as a layer of `track_codec`'s track.
  pub fn codec<'a>(&'a self, track_codec: &'a VideoCodec) -> &'a VideoCodec {
    self.maybe_codec.as_ref().unwrap_or(track_codec)
  }
}

impl VideoTrack {
  /// The codec of `layer`; the track's for a layer it doesn't have.
  pub fn layer_codec(&self, layer: u8) -> &VideoCodec {
    self
      .layers
      .iter()
      .find(|info| info.layer == layer)
      .map_or(&self.codec, |info| info.codec(&self.codec))
  }

  /// Whether some layer is in another codec than the track.
  pub fn has_mixed_codecs(&self) -> bool {
    self
      .layers
      .iter()
      .any(|info| info.codec(&self.codec) != &self.codec)
  }
}

fn short_video_frame_fragments() -> u16 {
  SHORT_VIDEO_FRAME_FRAGMENTS
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

impl ScreenShareResolution {
  /// What a share asked for at `self` is sent at under a workspace's `limit`: the smaller of the
  /// two. A value this version doesn't know counts as [`ScreenShareResolution::Standard`] on
  /// either side.
  pub fn within(&self, limit: &ScreenShareResolution) -> ScreenShareResolution {
    match (self, limit) {
      (Self::Uhd, Self::Uhd) => Self::Uhd,
      _ => Self::Standard,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn screen_share_resolution_stays_within_the_limit() {
    use ScreenShareResolution::{Standard, Uhd};
    assert_eq!(Uhd.within(&Uhd), Uhd);
    assert_eq!(Uhd.within(&Standard), Standard);
    assert_eq!(Standard.within(&Uhd), Standard);
    let newer = ScreenShareResolution::Unknown("8k".into());
    assert_eq!(newer.within(&Uhd), Standard);
    assert_eq!(Uhd.within(&newer), Standard);
    assert_eq!(serde_json::to_string(&Uhd).unwrap(), r#""uhd""#);
    assert_eq!(ScreenShareResolution::default(), Standard);
  }

  #[test]
  fn capabilities_wire_format_and_default() {
    let capabilities = MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::Av1, VideoCodec::H264],
      max_simulcast_layers: 3,
      max_frame_fragments: MAX_VIDEO_FRAME_FRAGMENTS,
      video_features: Vec::new(),
      audio_features: Vec::new(),
    };
    assert_eq!(
      serde_json::to_string(&capabilities).unwrap(),
      r#"{"audio_codecs":["opus"],"video_encoders":["h264"],"video_decoders":["av1","h264"],"max_simulcast_layers":3,"max_frame_fragments":4096}"#
    );
    assert!(MediaCapabilities::default().video_decoders.is_empty());
    assert_eq!(
      MediaCapabilities::default().max_frame_fragments,
      MAX_VIDEO_FRAME_FRAGMENTS
    );
  }

  /// Clients from before #941 send no `max_frame_fragments`: they take frames of up to 255.
  #[test]
  fn capabilities_without_frame_fragments_take_the_short_form() {
    let older: MediaCapabilities = serde_json::from_str(
      r#"{"audio_codecs":["opus"],"video_encoders":[],"video_decoders":["h264"],"max_simulcast_layers":3}"#,
    )
    .unwrap();
    assert_eq!(older.max_frame_fragments, SHORT_VIDEO_FRAME_FRAGMENTS);
  }

  #[test]
  fn video_track_wire_format_with_and_without_publication_id() {
    let mut track = VideoTrack {
      stream_id: MediaStreamId(7),
      device_id: "5".parse().unwrap(),
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      layers: Vec::new(),
      maybe_publication_id: Some(VideoPublicationId(3)),
    };
    let json = serde_json::to_string(&track).unwrap();
    assert_eq!(
      json,
      r#"{"stream_id":7,"device_id":"5","kind":"camera","codec":"h264","layers":[],"maybe_publication_id":3}"#
    );
    assert_eq!(serde_json::from_str::<VideoTrack>(&json).unwrap(), track);
    // Tracks of older clients, and from older SFUs, carry none; the field is left out then.
    track.maybe_publication_id = None;
    let older = r#"{"stream_id":7,"device_id":"5","kind":"camera","codec":"h264","layers":[]}"#;
    assert_eq!(serde_json::to_string(&track).unwrap(), older);
    assert_eq!(serde_json::from_str::<VideoTrack>(older).unwrap(), track);
    // Fields of newer peers are ignored, as older peers ignore this one.
    let newer = r#"{"stream_id":7,"device_id":"5","kind":"camera","codec":"h264","layers":[],"hint":"x"}"#;
    assert_eq!(serde_json::from_str::<VideoTrack>(newer).unwrap(), track);
  }

  /// #1733: a layer in another codec than its track names it; layers in the track's codec, and
  /// those of older clients, carry none. Features of newer clients are kept as unknown.
  #[test]
  fn layer_codecs_and_video_features_wire_format() {
    let base = r#"{"layer":0,"width":854,"height":480,"max_framerate":15,"max_bitrate_bps":600000}"#;
    let top =
      r#"{"layer":1,"width":2560,"height":1440,"max_framerate":30,"max_bitrate_bps":4000000,"codec":"av1"}"#;
    let track: VideoTrack = serde_json::from_str(&format!(
      r#"{{"stream_id":7,"device_id":"5","kind":"screen","codec":"h264","layers":[{base},{top}]}}"#
    ))
    .unwrap();
    assert_eq!(track.layers[0].maybe_codec, None);
    assert_eq!(track.layer_codec(0), &VideoCodec::H264);
    assert_eq!(track.layer_codec(1), &VideoCodec::Av1);
    assert_eq!(track.layer_codec(5), &VideoCodec::H264);
    assert!(track.has_mixed_codecs());
    assert_eq!(serde_json::to_string(&track.layers[1]).unwrap(), top);
    assert_eq!(serde_json::to_string(&track.layers[0]).unwrap(), base);

    let capabilities: MediaCapabilities = serde_json::from_str(
      r#"{"audio_codecs":["opus"],"video_encoders":[],"video_decoders":["h264"],"max_simulcast_layers":3,"video_features":["layer_codecs","later"]}"#,
    )
    .unwrap();
    assert!(capabilities.has_layer_codecs());
    assert_eq!(
      capabilities.video_features,
      vec![VideoFeature::LayerCodecs, VideoFeature::Unknown("later".into())]
    );
    assert!(!MediaCapabilities::default().has_layer_codecs());
  }

  /// #1893: long voice frames are announced; older clients send no `audio_features`, and features
  /// of newer ones are kept as unknown.
  #[test]
  fn audio_features_wire_format() {
    let capabilities = MediaCapabilities {
      audio_features: vec![AudioFeature::LongFrames],
      ..MediaCapabilities::default()
    };
    let json = serde_json::to_string(&capabilities).unwrap();
    assert!(json.ends_with(r#","audio_features":["long_frames"]}"#), "{json}");
    assert!(
      serde_json::from_str::<MediaCapabilities>(&json)
        .unwrap()
        .has_long_audio_frames()
    );
    let older: MediaCapabilities = serde_json::from_str(
      r#"{"audio_codecs":["opus"],"video_encoders":[],"video_decoders":[],"max_simulcast_layers":0}"#,
    )
    .unwrap();
    assert!(!older.has_long_audio_frames());
    let newer: MediaCapabilities = serde_json::from_str(
      r#"{"audio_codecs":["opus"],"video_encoders":[],"video_decoders":[],"max_simulcast_layers":0,"audio_features":["later"]}"#,
    )
    .unwrap();
    assert_eq!(newer.audio_features, vec![AudioFeature::Unknown("later".into())]);
    assert!(!newer.has_long_audio_frames());
  }

  #[test]
  fn unknown_codec_is_kept_and_ranked_last() {
    let codec: VideoCodec = serde_json::from_str(r#""vvc""#).unwrap();
    assert_eq!(codec, VideoCodec::Unknown("vvc".into()));
    assert!(codec.preference() > VideoCodec::H264.preference());
    assert!(VideoCodec::Av1.preference() < VideoCodec::Vp9.preference());
  }
}
