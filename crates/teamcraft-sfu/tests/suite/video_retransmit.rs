//! Video on a lossy downlink (#1762): a relay loses about every tenth video packet towards the
//! viewer. Before, a keyframe of many packets almost never arrived whole, nor did the one asked for
//! instead, and the picture froze. Now the viewer asks the SFU for the missing packets (NACK), the
//! SFU resends them from its history, and the picture keeps going: the first keyframe completes,
//! frames keep coming, and keyframe requests stay rare.
//!
//! The presenter shares a busy picture (the suite's busy camera at 720p, keyframes of a few dozen
//! packets) on one layer; the viewer watches through the relay, decodes what it gets and reads its
//! own stats.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::simulcast_layers;
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, VideoCodec, VideoTrackKind};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use tokio::time::Instant;
use wtransport::Identity;

use crate::common::busy_camera::{BusyCamera, FRAMERATE, HD};

/// The share's bitrate: a 720p busy picture's keyframes take a few dozen packets, as in #1762.
const SHARE_BPS: u32 = 2_500_000;
/// QUIC packets this large carry video: the presenter's sound (Opus, about 150 bytes a packet)
/// and the SFU's signaling are smaller.
const VIDEO_PACKET_BYTES: usize = 400;
/// One in this many video packets towards the viewer is lost (10 %), resent ones too.
const LOSS_ONE_IN: u32 = 10;
/// How long the first picture may take: securing the call, the first keyframe through the loss.
const FIRST_PICTURE_DEADLINE: Duration = Duration::from_secs(10);
/// How long the picture is watched after the first frame.
const WATCH: Duration = Duration::from_secs(6);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn video_keeps_going_through_a_lossy_downlink() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let relay = LossyRelay::start(sfu).await;
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let mut viewer = join(relay.address, certificate_hash, 2, 1).await;
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Screen,
    capture: Box::new(BusyCamera::new(std::time::Instant::now())),
    layers: simulcast_layers(HD, FRAMERATE, SHARE_BPS, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_picture_tap: None,
    maybe_idle_refresh: None,
  };
  presenter.start_video(config).await.unwrap();

  let decoded = Arc::new(AtomicU32::new(0));
  let first_at: Arc<Mutex<Option<Instant>>> = Arc::default();
  let deadline = Instant::now() + FIRST_PICTURE_DEADLINE;
  while Instant::now() < deadline && first_at.lock().unwrap().is_none() {
    let Ok(maybe_event) = tokio::time::timeout(Duration::from_millis(20), viewer.next_event()).await else {
      continue;
    };
    if let Some(CallEngineEvent::VideoPublished(track)) = maybe_event {
      let (count, first) = (Arc::clone(&decoded), Arc::clone(&first_at));
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |_frame| {
          count.fetch_add(1, Ordering::Relaxed);
          first.lock().unwrap().get_or_insert_with(Instant::now);
        }),
      );
    }
  }
  assert!(
    first_at.lock().unwrap().is_some(),
    "no picture within {FIRST_PICTURE_DEADLINE:?}: the keyframe never completed"
  );
  let decoded_before = decoded.load(Ordering::Relaxed);
  let watch_until = Instant::now() + WATCH;
  while Instant::now() < watch_until {
    // Events go unread otherwise; the call needs nothing from them.
    let _ = tokio::time::timeout(Duration::from_millis(50), viewer.next_event()).await;
  }
  let frames = decoded.load(Ordering::Relaxed) - decoded_before;
  let stats = serde_json::to_value(viewer.video_stats().await.expect("the call runs")).unwrap();
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();

  let (video, dropped) = (
    relay.video.load(Ordering::Relaxed),
    relay.dropped.load(Ordering::Relaxed),
  );
  let recovered = counter(&stats, "packets_recovered");
  let keyframe_requests = counter(&stats, "keyframe_requests");
  let expected = u32::from(FRAMERATE) * WATCH.as_secs() as u32;
  eprintln!(
    "dropped {dropped} of {video} packets, recovered {recovered}, {keyframe_requests} keyframe requests, \
     {frames} of {expected} frames decoded"
  );
  assert!(dropped > 20, "the relay lost only {dropped} packets");
  assert!(
    recovered > u64::from(dropped) / 2,
    "{recovered} of {dropped} lost packets came again"
  );
  // Without retransmission nearly every frame of several packets broke the picture until the
  // next keyframe; now most of them are shown.
  assert!(
    frames >= expected / 2,
    "{frames} of {expected} frames decoded through the loss"
  );
  assert!(
    keyframe_requests <= 4,
    "{keyframe_requests} keyframe requests in {WATCH:?}: losses weren't repaired"
  );
}

/// Relays one client's UDP to the SFU and back, losing one in [`LOSS_ONE_IN`] large packets
/// towards the client (a fixed pseudo-random pattern, so runs compare).
struct LossyRelay {
  address: SocketAddr,
  /// Video packets towards the client, and those lost.
  video: Arc<AtomicU32>,
  dropped: Arc<AtomicU32>,
}

impl LossyRelay {
  async fn start(sfu: SocketAddr) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let (video, dropped) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    let (from_client, to_sfu, known_client) = (
      Arc::clone(&client_side),
      Arc::clone(&sfu_side),
      Arc::clone(&client),
    );
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok((len, from)) = from_client.recv_from(&mut buffer).await {
        *known_client.lock().unwrap() = Some(from);
        let _ = to_sfu.send(&buffer[..len]).await;
      }
    });
    let (seen, counted) = (Arc::clone(&video), Arc::clone(&dropped));
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      // A linear congruential generator (Numerical Recipes): enough for a loss pattern.
      let mut state: u32 = 0x1762;
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        let is_lost = len >= VIDEO_PACKET_BYTES && {
          seen.fetch_add(1, Ordering::Relaxed);
          state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
          (state >> 16).is_multiple_of(LOSS_ONE_IN)
        };
        if is_lost {
          counted.fetch_add(1, Ordering::Relaxed);
          continue;
        }
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&buffer[..len], client).await;
        }
      }
    });
    Self {
      address,
      video,
      dropped,
    }
  }
}

/// The sum of a counter over the received tracks in a stats report.
fn counter(stats: &serde_json::Value, name: &str) -> u64 {
  match stats {
    serde_json::Value::Object(fields) => fields
      .iter()
      .map(|(key, value)| match value {
        serde_json::Value::Number(number) if key == name => number.as_u64().unwrap_or(0),
        other => counter(other, name),
      })
      .sum(),
    serde_json::Value::Array(values) => values.iter().map(|value| counter(value, name)).sum(),
    _ => 0,
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
      max_simulcast_layers: 3,
      max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
      video_features: Vec::new(),
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
