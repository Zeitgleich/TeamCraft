//! Browser driver for [`CallEngine`]: WebTransport to the SFU, WebCodecs Opus and Web Audio, on
//! the page's event loop.
//!
//! The engine is the same as on desktop (signaling, DAVE, bandwidth feedback); this module only
//! moves bytes and samples. Capture goes through an AudioWorklet in every browser: it works in
//! Chrome, Firefox and Safari alike (Chrome's MediaStreamTrackProcessor would only save a copy)
//! and yields the samples the audio level extension needs. WebCodecs encodes and decodes Opus, a
//! second worklet plays the mix. The browser's own echo cancellation, noise suppression and gain
//! control run on the microphone (`getUserMedia` constraints).
//!
//! Screen share and remote video live in [`video`].
//!
//! Every input (network, worklet, codecs, timers, app commands) is funneled into one channel that
//! a single task drains, so the engine needs no locking. Timers use `setInterval`, which browsers
//! do not throttle in tabs that play audio.
//!
//! Browsers can't migrate a WebTransport session to another network. When the session closes
//! without the call being over, the driver opens a new one with a fresh join ticket and the engine
//! rejoins within the SFU's grace period (ARCHITECTURE.md §6.5); audio and video carry over. A
//! session the SFU closed because it is going away (a restart, a deploy) is rejoined after a random
//! part of a short spread, like on native clients.
//!
//! A dead session doesn't have to wait for QUIC's idle timeout: where the browser reports the
//! session's received packets (`getStats()`), the SFU's keep-alives prove the path alive; in every
//! browser, so also in Safari whose counts stay zero, the datagrams and signaling frames the page
//! reads do, and so do the SFU's answers to the pings the page sends when nothing else arrived for
//! a second (`session_watch`). Silence beyond the stall timeout, or no proof soon after the page
//! reported a network change ([`WebCall::network_changed`]), counts as lost. A network change
//! while rejoining makes the next attempt due at once. When the page goes away (closed, reloaded),
//! the driver closes its sessions in the `pagehide` handler itself: the SFU frees the slot at
//! once instead of the others seeing "reconnecting" until the grace period ends.

mod meter;
mod session_watch;
mod video;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::future::Future;
use std::pin::{Pin, pin};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::future::{Either, select};
use js_sys::{Array, Float32Array, Function, Object, Promise, Reflect, Uint8Array};
use teamcraft_proto::{
  CallClientMessage, CallServerMessage, DeviceId, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE,
  ScreenContentMode, VideoCodec, VideoTrackKind,
};
use thiserror::Error;
use tracing::{debug, info, warn};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
  AudioContext, AudioContextOptions, AudioWorkletNode, AudioWorkletNodeOptions, Blob, BlobPropertyBag,
  HtmlCanvasElement, MediaDeviceInfo, MediaDeviceKind, MediaStream, MediaStreamAudioSourceNode,
  MediaStreamConstraints, MediaStreamTrack, MediaTrackConstraints, MessageEvent, ReadableStream,
  ReadableStreamDefaultReader, Url, WritableStreamDefaultWriter,
};
use web_time::Instant;

pub use self::meter::MicrophoneMeter;
use self::session_watch::{NetworkVerdict, PacketCount, PathVerdict, Reading, SessionWatch, may_hurry};
use self::video::{TrackSender, VideoView};

use crate::audio_frame::{
  EncodedAudioFrame, FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ, planned_loss_percent,
};
use crate::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource, sine_tone};
use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};
use crate::call_auth::CallKeyAuthenticator;
use crate::call_cues::{CallCue, CuePlayer, StatusCues};
use crate::call_engine::{
  AudioOrigin, CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
};
use crate::call_resilience::{
  Jitter, JoinTicket, JoinTicketError, ReconnectSchedule, ResilienceConfig, rejoin_attempt,
};
use crate::datagram::SimulcastLayer;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::rtp::AudioLevel;
use crate::video::EncodedVideoFrame;
use crate::web_api::{
  AudioData, AudioDecoder, AudioEncoder, EncodedAudioChunk, WORKLET_SOURCE, WebTransport,
  WebTransportBidirectionalStream, datagram_writable, dictionary, has_global,
};

/// Audio frame period.
const FRAME_INTERVAL_MS: i32 = 20;
/// Feedback and allocation period.
const TIMER_INTERVAL_MS: i32 = 100;
/// Upper bound of one signaling message.
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
/// At most this much audio waits in the playback worklet (latency bound).
const MAX_PLAYBACK_SAMPLES: u32 = 48_000 / 5;
/// Decoded audio kept per sender before the mixer takes it; more means the sender runs fast.
const MAX_DECODED_SAMPLES: usize = 5 * FRAME_SAMPLES_PER_CHANNEL;
/// Opus bitrate for voice.
const OPUS_BITRATE_BPS: u32 = 32_000;
/// Loss Opus plans in-band FEC for until the uplink is measured (as on native clients).
const OPUS_LOSS_FLOOR_PERCENT: u8 = 5;
/// The planned loss changes the encoder's configuration only in steps this big.
const OPUS_LOSS_STEP_PERCENT: u8 = 5;
/// What a page without `navigator.mediaDevices` lacks: browsers offer it only to HTTPS pages.
const SECURE_CONTEXT: &str = "camera and microphone access (the page needs HTTPS)";
/// Mixed frames above this amplitude count as audible in [`CallStats`].
const AUDIBLE_AMPLITUDE: f32 = 0.01;
/// The session's packet count is read every this many timer ticks (500 ms).
const STATS_EVERY_TICKS: u32 = 5;
/// How long a session whose signaling stream ended waits for its close reason.
const CLOSE_INFO_WAIT: Duration = Duration::from_millis(200);
/// WebTransport close code of a deliberate leave (as on native clients).
const LEAVE_CLOSE_CODE: u32 = 0;

/// A running call in the browser.
pub struct WebCall {
  inputs: mpsc::UnboundedSender<Input>,
  events: mpsc::UnboundedReceiver<CallEngineEvent>,
  finished: oneshot::Receiver<Result<CallStats, WebCallError>>,
}

/// How to reach the SFU and what to send and play.
#[derive(Debug, Clone)]
pub struct WebCallConfig {
  /// WebTransport URL from the join token response, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  /// Accept self-signed SFU certificates by their SHA-256 hashes (`serverCertificateHashes`,
  /// development and tests); empty uses the browser's trusted roots. For the first session: every
  /// rejoin uses its fresh ticket's ([`JoinTicket::certificate_hashes`]).
  pub certificate_hashes: Vec<[u8; 32]>,
  pub token: String,
  pub engine: CallEngineConfig,
  pub source: AudioSource,
  pub sink: AudioSink,
  pub maybe_trusted_roster: Option<Vec<DeviceId>>,
  /// Binds DAVE keys to device identities (e2ee.md §8); `None` only for development pages.
  pub maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  pub jitter: JitterBufferConfig,
  /// Loss detection and rejoin timing ([`ResilienceConfig::browser`]).
  pub resilience: ResilienceConfig,
  /// Fresh join tickets for rejoining; without one the first ticket is used again, which works
  /// only while it is valid (development pages).
  pub maybe_ticket_source: Option<Rc<dyn WebJoinTicketSource>>,
  /// Which call status sounds (reconnecting, reconnected, connection lost) play.
  pub status_cues: StatusCues,
  /// The microphone is muted from the first frame: nothing of it is sent before a later unmute.
  pub is_muted_at_start: bool,
  /// Where the page serves the AudioWorklet module (`call_web/teamcraft-call-worklet.js`). Pages
  /// whose Content-Security-Policy allows scripts only from their own origin need it; `None` loads
  /// the module from a blob URL.
  pub maybe_worklet_url: Option<String>,
}

/// Gets a fresh join ticket from `teamcraft-server` in the page.
pub trait WebJoinTicketSource {
  fn fetch(&self) -> LocalTicketFuture;
}

/// A join ticket on its way (on the page's event loop).
pub type LocalTicketFuture = Pin<Box<dyn Future<Output = Result<JoinTicket, JoinTicketError>>>>;

impl std::fmt::Debug for dyn WebJoinTicketSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("WebJoinTicketSource")
  }
}

/// The browser's microphones and speakers. Labels are empty until the page has microphone
/// permission (browser privacy rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserAudioDevices {
  pub inputs: Vec<AudioDeviceInfo>,
  pub outputs: Vec<AudioDeviceInfo>,
}

/// The browser's microphones, speakers and cameras. Labels are empty until the page has permission
/// for the kind (browser privacy rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserMediaDevices {
  pub inputs: Vec<AudioDeviceInfo>,
  pub outputs: Vec<AudioDeviceInfo>,
  /// Camera ids are `MediaDeviceInfo.deviceId`s, as [`capture_camera`] takes them.
  pub cameras: Vec<AudioDeviceInfo>,
}

/// What the page asked the browser for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaInput {
  Microphone,
  Camera,
  Screen,
}

/// Why the browser refused a microphone, camera or screen (the `DOMException` name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAccessReason {
  /// The user or a site setting refused (`NotAllowedError`, `SecurityError`). For the screen, the
  /// user closed the picker.
  Denied,
  /// No such device (`NotFoundError`, `OverconstrainedError`).
  NotFound,
  /// The device is there but can't be opened, usually because another app holds it
  /// (`NotReadableError`, `AbortError`).
  InUse,
  Other,
}

#[derive(Debug, Error)]
pub enum WebCallError {
  #[error("{context} failed: {message}")]
  Browser { context: &'static str, message: String },
  #[error("{input:?} unavailable ({reason:?}): {message}")]
  MediaAccess {
    input: MediaInput,
    reason: MediaAccessReason,
    message: String,
  },
  #[error("this browser lacks {0}")]
  Unsupported(&'static str),
  #[error(transparent)]
  Engine(#[from] CallEngineError),
  #[error(transparent)]
  Ticket(#[from] JoinTicketError),
  #[error("the connection to the call was lost and could not be restored in time")]
  ConnectionLost,
  #[error("the call ended")]
  Ended,
}

/// Everything the call task reacts to.
enum Input {
  /// A signaling message of the session with this generation; an older session's are dropped (a
  /// session taken for lost may still be alive and hear `connection_replaced` for this device).
  Signal {
    generation: u64,
    message: CallServerMessage,
  },
  /// The session with this generation ended (its signaling stream did).
  SessionEnded {
    generation: u64,
    end: SessionEnd,
  },
  /// What the browser reported about a session's received packets.
  PathPackets {
    reading: Reading,
    count: PacketCount,
  },
  /// The page saw the network come back or change, or woke up.
  NetworkChanged,
  /// A rejoin attempt opened a new session.
  Relinked {
    transport: Transport,
    ticket: JoinTicket,
    generation: u64,
  },
  /// Rejoining took longer than the SFU keeps the slot.
  ReconnectExpired,
  /// A datagram of the session with this generation.
  Datagram {
    generation: u64,
    bytes: Vec<u8>,
  },
  Captured(Vec<f32>),
  Encoded(Vec<u8>),
  Decoded {
    stream: RemoteStream,
    samples: Vec<f32>,
  },
  CodecFailed(String),
  FrameTick,
  TimerTick,
  VideoTick(VideoTrackKind),
  VideoEncoded {
    kind: VideoTrackKind,
    layer: u8,
    data: Vec<u8>,
    is_keyframe: bool,
    timestamp_us: f64,
  },
  VideoEnded(VideoTrackKind),
  /// The decoder of a watched track failed: a new one starts at the next keyframe.
  VideoDecodeFailed {
    stream_id: MediaStreamId,
    message: String,
  },
  Command(Command),
}

enum Command {
  Mute(bool),
  Deafen(bool),
  MoveHere,
  SwitchAudio {
    source: AudioSource,
    sink: AudioSink,
    done: oneshot::Sender<Result<(), WebCallError>>,
  },
  StartVideo {
    stream: MediaStream,
    // Boxed: the plan (with an effects pipeline) dwarfs the other commands.
    plan: Box<video::TrackPlan>,
    done: oneshot::Sender<Result<(), WebCallError>>,
  },
  StopVideo(VideoTrackKind),
  #[cfg(feature = "effects")]
  SetEffect(crate::effects::BackgroundEffect),
  WatchVideo {
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
  },
  UnwatchVideo(MediaStreamId),
  SetStatusCues(StatusCues),
  Leave,
}

/// How a session ended, as far as rejoining cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
  /// It broke, the SFU closed it for another reason, or the path went silent: rejoin at once.
  Lost,
  /// The SFU is shutting down (a restart, a deploy): rejoin after a random part of a short spread,
  /// so the restarted SFU doesn't get every client at once.
  GoingAway,
}

/// The sessions the call holds open: the current one, lost ones until the call is back (closing
/// one tells the SFU this device left; it keeps the slot only for lost connections) and one still
/// connecting. Shared with [`PageExit`], which closes them all when the page goes away.
#[derive(Clone, Default)]
struct OpenSessions(Rc<RefCell<Vec<WebTransport>>>);

/// Leaves the call from the page's `pagehide` handler, while the page can still send.
struct PageExit {
  window: web_sys::Window,
  listener: Closure<dyn FnMut()>,
}

/// The connection to the SFU.
struct Transport {
  transport: WebTransport,
  signaling: WritableStreamDefaultWriter,
  datagrams: WritableStreamDefaultWriter,
}

/// Web Audio graph: microphone capture (if any) and the playback worklet (if any).
struct WebAudio {
  context: AudioContext,
  maybe_capture: Option<Capture>,
  maybe_playback: Option<AudioWorkletNode>,
}

struct Capture {
  stream: MediaStream,
  _source: MediaStreamAudioSourceNode,
  node: AudioWorkletNode,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
}

/// Microphone or tone samples → 20 ms frames → WebCodecs Opus.
struct OpusEncoder {
  encoder: AudioEncoder,
  /// The loss the encoder's in-band FEC is planned for.
  planned_loss_percent: u8,
  pending: Vec<f32>,
  /// Levels of frames handed to the encoder whose output has not arrived yet (FIFO).
  levels: VecDeque<AudioLevel>,
  timestamp_us: f64,
  _output: Closure<dyn FnMut(EncodedAudioChunk)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// A remote device's microphone or shared screen sound.
type RemoteStream = (DeviceId, AudioOrigin);

/// One remote audio stream: jitter buffer, WebCodecs decoder and decoded samples waiting for the mix.
struct RemoteAudio {
  jitter: JitterBuffer<Vec<u8>>,
  decoder: AudioDecoder,
  decoded: VecDeque<f32>,
  timestamp_us: f64,
  _output: Closure<dyn FnMut(AudioData)>,
  _error: Closure<dyn FnMut(JsValue)>,
}

/// A `setInterval` timer, cleared on drop.
struct Interval {
  handle: i32,
  _callback: Closure<dyn FnMut()>,
}

/// Reassembles length-prefixed JSON signaling messages from stream chunks.
#[derive(Debug, Default)]
struct SignalingDecoder {
  buffer: Vec<u8>,
}

#[derive(Debug, Error, PartialEq, Eq)]
enum SignalingDecodeError {
  #[error("signaling message of {0} bytes is too large")]
  TooLarge(usize),
}

impl WebCall {
  /// Connects, joins and starts sending and playing. Call it from a user gesture (click) so the
  /// browser lets the page start audio.
  pub async fn join(config: WebCallConfig) -> Result<Self, WebCallError> {
    ensure_supported()?;
    let (inputs, input_rx) = mpsc::unbounded();
    let audio = WebAudio::open(
      &config.source,
      &config.sink,
      config.maybe_worklet_url.as_deref(),
      &inputs,
    )
    .await?;
    let sessions = OpenSessions::default();
    let transport =
      Transport::connect(&config.sfu_url, &config.certificate_hashes, &sessions, &inputs, 0).await?;
    let (event_tx, events) = mpsc::unbounded();
    let (finished_tx, finished) = oneshot::channel();
    let task_inputs = inputs.clone();
    spawn_local(async move {
      let channels = (task_inputs, input_rx, event_tx);
      let result = run_call(config, transport, sessions, audio, channels).await;
      // The app may have dropped the call without waiting for the result.
      let _ = finished_tx.send(result);
    });
    Ok(Self {
      inputs,
      events,
      finished,
    })
  }

  pub fn set_muted(&self, is_muted: bool) {
    // A closed channel means the call already ended; nothing to mute.
    let _ = self
      .inputs
      .unbounded_send(Input::Command(Command::Mute(is_muted)));
  }

  pub fn set_deafened(&self, is_deafened: bool) {
    let _ = self
      .inputs
      .unbounded_send(Input::Command(Command::Deafen(is_deafened)));
  }

  /// Takes the call over from the account's other devices.
  pub fn move_here(&self) {
    let _ = self.inputs.unbounded_send(Input::Command(Command::MoveHere));
  }

  /// Switches microphone and speakers (or test signals) mid-call. On error the previous ones keep
  /// running.
  pub async fn set_audio(&self, source: AudioSource, sink: AudioSink) -> Result<(), WebCallError> {
    let (done, done_rx) = oneshot::channel();
    self
      .inputs
      .unbounded_send(Input::Command(Command::SwitchAudio { source, sink, done }))
      .map_err(|_| WebCallError::Ended)?;
    done_rx.await.map_err(|_| WebCallError::Ended)?
  }

  /// Turns the camera on (three H.264 simulcast layers from 720p); `maybe_device` is a
  /// `MediaDeviceInfo.deviceId`. Call it from a user gesture, like `join`.
  pub async fn start_camera(&self, maybe_device: Option<&str>) -> Result<(), WebCallError> {
    let stream = video::capture_camera(maybe_device).await?;
    self.share_camera_stream(stream).await
  }

  /// Sends a camera stream the app opened itself ([`capture_camera`]), e.g. to show it as the
  /// self view too; replaces a running camera. Stopping the camera stops the stream's tracks.
  pub async fn share_camera_stream(&self, stream: MediaStream) -> Result<(), WebCallError> {
    self.start_video(stream, video::camera_plan()).await
  }

  /// [`WebCall::start_camera`] with background effects (segmentation and the WebGPU compositor);
  /// change the effect with [`WebCall::set_background_effect`].
  #[cfg(feature = "effects")]
  pub async fn start_camera_with_effects(
    &self,
    maybe_device: Option<&str>,
    effects: crate::effects::EffectsPipeline,
  ) -> Result<(), WebCallError> {
    let stream = video::capture_camera(maybe_device).await?;
    let plan = video::TrackPlan {
      maybe_effects: Some(effects),
      ..video::camera_plan()
    };
    self.start_video(stream, plan).await
  }

  #[cfg(feature = "effects")]
  pub fn set_background_effect(&self, effect: crate::effects::BackgroundEffect) {
    let _ = self.command(Command::SetEffect(effect));
  }

  pub fn stop_camera(&self) {
    let _ = self.command(Command::StopVideo(VideoTrackKind::Camera));
  }

  /// Asks the user for a screen or window and shares it (one H.264 layer, no simulcast). Call it
  /// from a user gesture, like `join`.
  pub async fn start_screen_share(&self, mode: ScreenContentMode) -> Result<(), WebCallError> {
    let stream = video::capture_screen(mode).await?;
    self.start_video(stream, video::screen_plan(mode)).await
  }

  /// Shares any video stream as the screen share (e.g. a canvas' `captureStream()`), replacing a
  /// running one.
  pub async fn share_video_stream(&self, stream: MediaStream) -> Result<(), WebCallError> {
    self
      .start_video(stream, video::screen_plan(ScreenContentMode::default()))
      .await
  }

  pub fn stop_screen_share(&self) {
    let _ = self.command(Command::StopVideo(VideoTrackKind::Screen));
  }

  async fn start_video(&self, stream: MediaStream, plan: video::TrackPlan) -> Result<(), WebCallError> {
    let (done, done_rx) = oneshot::channel();
    self.command(Command::StartVideo {
      stream,
      plan: Box::new(plan),
      done,
    })?;
    done_rx.await.map_err(|_| WebCallError::Ended)?
  }

  /// Receives another device's track (from [`CallEngineEvent::VideoPublished`]) and draws it into
  /// `canvas`, which is resized to the video.
  pub fn watch_video(&self, stream_id: MediaStreamId, canvas: HtmlCanvasElement) {
    let _ = self.command(Command::WatchVideo { stream_id, canvas });
  }

  pub fn unwatch_video(&self, stream_id: MediaStreamId) {
    let _ = self.command(Command::UnwatchVideo(stream_id));
  }

  /// Which call status sounds play from now on.
  pub fn set_status_cues(&self, status_cues: StatusCues) {
    // The call already ended if this fails.
    let _ = self.command(Command::SetStatusCues(status_cues));
  }

  fn command(&self, command: Command) -> Result<(), WebCallError> {
    self
      .inputs
      .unbounded_send(Input::Command(command))
      .map_err(|_| WebCallError::Ended)
  }

  /// The page saw the network come back or change, became visible again or resumed (the core's
  /// network-change signal): the session must prove it is still alive, a rejoin under way tries
  /// again at once.
  pub fn network_changed(&self) {
    // The call already ended if this fails.
    let _ = self.inputs.unbounded_send(Input::NetworkChanged);
  }

  /// The next event, or `None` once the call ended.
  pub async fn next_event(&mut self) -> Option<CallEngineEvent> {
    self.events.next().await
  }

  /// Leaves the call and returns its statistics.
  pub async fn leave(mut self) -> Result<CallStats, WebCallError> {
    let _ = self.inputs.unbounded_send(Input::Command(Command::Leave));
    (&mut self.finished).await.map_err(|_| WebCallError::Ended)?
  }
}

/// A call dropped without [`WebCall::leave`] still ends: the task holds its own input sender, so it
/// would otherwise keep the microphone, camera and connection open.
impl Drop for WebCall {
  fn drop(&mut self) {
    // After `leave` the task is gone and the channel closed; nothing left to end.
    let _ = self.inputs.unbounded_send(Input::Command(Command::Leave));
  }
}

/// Asks for the camera (`getUserMedia`); `maybe_device` is a `MediaDeviceInfo.deviceId`. Call it
/// from a user gesture.
pub async fn capture_camera(maybe_device: Option<&str>) -> Result<MediaStream, WebCallError> {
  video::capture_camera(maybe_device).await
}

/// Asks for the microphone (`getUserMedia`) with the browser's echo cancellation, noise
/// suppression and gain control, as the call sends it.
async fn capture_microphone(choice: &AudioDeviceChoice) -> Result<MediaStream, WebCallError> {
  let track_constraints = MediaTrackConstraints::new();
  track_constraints.set_echo_cancellation(&JsValue::TRUE);
  track_constraints.set_noise_suppression(&JsValue::TRUE);
  track_constraints.set_auto_gain_control(&JsValue::TRUE);
  track_constraints.set_channel_count(&JsValue::from(1));
  if let AudioDeviceChoice::Device(id) = choice {
    let exact = dictionary(&[("exact", JsValue::from_str(id.as_str()))]).map_err(browser("getUserMedia"))?;
    track_constraints.set_device_id(&exact);
  }
  let constraints = MediaStreamConstraints::new();
  constraints.set_audio(&track_constraints);
  let promise = window()?
    .navigator()
    .media_devices()
    .map_err(|_| WebCallError::Unsupported(SECURE_CONTEXT))?
    .get_user_media_with_constraints(&constraints)
    .map_err(media_access(MediaInput::Microphone))?;
  Ok(
    JsFuture::from(promise)
      .await
      .map_err(media_access(MediaInput::Microphone))?
      .unchecked_into(),
  )
}

/// Whether this browser has what calls need (WebTransport, WebCodecs audio, AudioWorklet); the
/// error names the first thing missing.
pub fn check_support() -> Result<(), WebCallError> {
  ensure_supported()
}

/// Lists microphones and speakers (`enumerateDevices`), defaults first.
pub async fn list_audio_devices() -> Result<BrowserAudioDevices, WebCallError> {
  let devices = list_media_devices().await?;
  Ok(BrowserAudioDevices {
    inputs: devices.inputs,
    outputs: devices.outputs,
  })
}

/// Lists microphones, speakers and cameras (`enumerateDevices`), defaults first.
pub async fn list_media_devices() -> Result<BrowserMediaDevices, WebCallError> {
  let media_devices = window()?
    .navigator()
    .media_devices()
    .map_err(browser("mediaDevices"))?;
  let promise = media_devices
    .enumerate_devices()
    .map_err(browser("enumerateDevices"))?;
  let list: Array = JsFuture::from(promise)
    .await
    .map_err(browser("enumerateDevices"))?
    .unchecked_into();
  let mut devices = BrowserMediaDevices::default();
  for entry in list.iter() {
    let info: MediaDeviceInfo = entry.unchecked_into();
    let device = AudioDeviceInfo {
      // Chrome lists the default device twice: once as "default", once by its own id.
      is_default: info.device_id() == "default",
      id: AudioDeviceId::new(info.device_id()),
      name: info.label(),
    };
    match info.kind() {
      MediaDeviceKind::Audioinput => devices.inputs.push(device),
      MediaDeviceKind::Audiooutput => devices.outputs.push(device),
      MediaDeviceKind::Videoinput => devices.cameras.push(device),
      _ => {},
    }
  }
  devices.inputs.sort_by_key(|device| !device.is_default);
  devices.outputs.sort_by_key(|device| !device.is_default);
  Ok(devices)
}

/// The call task's inputs (sender and receiver) and its events.
type CallChannels = (
  mpsc::UnboundedSender<Input>,
  mpsc::UnboundedReceiver<Input>,
  mpsc::UnboundedSender<CallEngineEvent>,
);

/// The call task: drives the engine from everything that arrives on `inputs`.
async fn run_call(
  config: WebCallConfig,
  transport: Transport,
  sessions: OpenSessions,
  mut audio: WebAudio,
  (inputs, mut input_rx, events): CallChannels,
) -> Result<CallStats, WebCallError> {
  let mut engine = CallEngine::new(config.engine.clone(), Instant::now());
  if let Some(roster) = &config.maybe_trusted_roster {
    engine.set_trusted_roster(roster.iter().copied());
  }
  if let Some(authenticator) = &config.maybe_authenticator {
    engine.set_authenticator(Arc::clone(authenticator));
  }
  if config.is_muted_at_start {
    engine.set_muted(true);
  }
  let _page_exit = PageExit::watch(&sessions, &inputs);
  let mut state = CallState {
    maybe_transport: Some(transport),
    lost_transports: Vec::new(),
    sessions,
    watch: SessionWatch::new(Instant::now(), config.resilience),
    maybe_lost_at: None,
    maybe_hurry: None,
    timer_ticks: 0,
    ticket: JoinTicket {
      sfu_url: config.sfu_url.clone(),
      token: config.token.clone(),
      certificate_hashes: config.certificate_hashes.clone(),
    },
    resilience: config.resilience,
    maybe_ticket_source: config.maybe_ticket_source.clone(),
    cues: CuePlayer::new(config.status_cues),
    is_ending_lost: false,
    inputs: &inputs,
    events: &events,
    jitter: config.jitter,
    encoder: OpusEncoder::new(&inputs)?,
    remotes: BTreeMap::new(),
    source: config.source.clone(),
    tone_position: 0,
    is_muted: config.is_muted_at_start,
    is_deafened: false,
    stats: CallStats::default(),
    own_device: config.engine.device_id,
    senders: Vec::new(),
    views: HashMap::new(),
  };
  let _frame_timer = Interval::start(FRAME_INTERVAL_MS, &inputs, || Input::FrameTick)?;
  let _feedback_timer = Interval::start(TIMER_INTERVAL_MS, &inputs, || Input::TimerTick)?;
  engine.join(config.token.clone());
  state.flush(&mut engine)?;

  let result = loop {
    let Some(input) = input_rx.next().await else {
      break Ok(());
    };
    match state.handle(input, &mut engine, &mut audio).await {
      Ok(Flow::Continue) => {},
      Ok(Flow::Leave) => break Ok(()),
      Err(error) => break Err(error),
    }
    if let Err(error) = state.flush(&mut engine) {
      break Err(error);
    }
    // Refused, ended by the host or replaced by another connection: nothing to rejoin.
    if engine.is_over() {
      break Ok(());
    }
    if engine.is_in_call() {
      state.maybe_lost_at = None;
      state.close_lost_sessions();
    }
  };

  audio.close();
  // The camera and the shared screen stop with the call (the browser's indicators go off).
  for sender in std::mem::take(&mut state.senders) {
    sender.stop();
  }
  // Leaves: the SFU frees the slot at once.
  state.sessions.close_all();
  let mut stats = state.stats;
  stats.maybe_privacy_code = engine.privacy_code();
  info!(?stats, "call ended");
  result.map(|()| stats)
}

enum Flow {
  Continue,
  Leave,
}

/// What the call task owns besides the engine and the audio graph.
struct CallState<'a> {
  /// The current session; `None` while rejoining.
  maybe_transport: Option<Transport>,
  /// Lost sessions, kept open until the call is back on a new one (see [`OpenSessions`]).
  lost_transports: Vec<Transport>,
  sessions: OpenSessions,
  /// Which session's news counts, and whether the current one's path is alive.
  watch: SessionWatch,
  /// When the connection was lost, while rejoining.
  maybe_lost_at: Option<Instant>,
  /// Asks the rejoin task to try again early, while rejoining.
  maybe_hurry: Option<mpsc::UnboundedSender<()>>,
  timer_ticks: u32,
  /// The ticket of the current (or last) session.
  ticket: JoinTicket,
  resilience: ResilienceConfig,
  maybe_ticket_source: Option<Rc<dyn WebJoinTicketSource>>,
  /// Status sounds, mixed into the playback (never sent).
  cues: CuePlayer,
  /// Rejoining failed; the call ends once the "connection lost" sound has played.
  is_ending_lost: bool,
  inputs: &'a mpsc::UnboundedSender<Input>,
  events: &'a mpsc::UnboundedSender<CallEngineEvent>,
  jitter: JitterBufferConfig,
  encoder: OpusEncoder,
  remotes: BTreeMap<RemoteStream, RemoteAudio>,
  source: AudioSource,
  tone_position: u64,
  is_muted: bool,
  is_deafened: bool,
  stats: CallStats,
  own_device: DeviceId,
  senders: Vec<TrackSender>,
  views: HashMap<MediaStreamId, VideoView>,
}

impl CallState<'_> {
  async fn handle(
    &mut self,
    input: Input,
    engine: &mut CallEngine,
    audio: &mut WebAudio,
  ) -> Result<Flow, WebCallError> {
    match input {
      Input::Signal { generation, message } => {
        if self.watch.is_current(generation) {
          let now = Instant::now();
          if let CallServerMessage::Pong { id } = message {
            self.watch.on_pong(id, now);
          } else {
            self.watch.on_heard(now);
            engine.handle_signal(message, now)?;
          }
        }
      },
      Input::SessionEnded { generation, end } => {
        if self.watch.is_current(generation) {
          return Ok(self.on_session_lost(engine, end));
        }
      },
      Input::PathPackets { reading, count } => return Ok(self.on_path_packets(engine, reading, count)),
      Input::NetworkChanged => self.on_network_changed()?,
      Input::Relinked {
        transport,
        ticket,
        generation,
      } => {
        info!("connected again, rejoining the call");
        let now = Instant::now();
        // Tracked in `OpenSessions` since it started connecting.
        self.maybe_transport = Some(transport);
        self.watch.relinked(generation, now);
        self.maybe_hurry = None;
        engine.rejoin(ticket.token.clone(), now);
        self.ticket = ticket;
      },
      Input::ReconnectExpired => {
        self.is_ending_lost = true;
        self.cues.play(CallCue::ConnectionLost);
      },
      Input::Datagram { generation, bytes } => {
        if self.watch.is_current(generation) {
          let now = Instant::now();
          self.watch.on_heard(now);
          engine.handle_datagram(&bytes, now);
        }
      },
      Input::Captured(samples) => {
        if !self.is_muted {
          self.encoder.push(&samples)?;
        }
      },
      Input::Encoded(opus) => {
        // One output per input frame; a stray extra output (encoder flush) counts as silence.
        let level = self.encoder.levels.pop_front().unwrap_or(AudioLevel::SILENCE);
        engine.send_audio(&EncodedAudioFrame { opus, level }, Instant::now());
        self.stats.frames_sent += 1;
      },
      Input::Decoded { stream, samples } => {
        if let Some(remote) = self.remotes.get_mut(&stream) {
          remote.decoded.extend(samples);
          let excess = remote.decoded.len().saturating_sub(MAX_DECODED_SAMPLES);
          remote.decoded.drain(..excess);
        }
      },
      Input::CodecFailed(message) => warn!(%message, "audio codec error"),
      Input::FrameTick => {
        self.on_frame_tick(audio)?;
        if self.is_ending_lost && !self.cues.is_playing(CallCue::ConnectionLost) {
          return Err(WebCallError::ConnectionLost);
        }
      },
      Input::TimerTick => {
        engine.on_timer(Instant::now());
        self.timer_ticks = self.timer_ticks.wrapping_add(1);
        if self.timer_ticks.is_multiple_of(STATS_EVERY_TICKS) {
          self.request_path_stats();
        }
        let tick = self.watch.on_tick(Instant::now());
        if let Some(id) = tick.maybe_ping {
          self.send_ping(id)?;
        }
        return Ok(self.on_path_verdict(engine, tick.verdict));
      },
      Input::VideoTick(kind) => {
        if let Some(sender) = self.senders.iter_mut().find(|sender| sender.kind == kind) {
          sender.capture().await?;
        }
      },
      Input::VideoEncoded {
        kind,
        layer,
        data,
        is_keyframe,
        timestamp_us,
      } => self.send_video_frame(engine, &kind, layer, data, is_keyframe, timestamp_us),
      Input::VideoEnded(kind) => self.stop_video(engine, &kind),
      Input::VideoDecodeFailed { stream_id, message } => {
        warn!(%message, stream_id = stream_id.0, "video decoder failed; waiting for a keyframe");
        if let Some(view) = self.views.get_mut(&stream_id) {
          view.reset_decoder();
          engine.request_keyframe(stream_id, Instant::now());
        }
      },
      Input::Command(command) => return self.on_command(command, engine, audio).await,
    }
    Ok(Flow::Continue)
  }

  fn flush(&mut self, engine: &mut CallEngine) -> Result<(), WebCallError> {
    while let Some(output) = engine.poll_output() {
      match output {
        // While rejoining there is no session; the engine renews what matters when it is back.
        EngineOutput::Signal(message) => {
          if let Some(transport) = &self.maybe_transport {
            transport.send_signal(&message)?;
          }
        },
        EngineOutput::Datagram(bytes) => {
          if let Some(transport) = &self.maybe_transport {
            transport.send_datagram(&bytes);
          }
        },
        EngineOutput::Event(CallEngineEvent::AudioFrame {
          sender,
          origin,
          sequence_number,
          opus,
        }) => {
          self.stats.frames_received += 1;
          let remote = match self.remotes.entry((sender, origin)) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
              entry.insert(RemoteAudio::new((sender, origin), self.jitter, self.inputs)?)
            },
          };
          remote.jitter.push(sequence_number, opus);
        },
        EngineOutput::Event(CallEngineEvent::VideoFrame(frame)) => {
          if let Some(view) = self.views.get_mut(&frame.stream_id) {
            view.decode(&frame)?;
          }
        },
        EngineOutput::Event(event) => {
          match &event {
            CallEngineEvent::ParticipantLeft(device_id) => {
              self.remotes.retain(|(sender, _), _| sender != device_id);
            },
            CallEngineEvent::ScreenAudioChanged {
              device_id,
              is_shared: false,
            } => {
              self.remotes.remove(&(*device_id, AudioOrigin::Screen));
            },
            CallEngineEvent::VideoPublished(track) if track.device_id == self.own_device => {
              if let Some(sender) = self
                .senders
                .iter_mut()
                .find(|sender| sender.kind == track.kind && sender.maybe_stream_id.is_none())
              {
                sender.maybe_stream_id = Some(track.stream_id);
              }
            },
            CallEngineEvent::VideoUnpublished(stream_id) => {
              self.views.remove(stream_id);
            },
            CallEngineEvent::KeyframeRequested { stream_id, layer } => {
              if let Some(sender) = self.sender_of(*stream_id) {
                sender.request_keyframe(*layer);
              }
            },
            CallEngineEvent::LayersNeeded { stream_id, layers } => {
              if let Some(sender) = self.sender_of(*stream_id) {
                sender.set_needed_layers(layers);
              }
            },
            CallEngineEvent::UplinkEstimate { loss_percent, .. } => {
              self.encoder.set_measured_loss_percent(*loss_percent)?;
            },
            _ => {},
          }
          self.cues.on_event(&event);
          // The app may have stopped listening; the call goes on.
          let _ = self.events.unbounded_send(event);
        },
      }
    }
    Ok(())
  }

  /// The session ended or went silent. Unless the call is over, the device rejoins on a new
  /// session; the old one stays open until then.
  fn on_session_lost(&mut self, engine: &mut CallEngine, end: SessionEnd) -> Flow {
    if engine.is_over() {
      return Flow::Leave;
    }
    match end {
      SessionEnd::Lost => warn!("lost the connection to the call, rejoining"),
      SessionEnd::GoingAway => info!("the SFU is going away, rejoining"),
    }
    if let Some(transport) = self.maybe_transport.take() {
      self.lost_transports.push(transport);
    }
    engine.connection_lost();
    let now = Instant::now();
    let lost_at = *self.maybe_lost_at.get_or_insert(now);
    let grace = engine
      .maybe_reconnect_grace()
      .unwrap_or(self.resilience.default_reconnect_grace);
    // From here on, news of the lost session is stale.
    let generation = self.watch.lose();
    let schedule = reconnect_schedule(lost_at, grace, end, now, &self.resilience, Jitter::random());
    let (hurry, hurry_rx) = mpsc::unbounded();
    self.maybe_hurry = Some(hurry);
    self.spawn_reconnect(schedule, hurry_rx, generation);
    Flow::Continue
  }

  /// A packet count arrived: a path silent for too long, or after a network change, is lost.
  fn on_path_packets(&mut self, engine: &mut CallEngine, reading: Reading, count: PacketCount) -> Flow {
    let verdict = self.watch.on_reading(reading, count, Instant::now());
    self.on_path_verdict(engine, verdict)
  }

  /// Acts on what a packet count or a timer tick said about the session's path.
  fn on_path_verdict(&mut self, engine: &mut CallEngine, verdict: PathVerdict) -> Flow {
    match verdict {
      PathVerdict::Unchanged => Flow::Continue,
      PathVerdict::Unwatchable => {
        debug!("the browser reports no packet counts: the page's own traffic and pings watch the session");
        Flow::Continue
      },
      PathVerdict::Unanswered => {
        debug!(
          "the SFU doesn't answer pings: only packet counts, where the browser has them, watch the session"
        );
        Flow::Continue
      },
      PathVerdict::Lost => {
        info!("nothing arrived on the call's session for too long");
        self.on_session_lost(engine, SessionEnd::Lost)
      },
    }
  }

  /// The page reported a network change: the session has to show it still works, a rejoin under
  /// way may try again early.
  fn on_network_changed(&mut self) -> Result<(), WebCallError> {
    match self.watch.on_network_changed(Instant::now()) {
      NetworkVerdict::Check {
        maybe_reading,
        maybe_ping,
      } => {
        debug!("network changed, checking the call's session");
        if let Some(reading) = maybe_reading {
          self.read_packets(reading);
        }
        if let Some(id) = maybe_ping {
          self.send_ping(id)?;
        }
      },
      NetworkVerdict::HurryRejoin => {
        if let Some(hurry) = &self.maybe_hurry {
          // The rejoin task is gone once it gave up; nothing to hurry then.
          let _ = hurry.unbounded_send(());
        }
      },
      NetworkVerdict::Nothing => {},
    }
    Ok(())
  }

  /// Asks the SFU to answer on the current session (its answer arrives as a signal).
  fn send_ping(&self, id: u64) -> Result<(), WebCallError> {
    match &self.maybe_transport {
      Some(transport) => transport.send_signal(&CallClientMessage::Ping { id }),
      None => Ok(()),
    }
  }

  /// Asks the browser for the current session's packet count, unless a reading is on its way.
  fn request_path_stats(&mut self) {
    if let Some(reading) = self.watch.next_reading() {
      self.read_packets(reading);
    }
  }

  /// The answer arrives as [`Input::PathPackets`].
  fn read_packets(&self, reading: Reading) {
    let Some(transport) = &self.maybe_transport else {
      return;
    };
    let session = transport.transport.clone();
    let inputs = self.inputs.clone();
    spawn_local(async move {
      let count = received_packets(&session).await;
      // The call ended meanwhile if nobody reads the inputs.
      let _ = inputs.unbounded_send(Input::PathPackets { reading, count });
    });
  }

  /// Back in the call on a new session: the lost ones can go.
  fn close_lost_sessions(&mut self) {
    for transport in self.lost_transports.drain(..) {
      self.sessions.close(&transport.transport);
    }
  }

  /// Tries to open a new session until one is up or the grace period is over.
  fn spawn_reconnect(
    &self,
    mut schedule: ReconnectSchedule,
    mut hurry: mpsc::UnboundedReceiver<()>,
    generation: u64,
  ) {
    let inputs = self.inputs.clone();
    let sessions = self.sessions.clone();
    let maybe_source = self.maybe_ticket_source.clone();
    let fallback = self.ticket.clone();
    let resilience = self.resilience;
    spawn_local(async move {
      let mut maybe_last_failure_at = None;
      // The call ended meanwhile when nobody reads the inputs any more.
      while !inputs.is_closed() {
        let now = Instant::now();
        if schedule.has_expired(now) {
          let _ = inputs.unbounded_send(Input::ReconnectExpired);
          return;
        }
        let wait = schedule.next_attempt_at().saturating_duration_since(now);
        match select(pin!(sleep(wait)), hurry.next()).await {
          Either::Left(_) => {},
          // The network came back: no need to wait out the whole back-off.
          Either::Right((Some(()), _)) => {
            let now = Instant::now();
            if may_hurry(&schedule, maybe_last_failure_at, now, &resilience) {
              schedule.hurry(now);
            }
            continue;
          },
          // The call task is gone.
          Either::Right((None, _)) => return,
        }
        let maybe_fetch = maybe_source.as_ref().map(|source| source.fetch());
        let attempt = rejoin_attempt(maybe_fetch, fallback.clone(), |ticket: JoinTicket| {
          let (inputs, sessions) = (inputs.clone(), sessions.clone());
          async move {
            let hashes = &ticket.certificate_hashes;
            Transport::connect(&ticket.sfu_url, hashes, &sessions, &inputs, generation).await
          }
        });
        match attempt.await {
          Ok((transport, ticket)) => {
            let session = transport.transport.clone();
            let relinked = Input::Relinked {
              transport,
              ticket,
              generation,
            };
            if inputs.unbounded_send(relinked).is_err() {
              // The call ended while this session connected: it must not linger.
              sessions.close(&session);
            }
            return;
          },
          Err(error) => {
            debug!(%error, "rejoin attempt failed");
            let now = Instant::now();
            maybe_last_failure_at = Some(now);
            schedule.on_failed_attempt(now, &resilience);
          },
        }
      }
    });
  }

  /// Every 20 ms: the tone source's next frame, decoding due packets and playing the mix.
  fn on_frame_tick(&mut self, audio: &WebAudio) -> Result<(), WebCallError> {
    if let AudioSource::Tone {
      frequency_hz,
      amplitude,
    } = self.source
    {
      let samples = sine_tone(
        frequency_hz,
        amplitude,
        self.tone_position,
        FRAME_SAMPLES_PER_CHANNEL,
      );
      self.tone_position += FRAME_SAMPLES_PER_CHANNEL as u64;
      if !self.is_muted {
        self.encoder.push(&samples)?;
      }
    }
    let mut mix = vec![0.0f32; FRAME_SAMPLES_PER_CHANNEL];
    for remote in self.remotes.values_mut() {
      remote.decode_due()?;
      let available = remote.decoded.len().min(FRAME_SAMPLES_PER_CHANNEL);
      for (slot, sample) in mix.iter_mut().zip(remote.decoded.drain(..available)) {
        *slot += sample;
      }
    }
    if self.is_deafened {
      mix.fill(0.0);
    } else if mix.iter().any(|sample| sample.abs() > AUDIBLE_AMPLITUDE) {
      self.stats.audible_frames_played += 1;
    }
    // Status sounds play even when deafened: they are status, not call audio.
    for (sample, cue) in mix
      .iter_mut()
      .zip(self.cues.next_samples(FRAME_SAMPLES_PER_CHANNEL))
    {
      *sample = (*sample + cue).clamp(-1.0, 1.0);
    }
    audio.play(&mix)
  }

  async fn on_command(
    &mut self,
    command: Command,
    engine: &mut CallEngine,
    audio: &mut WebAudio,
  ) -> Result<Flow, WebCallError> {
    match command {
      Command::Mute(is_muted) => {
        engine.set_muted(is_muted);
        self.is_muted = is_muted;
        self.encoder.pending.clear();
      },
      Command::MoveHere => engine.move_here(),
      Command::Deafen(is_deafened) => {
        engine.set_deafened(is_deafened);
        self.is_deafened = is_deafened;
      },
      Command::SwitchAudio { source, sink, done } => {
        // Inputs queue up while the browser opens the new devices (or asks for permission).
        let result = audio.switch(&source, &sink, self.inputs).await;
        match &result {
          Ok(()) => self.source = source,
          Err(error) => warn!(%error, "switching audio devices failed"),
        }
        // The caller may have given up waiting.
        let _ = done.send(result);
      },
      Command::StartVideo { stream, plan, done } => {
        let result = self.start_video(engine, stream, *plan).await;
        let _ = done.send(result);
      },
      Command::StopVideo(kind) => self.stop_video(engine, &kind),
      #[cfg(feature = "effects")]
      Command::SetEffect(effect) => {
        for sender in &mut self.senders {
          sender.set_effect(effect.clone());
        }
      },
      Command::WatchVideo { stream_id, canvas } => {
        let view = VideoView::new(stream_id, canvas, self.inputs);
        self.views.insert(stream_id, view);
        engine.subscribe_video(stream_id, Some(0));
      },
      Command::SetStatusCues(status_cues) => self.cues.set_setting(status_cues),
      Command::UnwatchVideo(stream_id) => {
        if self.views.remove(&stream_id).is_some() {
          engine.subscribe_video(stream_id, None);
        }
      },
      Command::Leave => return Ok(Flow::Leave),
    }
    Ok(Flow::Continue)
  }
}

impl CallState<'_> {
  /// Starts capturing and encoding a track, replacing one of the same kind.
  async fn start_video(
    &mut self,
    engine: &mut CallEngine,
    stream: MediaStream,
    plan: video::TrackPlan,
  ) -> Result<(), WebCallError> {
    let kind = plan.kind.clone();
    self.stop_video(engine, &kind);
    let sender = TrackSender::start(stream, plan, self.inputs).await?;
    engine.publish_video(kind, VideoCodec::H264, sender.layers());
    self.senders.push(sender);
    Ok(())
  }

  fn stop_video(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind) {
    let (stopped, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.senders)
      .into_iter()
      .partition(|sender| sender.kind == *kind);
    self.senders = kept;
    for sender in stopped {
      if let Some(stream_id) = sender.maybe_stream_id {
        engine.unpublish_video(stream_id);
      }
      sender.stop();
    }
  }

  fn sender_of(&mut self, stream_id: MediaStreamId) -> Option<&mut TrackSender> {
    self
      .senders
      .iter_mut()
      .find(|sender| sender.maybe_stream_id == Some(stream_id))
  }

  fn send_video_frame(
    &mut self,
    engine: &mut CallEngine,
    kind: &VideoTrackKind,
    layer: u8,
    data: Vec<u8>,
    is_keyframe: bool,
    timestamp_us: f64,
  ) {
    let Some(stream_id) = self
      .senders
      .iter()
      .find(|sender| sender.kind == *kind)
      .and_then(|sender| sender.maybe_stream_id)
    else {
      return;
    };
    let Ok(layer) = SimulcastLayer::new(layer) else {
      return;
    };
    let frame = EncodedVideoFrame {
      codec: VideoCodec::H264,
      layer,
      // Encoder timestamps start at 0 and stay far below u64::MAX microseconds.
      timestamp: EncodedVideoFrame::rtp_timestamp(timestamp_us.max(0.0) as u64),
      is_keyframe,
      data,
    };
    engine.send_video(stream_id, &frame, Instant::now());
  }
}

impl Transport {
  /// Opens a session, tracked in `sessions` from the start (the page may go away while it
  /// connects); a session that fails to open is closed again. Its signals and datagrams arrive
  /// tagged with `generation`, its end as [`Input::SessionEnded`].
  async fn connect(
    sfu_url: &str,
    certificate_hashes: &[[u8; 32]],
    sessions: &OpenSessions,
    inputs: &mpsc::UnboundedSender<Input>,
    generation: u64,
  ) -> Result<Self, WebCallError> {
    let options = Object::new();
    if !certificate_hashes.is_empty() {
      let entries = Array::new();
      for hash in certificate_hashes {
        let entry = dictionary(&[
          ("algorithm", JsValue::from_str("sha-256")),
          ("value", Uint8Array::from(&hash[..]).into()),
        ])
        .map_err(browser("WebTransport options"))?;
        entries.push(&entry);
      }
      Reflect::set(&options, &JsValue::from_str("serverCertificateHashes"), &entries)
        .map_err(browser("WebTransport options"))?;
    }
    let transport = WebTransport::new(sfu_url, &options).map_err(browser("WebTransport"))?;
    sessions.track(&transport);
    let opened = Self::open(transport.clone(), inputs, generation).await;
    if opened.is_err() {
      sessions.close(&transport);
    }
    opened
  }

  async fn open(
    transport: WebTransport,
    inputs: &mpsc::UnboundedSender<Input>,
    generation: u64,
  ) -> Result<Self, WebCallError> {
    JsFuture::from(transport.ready())
      .await
      .map_err(browser("WebTransport connect"))?;
    let stream: WebTransportBidirectionalStream = JsFuture::from(transport.create_bidirectional_stream())
      .await
      .map_err(browser("signaling stream"))?
      .unchecked_into();
    let signaling = stream
      .writable()
      .get_writer()
      .map_err(browser("signaling stream"))?;
    let datagram_stream = transport.datagrams();
    let datagrams = datagram_writable(&datagram_stream)
      .and_then(|stream| stream.get_writer())
      .map_err(browser("datagram stream"))?;
    spawn_local(read_signaling(
      stream.readable(),
      transport.closed(),
      inputs.clone(),
      generation,
    ));
    spawn_local(read_datagrams(
      datagram_stream.readable(),
      inputs.clone(),
      generation,
    ));
    Ok(Self {
      transport,
      signaling,
      datagrams,
    })
  }

  fn send_signal(&self, message: &CallClientMessage) -> Result<(), WebCallError> {
    let body = serde_json::to_vec(message).map_err(|error| WebCallError::Browser {
      context: "encoding a signaling message",
      message: error.to_string(),
    })?;
    let length = u32::try_from(body.len()).map_err(|_| WebCallError::Browser {
      context: "encoding a signaling message",
      message: "too large".into(),
    })?;
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend_from_slice(&body);
    // Writes queue in order; a failed write surfaces as the stream closing.
    let _ = self
      .signaling
      .write_with_chunk(&Uint8Array::from(framed.as_slice()));
    Ok(())
  }

  fn send_datagram(&self, bytes: &[u8]) {
    // Unreliable by design: the browser drops datagrams when its queue is full.
    let _ = self.datagrams.write_with_chunk(&Uint8Array::from(bytes));
  }
}

impl OpenSessions {
  fn track(&self, session: &WebTransport) {
    self.0.borrow_mut().push(session.clone());
  }

  /// Closes one session as a leave.
  fn close(&self, session: &WebTransport) {
    self.0.borrow_mut().retain(|open| open != session);
    close_session(session);
  }

  fn close_all(&self) {
    for session in self.0.borrow_mut().drain(..) {
      close_session(&session);
    }
  }
}

impl PageExit {
  /// `None` without a window to listen on: the session then ends without a leave on page close.
  fn watch(sessions: &OpenSessions, inputs: &mpsc::UnboundedSender<Input>) -> Option<Self> {
    let window = web_sys::window()?;
    let sessions = sessions.clone();
    let inputs = inputs.clone();
    let listener = Closure::<dyn FnMut()>::new(move || {
      info!("the page is going away, leaving the call");
      // Closed here: the page may get no other chance to run.
      sessions.close_all();
      // The call already ended if nobody reads the inputs.
      let _ = inputs.unbounded_send(Input::Command(Command::Leave));
    });
    window
      .add_event_listener_with_callback("pagehide", listener.as_ref().unchecked_ref())
      .inspect_err(|error| debug!(error = %js_message(error), "cannot watch the page closing"))
      .ok()?;
    Some(Self { window, listener })
  }
}

impl Drop for PageExit {
  fn drop(&mut self) {
    // A listener that was never added or is gone already: nothing to remove.
    let _ = self
      .window
      .remove_event_listener_with_callback("pagehide", self.listener.as_ref().unchecked_ref());
  }
}

/// Closes a session as a leave; a closed one stays closed.
fn close_session(session: &WebTransport) {
  let info = dictionary(&[
    ("closeCode", JsValue::from(LEAVE_CLOSE_CODE)),
    ("reason", JsValue::from_str("leave")),
  ]);
  let result = info.and_then(|info| session.close(&info));
  if let Err(error) = result {
    debug!(error = %js_message(&error), "closing a call session failed");
  }
}

/// The packets the session received so far (`getStats().packetsReceived`).
async fn received_packets(session: &WebTransport) -> PacketCount {
  let Ok(promise) = session.get_stats() else {
    return PacketCount::Unsupported;
  };
  let Ok(stats) = JsFuture::from(promise).await else {
    return PacketCount::Unknown;
  };
  PacketCount::from_stats(
    Reflect::get(&stats, &JsValue::from_str("packetsReceived"))
      .ok()
      .and_then(|count| count.as_f64()),
  )
}

/// How a session whose signaling stream ended was closed: `closed` resolves with the SFU's close
/// code when the SFU closed it, and rejects when it broke.
async fn session_end_of(closed: Promise) -> SessionEnd {
  let closed = JsFuture::from(closed);
  match select(pin!(closed), pin!(sleep(CLOSE_INFO_WAIT))).await {
    Either::Left((Ok(info), _)) => {
      let maybe_code = Reflect::get(&info, &JsValue::from_str("closeCode"))
        .ok()
        .and_then(|code| code.as_f64())
        // Close codes are 32-bit.
        .map(|code| code as u32);
      session_end(maybe_code)
    },
    // Broke, or still open with its signaling stream gone: either way the session is lost.
    Either::Left((Err(_), _)) | Either::Right(_) => SessionEnd::Lost,
  }
}

/// What a session closed with this code (none: it broke) means for rejoining.
fn session_end(maybe_close_code: Option<u32>) -> SessionEnd {
  if maybe_close_code == Some(SFU_GOING_AWAY_CLOSE_CODE) {
    SessionEnd::GoingAway
  } else {
    SessionEnd::Lost
  }
}

/// The rejoin attempts after a session ended at `now`: the first at once, or a little later when the
/// SFU is going away.
fn reconnect_schedule(
  lost_at: Instant,
  grace: Duration,
  end: SessionEnd,
  now: Instant,
  config: &ResilienceConfig,
  jitter: Jitter,
) -> ReconnectSchedule {
  let mut schedule = ReconnectSchedule::with_jitter(lost_at, grace, jitter);
  if end == SessionEnd::GoingAway {
    schedule.spread_first_attempt(now, config);
  }
  schedule
}

impl WebAudio {
  async fn open(
    source: &AudioSource,
    sink: &AudioSink,
    maybe_worklet_url: Option<&str>,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let options = AudioContextOptions::new();
    options.set_sample_rate(SAMPLE_RATE_HZ as f32);
    let context = AudioContext::new_with_context_options(&options).map_err(browser("AudioContext"))?;
    load_worklet(&context, maybe_worklet_url).await?;
    let mut audio = Self {
      context,
      maybe_capture: None,
      maybe_playback: None,
    };
    audio.switch(source, sink, inputs).await?;
    Ok(audio)
  }

  /// Opens the new microphone and speakers before releasing the old ones.
  async fn switch(
    &mut self,
    source: &AudioSource,
    sink: &AudioSink,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<(), WebCallError> {
    let maybe_capture = match source {
      AudioSource::Microphone(choice) => Some(Capture::open(&self.context, choice, inputs).await?),
      AudioSource::Silence | AudioSource::Tone { .. } => None,
    };
    let maybe_playback = match sink {
      AudioSink::Speakers(choice) => {
        set_sink(&self.context, choice).await;
        Some(open_playback(&self.context)?)
      },
      AudioSink::Discard => None,
    };
    if let Some(old) = std::mem::replace(&mut self.maybe_capture, maybe_capture) {
      old.close();
    }
    if let Some(old) = std::mem::replace(&mut self.maybe_playback, maybe_playback) {
      // Already detached from the graph or never attached: nothing to undo.
      let _ = old.disconnect();
    }
    // Autoplay policies may have created the context suspended; join runs from a click.
    let _ = self.context.resume();
    Ok(())
  }

  fn play(&self, mix: &[f32]) -> Result<(), WebCallError> {
    let Some(node) = &self.maybe_playback else {
      return Ok(());
    };
    node
      .port()
      .and_then(|port| port.post_message(&Float32Array::from(mix)))
      .map_err(browser("playback"))
  }

  fn close(self) {
    if let Some(capture) = self.maybe_capture {
      capture.close();
    }
    // Closing releases the audio hardware; the result only says when that finished.
    let _ = self.context.close();
  }
}

impl Capture {
  async fn open(
    context: &AudioContext,
    choice: &AudioDeviceChoice,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let stream = capture_microphone(choice).await?;
    let source = context
      .create_media_stream_source(&stream)
      .map_err(browser("microphone source"))?;
    let options = AudioWorkletNodeOptions::new();
    options.set_number_of_outputs(1);
    let node = AudioWorkletNode::new_with_options(context, "teamcraft-capture", &options)
      .map_err(browser("capture worklet"))?;
    let sender = inputs.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
      let samples: Float32Array = event.data().unchecked_into();
      // The call task may be gone; the capture stops when it drops this closure.
      let _ = sender.unbounded_send(Input::Captured(samples.to_vec()));
    });
    node
      .port()
      .map_err(browser("capture worklet"))?
      .set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    source
      .connect_with_audio_node(&node)
      .map_err(browser("capture graph"))?;
    // Connected to the output so the browser runs it; the processor writes silence.
    node
      .connect_with_audio_node(&context.destination())
      .map_err(browser("capture graph"))?;
    Ok(Self {
      stream,
      _source: source,
      node,
      _on_message: on_message,
    })
  }

  fn close(self) {
    let _ = self.node.disconnect();
    for track in self.stream.get_tracks().iter() {
      track.unchecked_into::<MediaStreamTrack>().stop();
    }
  }
}

impl OpusEncoder {
  fn new(inputs: &mpsc::UnboundedSender<Input>) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let output = Closure::<dyn FnMut(EncodedAudioChunk)>::new(move |chunk: EncodedAudioChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      match chunk.copy_to(&bytes) {
        Ok(()) => {
          let _ = sender.unbounded_send(Input::Encoded(bytes.to_vec()));
        },
        Err(error) => {
          let _ = sender.unbounded_send(Input::CodecFailed(js_message(&error)));
        },
      }
    });
    let error = codec_error_callback(inputs);
    let init = dictionary(&[
      ("output", output.as_ref().clone()),
      ("error", error.as_ref().clone()),
    ])
    .map_err(browser("AudioEncoder"))?;
    let encoder = AudioEncoder::new(&init).map_err(browser("AudioEncoder"))?;
    encoder
      .configure(&opus_config(OPUS_LOSS_FLOOR_PERCENT)?)
      .map_err(browser("AudioEncoder"))?;
    Ok(Self {
      encoder,
      planned_loss_percent: OPUS_LOSS_FLOOR_PERCENT,
      pending: Vec::with_capacity(2 * FRAME_SAMPLES_PER_CHANNEL),
      levels: VecDeque::new(),
      timestamp_us: 0.0,
      _output: output,
      _error: error,
    })
  }

  /// Plans in-band FEC for the loss measured on the uplink; reconfigures only on real changes.
  fn set_measured_loss_percent(&mut self, measured_percent: u8) -> Result<(), WebCallError> {
    let planned = planned_loss_percent(OPUS_LOSS_FLOOR_PERCENT, measured_percent);
    if planned.abs_diff(self.planned_loss_percent) < OPUS_LOSS_STEP_PERCENT {
      return Ok(());
    }
    self.planned_loss_percent = planned;
    self
      .encoder
      .configure(&opus_config(planned)?)
      .map_err(browser("AudioEncoder"))
  }

  /// Encodes every complete 20 ms frame; the encoded bytes arrive as [`Input::Encoded`].
  fn push(&mut self, samples: &[f32]) -> Result<(), WebCallError> {
    self.pending.extend_from_slice(samples);
    while self.pending.len() >= FRAME_SAMPLES_PER_CHANNEL {
      let frame: Vec<f32> = self.pending.drain(..FRAME_SAMPLES_PER_CHANNEL).collect();
      let pcm: Vec<i16> = frame.iter().map(|sample| to_i16(*sample)).collect();
      self
        .levels
        .push_back(measure_audio_level(&pcm, DEFAULT_VOICE_THRESHOLD_DBOV));
      let data = AudioData::new(
        &dictionary(&[
          ("format", JsValue::from_str("f32-planar")),
          ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
          ("numberOfFrames", JsValue::from(FRAME_SAMPLES_PER_CHANNEL as u32)),
          ("numberOfChannels", JsValue::from(1)),
          ("timestamp", JsValue::from(self.timestamp_us)),
          ("data", Float32Array::from(frame.as_slice()).into()),
        ])
        .map_err(browser("AudioData"))?,
      )
      .map_err(browser("AudioData"))?;
      self.timestamp_us += 20_000.0;
      let result = self.encoder.encode(&data);
      data.close();
      result.map_err(browser("AudioEncoder"))?;
    }
    Ok(())
  }
}

/// Voice at 32 kbit/s in 20 ms frames with in-band FEC planned for `loss_percent` (WebCodecs'
/// Opus registration).
fn opus_config(loss_percent: u8) -> Result<Object, WebCallError> {
  let opus = dictionary(&[
    ("frameDuration", JsValue::from(20_000)),
    ("useinbandfec", JsValue::TRUE),
    ("packetlossperc", JsValue::from(loss_percent)),
  ])
  .map_err(browser("AudioEncoder"))?;
  dictionary(&[
    ("codec", JsValue::from_str("opus")),
    ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
    ("numberOfChannels", JsValue::from(1)),
    ("bitrate", JsValue::from(OPUS_BITRATE_BPS)),
    ("opus", opus.into()),
  ])
  .map_err(browser("AudioEncoder"))
}

/// Waits on the page's event loop.
async fn sleep(duration: Duration) {
  if duration.is_zero() {
    return;
  }
  let millis = i32::try_from(duration.as_millis()).unwrap_or(i32::MAX);
  let promise = js_sys::Promise::new(&mut |resolve, _reject| {
    if let Ok(window) = window() {
      // A timer that can't be set resolves never; the grace period ends the waiting then.
      let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
    }
  });
  // The promise only resolves.
  let _ = JsFuture::from(promise).await;
}

impl Drop for OpusEncoder {
  fn drop(&mut self) {
    // Closing an already closed (errored) encoder throws; nothing left to release then.
    let _ = self.encoder.close();
  }
}

impl RemoteAudio {
  fn new(
    stream: RemoteStream,
    jitter: JitterBufferConfig,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let decoded_tx = inputs.clone();
    let output = Closure::<dyn FnMut(AudioData)>::new(move |data: AudioData| {
      let samples = Float32Array::new_with_length(data.number_of_frames());
      let copied = dictionary(&[
        ("planeIndex", JsValue::from(0)),
        ("format", JsValue::from_str("f32-planar")),
      ])
      .and_then(|options| data.copy_to(&samples, &options));
      data.close();
      let input = match copied {
        Ok(()) => Input::Decoded {
          stream,
          samples: samples.to_vec(),
        },
        Err(error) => Input::CodecFailed(js_message(&error)),
      };
      let _ = decoded_tx.unbounded_send(input);
    });
    let error = codec_error_callback(inputs);
    let init = dictionary(&[
      ("output", output.as_ref().clone()),
      ("error", error.as_ref().clone()),
    ])
    .map_err(browser("AudioDecoder"))?;
    let decoder = AudioDecoder::new(&init).map_err(browser("AudioDecoder"))?;
    let config = dictionary(&[
      ("codec", JsValue::from_str("opus")),
      ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
      ("numberOfChannels", JsValue::from(1)),
    ])
    .map_err(browser("AudioDecoder"))?;
    decoder.configure(&config).map_err(browser("AudioDecoder"))?;
    debug!(sender = %stream.0, origin = ?stream.1, "decoding a new audio stream");
    Ok(Self {
      jitter: JitterBuffer::new(jitter),
      decoder,
      decoded: VecDeque::new(),
      timestamp_us: 0.0,
      _output: output,
      _error: error,
    })
  }

  /// Hands the packet due now to the decoder. WebCodecs has no loss concealment call, so a lost
  /// packet is a 20 ms gap.
  fn decode_due(&mut self) -> Result<(), WebCallError> {
    let Some(Playout::Packet { item, .. }) = self.jitter.pop() else {
      return Ok(());
    };
    let chunk = EncodedAudioChunk::new(
      &dictionary(&[
        ("type", JsValue::from_str("key")),
        ("timestamp", JsValue::from(self.timestamp_us)),
        ("data", Uint8Array::from(item.as_slice()).into()),
      ])
      .map_err(browser("EncodedAudioChunk"))?,
    )
    .map_err(browser("EncodedAudioChunk"))?;
    self.timestamp_us += 20_000.0;
    self.decoder.decode(&chunk).map_err(browser("AudioDecoder"))
  }
}

impl Drop for RemoteAudio {
  fn drop(&mut self) {
    let _ = self.decoder.close();
  }
}

impl Interval {
  fn start(
    period_ms: i32,
    inputs: &mpsc::UnboundedSender<Input>,
    make_input: fn() -> Input,
  ) -> Result<Self, WebCallError> {
    let sender = inputs.clone();
    let callback = Closure::<dyn FnMut()>::new(move || {
      let _ = sender.unbounded_send(make_input());
    });
    let handle = window()?
      .set_interval_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), period_ms)
      .map_err(browser("setInterval"))?;
    Ok(Self {
      handle,
      _callback: callback,
    })
  }
}

impl Drop for Interval {
  fn drop(&mut self) {
    if let Ok(window) = window() {
      window.clear_interval_with_handle(self.handle);
    }
  }
}

impl SignalingDecoder {
  /// Appends a chunk and returns the complete message bodies it finished.
  fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, SignalingDecodeError> {
    self.buffer.extend_from_slice(chunk);
    let mut bodies = Vec::new();
    while let Some(prefix) = self.buffer.first_chunk::<4>() {
      let length = u32::from_be_bytes(*prefix) as usize;
      if length > MAX_SIGNALING_BYTES {
        return Err(SignalingDecodeError::TooLarge(length));
      }
      if self.buffer.len() < 4 + length {
        break;
      }
      bodies.push(self.buffer[4..4 + length].to_vec());
      self.buffer.drain(..4 + length);
    }
    Ok(bodies)
  }
}

async fn read_signaling(
  stream: ReadableStream,
  closed: Promise,
  inputs: mpsc::UnboundedSender<Input>,
  generation: u64,
) {
  let mut decoder = SignalingDecoder::default();
  let result = read_chunks(stream, |chunk| {
    let bodies = match decoder.push(&chunk) {
      Ok(bodies) => bodies,
      Err(error) => {
        warn!(%error, "closing signaling");
        return false;
      },
    };
    for body in bodies {
      match serde_json::from_slice::<CallServerMessage>(&body) {
        Ok(message) => {
          if inputs
            .unbounded_send(Input::Signal { generation, message })
            .is_err()
          {
            return false;
          }
        },
        Err(error) => warn!(%error, "invalid signaling message"),
      }
    }
    true
  })
  .await;
  if let Err(error) = result {
    debug!(error = %js_message(&error), "signaling stream ended");
  }
  let end = session_end_of(closed).await;
  let _ = inputs.unbounded_send(Input::SessionEnded { generation, end });
}

async fn read_datagrams(stream: ReadableStream, inputs: mpsc::UnboundedSender<Input>, generation: u64) {
  let result = read_chunks(stream, |bytes| {
    inputs
      .unbounded_send(Input::Datagram { generation, bytes })
      .is_ok()
  })
  .await;
  if let Err(error) = result {
    debug!(error = %js_message(&error), "datagram stream ended");
  }
}

/// Reads byte chunks until the stream ends or `on_chunk` returns false.
async fn read_chunks(
  stream: ReadableStream,
  mut on_chunk: impl FnMut(Vec<u8>) -> bool,
) -> Result<(), JsValue> {
  let reader: ReadableStreamDefaultReader = stream.get_reader().unchecked_into();
  loop {
    let result = JsFuture::from(reader.read()).await?;
    if Reflect::get(&result, &JsValue::from_str("done"))?.is_truthy() {
      return Ok(());
    }
    let value: Uint8Array = Reflect::get(&result, &JsValue::from_str("value"))?.unchecked_into();
    if !on_chunk(value.to_vec()) {
      reader.release_lock();
      return Ok(());
    }
  }
}

/// Registers both worklet processors with the context (from a blob URL, no extra file to serve).
async fn load_worklet(context: &AudioContext, maybe_url: Option<&str>) -> Result<(), WebCallError> {
  if let Some(url) = maybe_url {
    let promise = context
      .audio_worklet()
      .and_then(|worklet| worklet.add_module(url))
      .map_err(browser("AudioWorklet"))?;
    return JsFuture::from(promise)
      .await
      .map(|_| ())
      .map_err(browser("AudioWorklet"));
  }
  let options = BlobPropertyBag::new();
  options.set_type("text/javascript");
  let blob =
    Blob::new_with_str_sequence_and_options(&Array::of1(&JsValue::from_str(WORKLET_SOURCE)), &options)
      .map_err(browser("worklet module"))?;
  let url = Url::create_object_url_with_blob(&blob).map_err(browser("worklet module"))?;
  let promise = context
    .audio_worklet()
    .and_then(|worklet| worklet.add_module(&url))
    .map_err(browser("AudioWorklet"))?;
  let result = JsFuture::from(promise).await;
  // The module is loaded (or failed); the URL is no longer needed either way.
  let _ = Url::revoke_object_url(&url);
  result.map(|_| ()).map_err(browser("AudioWorklet"))
}

fn open_playback(context: &AudioContext) -> Result<AudioWorkletNode, WebCallError> {
  let options = AudioWorkletNodeOptions::new();
  options.set_number_of_inputs(0);
  options.set_output_channel_count(&Array::of1(&JsValue::from(1)));
  let processor_options = dictionary(&[("maxSamples", JsValue::from(MAX_PLAYBACK_SAMPLES))])
    .map_err(browser("playback worklet"))?;
  options.set_processor_options(Some(&processor_options));
  let node = AudioWorkletNode::new_with_options(context, "teamcraft-playback", &options)
    .map_err(browser("playback worklet"))?;
  node
    .connect_with_audio_node(&context.destination())
    .map_err(browser("playback graph"))?;
  Ok(node)
}

/// Routes the context to the chosen speakers (`AudioContext.setSinkId`, where supported).
async fn set_sink(context: &AudioContext, choice: &AudioDeviceChoice) {
  let id = match choice {
    AudioDeviceChoice::Default => "",
    AudioDeviceChoice::Device(id) => id.as_str(),
  };
  let Ok(set_sink_id) = Reflect::get(context, &JsValue::from_str("setSinkId")) else {
    return;
  };
  let Some(set_sink_id) = set_sink_id.dyn_ref::<Function>() else {
    if !id.is_empty() {
      warn!("this browser cannot choose the speakers; using the default");
    }
    return;
  };
  let result = match set_sink_id.call1(context, &JsValue::from_str(id)) {
    Ok(promise) => JsFuture::from(js_sys::Promise::from(promise)).await.map(|_| ()),
    Err(error) => Err(error),
  };
  if let Err(error) = result {
    warn!(error = %js_message(&error), "choosing the speakers failed; using the default");
  }
}

fn codec_error_callback(inputs: &mpsc::UnboundedSender<Input>) -> Closure<dyn FnMut(JsValue)> {
  let sender = inputs.clone();
  Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
    let _ = sender.unbounded_send(Input::CodecFailed(js_message(&error)));
  })
}

fn ensure_supported() -> Result<(), WebCallError> {
  for name in ["WebTransport", "AudioEncoder", "AudioDecoder", "AudioWorkletNode"] {
    if !has_global(name) {
      return Err(WebCallError::Unsupported(match name {
        "WebTransport" => "WebTransport",
        "AudioEncoder" | "AudioDecoder" => "WebCodecs audio",
        _ => "AudioWorklet",
      }));
    }
  }
  Ok(())
}

fn window() -> Result<web_sys::Window, WebCallError> {
  web_sys::window().ok_or(WebCallError::Unsupported("a window (calls run on the page)"))
}

fn browser(context: &'static str) -> impl Fn(JsValue) -> WebCallError {
  move |error| WebCallError::Browser {
    context,
    message: js_message(&error),
  }
}

/// A refused `getUserMedia`/`getDisplayMedia`, classified by the `DOMException`'s name. A missing
/// `mediaDevices` (pages that aren't a secure context) counts as denied.
fn media_access(input: MediaInput) -> impl Fn(JsValue) -> WebCallError {
  move |error| {
    let name = Reflect::get(&error, &JsValue::from_str("name"))
      .ok()
      .and_then(|name| name.as_string())
      .unwrap_or_default();
    WebCallError::MediaAccess {
      input,
      reason: media_access_reason(&name),
      message: js_message(&error),
    }
  }
}

fn media_access_reason(dom_exception_name: &str) -> MediaAccessReason {
  match dom_exception_name {
    "NotAllowedError" | "SecurityError" => MediaAccessReason::Denied,
    "NotFoundError" | "OverconstrainedError" => MediaAccessReason::NotFound,
    "NotReadableError" | "AbortError" => MediaAccessReason::InUse,
    _ => MediaAccessReason::Other,
  }
}

/// A readable message from a thrown JS value (usually a `DOMException` or `Error`).
fn js_message(error: &JsValue) -> String {
  Reflect::get(error, &JsValue::from_str("message"))
    .ok()
    .and_then(|message| message.as_string())
    .or_else(|| error.as_string())
    .unwrap_or_else(|| format!("{error:?}"))
}

fn to_i16(sample: f32) -> i16 {
  (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
}

#[cfg(test)]
mod tests {
  use super::*;

  const GRACE: Duration = Duration::from_secs(30);

  #[test]
  fn refused_media_is_classified_by_the_exception_name() {
    for (name, reason) in [
      ("NotAllowedError", MediaAccessReason::Denied),
      ("SecurityError", MediaAccessReason::Denied),
      ("NotFoundError", MediaAccessReason::NotFound),
      ("OverconstrainedError", MediaAccessReason::NotFound),
      ("NotReadableError", MediaAccessReason::InUse),
      ("AbortError", MediaAccessReason::InUse),
      ("TypeError", MediaAccessReason::Other),
      ("", MediaAccessReason::Other),
      ("SomeFutureError", MediaAccessReason::Other),
    ] {
      assert_eq!(media_access_reason(name), reason, "{name}");
    }
  }

  mod session_end {
    use super::*;

    #[test]
    fn only_the_sfu_going_away_spreads_the_rejoin() {
      assert_eq!(
        session_end(Some(SFU_GOING_AWAY_CLOSE_CODE)),
        SessionEnd::GoingAway
      );
      // Broken, or closed for another reason (the engine knows from signaling whether the call is
      // over).
      for maybe_code in [None, Some(0), Some(0x4801), Some(0x4804)] {
        assert_eq!(session_end(maybe_code), SessionEnd::Lost, "{maybe_code:?}");
      }
    }

    #[test]
    fn a_lost_session_is_rejoined_at_once() {
      let lost_at = Instant::now();
      let now = lost_at + Duration::from_millis(50);
      let schedule = reconnect_schedule(
        lost_at,
        GRACE,
        SessionEnd::Lost,
        now,
        &ResilienceConfig::browser(),
        Jitter::seeded(1),
      );
      assert!(schedule.next_attempt_at() <= now);
    }

    #[test]
    fn going_away_spreads_the_first_rejoin_over_clients() {
      let lost_at = Instant::now();
      let config = ResilienceConfig::browser();
      let firsts: Vec<Instant> = (0..8)
        .map(|seed| {
          reconnect_schedule(
            lost_at,
            GRACE,
            SessionEnd::GoingAway,
            lost_at,
            &config,
            Jitter::seeded(seed),
          )
          .next_attempt_at()
        })
        .collect();
      for first in &firsts {
        assert!(*first >= lost_at && *first < lost_at + config.going_away_spread);
      }
      let mut distinct = firsts.clone();
      distinct.sort();
      distinct.dedup();
      assert!(distinct.len() >= 6, "{firsts:?}");
    }
  }

  mod signaling_decoder {
    use super::*;

    #[test]
    fn messages_split_across_chunks_are_reassembled() {
      let mut decoder = SignalingDecoder::default();
      let mut bytes = framed(b"{\"a\":1}");
      bytes.extend(framed(b"{}"));
      let (first, second) = bytes.split_at(6);
      assert!(decoder.push(first).unwrap().is_empty());
      assert_eq!(
        decoder.push(second).unwrap(),
        vec![b"{\"a\":1}".to_vec(), b"{}".to_vec()]
      );
      assert!(decoder.push(&[]).unwrap().is_empty());
    }

    #[test]
    fn oversized_length_is_rejected() {
      let mut decoder = SignalingDecoder::default();
      let length = (MAX_SIGNALING_BYTES as u32 + 1).to_be_bytes();
      assert_eq!(
        decoder.push(&length),
        Err(SignalingDecodeError::TooLarge(MAX_SIGNALING_BYTES + 1))
      );
    }

    #[test]
    fn truncated_prefix_waits_for_more() {
      let mut decoder = SignalingDecoder::default();
      assert!(decoder.push(&[0, 0]).unwrap().is_empty());
      assert_eq!(decoder.push(&[0, 1, b'x']).unwrap(), vec![b"x".to_vec()]);
    }
  }

  fn framed(body: &[u8]) -> Vec<u8> {
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
  }
}
