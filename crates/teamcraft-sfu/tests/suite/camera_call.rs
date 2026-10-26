//! A camera through the SFU: the presenter captures a test pattern, encodes three simulcast layers
//! (H.264) and publishes them; the viewer watches the track and decodes frames.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{VideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CaptureFormat, TestPatternCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, VideoCodec, VideoTrackKind};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use wtransport::Identity;

const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn camera_simulcast_reaches_a_viewer_and_decodes() {
  camera_reaches_the_viewer(Start::WhenSecured).await;
}

/// The desktop turns the camera on right after joining, before the call is end-to-end encrypted:
/// the pictures until then are dropped, the first keyframe among them, and the viewer still gets
/// a picture at once (issue #683), not after the next periodic keyframe (300 frames).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_started_before_the_call_is_secured_reaches_the_viewer() {
  camera_reaches_the_viewer(Start::AtOnce).await;
}

/// When the presenter starts its camera.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Start {
  WhenSecured,
  AtOnce,
}

async fn camera_reaches_the_viewer(start: Start) {
  let (port, certificate_hash) = start_sfu().await;
  let mut presenter = join(port, certificate_hash, 1, 2).await;
  let mut viewer = join(port, certificate_hash, 2, 1).await;

  let decoded: Arc<Mutex<Vec<VideoFrame>>> = Arc::default();
  let mut is_presenter_secured = false;
  let mut has_started = false;
  let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
  while tokio::time::Instant::now() < deadline && decoded.lock().unwrap().len() < 20 {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(10), presenter.next_event()).await
      && matches!(event, CallEngineEvent::Secured { .. })
    {
      is_presenter_secured = true;
    }
    if (is_presenter_secured || start == Start::AtOnce) && !has_started {
      let config = VideoSendConfig {
        kind: VideoTrackKind::Camera,
        capture: Box::new(TestPatternCapture::new(CaptureFormat {
          resolution: HD,
          framerate: 30,
        })),
        layers: simulcast_layers(HD, 30, 1_500_000, 3),
        encoders: software_encoders(),
        maybe_filter: None,
        maybe_idle_refresh: None,
      };
      presenter.start_video(config).await.unwrap();
      has_started = true;
    }
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(10), viewer.next_event()).await
    {
      assert_eq!(
        (track.kind.clone(), track.codec.clone()),
        (VideoTrackKind::Camera, VideoCodec::H264)
      );
      assert_eq!(track.layers.len(), 3);
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| sink.lock().unwrap().push(frame)),
      );
    }
  }
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  let decoded = decoded.lock().unwrap();
  assert!(decoded.len() >= 20, "{} frames decoded", decoded.len());
  // The SFU starts a receiver on a layer its (initial) estimate allows; any of the three is fine.
  let resolution = decoded.last().unwrap().resolution();
  assert!([320, 640, 1280].contains(&resolution.width), "{resolution:?}");
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
