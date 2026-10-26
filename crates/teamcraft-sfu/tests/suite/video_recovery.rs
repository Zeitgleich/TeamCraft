//! Video that has to recover through the SFU (issue #683): a viewer that lost a keyframe asks for
//! a new one instead of waiting for the publisher's periodic keyframe (300 frames, 10 s here).

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
use tokio::net::UdpSocket;
use wtransport::Identity;

const VGA: VideoResolution = VideoResolution {
  width: 640,
  height: 360,
};
/// Well below the periodic keyframe (300 frames at 30 fps): only a requested one arrives in time.
const RECOVERY_DEADLINE: Duration = Duration::from_secs(5);
/// QUIC packets this large carry video fragments (1100 bytes each); signaling after the call is
/// secured is far smaller, and so are the deltas of the small test layer.
const VIDEO_PACKET_BYTES: usize = 1000;
/// Covers the keyframes the start of a subscription brings (the publisher's own, the SFU's).
const LOSS_WINDOW: Duration = Duration::from_millis(500);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_viewer_that_lost_the_first_keyframe_asks_for_another() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let relay = LossyRelay::start(sfu).await;
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let mut viewer = join(relay.address, certificate_hash, 2, 1).await;

  // Once the viewer is secured, the keyframes the start of its subscription brings are lost;
  // the deltas after them (smaller than a fragment) arrive and can't be decoded.
  wait_until_secured(&mut viewer).await;
  relay.lose_video_for(LOSS_WINDOW);
  let config = VideoSendConfig {
    kind: VideoTrackKind::Screen,
    capture: Box::new(TestPatternCapture::new(CaptureFormat {
      resolution: VGA,
      framerate: 30,
    })),
    // A screen share: the downlink estimate, hit by the loss, may not pause it (a camera it may).
    // One small layer, so no layer switch brings a keyframe either.
    layers: simulcast_layers(VGA, 30, 150_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
  };
  presenter.start_video(config).await.unwrap();

  let decoded: Arc<Mutex<Vec<VideoFrame>>> = Arc::default();
  let deadline = tokio::time::Instant::now() + RECOVERY_DEADLINE;
  while tokio::time::Instant::now() < deadline && decoded.lock().unwrap().is_empty() {
    let Ok(maybe_event) = tokio::time::timeout(Duration::from_millis(20), viewer.next_event()).await else {
      continue;
    };
    if let Some(CallEngineEvent::VideoPublished(track)) = maybe_event {
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| sink.lock().unwrap().push(frame)),
      );
    }
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  assert!(
    relay.dropped_packets() > 0,
    "the relay never saw a video packet to drop"
  );
  assert!(
    !decoded.lock().unwrap().is_empty(),
    "no picture within {RECOVERY_DEADLINE:?}: the lost keyframe wasn't replaced"
  );
}

/// Relays one client's UDP to the SFU and back. Told to, it loses the large packets towards the
/// client (video fragments) for a while from the first one on: the keyframes at the start of a
/// subscription, while the small deltas after them still arrive.
struct LossyRelay {
  address: SocketAddr,
  loss: Arc<Mutex<Loss>>,
}

/// What the relay does with large packets towards the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Loss {
  None,
  /// From the next large packet on, for this long.
  Armed(Duration),
  Losing {
    until: tokio::time::Instant,
    dropped: u32,
  },
  Over {
    dropped: u32,
  },
}

impl LossyRelay {
  async fn start(sfu: SocketAddr) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let loss = Arc::new(Mutex::new(Loss::None));
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
    let relay_loss = Arc::clone(&loss);
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        let is_lost =
          len >= VIDEO_PACKET_BYTES && relay_loss.lock().unwrap().loses_one(tokio::time::Instant::now());
        let maybe_client = *client.lock().unwrap();
        if let (false, Some(client)) = (is_lost, maybe_client) {
          let _ = client_side.send_to(&buffer[..len], client).await;
        }
      }
    });
    Self { address, loss }
  }

  fn lose_video_for(&self, window: Duration) {
    *self.loss.lock().unwrap() = Loss::Armed(window);
  }

  fn dropped_packets(&self) -> u32 {
    match *self.loss.lock().unwrap() {
      Loss::Losing { dropped, .. } | Loss::Over { dropped } => dropped,
      Loss::None | Loss::Armed(_) => 0,
    }
  }
}

impl Loss {
  /// Whether a large packet arriving `now` is lost.
  fn loses_one(&mut self, now: tokio::time::Instant) -> bool {
    match *self {
      Self::None | Self::Over { .. } => false,
      Self::Armed(window) => {
        *self = Self::Losing {
          until: now + window,
          dropped: 1,
        };
        true
      },
      Self::Losing { until, dropped } if now < until => {
        *self = Self::Losing {
          until,
          dropped: dropped + 1,
        };
        true
      },
      Self::Losing { dropped, .. } => {
        *self = Self::Over { dropped };
        false
      },
    }
  }
}

async fn wait_until_secured(call: &mut NativeCall) {
  let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
  loop {
    assert!(
      tokio::time::Instant::now() < deadline,
      "the call was never secured"
    );
    if let Ok(Some(CallEngineEvent::Secured { .. })) =
      tokio::time::timeout(Duration::from_millis(50), call.next_event()).await
    {
      return;
    }
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
