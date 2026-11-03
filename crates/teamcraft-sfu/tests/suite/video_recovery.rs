//! Video that has to recover through the SFU (issue #683): a viewer that lost a keyframe asks for
//! a new one instead of waiting for the publisher's periodic keyframe (300 frames, 10 s here), and
//! a still screen, which delivers no frames, still answers a keyframe request. Own cameras that
//! misbehave (#683, #922): one that stops delivering ends its track, and a camera that is slow to
//! open or to stop leaves the call's task free.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent, OwnVideoEnd};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{VideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{
  CaptureError, CaptureFormat, FrameSender, TestPatternCapture, VideoCapture, test_pattern,
};
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
/// A viewer that subscribes this long after the share started: it missed the first picture.
const LATE_SUBSCRIBE: Duration = Duration::from_millis(500);
/// How long a camera of these tests may deliver nothing before its track ends.
const STALL_LIMIT: Duration = Duration::from_millis(500);
/// How long the call's task may take to answer a request while a camera opens or stops: far
/// below the time a stop used to hold it (`SENDER_STOP_WAIT`, 1 s), far above an answer's.
const RESPONSIVE: Duration = Duration::from_millis(500);

/// A shared screen that doesn't change after its first picture (ScreenCaptureKit and PipeWire
/// deliver nothing then). The viewer subscribes later; the SFU asks for a keyframe, and the
/// presenter answers with the screen's last picture.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_late_viewer_of_a_still_screen_gets_its_picture() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let mut presenter = join(sfu, certificate_hash, 1, 2).await;
  let mut viewer = join(sfu, certificate_hash, 2, 1).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;

  let screen = StillScreen::default();
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Screen,
    capture: Box::new(screen.clone()),
    layers: simulcast_layers(VGA, 30, 800_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    // Only the answer to the keyframe request may bring the picture, not the idle refresh.
    maybe_idle_refresh: None,
  };
  let first_picture = presenter.start_video_checked(config).await.unwrap();
  screen.show_one_picture();
  first_picture.wait().await.unwrap();

  let decoded: Arc<Mutex<Vec<VideoFrame>>> = Arc::default();
  let mut maybe_subscribe_at = None;
  let deadline = tokio::time::Instant::now() + RECOVERY_DEADLINE;
  while tokio::time::Instant::now() < deadline && decoded.lock().unwrap().is_empty() {
    if let Ok(Some(CallEngineEvent::VideoPublished(track))) =
      tokio::time::timeout(Duration::from_millis(20), viewer.next_event()).await
    {
      maybe_subscribe_at = Some((track.stream_id, tokio::time::Instant::now() + LATE_SUBSCRIBE));
    }
    if let Some((stream_id, at)) = maybe_subscribe_at
      && tokio::time::Instant::now() >= at
    {
      maybe_subscribe_at = None;
      let sink = Arc::clone(&decoded);
      viewer.watch_video(
        stream_id,
        None,
        Box::new(move |frame| sink.lock().unwrap().push(frame)),
      );
    }
  }
  presenter.stop_video(VideoTrackKind::Screen);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  assert!(
    !decoded.lock().unwrap().is_empty(),
    "the late viewer never saw the still screen"
  );
}

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
    maybe_layer_plan: None,
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
    // Frames come all the time: only a requested keyframe can help.
    maybe_idle_refresh: None,
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

/// A camera whose driver hangs: it never stops. Stopping it and leaving still go through, and
/// the audio task stays free meanwhile.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capture_that_never_stops_does_not_freeze_the_call() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let _viewer = join(sfu, certificate_hash, 2, 1).await;
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(HangingCamera::default()),
    layers: simulcast_layers(VGA, 30, 150_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  };
  presenter.start_video(config).await.unwrap();
  presenter.stop_video(VideoTrackKind::Camera);
  // Without a bound the call's task would wait for the camera for good, and so would leaving.
  let left = tokio::time::timeout(Duration::from_secs(10), presenter.leave()).await;
  assert!(
    matches!(left, Ok(Ok(_))),
    "the call hung behind the camera: {left:?}"
  );
}

/// A camera that stops delivering without ending, as an unplugged one on macOS (#922): its track
/// ends after the camera's stall limit, and the app hears that it ended while sending.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_stops_delivering_ends_its_track() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let mut presenter = join(sfu, certificate_hash, 1, 2).await;
  let _viewer = join(sfu, certificate_hash, 2, 1).await;
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(GoneCamera::default()),
    layers: simulcast_layers(VGA, 30, 150_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  };
  let first_picture = presenter.start_video_checked(config).await.unwrap();
  first_picture.wait().await.unwrap();
  let started_at = tokio::time::Instant::now();
  let deadline = started_at + STALL_LIMIT * 20;
  let end = loop {
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    match tokio::time::timeout(left, presenter.next_event()).await {
      Ok(Some(CallEngineEvent::OwnVideoEnded { kind, end })) => {
        assert_eq!(kind, VideoTrackKind::Camera);
        break end;
      },
      Ok(Some(_)) => {},
      Ok(None) => panic!("the call ended"),
      Err(_) => panic!("the silent camera's track never ended"),
    }
  };
  assert_eq!(end, OwnVideoEnd::WhileSending);
  assert!(
    started_at.elapsed() >= STALL_LIMIT,
    "ended after {:?}, before the camera's limit",
    started_at.elapsed()
  );
  presenter.leave().await.unwrap();
}

/// A camera that takes long to open (an iPhone camera takes seconds, #922): the call's task
/// answers other requests meanwhile (its audio and signaling run on the same task), and the start
/// answers once the camera opened.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_is_slow_to_open_leaves_the_call_free() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let _viewer = join(sfu, certificate_hash, 2, 1).await;
  let (camera, mut gates) = GatedCamera::new(Gate::Open);
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(camera),
    layers: simulcast_layers(VGA, 30, 150_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  };
  {
    let starting = presenter.start_video(config);
    tokio::pin!(starting);
    tokio::select! {
      started = &mut starting => panic!("the start answered before the camera opened: {started:?}"),
      () = gates.entered() => {},
    }
    // The camera is opening and stays so until released.
    let answered = tokio::time::timeout(
      RESPONSIVE,
      presenter.set_audio(AudioSource::Silence, AudioSink::Discard),
    )
    .await;
    assert!(
      matches!(answered, Ok(Ok(()))),
      "the call's task waited for the camera to open: {answered:?}"
    );
    gates.release();
    tokio::time::timeout(Duration::from_secs(10), starting)
      .await
      .expect("the start answered once the camera opened")
      .unwrap();
  }
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
}

/// A camera that takes long to let go: stopping it (or switching away from it) doesn't hold up
/// the call's task, which used to wait up to a second and a half for it (#922).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_camera_that_is_slow_to_stop_leaves_the_call_free() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let presenter = join(sfu, certificate_hash, 1, 2).await;
  let _viewer = join(sfu, certificate_hash, 2, 1).await;
  let (camera, mut gates) = GatedCamera::new(Gate::Stop);
  let config = VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(camera),
    layers: simulcast_layers(VGA, 30, 150_000, 1),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  };
  presenter.start_video(config).await.unwrap();
  presenter.stop_video(VideoTrackKind::Camera);
  gates.entered().await;
  // The camera is stopping and stays so until released.
  let started_at = tokio::time::Instant::now();
  let answered = presenter
    .set_audio(AudioSource::Silence, AudioSink::Discard)
    .await;
  let took = started_at.elapsed();
  assert!(answered.is_ok(), "{answered:?}");
  assert!(
    took < RESPONSIVE,
    "the call's task waited {took:?} for the camera to stop"
  );
  gates.release();
  let left = tokio::time::timeout(Duration::from_secs(10), presenter.leave()).await;
  assert!(matches!(left, Ok(Ok(_))), "{left:?}");
}

/// A camera that delivers a few frames and then nothing, without ending: its queue stays open,
/// as nokhwa's AVFoundation camera's read when the camera is unplugged.
#[derive(Default)]
struct GoneCamera {
  maybe_running: Option<(std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>)>,
}

impl VideoCapture for GoneCamera {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: VGA,
      framerate: 30,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
      for index in 0..10 {
        let _ = frames.send(test_pattern(VGA, index, index * 33_333).unwrap());
        if !matches!(
          stopped.recv_timeout(Duration::from_millis(33)),
          Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
          return;
        }
      }
      // Gone: no frame any more, and no end either, until stopped.
      let _ = stopped.recv();
      drop(frames);
    });
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      drop(stop);
      let _ = thread.join();
    }
  }

  fn stall_limit(&self) -> Option<Duration> {
    Some(STALL_LIMIT)
  }
}

/// Where a [`GatedCamera`] waits until the test releases it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Gate {
  /// Its start (opening the camera).
  Open,
  /// Its stop (letting the camera go).
  Stop,
}

/// A camera delivering a test pattern that waits at its [`Gate`] until released.
struct GatedCamera {
  gate: Gate,
  capture: TestPatternCapture,
  entered: Option<tokio::sync::oneshot::Sender<()>>,
  released: std::sync::mpsc::Receiver<()>,
}

/// The test's side of a [`GatedCamera`].
struct CameraGates {
  entered: tokio::sync::oneshot::Receiver<()>,
  release: std::sync::mpsc::Sender<()>,
}

impl GatedCamera {
  fn new(gate: Gate) -> (Self, CameraGates) {
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let camera = Self {
      gate,
      capture: TestPatternCapture::new(CaptureFormat {
        resolution: VGA,
        framerate: 30,
      }),
      entered: Some(entered),
      released,
    };
    let gates = CameraGates {
      entered: entered_rx,
      release,
    };
    (camera, gates)
  }

  fn wait_at(&mut self, gate: Gate) {
    if gate != self.gate {
      return;
    }
    if let Some(entered) = self.entered.take() {
      let _ = entered.send(());
      // Released, or the test is gone.
      let _ = self.released.recv();
    }
  }
}

impl VideoCapture for GatedCamera {
  fn format(&self) -> CaptureFormat {
    self.capture.format()
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    self.wait_at(Gate::Open);
    self.capture.start(frames)
  }

  fn stop(&mut self) {
    self.capture.stop();
    self.wait_at(Gate::Stop);
  }
}

impl CameraGates {
  /// Once the camera waits at its gate (call once).
  async fn entered(&mut self) {
    // An error means the camera went without reaching its gate; the test's checks then fail.
    let _ = (&mut self.entered).await;
  }

  fn release(&self) {
    let _ = self.release.send(());
  }
}

/// A camera that delivers frames but whose stop never returns (a driver stuck in its read).
#[derive(Default)]
struct HangingCamera {
  maybe_running: Option<TestPatternCapture>,
}

impl VideoCapture for HangingCamera {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: VGA,
      framerate: 30,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let mut capture = TestPatternCapture::new(self.format());
    capture.start(frames)?;
    self.maybe_running = Some(capture);
    Ok(())
  }

  fn stop(&mut self) {
    if let Some(mut capture) = self.maybe_running.take() {
      capture.stop();
    }
    // Hangs for good, as a read that never returns: the sender lives as long as the wait.
    let (_never_sent, blocked) = std::sync::mpsc::channel::<()>();
    let _ = blocked.recv();
  }
}

/// A shared screen that shows one picture when told to, and then stands still.
#[derive(Clone, Default)]
struct StillScreen(Arc<Mutex<Option<FrameSender>>>);

impl StillScreen {
  fn show_one_picture(&self) {
    let frames = self.0.lock().unwrap();
    let frames = frames.as_ref().expect("the capture runs");
    assert!(frames.send(test_pattern(VGA, 0, 0).unwrap()));
  }
}

impl VideoCapture for StillScreen {
  fn format(&self) -> CaptureFormat {
    CaptureFormat {
      resolution: VGA,
      framerate: 30,
    }
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    *self.0.lock().unwrap() = Some(frames);
    Ok(())
  }

  fn stop(&mut self) {
    self.0.lock().unwrap().take();
  }
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
