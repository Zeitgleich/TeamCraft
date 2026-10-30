//! A viewer whose downlink can't carry 360p still sees a 1080p camera, at 180p (#978), through a
//! real SFU. Since #967 a 1080p camera's layers were 1080p, 720p and 360p: such a viewer got no
//! picture but the SFU's resume probes of the 360p layer, about 15 frames every 5 s (30 frames in
//! 8 s here with the three layers; with four, 150 to 190 frames, most of them 180p).
//!
//! Until #994 the SFU probed the 360p layer now and then (the downlink estimate grows while the
//! link isn't full, and the next layer was the probe); the link dropped it, and the picture stood
//! still for 2.6 to 5.2 s until the viewer was back on 180p. Now padding bursts probe the link
//! before a switch up, and the viewer keeps 180p without a stall: every frame 180p, the longest
//! gap under 200 ms on a laptop (eight copies in parallel too).
//!
//! The presenter sends a busy camera at 1080p (the suite's busy camera scaled up, so OpenH264
//! spends each layer's whole bitrate: 360p needs its 375 kbit/s) in the layers the desktop plans
//! for it (`video::camera_layers`), within what the SFU says it takes. The viewer asks for the top
//! layer, as a large tile does, through a relay that limits its downlink below 360p
//! ([`LIMIT_BPS`]); it receives the encoded frames and tells their layers apart, as its stats do.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{TOP_LAYER, VideoResolution, camera_layers, camera_limit};
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{
  AudioCodec, DeviceId, MAX_SIMULCAST_LAYERS, MediaCapabilities, MediaStreamId, VideoCodec, VideoLayerInfo,
  VideoTrackKind,
};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::time::Instant;
use wtransport::Identity;

use crate::common::busy_camera::{BusyCamera, FRAMERATE};
use crate::common::relay::Relay;

const FULL_HD: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
/// The viewer's downlink: below what 360p takes (375 kbit/s, and the SFU keeps 120 kbit/s of a
/// downlink for audio), above what 180p takes (94 kbit/s).
const LIMIT_BPS: u64 = 250_000;
/// How long the 180p layer may take to arrive: securing the call, the first keyframe, and a
/// slow CI runner encoding a busy 1080p picture in a debug build.
const ARRIVAL_TIMEOUT: Duration = Duration::from_secs(30);
/// Frames in a row on a layer that make it the one received.
const SETTLED_FRAMES: usize = 15;
/// How long the viewer's picture is watched once 180p arrives.
const MEASURE: Duration = Duration::from_secs(12);
/// The fewest 180p frames the viewer gets meanwhile: 2 a second, for a loaded CI runner that
/// encodes far fewer than the camera's 30 (a laptop: about 20 a second).
const MIN_FRAMES_AT_180P: usize = 24;
/// The longest the viewer's picture may stand still meanwhile: a probe of the next layer stalled
/// it for 2.6 to 5.2 s, a pause until the resume probe for 2 s at least; room above the under
/// 200 ms of a laptop for a CI runner that encodes the busy 1080p picture slowly.
const MAX_GAP: Duration = Duration::from_millis(1500);

/// #978: the viewer limited below 360p gets the 1080p camera's 180p layer, and keeps getting it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_limited_below_360p_gets_the_180p_layer() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let relay = Relay::start(sfu, std::time::Instant::now()).await;
  relay.to_client.set_limit(Some(LIMIT_BPS));
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let mut viewer = join(relay.address, certificate_hash, 2, 1).await;
  presenter.start_video(camera_config()).await.unwrap();
  let (stream, layers) = receive_camera(&mut viewer).await;
  let arrived = wait_for_layer(&mut viewer, stream, 0).await;
  let watched = if arrived.is_ok() {
    watch(&mut viewer, stream).await
  } else {
    Watched::default()
  };

  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  // Four layers: the SFU took them, and the lowest is 180p.
  let sides: Vec<(u8, u16, u16)> = layers
    .iter()
    .map(|info| (info.layer, info.width, info.height))
    .collect();
  assert_eq!(
    sides,
    vec![(0, 320, 180), (1, 640, 360), (2, 1280, 720), (3, 1920, 1080)]
  );
  arrived.unwrap_or_else(|seen| panic!("180p never arrived in a row; layers that came: {seen:?}"));
  let at_180p = watched.frames_of(0);
  let total = watched.frames.len();
  println!(
    "{total} frames in {MEASURE:?}, {at_180p} of them 180p; longest gap {:?}",
    watched.longest_gap
  );
  assert!(
    at_180p >= MIN_FRAMES_AT_180P,
    "only {at_180p} frames at 180p in {MEASURE:?}: {watched:?}"
  );
  // Nothing above 180p gets through this link: a frame of a higher layer is one of a switch up
  // the link can't carry.
  assert!(
    at_180p * 10 >= total * 9,
    "only {at_180p} of {total} frames at 180p in {MEASURE:?}: {watched:?}"
  );
  assert!(
    watched.longest_gap < MAX_GAP,
    "the picture stood still for {:?} in {MEASURE:?}: {watched:?}",
    watched.longest_gap
  );
}

/// What the viewer received while it was watched.
#[derive(Debug, Default)]
struct Watched {
  /// The layer of each frame, in order.
  frames: Vec<u8>,
  longest_gap: Duration,
}

impl Watched {
  fn frames_of(&self, layer: u8) -> usize {
    self.frames.iter().filter(|seen| **seen == layer).count()
  }
}

/// The frames of `stream` the viewer receives for [`MEASURE`].
async fn watch(viewer: &mut NativeCall, stream: MediaStreamId) -> Watched {
  let started = Instant::now();
  let mut last = started;
  let mut watched = Watched::default();
  while started.elapsed() < MEASURE {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoFrame(frame) = event
      && frame.stream_id == stream
    {
      let now = Instant::now();
      watched.longest_gap = watched.longest_gap.max(now - last);
      last = now;
      watched.frames.push(frame.layer.value());
    }
  }
  watched.longest_gap = watched.longest_gap.max(Instant::now() - last);
  watched
}

/// The presenter's camera track once the viewer knows it, received (encoded frames as events)
/// up to its top layer, with its layers.
async fn receive_camera(viewer: &mut NativeCall) -> (MediaStreamId, Vec<VideoLayerInfo>) {
  let deadline = Instant::now() + ARRIVAL_TIMEOUT;
  while Instant::now() < deadline {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoPublished(track) = event
      && track.kind == VideoTrackKind::Camera
    {
      // A large tile: Auto asks for the top layer, the SFU caps it by the downlink.
      viewer.set_video_shown(track.stream_id, FULL_HD);
      viewer.subscribe_video(track.stream_id, Some(TOP_LAYER));
      return (track.stream_id, track.layers);
    }
  }
  panic!("the presenter's camera never showed up");
}

/// Waits until [`SETTLED_FRAMES`] frames in a row of `stream` came from `layer`; the layers that
/// came meanwhile otherwise.
async fn wait_for_layer(viewer: &mut NativeCall, stream: MediaStreamId, layer: u8) -> Result<(), Vec<u8>> {
  let deadline = Instant::now() + ARRIVAL_TIMEOUT;
  let mut in_a_row = 0;
  let mut seen: Vec<u8> = Vec::new();
  while Instant::now() < deadline {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    let CallEngineEvent::VideoFrame(frame) = event else {
      continue;
    };
    if frame.stream_id != stream {
      continue;
    }
    let arrived = frame.layer.value();
    if seen.last() != Some(&arrived) {
      seen.push(arrived);
    }
    in_a_row = if arrived == layer { in_a_row + 1 } else { 0 };
    if in_a_row >= SETTLED_FRAMES {
      return Ok(());
    }
  }
  Err(seen)
}

/// The desktop's camera of a 1080p camera: its layers planned for what it delivers, through
/// OpenH264 (VideoToolbox keeps the busy picture far below its bitrate, so a limit wouldn't
/// limit it).
fn camera_config() -> VideoSendConfig {
  let limit = camera_limit(None);
  VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture: Box::new(BusyCamera::at(std::time::Instant::now(), FULL_HD)),
    layers: camera_layers(FULL_HD, FRAMERATE, limit),
    maybe_layer_plan: Some(Box::new(move |source| camera_layers(source, FRAMERATE, limit))),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  }
}

async fn join(sfu: SocketAddr, certificate_hash: [u8; 32], device: i64, other: i64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id(device),
    device_id: id(device),
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264],
      max_simulcast_layers: MAX_SIMULCAST_LAYERS,
      max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
    },
    bwe: BweConfig::default(),
  };
  let sfu_url = format!("https://{sfu}{CALL_PATH}");
  let mut config = NativeCallConfig::new(sfu_url, format!("dev:14:{device}:{device}"), engine);
  config.maybe_certificate_hash = Some(certificate_hash);
  config.maybe_trusted_roster = Some(vec![id::<DeviceId>(other)]);
  NativeCall::join(config).await.unwrap()
}

async fn start_sfu() -> (u16, [u8; 32]) {
  let config = SfuConfig {
    bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
    ..SfuConfig::default()
  };
  let identity = Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
  let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
  let port = sfu.local_addr().unwrap().port();
  let certificate_hash = *sfu.certificate_hash().as_ref();
  tokio::spawn(sfu.run());
  (port, certificate_hash)
}

fn id<T: std::str::FromStr>(raw: i64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
