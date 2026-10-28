//! A camera turned on while the call can't send it yet, checked the way the desktop checks it: it
//! waits for the track's first picture and turns the camera off without one (issue #935).
//!
//! - Turned on in a call that so far sent only sound. Such a call leaves an uplink estimate behind
//!   that says little about the path: it measured what the sound needed, and one lossy moment cuts
//!   it below what even the lowest camera layer takes. The camera started from it with every layer
//!   held back, nothing was encoded, the desktop turned it off after ten seconds ("the camera
//!   delivered 257 pictures, but none could be encoded") and the others saw black. A rejoin, with
//!   a fresh estimate, made it work.
//! - Turned on right after joining, before the call is secured and before the other side is there.
//!
//! Through a real SFU, with the encoders the release uses (`preferred_encoders`: VideoToolbox or
//! VA-API with the `gstreamer` feature, OpenH264 behind them) and a camera that takes a moment to
//! deliver its first picture, like a real one.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use teamcraft_media::audio_io::AudioSource;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::picture_check::{PictureContent, picture_content};
use teamcraft_media::uplink_layers::UplinkPolicy;
use teamcraft_media::video::{VideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture, test_pattern};
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, VideoCodec, VideoTrackKind};
use teamcraft_sfu::auth::DevTokenVerifier;
use teamcraft_sfu::config::SfuConfig;
use teamcraft_sfu::server::{CALL_PATH, Sfu};
use tokio::net::UdpSocket;
use wtransport::Identity;

const HD: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
const FRAMERATE: u8 = 30;
/// How long the camera takes to its first picture: a MacBook or iPhone camera takes about this
/// long, and the call has answered the publication by then.
const CAMERA_WARM_UP: Duration = Duration::from_millis(800);
/// Pictures with content the viewer has to see.
const WANTED_PICTURES: usize = 15;
/// Well below the encoder's first-picture check (10 s): the viewer sees the camera at once.
const PICTURE_DEADLINE: Duration = Duration::from_secs(8);
/// Bounds the steps before the camera starts (securing, the lossy moment).
const STEP_DEADLINE: Duration = Duration::from_secs(10);

/// The camera goes on after a lossy moment cut the sending side's estimate to what its sound got
/// through. The viewer gets the pictures, and the camera isn't given up on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_turned_on_after_a_lossy_moment_reaches_the_viewer() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let relay = UplinkRelay::start(sfu).await;
  let mut presenter = join(relay.address, certificate_hash, 1, 2).await;
  let mut viewer = join(sfu, certificate_hash, 2, 1).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;

  // Sound only, then a moment of loss on the presenter's uplink: the estimate drops below what
  // the sound is given, and below every camera layer.
  relay.set_losing(true);
  let cut = wait_for_estimate(&mut presenter, |bps, _| {
    bps < UplinkPolicy::default().audio_reserve_bps
  })
  .await;
  relay.set_losing(false);
  // The loss is over (the estimator no longer sees it); the estimate is still low.
  let _ = wait_for_estimate(&mut presenter, |_, loss_percent| loss_percent == 0).await;

  let camera = WarmingCamera::default();
  let has_delivered = camera.delivered();
  let first_picture = presenter
    .start_video_checked(camera_config(camera))
    .await
    .unwrap();
  let checked = tokio::spawn(first_picture.wait());
  let watched = watch_camera(&mut presenter, &mut viewer).await;
  // What the desktop waits for before it keeps the camera on (it ends the track otherwise).
  let first = tokio::time::timeout(STEP_DEADLINE, checked)
    .await
    .unwrap()
    .unwrap();
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();

  assert!(
    has_delivered.load(Ordering::Relaxed),
    "the camera never delivered"
  );
  assert_eq!(
    first,
    Ok(()),
    "the camera was given up on (estimate cut to {cut} bit/s)"
  );
  assert!(!watched.has_ended, "the camera's track ended");
  assert!(
    watched.pictures >= WANTED_PICTURES,
    "{} pictures with content reached the viewer (estimate cut to {cut} bit/s)",
    watched.pictures
  );
}

/// The camera goes on right after joining, before the call is secured and before the viewer
/// joins: its first pictures are dropped, and the viewer still gets pictures at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_turned_on_before_the_other_side_joins_reaches_it() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let mut presenter = join(sfu, certificate_hash, 1, 2).await;
  let first_picture = presenter
    .start_video_checked(camera_config(WarmingCamera::default()))
    .await
    .unwrap();
  let checked = tokio::spawn(first_picture.wait());
  let mut viewer = join(sfu, certificate_hash, 2, 1).await;
  let watched = watch_camera(&mut presenter, &mut viewer).await;
  let first = tokio::time::timeout(STEP_DEADLINE, checked)
    .await
    .unwrap()
    .unwrap();
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();

  assert_eq!(first, Ok(()), "the camera was given up on");
  assert!(!watched.has_ended, "the camera's track ended");
  assert!(
    watched.pictures >= WANTED_PICTURES,
    "{} pictures with content reached the viewer",
    watched.pictures
  );
}

/// What the viewer saw of the presenter's camera.
struct Watched {
  /// Decoded pictures that show something, not black.
  pictures: usize,
  /// The presenter's own track ended meanwhile.
  has_ended: bool,
}

/// Lets `viewer` watch the presenter's camera once it is published, until it decoded
/// [`WANTED_PICTURES`] with content or [`PICTURE_DEADLINE`] passed.
async fn watch_camera(presenter: &mut NativeCall, viewer: &mut NativeCall) -> Watched {
  let decoded: Arc<Mutex<Vec<VideoFrame>>> = Arc::default();
  let mut has_ended = false;
  let deadline = tokio::time::Instant::now() + PICTURE_DEADLINE;
  while tokio::time::Instant::now() < deadline && count_detailed(&decoded) < WANTED_PICTURES {
    if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(5), presenter.next_event()).await
      && matches!(event, CallEngineEvent::OwnVideoEnded { .. })
    {
      has_ended = true;
    }
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(5), viewer.next_event()).await
    {
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        track.stream_id,
        None,
        Box::new(move |frame| sink.lock().unwrap().push(frame)),
      );
    }
  }
  Watched {
    pictures: count_detailed(&decoded),
    has_ended,
  }
}

/// Pictures decoded so far that show something, not black.
fn count_detailed(decoded: &Mutex<Vec<VideoFrame>>) -> usize {
  decoded
    .lock()
    .unwrap()
    .iter()
    .filter(|frame| picture_content(frame) == PictureContent::Detailed)
    .count()
}

fn camera_config(camera: WarmingCamera) -> VideoSendConfig {
  VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture: Box::new(camera),
    layers: simulcast_layers(HD, FRAMERATE, 1_500_000, 3),
    encoders: preferred_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  }
}

/// A test pattern camera that delivers its first picture only after [`CAMERA_WARM_UP`].
#[derive(Default)]
struct WarmingCamera {
  maybe_running: Option<(std_mpsc::Sender<()>, JoinHandle<()>)>,
  has_delivered: Arc<AtomicBool>,
}

impl WarmingCamera {
  /// Whether it delivered a picture yet, readable once the call has the camera.
  fn delivered(&self) -> Arc<AtomicBool> {
    Arc::clone(&self.has_delivered)
  }
}

impl VideoCapture for WarmingCamera {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: HD,
      framerate: FRAMERATE,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std_mpsc::channel::<()>();
    let has_delivered = Arc::clone(&self.has_delivered);
    let thread = std::thread::spawn(move || {
      let is_stopped = |wait| {
        !matches!(
          stopped.recv_timeout(wait),
          Err(std_mpsc::RecvTimeoutError::Timeout)
        )
      };
      if is_stopped(CAMERA_WARM_UP) {
        return;
      }
      let period = Duration::from_secs(1) / u32::from(FRAMERATE);
      for index in 0u64.. {
        let frame = test_pattern(HD, index, index * 33_333).unwrap();
        if !frames.send(frame) {
          return;
        }
        has_delivered.store(true, Ordering::Relaxed);
        if is_stopped(period) {
          return;
        }
      }
    });
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      drop(stop);
      thread.join().unwrap();
    }
  }
}

/// Relays one client's UDP to the SFU and back; told to, it loses every other packet towards the
/// SFU.
struct UplinkRelay {
  address: SocketAddr,
  is_losing: Arc<AtomicBool>,
}

impl UplinkRelay {
  async fn start(sfu: SocketAddr) -> Self {
    let client_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    let sfu_side = Arc::new(UdpSocket::bind((Ipv6Addr::LOCALHOST, 0)).await.unwrap());
    sfu_side.connect(sfu).await.unwrap();
    let address = client_side.local_addr().unwrap();
    let is_losing = Arc::new(AtomicBool::new(false));
    let client: Arc<Mutex<Option<SocketAddr>>> = Arc::default();
    let (from_client, to_sfu, known_client, losing) = (
      Arc::clone(&client_side),
      Arc::clone(&sfu_side),
      Arc::clone(&client),
      Arc::clone(&is_losing),
    );
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      let mut is_odd = false;
      while let Ok((len, from)) = from_client.recv_from(&mut buffer).await {
        *known_client.lock().unwrap() = Some(from);
        is_odd = !is_odd;
        if losing.load(Ordering::Relaxed) && is_odd {
          continue;
        }
        let _ = to_sfu.send(&buffer[..len]).await;
      }
    });
    tokio::spawn(async move {
      let mut buffer = vec![0; 65_536];
      while let Ok(len) = sfu_side.recv(&mut buffer).await {
        let maybe_client = *client.lock().unwrap();
        if let Some(client) = maybe_client {
          let _ = client_side.send_to(&buffer[..len], client).await;
        }
      }
    });
    Self { address, is_losing }
  }

  fn set_losing(&self, is_losing: bool) {
    self.is_losing.store(is_losing, Ordering::Relaxed);
  }
}

/// Reads `call`'s events until an uplink estimate satisfies `is_wanted(bps, loss_percent)`; that
/// estimate.
async fn wait_for_estimate(call: &mut NativeCall, is_wanted: impl Fn(u32, u8) -> bool) -> u32 {
  let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
  loop {
    assert!(tokio::time::Instant::now() < deadline, "no such uplink estimate");
    if let Ok(Some(CallEngineEvent::UplinkEstimate { bps, loss_percent })) =
      tokio::time::timeout(Duration::from_millis(50), call.next_event()).await
      && is_wanted(bps, loss_percent)
    {
      return bps;
    }
  }
}

async fn wait_until_secured(call: &mut NativeCall) {
  let deadline = tokio::time::Instant::now() + STEP_DEADLINE;
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
  // The call's sound: what the uplink estimate measures before the camera goes on.
  config.source = AudioSource::Tone {
    frequency_hz: 440.0,
    amplitude: 0.3,
  };
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
