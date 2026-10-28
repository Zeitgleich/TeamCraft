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
//! Screen share and remote video live in [`video`]; a shared screen's sound in [`screen_sound`].
//!
//! Every input (network, worklet, codecs, timers, app commands) is funneled into one channel that
//! a single task drains, so the engine needs no locking. Periodic work (audio frames, feedback,
//! video capture) runs on pulses of the audio clock, which keeps going in a hidden tab whose page
//! timers the browser throttles, with a page timer as the fallback ([`cadence`], issue #923).
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

mod cadence;
#[cfg(feature = "effects")]
mod gl_compositor;
mod meter;
#[cfg(feature = "effects")]
mod picture;
mod screen_sound;
#[cfg(feature = "effects")]
mod segmentation_worker;
mod session_watch;
mod video;

#[cfg(feature = "effects")]
pub use gl_compositor::GlCompositor;
#[cfg(feature = "effects")]
pub use picture::decode_picture;
#[cfg(feature = "effects")]
pub use segmentation_worker::{SegmentationWorker, WorkerMask};
#[cfg(feature = "effects")]
pub use video::CameraEffects;

use std::cell::{Cell, RefCell};
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
  ScreenContentMode, VideoCodec, VideoPublicationId, VideoTrack, VideoTrackKind,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};
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

use self::cadence::Cadence;
pub use self::meter::MicrophoneMeter;
use self::screen_sound::ScreenSound;
use self::session_watch::{NetworkVerdict, PacketCount, PathVerdict, Reading, SessionWatch, may_hurry};
use self::video::{EncoderGaveUp, FrameDecoder, Publication, TrackSender, VideoView};

use crate::audio_frame::{
  EncodedAudioFrame, FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ, planned_loss_percent,
};
use crate::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource, sine_tone};
use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};
use crate::call_auth::CallKeyAuthenticator;
use crate::call_cues::{CallCue, CuePlayer, StatusCues};
use crate::call_engine::{
  AudioOrigin, CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
  OwnVideoEnd,
};
use crate::call_resilience::{
  Jitter, JoinTicket, JoinTicketError, ReconnectSchedule, ResilienceConfig, rejoin_attempt,
};
use crate::call_video::ReceivedVideoFrame;
use crate::datagram::SimulcastLayer;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::rtp::AudioLevel;
use crate::video::{EncodedVideoFrame, TOP_LAYER, VideoResolution, layer_for_view};
use crate::web_api::{
  AudioData, AudioDecoder, AudioEncoder, EncodedAudioChunk, WORKLET_SOURCE, WebTransport,
  WebTransportBidirectionalStream, datagram_writable, dictionary, has_global,
};

/// Audio frame period.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Audio frames one late pulse may catch up on (more are skipped); the playback holds 200 ms.
const MAX_FRAMES_PER_PULSE: u32 = 5;
/// Feedback and allocation period.
const TIMER_INTERVAL: Duration = Duration::from_millis(100);
/// The page timer's pulse period (the fallback clock; throttled in hidden tabs).
const PULSE_INTERVAL_MS: i32 = 10;
/// The audio clock pulses every this many frames of audio (10 ms at 48 kHz).
const CLOCK_EVERY_FRAMES: u32 = SAMPLE_RATE_HZ / 100;
/// Upper bound of one signaling message.
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
/// At most this much audio waits in the playback worklet (latency bound).
const MAX_PLAYBACK_SAMPLES: u32 = 48_000 / 5;
/// Decoded audio kept per sender before the mixer takes it; more means the sender runs fast.
const MAX_DECODED_SAMPLES: usize = 5 * FRAME_SAMPLES_PER_CHANNEL;
/// Opus bitrate for voice.
const OPUS_BITRATE_BPS: u32 = 32_000;
/// Opus bitrate of a shared screen's sound: music and video sound need more than speech (as on
/// the desktop, `OpusEncoderConfig::screen_audio`).
const SCREEN_SOUND_BITRATE_BPS: u32 = 64_000;
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
  /// The screen picker was asked for outside a click: the browser wants `getDisplayMedia` called
  /// while the click's user activation lasts (`InvalidAccessError` in Safari, `InvalidStateError`
  /// in the spec and Firefox; issue #930).
  NoUserActivation,
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

impl WebCallError {
  /// Whether this is the user closing the screen picker without choosing, which is no failure. A
  /// refusal by the system or a policy ("Permission denied by system" in Chromium) is one.
  pub fn is_screen_picker_closed(&self) -> bool {
    matches!(
      self,
      Self::MediaAccess {
        input: MediaInput::Screen,
        reason: MediaAccessReason::Denied,
        message,
      } if !message.contains("system")
    )
  }
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
  /// Samples of the microphone or of a shared screen's sound.
  Captured {
    origin: AudioOrigin,
    samples: Vec<f32>,
  },
  /// An Opus packet of the microphone or of a shared screen's sound.
  Encoded {
    origin: AudioOrigin,
    opus: Vec<u8>,
  },
  /// The browser ended the shared screen's sound by itself.
  ScreenSoundEnded,
  Decoded {
    stream: RemoteStream,
    samples: Vec<f32>,
  },
  /// A codec reported an error that only needs logging (audio codecs, drawing a video frame).
  CodecFailed {
    context: &'static str,
    message: String,
  },
  /// A pulse of the audio clock or the page timer: periodic work that fell due runs.
  Pulse,
  VideoEncoded {
    kind: VideoTrackKind,
    layer: u8,
    data: Vec<u8>,
    is_keyframe: bool,
    timestamp_us: f64,
  },
  VideoEnded(VideoTrackKind),
  /// WebCodecs closed a layer's encoder after an error (`generation` tells which encoder): the
  /// track replaces it.
  VideoEncodeFailed {
    kind: VideoTrackKind,
    layer: u8,
    generation: u64,
    message: String,
  },
  /// WebCodecs closed a watched track's decoder after an error (`generation` tells which
  /// decoder): a new one starts at the next keyframe, which is asked for.
  VideoDecodeFailed {
    stream_id: MediaStreamId,
    generation: u64,
    message: String,
  },
  /// A watched track's canvas is now shown this large (device pixels).
  VideoResized {
    stream_id: MediaStreamId,
    shown: VideoResolution,
  },
  /// The session's round trip (`getStats().smoothedRtt`), for the next video stats; `None` where
  /// the browser doesn't tell.
  #[cfg(feature = "call-view")]
  RoundTrip(Option<u32>),
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
  /// Sends the running screen share's sound (an audio track of its stream).
  StartScreenSound {
    track: MediaStreamTrack,
    done: oneshot::Sender<Result<(), WebCallError>>,
  },
  #[cfg(feature = "effects")]
  SetEffect(crate::effects::BackgroundEffect),
  WatchVideo {
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
  },
  UnwatchVideo(MediaStreamId),
  SetStatusCues(StatusCues),
  /// Reads what the call's video does (stats for nerds, #938).
  #[cfg(feature = "call-view")]
  VideoStats(oneshot::Sender<teamcraft_client_api::video_stats::VideoStatsReport>),
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

/// Wakes the call task for its periodic work ([`Input::Pulse`]). Pulses of both clocks coalesce
/// while one waits in the channel, so a busy task (a permission prompt, a slow capture) doesn't
/// find a backlog of them.
#[derive(Clone)]
struct Pulses {
  inputs: mpsc::UnboundedSender<Input>,
  is_pending: Rc<Cell<bool>>,
}

/// The audio clock: a worklet that pulses every [`CLOCK_EVERY_FRAMES`] of audio, also in a hidden
/// tab; disconnected on drop.
struct AudioClock {
  node: AudioWorkletNode,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
}

struct Capture {
  stream: MediaStream,
  source: MediaStreamAudioSourceNode,
  node: AudioWorkletNode,
  _on_message: Closure<dyn FnMut(MessageEvent)>,
}

/// How a captured stream's channels reach the (mono) encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureMix {
  /// The first channel (a microphone, asked for in mono).
  FirstChannel,
  /// Every channel mixed down (a shared tab's stereo sound).
  DownToMono,
}

/// What an Opus encoder is tuned for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpusProfile {
  /// The microphone: speech at a variable bitrate.
  Voice,
  /// A shared screen's sound: music and video at a constant bitrate (packet sizes would tell what
  /// plays), like the desktop's.
  ScreenSound,
}

/// Microphone, tone or screen sound samples → 20 ms frames → WebCodecs Opus.
struct OpusEncoder {
  encoder: AudioEncoder,
  profile: OpusProfile,
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

/// A `setInterval` timer that pulses, cleared on drop (the fallback clock).
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

  /// [`WebCall::start_camera`] with background effects (the segmentation worker and a compositor);
  /// change the effect with [`WebCall::set_background_effect`].
  #[cfg(feature = "effects")]
  pub async fn start_camera_with_effects(
    &self,
    maybe_device: Option<&str>,
    effects: CameraEffects,
  ) -> Result<(), WebCallError> {
    let stream = video::capture_camera(maybe_device).await?;
    self.share_camera_stream_with_effects(stream, effects).await
  }

  /// [`WebCall::share_camera_stream`] through background effects (#439).
  #[cfg(feature = "effects")]
  pub async fn share_camera_stream_with_effects(
    &self,
    stream: MediaStream,
    effects: CameraEffects,
  ) -> Result<(), WebCallError> {
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
  /// from a user gesture, like `join`; where the gesture may be gone by the time this runs, open
  /// the picker with [`request_screen`] in the click and share with
  /// [`WebCall::share_screen_stream`].
  ///
  /// With `has_sound` the picker also offers the tab's or the system's sound; answers whether it
  /// is sent (see [`WebCall::share_screen_stream`]).
  pub async fn start_screen_share(
    &self,
    mode: ScreenContentMode,
    has_sound: bool,
  ) -> Result<bool, WebCallError> {
    let stream = video::capture_screen(mode, has_sound).await?;
    self.share_screen_stream(stream, mode).await
  }

  /// Shares a screen the app asked for itself ([`request_screen`]) as the screen share, replacing
  /// a running one, and its sound when the stream has an audio track (the user shared the tab's
  /// or the system's sound). Answers whether the sound is sent: a share whose sound can't be
  /// captured or encoded goes on without it (the track stops).
  pub async fn share_screen_stream(
    &self,
    stream: MediaStream,
    mode: ScreenContentMode,
  ) -> Result<bool, WebCallError> {
    let maybe_sound = screen_sound::sound_track(&stream);
    self.start_video(stream, video::screen_plan(mode)).await?;
    let Some(track) = maybe_sound else {
      return Ok(false);
    };
    let (done, done_rx) = oneshot::channel();
    self.command(Command::StartScreenSound { track, done })?;
    match done_rx.await.map_err(|_| WebCallError::Ended)? {
      Ok(()) => Ok(true),
      Err(WebCallError::Ended) => Err(WebCallError::Ended),
      Err(error) => {
        warn!(%error, "the screen's sound can't be sent; sharing the picture alone");
        Ok(false)
      },
    }
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

  /// What the call's video does now: every own and received track's counters and the round trip
  /// to the SFU (stats for nerds, #938). `None` once the call ended.
  #[cfg(feature = "call-view")]
  pub async fn video_stats(&self) -> Option<teamcraft_client_api::video_stats::VideoStatsReport> {
    let (done, done_rx) = oneshot::channel();
    self.command(Command::VideoStats(done)).ok()?;
    done_rx.await.ok()
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

/// Opens the browser's screen picker (`getDisplayMedia`) at once and returns its promise. Call it
/// synchronously in the click handler, before anything is awaited: Safari refuses the picker once
/// the click's user activation is gone (issue #930). [`picked_screen`] waits for the choice. With
/// `has_sound` the picker also offers the tab's or the system's sound, where the browser can
/// capture it (Chromium).
pub fn request_screen(mode: ScreenContentMode, has_sound: bool) -> Result<Promise, WebCallError> {
  video::request_screen(mode, has_sound)
}

/// The screen the user chose in a picker [`request_screen`] opened (possibly in another wasm
/// module of the page); closing the picker is a [`MediaAccessReason::Denied`] refusal.
pub async fn picked_screen(request: Promise) -> Result<MediaStream, WebCallError> {
  video::picked_screen(request).await
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
    encoder: OpusEncoder::new(&inputs, OpusProfile::Voice)?,
    maybe_screen_sound: None,
    remotes: BTreeMap::new(),
    source: config.source.clone(),
    tone_position: 0,
    is_muted: config.is_muted_at_start,
    is_deafened: false,
    stats: CallStats::default(),
    own_device: config.engine.device_id,
    senders: Vec::new(),
    next_publication: VideoPublicationId(0),
    encoded_kinds: Vec::new(),
    views: HashMap::new(),
    remote_tracks: HashMap::new(),
    pulses: Pulses {
      inputs: inputs.clone(),
      is_pending: Rc::new(Cell::new(false)),
    },
    frame_cadence: Cadence::new(FRAME_INTERVAL, MAX_FRAMES_PER_PULSE, Instant::now()),
    timer_cadence: Cadence::new(TIMER_INTERVAL, 1, Instant::now()),
    #[cfg(feature = "call-view")]
    started_at: Instant::now(),
    #[cfg(feature = "call-view")]
    maybe_rtt_ms: None,
  };
  let _audio_clock = AudioClock::start(&audio.context, state.pulses.clone())
    .inspect_err(|error| warn!(%error, "no audio clock; video and timers slow down in a hidden tab"))
    .ok();
  let _pulse_timer = Interval::start(PULSE_INTERVAL_MS, state.pulses.clone())?;
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

  if let Some(sound) = state.maybe_screen_sound.take() {
    sound.stop();
  }
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
  /// The running screen share's sound, while it is sent.
  maybe_screen_sound: Option<ScreenSound>,
  remotes: BTreeMap<RemoteStream, RemoteAudio>,
  source: AudioSource,
  tone_position: u64,
  is_muted: bool,
  is_deafened: bool,
  stats: CallStats,
  own_device: DeviceId,
  senders: Vec<TrackSender>,
  /// The id the next own video publication goes under.
  next_publication: VideoPublicationId,
  /// Kinds whose running own track encoded a picture: one that ends before did not start.
  encoded_kinds: Vec<VideoTrackKind>,
  views: HashMap<MediaStreamId, Watched>,
  /// Other devices' published tracks: their kind and layers decide which layer a tile asks for.
  remote_tracks: HashMap<MediaStreamId, VideoTrack>,
  pulses: Pulses,
  frame_cadence: Cadence,
  timer_cadence: Cadence,
  /// The base of the video stats' clock.
  #[cfg(feature = "call-view")]
  started_at: Instant,
  /// The session's round trip as last read, for the video stats.
  #[cfg(feature = "call-view")]
  maybe_rtt_ms: Option<u32>,
}

/// A watched track: its tile's decoder, and which layer its size asks for.
struct Watched {
  view: VideoView,
  /// The canvas' size as last reported (device pixels).
  shown: VideoResolution,
  max_layer: u8,
  /// Reports the canvas' size as [`Input::VideoResized`]; `None` where the browser can't.
  _maybe_size_watch: Option<video::SizeWatch>,
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
      Input::Captured {
        origin: AudioOrigin::Microphone,
        samples,
      } => {
        if !self.is_muted {
          self.encoder.push(&samples)?;
        }
      },
      // Not muted with the microphone, like the desktop's.
      Input::Captured {
        origin: AudioOrigin::Screen,
        samples,
      } => {
        // A sound that can't be encoded stops; it never ends the call.
        if let Some(sound) = &mut self.maybe_screen_sound
          && let Err(error) = sound.encoder.push(&samples)
        {
          warn!(%error, "the shared screen's sound can't be encoded; the picture goes on without it");
          self.end_screen_sound(engine);
        }
      },
      Input::Encoded {
        origin: AudioOrigin::Microphone,
        opus,
      } => {
        // One output per input frame; a stray extra output (encoder flush) counts as silence.
        let level = self.encoder.levels.pop_front().unwrap_or(AudioLevel::SILENCE);
        engine.send_audio(&EncodedAudioFrame { opus, level }, Instant::now());
        self.stats.frames_sent += 1;
      },
      Input::Encoded {
        origin: AudioOrigin::Screen,
        opus,
      } => {
        // A packet a stopped sound's encoder still put out goes nowhere.
        if let Some(sound) = &mut self.maybe_screen_sound {
          let level = sound.encoder.levels.pop_front().unwrap_or(AudioLevel::SILENCE);
          engine.send_screen_audio(&EncodedAudioFrame { opus, level }, Instant::now());
        }
      },
      Input::ScreenSoundEnded => {
        if self.maybe_screen_sound.is_some() {
          info!("the browser ended the shared screen's sound; the picture goes on");
          self.end_screen_sound(engine);
        }
      },
      Input::Decoded { stream, samples } => {
        if let Some(remote) = self.remotes.get_mut(&stream) {
          remote.decoded.extend(samples);
          let excess = remote.decoded.len().saturating_sub(MAX_DECODED_SAMPLES);
          remote.decoded.drain(..excess);
        }
      },
      Input::CodecFailed { context, message } => warn!(context, %message, "codec error"),
      Input::Pulse => return self.on_pulse(engine, audio).await,
      Input::VideoEncoded {
        kind,
        layer,
        data,
        is_keyframe,
        timestamp_us,
      } => {
        if let Some(sender) = self.sender_of_kind(&kind) {
          sender.on_encoded(layer);
        }
        self.send_video_frame(engine, &kind, layer, data, is_keyframe, timestamp_us);
      },
      Input::VideoEnded(kind) => self.end_video(engine, &kind),
      Input::VideoEncodeFailed {
        kind,
        layer,
        generation,
        message,
      } => {
        warn!(%message, ?kind, layer, "video encoder failed");
        if let Some(sender) = self.sender_of_kind(&kind)
          && let Err(gave_up) = sender.on_encoder_failed(layer, generation)
        {
          self.on_encoders_gave_up(engine, &kind, gave_up);
        }
      },
      Input::VideoDecodeFailed {
        stream_id,
        generation,
        message,
      } => {
        if let Some(watched) = self.views.get_mut(&stream_id) {
          recover_decoder(&mut watched.view, stream_id, generation, &message, engine);
        }
      },
      Input::VideoResized { stream_id, shown } => {
        if let Some(watched) = self.views.get_mut(&stream_id) {
          watched.shown = shown;
        }
        self.update_layer(engine, stream_id);
      },
      #[cfg(feature = "call-view")]
      Input::RoundTrip(maybe_rtt_ms) => self.maybe_rtt_ms = maybe_rtt_ms,
      Input::Command(command) => return self.on_command(command, engine, audio).await,
    }
    Ok(Flow::Continue)
  }

  /// The engine's video stats with what the browser adds: the encoders, the capture size and
  /// frames taken, and the round trip as last read. Starts the next reading of the round trip:
  /// the stats are read once a second while someone looks at them.
  #[cfg(feature = "call-view")]
  fn video_stats(&self, engine: &CallEngine) -> teamcraft_client_api::video_stats::VideoStatsReport {
    use teamcraft_client_api::video_stats::{
      EncoderImplementation, VideoEncoderInfo, VideoSize, VideoTrackStats,
    };

    let at_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = engine.video_stats(at_ms);
    report.maybe_rtt_ms = self.maybe_rtt_ms;
    for track in &mut report.tracks {
      let VideoTrackStats::Sent(sent) = track else {
        continue;
      };
      let Some(sender) = self
        .senders
        .iter()
        .find(|sender| sender.publication.own.maybe_stream_id == Some(sent.stream_id))
      else {
        continue;
      };
      // WebCodecs picks hardware or software itself and doesn't say which.
      sent.maybe_encoder = Some(VideoEncoderInfo {
        name: "WebCodecs".to_owned(),
        implementation: EncoderImplementation::Browser,
      });
      sent.maybe_capture = sender.source_size().map(|size| VideoSize {
        width: size.width,
        height: size.height,
      });
      sent.maybe_frames_captured = Some(sender.frames_captured());
    }
    if let Some(transport) = &self.maybe_transport {
      let session = transport.transport.clone();
      let inputs = self.inputs.clone();
      spawn_local(async move {
        let maybe_rtt_ms = round_trip_ms(&session).await;
        // The call ended meanwhile if nobody reads the inputs.
        let _ = inputs.unbounded_send(Input::RoundTrip(maybe_rtt_ms));
      });
    }
    report
  }

  /// Runs what fell due since the last pulse: audio frames, the feedback timer, video captures.
  async fn on_pulse(&mut self, engine: &mut CallEngine, audio: &WebAudio) -> Result<Flow, WebCallError> {
    self.pulses.taken();
    let now = Instant::now();
    for _ in 0..self.frame_cadence.due(now) {
      self.on_frame_tick(audio)?;
      if self.is_ending_lost && !self.cues.is_playing(CallCue::ConnectionLost) {
        return Err(WebCallError::ConnectionLost);
      }
    }
    if self.timer_cadence.due(now) > 0
      && let Flow::Leave = self.on_timer_tick(engine, now)?
    {
      return Ok(Flow::Leave);
    }
    let due: Vec<VideoTrackKind> = self
      .senders
      .iter_mut()
      .filter_map(|sender| sender.is_due(now).then(|| sender.publication.kind.clone()))
      .collect();
    // A video error stays with its track or tile: it never ends the call (issue #919).
    for kind in due {
      if let Some(sender) = self.sender_of_kind(&kind)
        && let Err(gave_up) = sender.capture().await
      {
        self.on_encoders_gave_up(engine, &kind, gave_up);
      }
    }
    Ok(Flow::Continue)
  }

  fn on_timer_tick(&mut self, engine: &mut CallEngine, now: Instant) -> Result<Flow, WebCallError> {
    engine.on_timer(now);
    self.timer_ticks = self.timer_ticks.wrapping_add(1);
    if self.timer_ticks.is_multiple_of(STATS_EVERY_TICKS) {
      self.request_path_stats();
    }
    let tick = self.watch.on_tick(now);
    if let Some(id) = tick.maybe_ping {
      self.send_ping(id)?;
    }
    Ok(self.on_path_verdict(engine, tick.verdict))
  }

  /// Asks for the layer a watched track's tile size calls for, when it changed.
  fn update_layer(&mut self, engine: &mut CallEngine, stream_id: MediaStreamId) {
    let Some(watched) = self.views.get_mut(&stream_id) else {
      return;
    };
    let Some(track) = self.remote_tracks.get(&stream_id) else {
      return;
    };
    let max_layer = layer_for_view(&track.kind, &track.layers, watched.shown);
    if max_layer != watched.max_layer {
      debug!(stream_id = stream_id.0, max_layer, shown = ?watched.shown, "video tile asks for another layer");
      watched.max_layer = max_layer;
      engine.set_video_max_layer(stream_id, max_layer);
    }
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
          if let Some(watched) = self.views.get_mut(&frame.stream_id) {
            decode_video(&mut watched.view, &frame, engine);
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
            CallEngineEvent::VideoPublished(track) => {
              self.on_publication(&event);
              // A tile may have been watching before its layers were known.
              self.update_layer(engine, track.stream_id);
            },
            CallEngineEvent::VideoUnpublished(_) => self.on_publication(&event),
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
              if let Some(sound) = &mut self.maybe_screen_sound
                && let Err(error) = sound.encoder.set_measured_loss_percent(*loss_percent)
              {
                // Its encoder failed: the next samples stop the sound.
                debug!(%error, "the shared screen's sound encoder takes no new configuration");
              }
            },
            _ => {},
          }
          self.cues.on_event(&event);
          // The app may have stopped listening; the call goes on.
          let _ = self.events.unbounded_send(event);
        },
      }
    }
    // The SFU stopped our shared sound (a host's "mute all"): the picture goes on without it.
    if self.maybe_screen_sound.is_some() && !engine.is_sharing_screen_audio() {
      self.stop_screen_sound(engine);
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
      Command::StartScreenSound { track, done } => {
        let result = self.start_screen_sound(engine, audio, track);
        // The caller may have given up waiting.
        let _ = done.send(result);
      },
      #[cfg(feature = "effects")]
      Command::SetEffect(effect) => {
        for sender in &mut self.senders {
          sender.set_effect(effect.clone());
        }
      },
      Command::WatchVideo { stream_id, canvas } => {
        let shown = video::shown_size(&canvas);
        let maybe_size_watch = video::SizeWatch::start(stream_id, &canvas, self.inputs)
          .inspect_err(|error| debug!(%error, "tile sizes unknown; the video keeps its first layer"))
          .ok();
        // Unknown layers (not published yet): the top, like the desktop; the SFU caps by downlink.
        let max_layer = self.remote_tracks.get(&stream_id).map_or(TOP_LAYER, |track| {
          layer_for_view(&track.kind, &track.layers, shown)
        });
        let view = VideoView::new(stream_id, canvas, self.inputs);
        self.views.insert(
          stream_id,
          Watched {
            view,
            shown,
            max_layer,
            _maybe_size_watch: maybe_size_watch,
          },
        );
        engine.subscribe_video(stream_id, Some(max_layer));
      },
      Command::SetStatusCues(status_cues) => self.cues.set_setting(status_cues),
      #[cfg(feature = "call-view")]
      Command::VideoStats(done) => {
        // The caller may have given up waiting.
        let _ = done.send(self.video_stats(engine));
      },
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
    let publication = self.next_publication;
    self.next_publication = VideoPublicationId(publication.0.wrapping_add(1));
    let sender = TrackSender::start(stream, plan, publication, self.inputs).await?;
    engine.publish_video(publication, kind, VideoCodec::H264, sender.layers());
    self.senders.push(sender);
    Ok(())
  }

  /// Sends the screen share's sound. Only with a running share: a share stopped meanwhile took its
  /// sound with it.
  fn start_screen_sound(
    &mut self,
    engine: &mut CallEngine,
    audio: &WebAudio,
    track: MediaStreamTrack,
  ) -> Result<(), WebCallError> {
    self.stop_screen_sound(engine);
    if self.sender_of_kind(&VideoTrackKind::Screen).is_none() {
      track.stop();
      return Err(WebCallError::Ended);
    }
    match ScreenSound::start(&audio.context, track.clone(), self.inputs) {
      Ok(sound) => {
        self.maybe_screen_sound = Some(sound);
        engine.publish_screen_audio();
        Ok(())
      },
      Err(error) => {
        track.stop();
        Err(error)
      },
    }
  }

  fn stop_screen_sound(&mut self, engine: &mut CallEngine) {
    if let Some(sound) = self.maybe_screen_sound.take() {
      sound.stop();
      engine.unpublish_screen_audio();
    }
  }

  /// The shared sound ended by itself (the browser ended its capture, or it can't be encoded): it
  /// stops, the picture goes on, and the app hears so (the UI shows the share without sound).
  fn end_screen_sound(&mut self, engine: &mut CallEngine) {
    self.stop_screen_sound(engine);
    // The app may have stopped listening; the call goes on.
    let _ = self.events.unbounded_send(CallEngineEvent::ScreenAudioChanged {
      device_id: self.own_device,
      is_shared: false,
    });
  }

  fn stop_video(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind) {
    // The sound goes with its share.
    if *kind == VideoTrackKind::Screen {
      self.stop_screen_sound(engine);
    }
    self.encoded_kinds.retain(|encoded| encoded != kind);
    let (stopped, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.senders)
      .into_iter()
      .partition(|sender| sender.publication.kind == *kind);
    self.senders = kept;
    for sender in stopped {
      // Also before the SFU confirmed it: its answer is then unpublished when it comes (#929).
      engine.withdraw_video(sender.publication.own.id);
      sender.stop();
    }
  }

  /// An own track ended by itself (the browser ended its capture: "Stop sharing" in its bar, an
  /// unplugged camera, a revoked permission; or its encoders gave up): it stops, and the app
  /// hears about it, so the UI doesn't show the camera or share on any more (issue #920).
  fn end_video(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind) {
    if self.sender_of_kind(kind).is_none() {
      // Stopped already: nothing ended by itself.
      return;
    }
    let end = if self.encoded_kinds.contains(kind) {
      OwnVideoEnd::WhileSending
    } else {
      OwnVideoEnd::BeforeFirstPicture
    };
    self.stop_video(engine, kind);
    // The app may have stopped listening; the call goes on.
    let _ = self.events.unbounded_send(CallEngineEvent::OwnVideoEnded {
      kind: kind.clone(),
      end,
    });
  }

  /// A layer's encoders kept failing: the track stops as if the browser had ended it. Others see
  /// its tile go, and the app shows the camera or share off ([`Self::end_video`]).
  fn on_encoders_gave_up(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind, gave_up: EncoderGaveUp) {
    error!(
      ?kind,
      layer = gave_up.layer,
      "the browser can't encode this video; stopping it"
    );
    self.end_video(engine, kind);
  }

  /// Own tracks follow their publications ([`follow_publication`]); a track that is gone loses
  /// its view.
  fn on_publication(&mut self, event: &CallEngineEvent) {
    match event {
      CallEngineEvent::VideoUnpublished(stream_id) => {
        self.views.remove(stream_id);
        self.remote_tracks.remove(stream_id);
      },
      CallEngineEvent::VideoPublished(track) if track.device_id != self.own_device => {
        self.remote_tracks.insert(track.stream_id, track.clone());
      },
      _ => {},
    }
    let publications = self.senders.iter_mut().map(|sender| &mut sender.publication);
    if let Some(index) = follow_publication(publications, self.own_device, event)
      && let Some(sender) = self.senders.get_mut(index)
    {
      // Receivers of the new stream start with a keyframe on every layer.
      sender.request_keyframes();
    }
  }

  fn sender_of(&mut self, stream_id: MediaStreamId) -> Option<&mut TrackSender> {
    self
      .senders
      .iter_mut()
      .find(|sender| sender.publication.own.maybe_stream_id == Some(stream_id))
  }

  fn sender_of_kind(&mut self, kind: &VideoTrackKind) -> Option<&mut TrackSender> {
    self
      .senders
      .iter_mut()
      .find(|sender| sender.publication.kind == *kind)
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
    let has_sender = self.sender_of_kind(kind).is_some();
    if has_sender && !self.encoded_kinds.contains(kind) {
      self.encoded_kinds.push(kind.clone());
    }
    let Some(stream_id) = self
      .senders
      .iter()
      .find(|sender| sender.publication.kind == *kind)
      .and_then(|sender| sender.publication.own.maybe_stream_id)
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

/// The session's smoothed round trip in milliseconds (`getStats().smoothedRtt`); `None` where the
/// browser has no such statistic.
#[cfg(feature = "call-view")]
async fn round_trip_ms(session: &WebTransport) -> Option<u32> {
  let stats = JsFuture::from(session.get_stats().ok()?).await.ok()?;
  let rtt = Reflect::get(&stats, &JsValue::from_str("smoothedRtt"))
    .ok()?
    .as_f64()?;
  // A positive duration in milliseconds; the conversion saturates.
  (rtt.is_finite() && rtt >= 0.0).then(|| rtt.round() as u32)
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

impl Pulses {
  fn send(&self) {
    if !self.is_pending.replace(true) {
      // The call task may be gone; its clocks stop when it drops them.
      let _ = self.inputs.unbounded_send(Input::Pulse);
    }
  }

  /// The call task took the waiting pulse: the next one goes out again.
  fn taken(&self) {
    self.is_pending.set(false);
  }
}

impl AudioClock {
  /// Starts the clock worklet. It fails with a worklet module from before the clock (a page that
  /// cached an old one); the page timer pulses alone then.
  fn start(context: &AudioContext, pulses: Pulses) -> Result<Self, WebCallError> {
    let options = AudioWorkletNodeOptions::new();
    options.set_number_of_inputs(0);
    options.set_output_channel_count(&Array::of1(&JsValue::from(1)));
    let processor_options =
      dictionary(&[("everyFrames", JsValue::from(CLOCK_EVERY_FRAMES))]).map_err(browser("clock worklet"))?;
    options.set_processor_options(Some(&processor_options));
    let node = AudioWorkletNode::new_with_options(context, "teamcraft-clock", &options)
      .map_err(browser("clock worklet"))?;
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |_: MessageEvent| pulses.send());
    node
      .port()
      .map_err(browser("clock worklet"))?
      .set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    // Connected to the output so the browser runs it; the processor writes silence.
    node
      .connect_with_audio_node(&context.destination())
      .map_err(browser("clock graph"))?;
    Ok(Self {
      node,
      _on_message: on_message,
    })
  }
}

impl Drop for AudioClock {
  fn drop(&mut self) {
    // Already detached with its closed context: nothing to undo.
    let _ = self.node.disconnect();
  }
}

impl Capture {
  async fn open(
    context: &AudioContext,
    choice: &AudioDeviceChoice,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let stream = capture_microphone(choice).await?;
    Self::from_stream(context, stream, CaptureMix::FirstChannel, inputs)
  }

  /// Captures an audio stream; its samples arrive as [`Input::Captured`]: the first channel as the
  /// microphone's, or every channel mixed down as a shared screen's sound.
  fn from_stream(
    context: &AudioContext,
    stream: MediaStream,
    mix: CaptureMix,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let origin = match mix {
      CaptureMix::FirstChannel => AudioOrigin::Microphone,
      CaptureMix::DownToMono => AudioOrigin::Screen,
    };
    let source = context
      .create_media_stream_source(&stream)
      .map_err(browser("audio source"))?;
    let options = AudioWorkletNodeOptions::new();
    options.set_number_of_outputs(1);
    if mix == CaptureMix::DownToMono {
      // Web Audio mixes the input down to the one channel the worklet takes ("speakers" rules).
      for (name, value) in [
        ("channelCount", JsValue::from(1)),
        ("channelCountMode", JsValue::from_str("explicit")),
        ("channelInterpretation", JsValue::from_str("speakers")),
      ] {
        Reflect::set(&options, &JsValue::from_str(name), &value).map_err(browser("capture worklet"))?;
      }
    }
    let node = AudioWorkletNode::new_with_options(context, "teamcraft-capture", &options)
      .map_err(browser("capture worklet"))?;
    let sender = inputs.clone();
    let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
      let samples: Float32Array = event.data().unchecked_into();
      // The call task may be gone; the capture stops when it drops this closure.
      let _ = sender.unbounded_send(Input::Captured {
        origin,
        samples: samples.to_vec(),
      });
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
      source,
      node,
      _on_message: on_message,
    })
  }

  fn close(self) {
    // Already detached with a closed context: nothing to undo.
    let _ = self.source.disconnect();
    let _ = self.node.disconnect();
    for track in self.stream.get_tracks().iter() {
      track.unchecked_into::<MediaStreamTrack>().stop();
    }
  }
}

impl OpusEncoder {
  fn new(inputs: &mpsc::UnboundedSender<Input>, profile: OpusProfile) -> Result<Self, WebCallError> {
    let origin = profile.origin();
    let sender = inputs.clone();
    let output = Closure::<dyn FnMut(EncodedAudioChunk)>::new(move |chunk: EncodedAudioChunk| {
      let bytes = Uint8Array::new_with_length(chunk.byte_length());
      match chunk.copy_to(&bytes) {
        Ok(()) => {
          let _ = sender.unbounded_send(Input::Encoded {
            origin,
            opus: bytes.to_vec(),
          });
        },
        Err(error) => {
          let _ = sender.unbounded_send(Input::CodecFailed {
            context: "audio encoder output",
            message: js_message(&error),
          });
        },
      }
    });
    let error = codec_error_callback(inputs, "audio encoder");
    let init = dictionary(&[
      ("output", output.as_ref().clone()),
      ("error", error.as_ref().clone()),
    ])
    .map_err(browser("AudioEncoder"))?;
    let encoder = AudioEncoder::new(&init).map_err(browser("AudioEncoder"))?;
    encoder
      .configure(&opus_config(profile, OPUS_LOSS_FLOOR_PERCENT)?)
      .map_err(browser("AudioEncoder"))?;
    Ok(Self {
      encoder,
      profile,
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
      .configure(&opus_config(self.profile, planned)?)
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

impl OpusProfile {
  /// Which of this device's streams the packets go out on.
  fn origin(self) -> AudioOrigin {
    match self {
      Self::Voice => AudioOrigin::Microphone,
      Self::ScreenSound => AudioOrigin::Screen,
    }
  }

  fn bitrate_bps(self) -> u32 {
    match self {
      Self::Voice => OPUS_BITRATE_BPS,
      Self::ScreenSound => SCREEN_SOUND_BITRATE_BPS,
    }
  }

  /// Members of the `AudioEncoderConfig` beside codec, rate, channels and bitrate, and of its
  /// `opus` (`OpusEncoderConfig`) beside frame duration and FEC. Voice keeps the browser's
  /// defaults; a shared screen's sound is tuned for music at a constant bitrate. Browsers that
  /// don't know a member ignore it.
  fn tuning(self) -> OpusTuning {
    match self {
      Self::Voice => OpusTuning::default(),
      Self::ScreenSound => OpusTuning {
        config: &[("bitrateMode", TuningValue::Text("constant"))],
        opus: &[
          ("application", TuningValue::Text("audio")),
          ("signal", TuningValue::Text("music")),
          // Quiet passages of music are not silence.
          ("usedtx", TuningValue::Flag(false)),
        ],
      },
    }
  }
}

/// See [`OpusProfile::tuning`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct OpusTuning {
  config: &'static [(&'static str, TuningValue)],
  opus: &'static [(&'static str, TuningValue)],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuningValue {
  Text(&'static str),
  Flag(bool),
}

fn tuning_value(value: TuningValue) -> JsValue {
  match value {
    TuningValue::Text(text) => JsValue::from_str(text),
    TuningValue::Flag(flag) => JsValue::from_bool(flag),
  }
}

/// Mono Opus for `profile` in 20 ms frames with in-band FEC planned for `loss_percent` (WebCodecs'
/// Opus registration).
fn opus_config(profile: OpusProfile, loss_percent: u8) -> Result<Object, WebCallError> {
  let tuning = profile.tuning();
  let mut opus_entries = vec![
    ("frameDuration", JsValue::from(20_000)),
    ("useinbandfec", JsValue::TRUE),
    ("packetlossperc", JsValue::from(loss_percent)),
  ];
  opus_entries.extend(
    tuning
      .opus
      .iter()
      .map(|(name, value)| (*name, tuning_value(*value))),
  );
  let opus = dictionary(&opus_entries).map_err(browser("AudioEncoder"))?;
  let mut entries = vec![
    ("codec", JsValue::from_str("opus")),
    ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
    ("numberOfChannels", JsValue::from(1)),
    ("bitrate", JsValue::from(profile.bitrate_bps())),
    ("opus", opus.into()),
  ];
  entries.extend(
    tuning
      .config
      .iter()
      .map(|(name, value)| (*name, tuning_value(*value))),
  );
  dictionary(&entries).map_err(browser("AudioEncoder"))
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
        Err(error) => Input::CodecFailed {
          context: "audio decoder output",
          message: js_message(&error),
        },
      };
      let _ = decoded_tx.unbounded_send(input);
    });
    let error = codec_error_callback(inputs, "audio decoder");
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
  fn start(period_ms: i32, pulses: Pulses) -> Result<Self, WebCallError> {
    let callback = Closure::<dyn FnMut()>::new(move || pulses.send());
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

/// Keeps own tracks' stream ids in step with the engine, like the native driver: an own track
/// takes the stream the SFU confirmed for its own publication, never one meant for an earlier or
/// later track of its kind (#929), and gives it up when the SFU dropped it (a rejoin without the
/// slot), so the engine's renewed publication of the track reaches it (issue #919). Returns the
/// index of the track that got a stream id.
fn follow_publication<'a>(
  publications: impl IntoIterator<Item = &'a mut Publication>,
  own_device: DeviceId,
  event: &CallEngineEvent,
) -> Option<usize> {
  let mut maybe_confirmed = None;
  for (index, publication) in publications.into_iter().enumerate() {
    if publication.own.follow(own_device, event) {
      maybe_confirmed = Some(index);
    }
  }
  maybe_confirmed
}

/// Decodes a frame of a watched track. A failure stays with its tile: the failed decoder is gone
/// and a keyframe, from which a new one starts, is asked for; the call goes on.
fn decode_video<D: FrameDecoder>(
  view: &mut VideoView<D>,
  frame: &ReceivedVideoFrame,
  engine: &mut CallEngine,
) {
  if let Err(error) = view.decode(frame) {
    warn!(%error, stream_id = frame.stream_id.0, "decoding a video frame failed; asking for a keyframe");
    engine.request_keyframe(frame.stream_id, Instant::now());
  }
}

/// WebCodecs closed a tile's decoder: the view drops it and a keyframe is asked for. The report of
/// a decoder the view already replaced changes nothing.
fn recover_decoder<D: FrameDecoder>(
  view: &mut VideoView<D>,
  stream_id: MediaStreamId,
  generation: u64,
  message: &str,
  engine: &mut CallEngine,
) {
  if view.on_decoder_failed(generation) {
    warn!(%message, stream_id = stream_id.0, "video decoder failed; asking for a keyframe");
    engine.request_keyframe(stream_id, Instant::now());
  } else {
    debug!(%message, stream_id = stream_id.0, "a replaced video decoder failed");
  }
}

fn codec_error_callback(
  inputs: &mpsc::UnboundedSender<Input>,
  context: &'static str,
) -> Closure<dyn FnMut(JsValue)> {
  let sender = inputs.clone();
  Closure::<dyn FnMut(JsValue)>::new(move |error: JsValue| {
    let _ = sender.unbounded_send(Input::CodecFailed {
      context,
      message: js_message(&error),
    });
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
      reason: media_access_reason(input, &name),
      message: js_message(&error),
    }
  }
}

fn media_access_reason(input: MediaInput, dom_exception_name: &str) -> MediaAccessReason {
  match (input, dom_exception_name) {
    (_, "NotAllowedError" | "SecurityError") => MediaAccessReason::Denied,
    (_, "NotFoundError" | "OverconstrainedError") => MediaAccessReason::NotFound,
    (_, "NotReadableError" | "AbortError") => MediaAccessReason::InUse,
    // Only `getDisplayMedia` needs the click's activation; for `getUserMedia` these names mean
    // something else (a document that isn't active).
    (MediaInput::Screen, "InvalidAccessError" | "InvalidStateError") => MediaAccessReason::NoUserActivation,
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
  use teamcraft_proto::{
    CallEvent, CallId, CallParticipant, Id, MediaCapabilities, Snowflake, UserId, VideoLayerInfo, VideoTrack,
  };

  use super::*;
  use crate::bwe::BweConfig;
  use crate::call_engine::{OwnPublication, VideoSendOutcome};

  const GRACE: Duration = Duration::from_secs(30);

  mod opus_profiles {
    use super::*;

    #[test]
    fn the_microphone_keeps_its_voice_configuration() {
      let voice = OpusProfile::Voice;
      assert_eq!(voice.origin(), AudioOrigin::Microphone);
      assert_eq!(voice.bitrate_bps(), 32_000);
      assert_eq!(voice.tuning(), OpusTuning::default());
    }

    #[test]
    fn screen_sound_goes_out_tuned_for_music() {
      let sound = OpusProfile::ScreenSound;
      assert_eq!(sound.origin(), AudioOrigin::Screen);
      assert_eq!(sound.bitrate_bps(), 64_000);
      let tuning = sound.tuning();
      // A constant bitrate: packet sizes would tell what plays.
      assert!(
        tuning
          .config
          .contains(&("bitrateMode", TuningValue::Text("constant")))
      );
      assert!(tuning.opus.contains(&("application", TuningValue::Text("audio"))));
      // Quiet passages are sent, not suppressed.
      assert!(tuning.opus.contains(&("usedtx", TuningValue::Flag(false))));
    }

    /// Browsers send a shared screen's sound like the desktop does, so either side hears the same.
    #[cfg(feature = "native")]
    #[test]
    fn screen_sound_matches_the_desktops() {
      let desktop = crate::audio_codec::OpusEncoderConfig::screen_audio();
      assert_eq!(
        i32::try_from(OpusProfile::ScreenSound.bitrate_bps()).ok(),
        Some(desktop.bitrate_bps)
      );
      assert!(!desktop.has_dtx && !desktop.has_variable_bitrate);
      assert_eq!(desktop.channels, crate::audio_codec::AudioChannels::Mono);
    }
  }

  mod media_access {
    use super::*;

    #[test]
    fn refused_media_is_classified_by_the_exception_name() {
      for input in [MediaInput::Microphone, MediaInput::Camera, MediaInput::Screen] {
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
          assert_eq!(media_access_reason(input, name), reason, "{input:?} {name}");
        }
      }
    }

    /// Issue #930: Safari's refusal of a picker asked for after the click's activation was gone.
    #[test]
    fn a_screen_picker_asked_for_without_the_click_is_told_apart() {
      for name in ["InvalidAccessError", "InvalidStateError"] {
        assert_eq!(
          media_access_reason(MediaInput::Screen, name),
          MediaAccessReason::NoUserActivation,
          "{name}"
        );
        for input in [MediaInput::Microphone, MediaInput::Camera] {
          assert_eq!(
            media_access_reason(input, name),
            MediaAccessReason::Other,
            "{input:?} {name}"
          );
        }
      }
    }

    #[test]
    fn only_the_user_closing_the_screen_picker_counts_as_closed() {
      let closed = [
        // Chromium and Firefox.
        "Permission denied",
        // Safari.
        "The request is not allowed by the user agent or the platform in the current context, \
         possibly because the user denied permission.",
      ];
      for message in closed {
        assert!(
          screen_refusal(MediaAccessReason::Denied, message).is_screen_picker_closed(),
          "{message}"
        );
      }
      // Chromium, when macOS's Screen Recording permission or a policy refuses.
      assert!(
        !screen_refusal(MediaAccessReason::Denied, "Permission denied by system").is_screen_picker_closed()
      );
      for reason in [
        MediaAccessReason::NoUserActivation,
        MediaAccessReason::NotFound,
        MediaAccessReason::InUse,
        MediaAccessReason::Other,
      ] {
        assert!(
          !screen_refusal(reason, "Permission denied").is_screen_picker_closed(),
          "{reason:?}"
        );
      }
      let camera = WebCallError::MediaAccess {
        input: MediaInput::Camera,
        reason: MediaAccessReason::Denied,
        message: "Permission denied".into(),
      };
      assert!(!camera.is_screen_picker_closed());
      assert!(!WebCallError::Ended.is_screen_picker_closed());
    }

    fn screen_refusal(reason: MediaAccessReason, message: &str) -> WebCallError {
      WebCallError::MediaAccess {
        input: MediaInput::Screen,
        reason,
        message: message.into(),
      }
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

  mod own_video {
    use super::*;

    const CAMERA: VideoPublicationId = VideoPublicationId(0);
    const SCREEN: VideoPublicationId = VideoPublicationId(1);

    #[test]
    fn an_own_track_takes_the_stream_published_for_it() {
      let mut publications = vec![
        publication(VideoTrackKind::Camera, CAMERA),
        publication(VideoTrackKind::Screen, SCREEN),
      ];
      let others = CallEngineEvent::VideoPublished(video_track(8, 2, VideoTrackKind::Screen, Some(SCREEN)));
      assert_eq!(follow_publication(&mut publications, device(1), &others), None);
      let own = CallEngineEvent::VideoPublished(video_track(7, 1, VideoTrackKind::Screen, Some(SCREEN)));
      assert_eq!(follow_publication(&mut publications, device(1), &own), Some(1));
      assert_eq!(publications[0].own.maybe_stream_id, None);
      assert_eq!(publications[1].own.maybe_stream_id, Some(MediaStreamId(7)));
      // Another device's track going away changes nothing here.
      let gone = CallEngineEvent::VideoUnpublished(MediaStreamId(8));
      follow_publication(&mut publications, device(1), &gone);
      assert_eq!(publications[1].own.maybe_stream_id, Some(MediaStreamId(7)));
    }

    /// Issue #929: the camera is stopped and started again before the SFU answered. The answer to
    /// the stopped one is unpublished, and the new camera sends on the stream of its own request.
    #[test]
    fn a_camera_restarted_before_its_answer_sends_on_its_own_stream() {
      let now = Instant::now();
      let mut engine = joined_engine(now);
      let stopped = VideoPublicationId(4);
      engine.publish_video(stopped, VideoTrackKind::Camera, VideoCodec::H264, layers());
      // `stop_video`: the driver forgets the track and withdraws its publication.
      engine.withdraw_video(stopped);
      let restarted = VideoPublicationId(5);
      let mut publications = vec![publication(VideoTrackKind::Camera, restarted)];
      engine.publish_video(restarted, VideoTrackKind::Camera, VideoCodec::H264, layers());
      drain(&mut engine);
      for (seq, (stream, id)) in [(50, stopped), (51, restarted)].into_iter().enumerate() {
        let track = video_track(stream, 1, VideoTrackKind::Camera, Some(id));
        let answer = event(seq as u64 + 2, CallEvent::VideoPublished { track });
        engine.handle_signal(answer, now).unwrap();
      }
      let outputs = drain(&mut engine);
      assert!(
        outputs.contains(&EngineOutput::Signal(CallClientMessage::UnpublishVideo {
          stream_id: MediaStreamId(50)
        }))
      );
      assert_eq!(follow_outputs(&outputs, &mut publications), vec![0]);
      assert_eq!(publications[0].own.maybe_stream_id, Some(MediaStreamId(51)));
    }

    /// Issue #919: after an SFU restart the engine publishes the camera again; the web driver must
    /// send on the new stream, not the old one (dropped as not published, a black tile for others).
    #[test]
    fn a_rejoin_without_the_slot_moves_own_video_to_its_new_stream() {
      let now = Instant::now();
      let mut engine = joined_engine(now);
      let mut publications = vec![publication(VideoTrackKind::Camera, CAMERA)];
      engine.publish_video(CAMERA, VideoTrackKind::Camera, VideoCodec::H264, layers());
      let published = event(
        2,
        CallEvent::VideoPublished {
          track: video_track(50, 1, VideoTrackKind::Camera, Some(CAMERA)),
        },
      );
      engine.handle_signal(published, now).unwrap();
      assert_eq!(follow(&mut engine, &mut publications), vec![0]);
      assert_eq!(publications[0].own.maybe_stream_id, Some(MediaStreamId(50)));

      // The SFU restarted and lost the slot: the engine unpublishes and publishes again.
      engine.connection_lost();
      engine.rejoin("fresh".into(), now);
      follow(&mut engine, &mut publications);
      engine.handle_signal(event(1, joined(&[2])), now).unwrap();
      let outputs = drain(&mut engine);
      assert!(
        outputs.contains(&EngineOutput::Signal(CallClientMessage::PublishVideo {
          kind: VideoTrackKind::Camera,
          codec: VideoCodec::H264,
          layers: layers(),
          maybe_publication_id: Some(CAMERA),
        }))
      );
      follow_outputs(&outputs, &mut publications);
      assert_eq!(
        publications[0].own.maybe_stream_id, None,
        "the old stream is gone"
      );

      let republished = event(
        2,
        CallEvent::VideoPublished {
          track: video_track(51, 1, VideoTrackKind::Camera, Some(CAMERA)),
        },
      );
      engine.handle_signal(republished, now).unwrap();
      assert_eq!(follow(&mut engine, &mut publications), vec![0]);
      assert_eq!(publications[0].own.maybe_stream_id, Some(MediaStreamId(51)));
      let frame = EncodedVideoFrame {
        codec: VideoCodec::H264,
        layer: SimulcastLayer::new(0).unwrap(),
        timestamp: EncodedVideoFrame::rtp_timestamp(0),
        is_keyframe: true,
        data: vec![0, 0, 0, 1, 0x65],
      };
      assert_eq!(
        engine.send_video(MediaStreamId(50), &frame, now),
        VideoSendOutcome::NotPublished
      );
      assert_ne!(
        engine.send_video(MediaStreamId(51), &frame, now),
        VideoSendOutcome::NotPublished
      );
    }

    /// Feeds the engine's events through the driver's bookkeeping; returns the indexes of tracks
    /// that got a stream.
    fn follow(engine: &mut CallEngine, publications: &mut [Publication]) -> Vec<usize> {
      follow_outputs(&drain(engine), publications)
    }

    fn follow_outputs(outputs: &[EngineOutput], publications: &mut [Publication]) -> Vec<usize> {
      outputs
        .iter()
        .filter_map(|output| match output {
          EngineOutput::Event(event) => follow_publication(publications.iter_mut(), device(1), event),
          _ => None,
        })
        .collect()
    }

    fn publication(kind: VideoTrackKind, id: VideoPublicationId) -> Publication {
      Publication {
        kind,
        own: OwnPublication::new(id),
      }
    }
  }

  mod video_errors {
    use super::*;

    #[test]
    fn a_failed_decode_asks_for_a_keyframe_and_the_call_goes_on() {
      let now = Instant::now();
      let mut engine = watching_engine(now);
      let mut view = VideoView::with_decoders(MediaStreamId(60), |_codec: &VideoCodec, _generation| {
        Ok(BrokenDecoder)
      });
      decode_video(&mut view, &remote_keyframe(), &mut engine);
      assert!(engine.is_in_call());
      assert_eq!(keyframe_requests(&mut engine), vec![MediaStreamId(60)]);
    }

    #[test]
    fn only_the_current_decoders_failure_asks_for_a_keyframe() {
      let now = Instant::now();
      let mut engine = watching_engine(now);
      let mut view = VideoView::with_decoders(MediaStreamId(60), |_codec: &VideoCodec, _generation| {
        Ok(WorkingDecoder)
      });
      decode_video(&mut view, &remote_keyframe(), &mut engine);
      recover_decoder(&mut view, MediaStreamId(60), 3, "replaced", &mut engine);
      assert!(keyframe_requests(&mut engine).is_empty());
      recover_decoder(&mut view, MediaStreamId(60), 0, "failed", &mut engine);
      assert_eq!(keyframe_requests(&mut engine), vec![MediaStreamId(60)]);
    }

    /// Throws like a decoder WebCodecs closed.
    struct BrokenDecoder;

    struct WorkingDecoder;

    impl FrameDecoder for BrokenDecoder {
      fn codec(&self) -> &VideoCodec {
        &VideoCodec::H264
      }

      fn is_configured(&self) -> bool {
        true
      }

      fn decode(&self, _frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
        Err(WebCallError::Browser {
          context: "VideoDecoder",
          message: "InvalidStateError".into(),
        })
      }
    }

    impl FrameDecoder for WorkingDecoder {
      fn codec(&self) -> &VideoCodec {
        &VideoCodec::H264
      }

      fn is_configured(&self) -> bool {
        true
      }

      fn decode(&self, _frame: &ReceivedVideoFrame) -> Result<(), WebCallError> {
        Ok(())
      }
    }

    /// In a call where device 2 publishes a camera on stream 60.
    fn watching_engine(now: Instant) -> CallEngine {
      let mut engine = joined_engine(now);
      let published = event(
        2,
        CallEvent::VideoPublished {
          track: video_track(60, 2, VideoTrackKind::Camera, None),
        },
      );
      engine.handle_signal(published, now).unwrap();
      engine.subscribe_video(MediaStreamId(60), Some(0));
      drain(&mut engine);
      engine
    }

    fn keyframe_requests(engine: &mut CallEngine) -> Vec<MediaStreamId> {
      drain(engine)
        .into_iter()
        .filter_map(|output| match output {
          EngineOutput::Signal(CallClientMessage::RequestKeyframe { stream_id }) => Some(stream_id),
          _ => None,
        })
        .collect()
    }

    fn remote_keyframe() -> ReceivedVideoFrame {
      ReceivedVideoFrame {
        sender: device(2),
        stream_id: MediaStreamId(60),
        codec: VideoCodec::H264,
        layer: SimulcastLayer::new(0).unwrap(),
        timestamp: crate::rtp::RtpTimestamp(0),
        is_keyframe: true,
        data: vec![0, 0, 0, 1, 0x65],
      }
    }
  }

  /// Device 1 in a call with device 2.
  fn joined_engine(now: Instant) -> CallEngine {
    let mut engine = CallEngine::new(
      CallEngineConfig {
        user_id: UserId::from_snowflake(Snowflake::new(1001).unwrap()),
        device_id: device(1),
        capabilities: MediaCapabilities::default(),
        bwe: BweConfig::default(),
      },
      now,
    );
    engine.join("t".into());
    engine.handle_signal(event(1, joined(&[2])), now).unwrap();
    drain(&mut engine);
    engine
  }

  fn joined(others: &[i64]) -> CallEvent {
    CallEvent::Joined {
      call_id: CallId::from_snowflake(Snowflake::new(99).unwrap()),
      participant: participant(1),
      participants: others.iter().map(|other| participant(*other)).collect(),
      video_tracks: Vec::new(),
      screen_audio_tracks: Vec::new(),
      maybe_reconnect_grace_ms: Some(30_000),
    }
  }

  fn participant(id: i64) -> CallParticipant {
    CallParticipant {
      user_id: UserId::from_snowflake(Snowflake::new(1000 + id).unwrap()),
      device_id: device(id),
      audio_stream_id: MediaStreamId(u32::try_from(id).unwrap()),
    }
  }

  fn video_track(
    stream: u32,
    owner: i64,
    kind: VideoTrackKind,
    maybe_publication_id: Option<VideoPublicationId>,
  ) -> VideoTrack {
    VideoTrack {
      stream_id: MediaStreamId(stream),
      device_id: device(owner),
      kind,
      codec: VideoCodec::H264,
      layers: layers(),
      maybe_publication_id,
    }
  }

  fn layers() -> Vec<VideoLayerInfo> {
    vec![VideoLayerInfo {
      layer: 0,
      width: 320,
      height: 180,
      max_framerate: 30,
      max_bitrate_bps: 150_000,
    }]
  }

  fn event(seq: u64, event: CallEvent) -> CallServerMessage {
    CallServerMessage::Event { seq, event }
  }

  fn drain(engine: &mut CallEngine) -> Vec<EngineOutput> {
    std::iter::from_fn(|| engine.poll_output()).collect()
  }

  fn device(id: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn framed(body: &[u8]) -> Vec<u8> {
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend_from_slice(body);
    bytes
  }
}
