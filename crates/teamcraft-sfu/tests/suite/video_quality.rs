//! How sharp another device's camera comes (#967), through a real SFU: a quality the viewer
//! picks moves it to that layer, Auto follows the size of its tile again, and on a good link the
//! top layer arrives at about its bitrate (VideoToolbox sent a tenth of it before #967).
//!
//! The presenter sends a 720p camera with the desktop's layers (`video::camera_layers`: 720p,
//! 360p and 180p) through the encoders the desktop uses (`preferred_encoders`: hardware where the
//! build and the machine have it, OpenH264 otherwise). The viewer receives the encoded frames and
//! tells their layers apart, as its stats do.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{
  CAMERA_HD_BITRATE_BPS, TOP_LAYER, VideoResolution, camera_layers, camera_limit,
};
use teamcraft_media::video_capture::{CaptureFormat, TestPatternCapture, VideoCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, MediaStreamId, VideoCodec, VideoTrackKind};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::time::Instant;
use wtransport::Identity;

use super::busy_camera::BusyCamera;

const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
/// A small tile: Auto asks for the 180p layer.
const THUMBNAIL: VideoResolution = VideoResolution {
  width: 320,
  height: 180,
};
/// How long a step may take to reach its layer: the SFU switches on the layer's next keyframe,
/// and a CI runner's downlink estimate needs a few seconds to allow the top layer.
const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Frames in a row on a layer that make it the one received.
const SETTLED_FRAMES: usize = 15;
/// How long the top layer's bitrate is measured once it arrives.
const MEASURE: Duration = Duration::from_secs(3);

/// #967: the viewer picks 720p, then 360p, then Auto again; each time the frames that arrive
/// come from the layer the pick (or the tile) calls for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_picked_quality_moves_the_viewer_to_its_layer_and_auto_follows_the_tile() {
  let (port, certificate_hash) = start_sfu().await;
  let presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;
  presenter
    .start_video(camera_config(test_pattern()))
    .await
    .unwrap();
  let stream = receive_camera(&mut viewer, THUMBNAIL).await;

  let mut steps = Vec::new();
  steps.push(("Auto, a thumbnail", wait_for_layer(&mut viewer, stream, 0).await));
  viewer.set_video_quality(stream, Some(720));
  steps.push(("720p picked", wait_for_layer(&mut viewer, stream, 2).await));
  viewer.set_video_quality(stream, Some(360));
  steps.push(("360p picked", wait_for_layer(&mut viewer, stream, 1).await));
  viewer.set_video_quality(stream, None);
  steps.push(("Auto again", wait_for_layer(&mut viewer, stream, 0).await));
  // A larger tile under Auto: the top layer.
  viewer.set_video_shown(stream, HD);
  steps.push(("Auto, a large tile", wait_for_layer(&mut viewer, stream, 2).await));

  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  for (step, outcome) in &steps {
    assert!(outcome.is_ok(), "{step}: {outcome:?} (all steps: {steps:?})");
  }
}

/// #967: on a link that carries it, the top layer the viewer picked arrives at about its
/// bitrate. VideoToolbox at an average bitrate sent 155 to 306 kbit/s of a 720p layer's
/// 1.5 Mbit/s in calls; OpenH264 spends it on the busy camera's motion and noise.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_good_link_carries_the_top_layer_at_about_its_bitrate() {
  let (port, certificate_hash) = start_sfu().await;
  let presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;
  let camera = BusyCamera::new(std::time::Instant::now());
  presenter
    .start_video(camera_config(Box::new(camera)))
    .await
    .unwrap();
  let stream = receive_camera(&mut viewer, HD).await;
  viewer.set_video_quality(stream, Some(720));
  let reached = wait_for_layer(&mut viewer, stream, 2).await;

  let mut bytes = 0usize;
  let mut frames = 0usize;
  let started = Instant::now();
  while reached.is_ok() && started.elapsed() < MEASURE {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoFrame(frame) = event
      && frame.stream_id == stream
      && frame.layer.value() == 2
    {
      bytes += frame.data.len();
      frames += 1;
    }
  }
  let seconds = started.elapsed().as_secs_f64();
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  reached.unwrap();
  // DAVE adds a few bytes per frame; the encoders' own rate control may stay a bit below.
  let bitrate_bps = bytes as f64 * 8.0 / seconds;
  let share = bitrate_bps / f64::from(CAMERA_HD_BITRATE_BPS);
  assert!(
    (0.6..1.6).contains(&share),
    "the top layer came at {:.0} kbit/s ({frames} frames in {seconds:.1} s) of {} kbit/s",
    bitrate_bps / 1000.0,
    CAMERA_HD_BITRATE_BPS / 1000
  );
}

/// The presenter's camera track once the viewer knows it, received (encoded frames as events)
/// with its tile `shown` this large.
async fn receive_camera(viewer: &mut NativeCall, shown: VideoResolution) -> MediaStreamId {
  let deadline = Instant::now() + STEP_TIMEOUT;
  while Instant::now() < deadline {
    let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await else {
      continue;
    };
    if let CallEngineEvent::VideoPublished(track) = event
      && track.kind == VideoTrackKind::Camera
    {
      assert_eq!(track.layers, camera_layers(HD, 30, camera_limit(None)));
      viewer.set_video_shown(track.stream_id, shown);
      viewer.subscribe_video(track.stream_id, Some(TOP_LAYER));
      return track.stream_id;
    }
  }
  panic!("the presenter's camera never showed up");
}

/// Waits until [`SETTLED_FRAMES`] frames in a row of `stream` came from `layer`; the layers that
/// came meanwhile otherwise.
async fn wait_for_layer(viewer: &mut NativeCall, stream: MediaStreamId, layer: u8) -> Result<(), Vec<u8>> {
  let deadline = Instant::now() + STEP_TIMEOUT;
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

/// A moving test pattern, 720p at 30 fps.
fn test_pattern() -> Box<dyn VideoCapture> {
  Box::new(TestPatternCapture::new(CaptureFormat {
    resolution: HD,
    framerate: 30,
  }))
}

/// The desktop's camera of a 720p camera: `capture` in its three layers, through the encoders the
/// desktop uses.
fn camera_config(capture: Box<dyn VideoCapture>) -> VideoSendConfig {
  VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture,
    layers: camera_layers(HD, 30, camera_limit(None)),
    maybe_layer_plan: None,
    encoders: preferred_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  }
}

async fn join(port: u16, certificate_hash: [u8; 32], device: i64, other: i64) -> NativeCall {
  let engine = CallEngineConfig {
    user_id: id(device),
    device_id: id(device),
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264],
      max_simulcast_layers: 3,
      max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
    },
    bwe: BweConfig::default(),
  };
  let sfu_url = format!(
    "https://{}{CALL_PATH}",
    SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
  );
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
