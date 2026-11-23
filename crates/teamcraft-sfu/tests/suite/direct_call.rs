//! Direct media for 1:1 calls (#127) end to end with the real native client: two headless clients
//! with device-bound keys join through this SFU, exchange sealed offers, connect directly with
//! pinned certificates and move their audio there, while the SFU stops forwarding it. A peer the
//! policy places outside the organization never goes direct, and turning the setting off mid-call
//! sends the audio back through the SFU. A camera goes over the direct path as well, on the layer
//! the viewer takes and the direct connection carries, while the SFU forwards none of it.
//!
//! The clients offer the addresses of this machine's interfaces (loopback never counts), so the
//! tests need a network interface with a route; without one they say so and pass.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
  collections::HashMap,
  net::{Ipv6Addr, SocketAddr, UdpSocket},
  sync::{Arc, Mutex},
  time::Duration,
};

use rand_core::OsRng;
use teamcraft_client_api::video_stats::MediaPath;
use teamcraft_crypto::{
  identity::{DeviceKeyPair, IdentityKeyPair},
  ids::{DeviceId as CryptoDeviceId, UserId as CryptoUserId},
};
use teamcraft_media::{
  audio_io::{AudioSink, AudioSource},
  bwe::BweConfig,
  call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator},
  call_engine::{CallEngineConfig, CallEngineEvent},
  call_native::{DirectPathConfig, NativeCall, NativeCallConfig},
  video::{VideoResolution, simulcast_layers},
  video_capture::{CaptureFormat, TestPatternCapture},
  video_pipeline::{VideoSendConfig, software_encoders},
};
use teamcraft_proto::{
  AudioCodec, CallPeerStanding, DeviceId, DirectMediaPolicy, MediaCapabilities, VideoCodec, VideoTrackKind,
};
use teamcraft_sfu::{
  auth::DevTokenVerifier,
  config::SfuConfig,
  server::{CALL_PATH, Sfu},
};
use wtransport::Identity;

const CALL_ID: u64 = 127;
const DEVICES: [u64; 2] = [1, 2];
/// Offers, dialing and a second of answered pings take far less; a slow runner gets room.
const DIRECT_WITHIN: Duration = Duration::from_secs(20);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_members_move_their_audio_to_a_direct_path_and_still_hear_each_other() {
  if !has_routed_interface() {
    eprintln!("no network interface with a route: nothing to connect directly over");
    return;
  }
  let (port, certificate_hash) = start_sfu().await;
  let mut calls = join_both(port, certificate_hash, |_| CallPeerStanding::Member, audio_only()).await;

  let rtt = wait_for_path(&calls[0], true).await;
  wait_for_path(&calls[1], true).await;
  assert!(rtt.is_some(), "the direct connection measures its round trip");

  // Both reported their direct path: the SFU forwards no audio between them, so whatever the
  // listener hears from now on came the direct way.
  let heard = count_audio_frames(&mut calls[1], Duration::from_secs(2)).await;
  assert!(
    heard > 50,
    "two seconds of audio on the direct path, heard {heard}"
  );

  for call in calls {
    call.leave().await.unwrap();
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outsider_never_goes_direct_and_the_setting_off_goes_back_through_the_sfu() {
  if !has_routed_interface() {
    eprintln!("no network interface with a route: nothing to connect directly over");
    return;
  }
  // Device 2 is a guest to device 1: the default policy keeps the pair on the SFU.
  let (port, certificate_hash) = start_sfu().await;
  let calls = join_both(
    port,
    certificate_hash,
    |device| {
      if device == 2 {
        CallPeerStanding::Outsider
      } else {
        CallPeerStanding::Member
      }
    },
    audio_only(),
  )
  .await;
  tokio::time::sleep(Duration::from_secs(4)).await;
  for call in &calls {
    assert_eq!(path(call).await, Some(MediaPath::Relay));
  }
  for call in calls {
    call.leave().await.unwrap();
  }

  let (port, certificate_hash) = start_sfu().await;
  let mut calls = join_both(port, certificate_hash, |_| CallPeerStanding::Member, audio_only()).await;
  wait_for_path(&calls[0], true).await;
  calls[0].set_direct_connections(false);
  wait_for_path(&calls[0], false).await;
  wait_for_path(&calls[1], false).await;
  let heard = count_audio_frames(&mut calls[1], Duration::from_secs(2)).await;
  assert!(heard > 50, "the SFU forwards the audio again, heard {heard}");
  for call in calls {
    call.leave().await.unwrap();
  }
}

/// #127 part 4: once both report their direct path, the SFU forwards no video between them, so
/// every picture the viewer decodes from then on came the direct way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_goes_over_the_direct_path_and_the_viewer_decodes_it() {
  if !has_routed_interface() {
    eprintln!("no network interface with a route: nothing to connect directly over");
    return;
  }
  let (port, certificate_hash) = start_sfu().await;
  let mut calls = join_both(port, certificate_hash, |_| CallPeerStanding::Member, with_video()).await;
  wait_for_path(&calls[0], true).await;
  wait_for_path(&calls[1], true).await;
  calls[0].start_video(camera_config()).await.unwrap();

  let presenter: DeviceId = id_of(1);
  let decoded: Arc<Mutex<usize>> = Arc::default();
  let viewer = &mut calls[1];
  let watch_until = tokio::time::Instant::now() + DIRECT_WITHIN;
  let mut is_watching = false;
  while !is_watching && tokio::time::Instant::now() < watch_until {
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await
      && track.device_id == presenter
    {
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |_frame| *sink.lock().unwrap() += 1),
      );
      is_watching = true;
    }
  }
  assert!(is_watching, "the camera was announced");
  // Pictures start once the direct layer's keyframe arrives; then they keep coming.
  let first_until = tokio::time::Instant::now() + DIRECT_WITHIN;
  while *decoded.lock().unwrap() == 0 && tokio::time::Instant::now() < first_until {
    drain_events(viewer, Duration::from_millis(100)).await;
  }
  let before = *decoded.lock().unwrap();
  drain_events(viewer, Duration::from_secs(3)).await;
  let pictures = *decoded.lock().unwrap() - before;
  assert!(
    pictures >= 30,
    "three seconds of camera on the direct path, decoded {pictures}"
  );
  assert!(matches!(path(&calls[1]).await, Some(MediaPath::Direct { .. })));

  calls[0].stop_video(VideoTrackKind::Camera);
  for call in calls {
    call.leave().await.unwrap();
  }
}

/// Both devices join with tone sources; device 2 hands its received audio to the test as events.
/// `standing` is how every device places device `n`.
async fn join_both(
  port: u16,
  certificate_hash: [u8; 32],
  standing: impl Fn(u64) -> CallPeerStanding,
  capabilities: MediaCapabilities,
) -> Vec<NativeCall> {
  let mut authenticators = authenticators();
  let mut calls = Vec::new();
  for (index, device) in DEVICES.into_iter().enumerate() {
    let engine = CallEngineConfig {
      user_id: id_of(device),
      device_id: id_of(device),
      capabilities: capabilities.clone(),
      bwe: BweConfig::default(),
    };
    let sfu_url = format!(
      "https://{}{CALL_PATH}",
      SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
    );
    let mut config = NativeCallConfig::new(sfu_url, format!("dev:{CALL_ID}:{device}:{device}"), engine);
    config.maybe_certificate_hash = Some(certificate_hash);
    config.source = AudioSource::Tone {
      frequency_hz: 300.0 + 200.0 * index as f32,
      amplitude: 0.3,
    };
    config.sink = if device == 2 {
      AudioSink::Frames
    } else {
      AudioSink::Discard
    };
    config.maybe_trusted_roster = Some(
      DEVICES
        .iter()
        .filter(|other| **other != device)
        .map(|id| id_of(*id))
        .collect(),
    );
    config.maybe_authenticator = authenticators.remove(&device);
    config.maybe_direct = Some(DirectPathConfig {
      policy: DirectMediaPolicy::default(),
      does_user_allow: true,
      own: standing(device),
      standings: DEVICES.iter().map(|n| (id_of(*n), standing(*n))).collect(),
    });
    calls.push(NativeCall::join(config).await.unwrap());
  }
  calls
}

/// Waits until the call reports its audio direct (`true`) or through the SFU; returns the direct
/// connection's round trip.
async fn wait_for_path(call: &NativeCall, is_direct: bool) -> Option<u32> {
  tokio::time::timeout(DIRECT_WITHIN, async {
    loop {
      match path(call).await {
        Some(MediaPath::Direct { maybe_rtt_ms }) if is_direct => return maybe_rtt_ms,
        Some(MediaPath::Relay) if !is_direct => return None,
        _ => tokio::time::sleep(Duration::from_millis(100)).await,
      }
    }
  })
  .await
  .unwrap_or_else(|_| {
    panic!(
      "the path did not become {}",
      if is_direct { "direct" } else { "relayed" }
    )
  })
}

async fn path(call: &NativeCall) -> Option<MediaPath> {
  call
    .video_stats()
    .await
    .and_then(|report| report.maybe_media_path)
}

fn audio_only() -> MediaCapabilities {
  MediaCapabilities::default()
}

fn with_video() -> MediaCapabilities {
  MediaCapabilities {
    audio_codecs: vec![AudioCodec::Opus],
    video_encoders: vec![VideoCodec::H264],
    video_decoders: vec![VideoCodec::H264],
    max_simulcast_layers: 3,
    max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
    video_features: Vec::new(),
    audio_features: Vec::new(),
  }
}

/// A 720p test pattern in three simulcast layers.
fn camera_config() -> VideoSendConfig {
  let resolution = VideoResolution {
    width: 1280,
    height: 720,
  };
  VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(TestPatternCapture::new(CaptureFormat {
      resolution,
      framerate: 30,
    })),
    layers: simulcast_layers(resolution, 30, 1_500_000, 3),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_picture_tap: None,
    maybe_idle_refresh: None,
    maybe_screen_profile: None,
  }
}

/// Takes the call's events for `window`, so its frame callbacks run.
async fn drain_events(call: &mut NativeCall, window: Duration) {
  let end = tokio::time::Instant::now() + window;
  while let Ok(Some(_)) = tokio::time::timeout_at(end, call.next_event()).await {}
}

/// Audio frames that reach the listener within `window`.
async fn count_audio_frames(call: &mut NativeCall, window: Duration) -> usize {
  let mut heard = 0;
  let end = tokio::time::Instant::now() + window;
  while let Ok(Some(event)) = tokio::time::timeout_at(end, call.next_event()).await {
    if matches!(event, CallEngineEvent::AudioFrame { .. }) {
      heard += 1;
    }
  }
  heard
}

/// Whether this machine sends anywhere but loopback: the clients' host candidates come from it.
fn has_routed_interface() -> bool {
  let Ok(socket) = UdpSocket::bind("0.0.0.0:0") else {
    return false;
  };
  socket.connect("192.0.2.1:9").is_ok()
    && socket
      .local_addr()
      .is_ok_and(|local| !local.ip().is_loopback() && !local.ip().is_unspecified())
}

/// One identity per device; every client trusts both device certificates.
fn authenticators() -> HashMap<u64, Arc<dyn CallKeyAuthenticator>> {
  let mut key_pairs = Vec::new();
  let mut verified = HashMap::new();
  for device in DEVICES {
    let identity = IdentityKeyPair::generate(&mut OsRng);
    let user = CryptoUserId::new(1000 + device);
    let key_pair = DeviceKeyPair::generate(user, CryptoDeviceId::new(device), &mut OsRng);
    let certificate = identity.certify_device(user, &key_pair.public_keys()).unwrap();
    verified.insert(
      id_of::<DeviceId>(device),
      certificate.verify(&identity.public_key()).unwrap(),
    );
    key_pairs.push((device, key_pair));
  }
  key_pairs
    .into_iter()
    .map(|(device, key_pair)| {
      let authenticator: Arc<dyn CallKeyAuthenticator> = Arc::new(DeviceCallKeyAuthenticator::new(
        Arc::new(key_pair),
        verified.clone(),
      ));
      (device, authenticator)
    })
    .collect()
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

fn id_of<T: std::str::FromStr>(raw: u64) -> T
where
  T::Err: std::fmt::Debug,
{
  raw.to_string().parse().unwrap()
}
