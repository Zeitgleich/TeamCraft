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
//!
//! A call that had to take the WebSocket fallback tries QUIC again now and then where the browser
//! has WebTransport (#1338, as native clients do since #1309). A QUIC session that opens asks the
//! SFU to move the call's session over (`switch`, with the secret of its `joined`); once the SFU
//! says `switched`, everything goes out on QUIC, and the fallback is read up to its close before
//! the new session's signals reach the engine, so the engine sees one event sequence without a gap
//! (`switch_back`). The call keeps its slot and its DAVE group, and the others notice nothing.

mod cadence;
#[cfg(feature = "effects")]
mod effects_preview;
#[cfg(feature = "effects")]
mod effects_timing;
mod fallback;
#[cfg(feature = "effects")]
mod gl_compositor;
mod meter;
#[cfg(feature = "effects")]
mod picture;
mod screen_sound;
#[cfg(feature = "effects")]
mod segmentation_worker;
mod session_watch;
mod switch_back;
mod video;

#[cfg(feature = "effects")]
pub use effects_preview::{EffectsPreview, TimedEffects};
#[cfg(feature = "effects")]
pub use effects_timing::{COMPOSITING_KEY, TIMING_KEY};
#[cfg(feature = "effects")]
pub use gl_compositor::GlCompositor;
#[cfg(feature = "effects")]
pub use picture::decode_picture;
#[cfg(feature = "effects")]
pub use segmentation_worker::{SegmentationWorker, WorkerMask};
#[cfg(feature = "effects")]
pub use video::CameraEffects;
pub use video::CodecSupport;

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
  SFU_SWITCHED_CLOSE_CODE, ScreenShareTakeover, VideoCodec, VideoPublicationId, VideoTrack, VideoTrackKind,
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
use self::switch_back::{Answer, Due, Held, Moved, SwitchBack};
use self::video::{EncoderGaveUp, FrameDecoder, Publication, TrackSender, VideoView};

use crate::audio_frame::{
  EncodedAudioFrame, FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ, planned_loss_percent,
};
use crate::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource, sine_tone};
use crate::audio_level::{CallMeter, DEFAULT_VOICE_THRESHOLD_DBOV, SharedLevel, measure_audio_level};
use crate::call_auth::CallKeyAuthenticator;
use crate::call_cues::{CallCue, CuePlayer, StatusCues};
use crate::call_engine::{
  AudioOrigin, CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
  OwnVideoEnd,
};
use crate::call_resilience::{
  CallTransport, Jitter, JoinTicket, JoinTicketError, ReconnectSchedule, RejoinPacer, ResilienceConfig,
  connect_preferring_quic, fallback_delay, rejoin_attempt,
};
use crate::call_video::ReceivedVideoFrame;
use crate::capture_health::StallChange;
use crate::datagram::SimulcastLayer;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::receive_load::{
  DecodedFrame, FrameInterval, Importance, ReceiveLoad, ReceiveOverload, WatchedStream,
};
use crate::resample::MonoResampler;
use crate::rtp::AudioLevel;
use crate::share_policy::{ShareCodecs, ShareEncoders};
use crate::video::{EncodedVideoFrame, ScreenProfile, TOP_LAYER, VideoResolution, chosen_layer};
use crate::web_api::{
  AudioData, AudioDecoder, AudioEncoder, EncodedAudioChunk, QUIC_GLOBAL, WORKLET_SOURCE, WebTransport,
  WebTransportBidirectionalStream, datagram_writable, dictionary, has_global, missing_for_calls,
};

/// Audio frame period.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Audio frames one late pulse may catch up on (more are skipped); the playback holds 200 ms.
const MAX_FRAMES_PER_PULSE: u32 = 5;
/// Feedback and allocation period.
const TIMER_INTERVAL: Duration = Duration::from_millis(100);
/// The page timer's pulse period (the fallback clock; throttled in hidden tabs).
const PULSE_INTERVAL_MS: i32 = 10;
/// Pulses of the audio clock per second of audio (one every 10 ms).
const CLOCK_PULSES_PER_SECOND: u32 = 100;
/// Upper bound of one signaling message.
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
/// The playback worklet holds at most a second of audio divided by this (200 ms, latency bound).
const MAX_PLAYBACK_PER_SECOND: u32 = 5;
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
  /// The microphone's newest meter reading (#986).
  microphone_level: SharedLevel,
  video_decoders: Vec<VideoCodec>,
}

/// How to reach the SFU and what to send and play.
#[derive(Debug, Clone)]
pub struct WebCallConfig {
  /// WebTransport URL from the join token response, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  /// WebSocket URL of the SFU's fallback for networks that block UDP, from the join token response;
  /// used only when QUIC doesn't connect (ARCHITECTURE.md §6.6).
  pub maybe_fallback_url: Option<String>,
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
  /// The call's trusted devices afresh, when a device joins that the roster doesn't have yet (one
  /// added to the conversation during the call, e2ee.md §8.2). The page verifies with the core's
  /// devices itself; the module only needs the roster. Without one such a device ends the call.
  pub maybe_trust_source: Option<Rc<dyn WebTrustSource>>,
  /// Which call status sounds (reconnecting, reconnected, connection lost) play.
  pub status_cues: StatusCues,
  /// The microphone is muted from the first frame: nothing of it is sent before a later unmute.
  pub is_muted_at_start: bool,
  /// Where the page serves the AudioWorklet module (`call_web/teamcraft-call-worklet.js`). Pages
  /// whose Content-Security-Policy allows scripts only from their own origin need it; `None` loads
  /// the module from a blob URL.
  pub maybe_worklet_url: Option<String>,
  /// How this browser encodes AV1 and VP9 ([`CodecSupport::share_encoders`]): a screen share's
  /// top layer goes out in them at full size only from a hardware encoder (#1733).
  pub share_encoders: ShareEncoders,
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

/// Gets the call's trusted devices (the other devices) afresh from the page's client core.
pub trait WebTrustSource {
  fn fetch(&self) -> LocalTrustFuture;
}

/// A refreshed roster on its way (on the page's event loop); the error says why there is none.
pub type LocalTrustFuture = Pin<Box<dyn Future<Output = Result<Vec<DeviceId>, String>>>>;

impl std::fmt::Debug for dyn WebTrustSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("WebTrustSource")
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
  /// The audio graph refused a stream at its sample rate: Firefox before 148 connects a microphone
  /// only to a graph at the default device's rate (#1721). [`WebCall::join`] then runs the call's
  /// audio at that rate.
  #[error("the browser can't connect the audio at this sample rate: {0}")]
  SampleRate(String),
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
  /// A QUIC try's session opened and asked the SFU to take the call's session over (#1338); its
  /// signals and datagrams carry `generation`.
  SwitchAsked {
    transport: Transport,
    generation: u64,
  },
  /// A QUIC try's session didn't connect.
  SwitchFailed {
    generation: u64,
    error: WebCallError,
  },
  /// The trusted roster as the page's core knows it now ([`WebTrustSource`]).
  Trust(Result<Vec<DeviceId>, String>),
  /// A datagram of the session with this generation, and when it was read: the loop may get to
  /// it later on a busy machine (#1762).
  Datagram {
    generation: u64,
    bytes: Vec<u8>,
    read_at: Instant,
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
    /// What the layer's encoder encodes: a share's is AV1 where every viewer decodes it (#565).
    codec: VideoCodec,
    layer: u8,
    data: Vec<u8>,
    is_keyframe: bool,
    timestamp_us: f64,
    /// When the encoder handed the chunk out, stamped in its output callback: the call loop may
    /// handle it much later, and that wait is no encoding time (#1762).
    encoded_at: Instant,
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

/// What a layer's encoder put out, before it goes to the engine.
#[derive(Debug, Clone, PartialEq)]
struct EncodedChunk {
  codec: VideoCodec,
  layer: u8,
  data: Vec<u8>,
  is_keyframe: bool,
  timestamp_us: f64,
}

impl EncodedChunk {
  /// The frame the engine sends, in the codec the encoder encoded (#1733); `None` for a layer
  /// number out of range.
  fn into_frame(self) -> Option<EncodedVideoFrame> {
    Some(EncodedVideoFrame {
      codec: self.codec,
      layer: SimulcastLayer::new(self.layer).ok()?,
      // Encoder timestamps start at 0 and stay far below u64::MAX microseconds.
      timestamp: EncodedVideoFrame::rtp_timestamp(self.timestamp_us.max(0.0) as u64),
      is_keyframe: self.is_keyframe,
      data: self.data,
    })
  }
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
  /// The viewer picked at most these lines for a received track (`None`: Auto, #967).
  SetVideoQuality {
    stream_id: MediaStreamId,
    maybe_max_lines: Option<u16>,
  },
  SetStatusCues(StatusCues),
  /// Reads what the call's video does (stats for nerds, #938).
  #[cfg(feature = "call-view")]
  VideoStats(oneshot::Sender<teamcraft_client_api::video_stats::VideoStatsReport>),
  Leave,
  /// Leaves with the hang-up tone.
  HangUp,
}

/// How a session ended, as far as rejoining cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
  /// It broke, the SFU closed it for another reason, or the path went silent: rejoin at once.
  Lost,
  /// The SFU is shutting down (a restart, a deploy): rejoin after a random part of a short spread,
  /// so the restarted SFU doesn't get every client at once.
  GoingAway,
  /// The SFU moved the session to another one (#1338): everything queued for this one came before
  /// the close. The call goes on on the session it moved to; without one, it is lost.
  Switched,
}

/// The sessions the call holds open: the current one, lost ones until the call is back (closing
/// one tells the SFU this device left; it keeps the slot only for lost connections) and one still
/// connecting. Shared with [`PageExit`], which closes them all when the page goes away.
#[derive(Clone, Default)]
struct OpenSessions(Rc<RefCell<Vec<SessionHandle>>>);

/// A session as it is closed or asked about, whatever carries it.
#[derive(Clone, PartialEq)]
enum SessionHandle {
  Quic(WebTransport),
  Fallback(web_sys::WebSocket),
}

/// Closes a session that didn't finish opening, also when the race for the connection drops it.
struct OpeningSession<'a> {
  sessions: &'a OpenSessions,
  session: SessionHandle,
  is_open: bool,
}

/// Leaves the call from the page's `pagehide` handler, while the page can still send.
struct PageExit {
  window: web_sys::Window,
  listener: Closure<dyn FnMut()>,
}

/// The connection to the SFU.
struct Transport {
  session: SessionHandle,
  writers: Writers,
}

/// Where a session's signals and datagrams go.
enum Writers {
  Quic {
    signaling: WritableStreamDefaultWriter,
    datagrams: WritableStreamDefaultWriter,
  },
  Fallback(fallback::FallbackSession),
}

/// Web Audio graph: microphone capture (if any) and the playback worklet (if any).
struct WebAudio {
  context: AudioContext,
  maybe_capture: Option<Capture>,
  maybe_playback: Option<AudioWorkletNode>,
  /// The mix (48 kHz) to the graph's rate, where that differs.
  to_graph: RefCell<Option<MonoResampler>>,
}

/// The sample rate the call's audio graph runs at: the codec's 48 kHz, so samples go to and from
/// Opus as they are, or the browser's own where it connects a microphone only at the rate of its
/// default device (Firefox before 148, #1721). Captured samples are resampled to 48 kHz then, and
/// the mix from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GraphRate(u32);

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
    let ticket = JoinTicket {
      sfu_url: config.sfu_url.clone(),
      token: config.token.clone(),
      certificate_hashes: config.certificate_hashes.clone(),
      maybe_fallback_url: config.maybe_fallback_url.clone(),
    };
    let transport = Transport::connect_preferring_quic(
      &ticket,
      CallTransport::Quic,
      &config.resilience,
      &sessions,
      &inputs,
      0,
    )
    .await?;
    let (event_tx, events) = mpsc::unbounded();
    let (finished_tx, finished) = oneshot::channel();
    let task_inputs = inputs.clone();
    let microphone_level = SharedLevel::default();
    let meter = CallMeter::new(microphone_level.clone());
    let video_decoders = config.engine.capabilities.video_decoders.clone();
    spawn_local(async move {
      let channels = (task_inputs, input_rx, event_tx);
      let result = run_call(config, transport, sessions, audio, channels, meter).await;
      // The app may have dropped the call without waiting for the result.
      let _ = finished_tx.send(result);
    });
    Ok(Self {
      inputs,
      events,
      finished,
      microphone_level,
      video_decoders,
    })
  }

  /// The video codecs this device decodes, as the call announced them: others' tracks in another
  /// codec can't be shown here (#1793).
  pub fn video_decoders(&self) -> &[VideoCodec] {
    &self.video_decoders
  }

  /// The microphone's level (0.0 to 1.0) as the call captures it, also while muted, about ten
  /// readings a second; silence while no microphone delivers. For the call's device menu (#986).
  pub fn microphone_level(&self) -> SharedLevel {
    self.microphone_level.clone()
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

  /// Turns the camera on (three H.264 simulcast layers, from 1080p where the camera has it, sent
  /// within `limit`: `video::camera_limit`); `maybe_device` is a `MediaDeviceInfo.deviceId`. Call
  /// it from a user gesture, like `join`.
  pub async fn start_camera(
    &self,
    maybe_device: Option<&str>,
    limit: VideoResolution,
  ) -> Result<(), WebCallError> {
    let stream = video::capture_camera(maybe_device, limit).await?;
    self.share_camera_stream(stream, limit).await
  }

  /// Sends a camera stream the app opened itself ([`capture_camera`]) within `limit`, e.g. to
  /// show it as the self view too; replaces a running camera. Stopping the camera stops the
  /// stream's tracks.
  pub async fn share_camera_stream(
    &self,
    stream: MediaStream,
    limit: VideoResolution,
  ) -> Result<(), WebCallError> {
    self.start_video(stream, video::camera_plan(limit)).await
  }

  /// [`WebCall::start_camera`] with background effects (the segmentation worker and a compositor);
  /// change the effect with [`WebCall::set_background_effect`].
  #[cfg(feature = "effects")]
  pub async fn start_camera_with_effects(
    &self,
    maybe_device: Option<&str>,
    effects: CameraEffects,
    limit: VideoResolution,
  ) -> Result<(), WebCallError> {
    let stream = video::capture_camera(maybe_device, limit).await?;
    self
      .share_camera_stream_with_effects(stream, effects, limit)
      .await
  }

  /// [`WebCall::share_camera_stream`] through background effects (#439).
  #[cfg(feature = "effects")]
  pub async fn share_camera_stream_with_effects(
    &self,
    stream: MediaStream,
    effects: CameraEffects,
    limit: VideoResolution,
  ) -> Result<(), WebCallError> {
    let plan = video::TrackPlan {
      maybe_effects: Some(effects),
      ..video::camera_plan(limit)
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
    profile: &ScreenProfile,
    has_sound: bool,
  ) -> Result<bool, WebCallError> {
    let stream = video::capture_screen(profile, has_sound).await?;
    self.share_screen_stream(stream, profile).await
  }

  /// Shares a screen the app asked for itself ([`request_screen`]) as the screen share, replacing
  /// a running one, and its sound when the stream has an audio track (the user shared the tab's
  /// or the system's sound). Answers whether the sound is sent: a share whose sound can't be
  /// captured or encoded goes on without it (the track stops).
  pub async fn share_screen_stream(
    &self,
    stream: MediaStream,
    profile: &ScreenProfile,
  ) -> Result<bool, WebCallError> {
    let maybe_sound = screen_sound::sound_track(&stream);
    self.start_video(stream, video::screen_plan(profile)).await?;
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
      .start_video(stream, video::screen_plan(&ScreenProfile::default()))
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

  /// The viewer picked at most `maybe_max_lines` for a received track (`None`: Auto, by its
  /// tile; #967). Kept while the track is published; the SFU still caps it by what is sent and
  /// by the downlink.
  pub fn set_video_quality(&self, stream_id: MediaStreamId, maybe_max_lines: Option<u16>) {
    let _ = self.command(Command::SetVideoQuality {
      stream_id,
      maybe_max_lines,
    });
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

  /// Leaves a 1:1 call that ends for both (issue #997): the SFU hears of the leave at once, then
  /// the hang-up tone plays out (as the status sounds setting allows) before the audio closes.
  pub async fn hang_up(mut self) -> Result<CallStats, WebCallError> {
    let _ = self.inputs.unbounded_send(Input::Command(Command::HangUp));
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

/// Asks for the camera (`getUserMedia`) at the size of `limit` (`video::camera_limit`), or what
/// comes closest; `maybe_device` is a `MediaDeviceInfo.deviceId`. Call it from a user gesture.
pub async fn capture_camera(
  maybe_device: Option<&str>,
  limit: VideoResolution,
) -> Result<MediaStream, WebCallError> {
  video::capture_camera(maybe_device, limit).await
}

/// Opens the browser's screen picker (`getDisplayMedia`) at once and returns its promise. Call it
/// synchronously in the click handler, before anything is awaited: Safari refuses the picker once
/// the click's user activation is gone (issue #930). [`picked_screen`] waits for the choice. With
/// `has_sound` the picker also offers the tab's or the system's sound, where the browser can
/// capture it (Chromium).
pub fn request_screen(profile: &ScreenProfile, has_sound: bool) -> Result<Promise, WebCallError> {
  video::request_screen(profile, has_sound)
}

/// What this browser encodes and decodes of the video codecs a call may use (#565), for the
/// call's [`MediaCapabilities`]: H.264 always (what every client sends and takes), AV1 where
/// WebCodecs offers it for a screen share's top layer (encoder) and at all (decoder).
pub async fn video_codec_support() -> video::CodecSupport {
  video::codec_support().await
}

/// The screen the user chose in a picker [`request_screen`] opened (possibly in another wasm
/// module of the page); closing the picker is a [`MediaAccessReason::Denied`] refusal.
pub async fn picked_screen(request: Promise) -> Result<MediaStream, WebCallError> {
  video::picked_screen(request).await
}

/// Asks for the microphone (`getUserMedia`) with the browser's echo cancellation, noise
/// suppression and gain control, as the call sends it. A chosen microphone that is gone
/// (unplugged since it was picked) gives way to the default (#986).
async fn capture_microphone(choice: &AudioDeviceChoice) -> Result<MediaStream, WebCallError> {
  match open_microphone(choice).await {
    Err(error) if is_gone_device(choice, &error) => {
      warn!(%error, "the chosen microphone is gone; using the default");
      open_microphone(&AudioDeviceChoice::Default).await
    },
    other => other,
  }
}

/// Whether opening `choice` failed because the device isn't there (any more): a stored or picked
/// id the browser no longer lists. The default failing that way means there is no microphone.
fn is_gone_device(choice: &AudioDeviceChoice, error: &WebCallError) -> bool {
  matches!(choice, AudioDeviceChoice::Device(_))
    && matches!(
      error,
      WebCallError::MediaAccess {
        reason: MediaAccessReason::NotFound,
        ..
      }
    )
}

async fn open_microphone(choice: &AudioDeviceChoice) -> Result<MediaStream, WebCallError> {
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

/// Whether this browser has what calls need (WebCodecs audio, AudioWorklet; WebTransport only for
/// QUIC, the fallback does without it); the error names the first thing missing.
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
  meter: CallMeter,
) -> Result<CallStats, WebCallError> {
  let mut engine = CallEngine::new(config.engine.clone(), Instant::now());
  if let Some(roster) = &config.maybe_trusted_roster {
    engine.set_trusted_roster(roster.iter().copied());
  }
  if let Some(authenticator) = &config.maybe_authenticator {
    engine.set_authenticator(Arc::clone(authenticator));
  }
  if config.maybe_trust_source.is_some() {
    engine.enable_roster_refresh();
  }
  if config.is_muted_at_start {
    engine.set_muted(true);
  }
  let _page_exit = PageExit::watch(&sessions, &inputs);
  let transport_kind = transport.kind();
  let mut state = CallState {
    maybe_transport: Some(transport),
    lost_transports: Vec::new(),
    sessions,
    watch: SessionWatch::new(Instant::now(), config.resilience),
    maybe_lost_at: None,
    lost_before_joining: 0,
    pacer: RejoinPacer::default(),
    maybe_refused_retry_after: None,
    maybe_hurry: None,
    timer_ticks: 0,
    ticket: JoinTicket {
      sfu_url: config.sfu_url.clone(),
      token: config.token.clone(),
      certificate_hashes: config.certificate_hashes.clone(),
      maybe_fallback_url: config.maybe_fallback_url.clone(),
    },
    call_transport: transport_kind,
    switch: SwitchBack::new(),
    has_webtransport: has_global(QUIC_GLOBAL),
    quic_jitter: config.jitter,
    resilience: config.resilience,
    maybe_ticket_source: config.maybe_ticket_source.clone(),
    cues: CuePlayer::new(config.status_cues),
    is_ending_lost: false,
    is_hanging_up: false,
    inputs: &inputs,
    events: &events,
    jitter: config.jitter,
    encoder: OpusEncoder::new(&inputs, OpusProfile::Voice)?,
    maybe_screen_sound: None,
    remotes: BTreeMap::new(),
    meter,
    source: config.source.clone(),
    tone_position: 0,
    is_muted: config.is_muted_at_start,
    is_deafened: false,
    stats: CallStats::default(),
    own_device: config.engine.device_id,
    senders: Vec::new(),
    has_told_undecodable: false,
    share_encoders: config.share_encoders,
    next_publication: VideoPublicationId(0),
    encoded_kinds: Vec::new(),
    views: HashMap::new(),
    receive_load: ReceiveLoad::new(Instant::now()),
    maybe_datagram_read_at: None,
    remote_tracks: HashMap::new(),
    qualities: HashMap::new(),
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
  state.use_transport(transport_kind);
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
    if engine.take_roster_request()
      && let Some(source) = &config.maybe_trust_source
    {
      info!("a device the call's roster doesn't have yet: asking the page");
      let (fetch, inputs) = (source.fetch(), inputs.clone());
      spawn_local(async move {
        // The call may have ended meanwhile.
        let _ = inputs.unbounded_send(Input::Trust(fetch.await));
      });
    }
    // Refused, ended by the host or replaced by another connection: nothing to rejoin.
    if engine.is_over() {
      break Ok(());
    }
    if engine.is_in_call() {
      if state.maybe_lost_at.is_some() {
        state.pacer.on_back(Instant::now());
      }
      state.maybe_lost_at = None;
      state.lost_before_joining = 0;
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
  stats.encryption = engine.encryption_stats();
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
  /// Sessions in a row that were lost before the SFU took the join, which paces the next attempt.
  lost_before_joining: u32,
  /// Pauses rejoins when the call keeps dropping right after each one.
  pacer: RejoinPacer,
  /// The SFU turned the last join away for now and asked to wait this long (#1227).
  maybe_refused_retry_after: Option<Duration>,
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
  /// The call was left with the hang-up tone: the sessions are closed, and the call ends once
  /// the tone has played. Nothing but the audio clock counts any more.
  is_hanging_up: bool,
  inputs: &'a mpsc::UnboundedSender<Input>,
  events: &'a mpsc::UnboundedSender<CallEngineEvent>,
  /// For the remote streams of the current session's path.
  jitter: JitterBufferConfig,
  /// What the call's configuration says, for QUIC.
  quic_jitter: JitterBufferConfig,
  /// What the current (or last) session went over: a rejoin needn't wait for QUIC if that was the
  /// fallback.
  call_transport: CallTransport,
  /// Back to QUIC without a rejoin while on the fallback (#1338).
  switch: SwitchBack<Transport>,
  /// Safari before 26.4 has no WebTransport: a call there stays on the fallback.
  has_webtransport: bool,
  encoder: OpusEncoder,
  /// The running screen share's sound, while it is sent.
  maybe_screen_sound: Option<ScreenSound>,
  remotes: BTreeMap<RemoteStream, RemoteAudio>,
  /// Measures the microphone for the call's device menu (#986).
  meter: CallMeter,
  source: AudioSource,
  tone_position: u64,
  is_muted: bool,
  is_deafened: bool,
  stats: CallStats,
  own_device: DeviceId,
  senders: Vec<TrackSender>,
  /// Whether the user heard that an own track goes out in a codec not everyone decodes (#1793).
  has_told_undecodable: bool,
  /// How this browser encodes AV1 and VP9, for a share's plan (#1733).
  share_encoders: ShareEncoders,
  /// The id the next own video publication goes under.
  next_publication: VideoPublicationId,
  /// Kinds whose running own track encoded a picture: one that ends before did not start.
  encoded_kinds: Vec<VideoTrackKind>,
  views: HashMap<MediaStreamId, Watched>,
  /// Whether the received video's decoders keep up on this device; holds tiles below what they
  /// want while they don't (#1762).
  receive_load: ReceiveLoad,
  /// When the datagram being handled was read, while its frames come out of the engine.
  maybe_datagram_read_at: Option<Instant>,
  /// Other devices' published tracks: their kind and layers decide which layer a tile asks for.
  remote_tracks: HashMap<MediaStreamId, VideoTrack>,
  /// The lines the viewer picked per received track (#967); Auto where none. Kept while the
  /// track is published, so its tile shown again asks for the same.
  qualities: HashMap<MediaStreamId, u16>,
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
  /// The layer the tile's size and the viewer's quality choice ask for.
  wanted_layer: u8,
  /// The layer asked of the SFU: [`Self::wanted_layer`], less what this device's load holds back.
  max_layer: u8,
  /// The time between the stream's frames, for weighing a decoder's lateness.
  frame_interval: FrameInterval,
  /// Nothing was decoded while the tile was hidden: it needs a keyframe once shown.
  is_waiting_for_keyframe: bool,
  /// Reports the canvas' size as [`Input::VideoResized`]; `None` where the browser can't.
  maybe_size_watch: Option<video::SizeWatch>,
}

impl Watched {
  /// A tile laid out at zero size (hidden, collapsed). Without a size watch the size is never
  /// told again, so such a tile counts as shown.
  fn is_hidden(&self) -> bool {
    self.maybe_size_watch.is_some() && (self.shown.width == 0 || self.shown.height == 0)
  }
}

impl CallState<'_> {
  async fn handle(
    &mut self,
    input: Input,
    engine: &mut CallEngine,
    audio: &mut WebAudio,
  ) -> Result<Flow, WebCallError> {
    // Left already: only the tone plays out (or the page leaves for good).
    if self.is_hanging_up && !matches!(input, Input::Pulse | Input::Command(Command::Leave)) {
      return Ok(Flow::Continue);
    }
    match input {
      Input::Signal { generation, message } => {
        if self.watch.is_current(generation) {
          return self.on_signal(engine, message);
        }
        self.on_other_signal(engine, generation, message);
      },
      Input::SessionEnded { generation, end } => {
        if self.watch.is_current(generation) {
          return self.on_session_ended(engine, end);
        }
        if let Some(transport) = self
          .switch
          .on_ended(generation, end, Instant::now(), &self.resilience)
        {
          self.sessions.close(&transport.session);
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
        self.use_transport(transport.kind());
        // Tracked in `OpenSessions` since it started connecting.
        self.maybe_transport = Some(transport);
        self.watch.relinked(generation, now);
        self.maybe_hurry = None;
        engine.rejoin(ticket.token.clone(), now);
        self.ticket = ticket;
      },
      Input::Trust(fetched) => {
        match fetched {
          Ok(roster) => engine.set_trusted_roster(roster),
          Err(reason) => warn!(%reason, "the call's devices could not be refreshed"),
        }
        engine.resume_after_roster()?;
      },
      Input::ReconnectExpired => {
        self.is_ending_lost = true;
        self.cues.play(CallCue::ConnectionLost);
      },
      Input::SwitchAsked {
        transport,
        generation,
      } => self.on_switch_asked(transport, generation),
      Input::SwitchFailed { generation, error } => {
        debug!(%error, "QUIC try failed");
        self
          .switch
          .failed_to_open(generation, Instant::now(), &self.resilience);
      },
      Input::Datagram {
        generation,
        bytes,
        read_at,
      } => {
        // The arrival is when the datagram was read, not when this loop got to it: a busy
        // machine's delay isn't the network's (#1762). Frames it completes are late by the rest.
        self.maybe_datagram_read_at = Some(read_at);
        if self.watch.is_current(generation) {
          self.watch.on_heard(Instant::now());
          engine.handle_datagram(&bytes, read_at);
        } else if let Some(bytes) = self.switch.on_datagram(generation, bytes) {
          // The session the call moves to: its media plays at once.
          engine.handle_datagram(&bytes, read_at);
        }
      },
      Input::Captured {
        origin: AudioOrigin::Microphone,
        samples,
      } => {
        self.meter.push(&samples, Instant::now());
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
        codec,
        layer,
        data,
        is_keyframe,
        timestamp_us,
        encoded_at,
      } => {
        if let Some(sender) = self.sender_of_kind(&kind)
          && let Err(gave_up) = sender.on_encoded(layer, data.len(), timestamp_us, encoded_at)
        {
          self.on_encoders_gave_up(engine, &kind, gave_up);
        }
        let encoded = EncodedChunk {
          codec,
          layer,
          data,
          is_keyframe,
          timestamp_us,
        };
        self.send_video_frame(engine, &kind, encoded);
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
          // Shown again after nothing was decoded while hidden (#1762).
          if watched.is_waiting_for_keyframe && !watched.is_hidden() {
            watched.is_waiting_for_keyframe = false;
            engine.request_keyframe(stream_id, Instant::now());
          }
        }
        self.update_layer(engine, stream_id);
      },
      #[cfg(feature = "call-view")]
      Input::RoundTrip(maybe_rtt_ms) => self.maybe_rtt_ms = maybe_rtt_ms,
      Input::Command(command) => return self.on_command(command, engine, audio).await,
    }
    Ok(Flow::Continue)
  }

  /// The engine's video stats with what the browser adds: the encoders, the capture size, frames
  /// taken and those the encoders had no room for, and the round trip as last read. Starts the next reading of the round trip:
  /// the stats are read once a second while someone looks at them.
  #[cfg(feature = "call-view")]
  fn video_stats(&self, engine: &CallEngine) -> teamcraft_client_api::video_stats::VideoStatsReport {
    use teamcraft_client_api::video_stats::{
      EncoderImplementation, VideoEncoderInfo, VideoSize, VideoTrackStats,
    };

    let at_ms = u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = engine.video_stats(at_ms);
    report.maybe_rtt_ms = match self.call_transport {
      CallTransport::Quic => self.maybe_rtt_ms,
      CallTransport::Fallback => self.watch.round_trip_ms(),
    };
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
      let (given, skipped) = sender.encoder_frame_counts();
      sent.maybe_frames_given = Some(given);
      sent.maybe_frames_skipped = Some(skipped);
      sent.maybe_camera_limits = sender.camera_limits().map(camera_limits_stats);
      sent.maybe_share_limits = sender.share_limits().map(crate::share_policy::ShareLimits::stats);
      sent.maybe_capture_buffers = Some(sender.capture_counts().into());
    }
    self.add_decoding_stats(&mut report);
    if let Some(transport) = &self.maybe_transport
      && transport.kind() == CallTransport::Quic
    {
      let session = transport.session.clone();
      let inputs = self.inputs.clone();
      spawn_local(async move {
        let maybe_rtt_ms = round_trip_ms(&session).await;
        // The call ended meanwhile if nobody reads the inputs.
        let _ = inputs.unbounded_send(Input::RoundTrip(maybe_rtt_ms));
      });
    }
    report
  }

  /// What this device's decoders do with each received track (#1762): the queue now, and the
  /// layer this device holds it to for its own load.
  #[cfg(feature = "call-view")]
  fn add_decoding_stats(&self, report: &mut teamcraft_client_api::video_stats::VideoStatsReport) {
    use teamcraft_client_api::video_stats::{DecodeOverload, DeviceLimitStats, VideoTrackStats};

    let maybe_reason = self.receive_load.maybe_reason().map(|reason| match reason {
      ReceiveOverload::QueuedFrames => DecodeOverload::QueuedFrames,
      ReceiveOverload::LateFrames => DecodeOverload::LateFrames,
    });
    let unlimited: HashMap<MediaStreamId, u8> = self
      .watched_streams()
      .iter()
      .map(|stream| (stream.stream_id, stream.unlimited_layer()))
      .collect();
    for track in &mut report.tracks {
      let VideoTrackStats::Received(received) = track else {
        continue;
      };
      let Some(watched) = self.views.get(&received.stream_id) else {
        continue;
      };
      received.maybe_decode_queue = Some(watched.view.queue_size());
      let is_held_back = unlimited
        .get(&received.stream_id)
        .is_some_and(|layer| watched.max_layer < *layer);
      received.maybe_device_limit = is_held_back.then_some(DeviceLimitStats {
        layer: watched.max_layer,
        maybe_reason,
      });
    }
  }

  /// Leaves the call at once (the SFU frees the slot, and the other side of a 1:1 call hears of
  /// the end without waiting), then lets the hang-up tone play out. Nothing is sent any more.
  fn hang_up(&mut self) {
    info!("hanging up");
    self.sessions.close_all();
    self.maybe_transport = None;
    self.lost_transports.clear();
    drop(self.switch.stop());
    self.is_muted = true;
    self.is_hanging_up = true;
    self.cues.play(CallCue::HungUp);
  }

  /// Runs what fell due since the last pulse: audio frames, the feedback timer, video captures.
  async fn on_pulse(&mut self, engine: &mut CallEngine, audio: &WebAudio) -> Result<Flow, WebCallError> {
    self.pulses.taken();
    let now = Instant::now();
    // The fallback's paced upload: what its pacer lets out since the last pulse (#1848).
    if let Some(session) = self.sending_transport().and_then(Transport::fallback) {
      session.pump(now);
    }
    self.meter.tick(now);
    for _ in 0..self.frame_cadence.due(now) {
      self.on_frame_tick(audio)?;
      if self.is_ending_lost && !self.cues.is_playing(CallCue::ConnectionLost) {
        return Err(WebCallError::ConnectionLost);
      }
      if self.is_hanging_up && !self.cues.is_playing(CallCue::HungUp) {
        return Ok(Flow::Leave);
      }
    }
    // No keep-alives, rejoins or video once left.
    if self.is_hanging_up {
      return Ok(Flow::Continue);
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
      // A camera whose encoders don't keep up leaves its top layers out (#1762); the engine
      // ignores a limit it already has.
      if let Some(sender) = self.sender_of_kind(&kind)
        && let Some(stream_id) = sender.publication.own.maybe_stream_id
      {
        engine.limit_own_layers(stream_id, sender.maybe_top_layer());
      }
    }
    // A share's top layer follows what the uplink carries (#1733).
    let mut gave_up = Vec::new();
    for sender in &mut self.senders {
      if let Some(stream_id) = sender.publication.own.maybe_stream_id
        && let Some(top) = engine.uplink_top(stream_id)
        && let Err(layer) = sender.on_uplink(top, now)
      {
        gave_up.push((sender.publication.kind.clone(), layer));
      }
    }
    for (kind, layer) in gave_up {
      self.on_encoders_gave_up(engine, &kind, layer);
    }
    // A share whose CPU ladder or uplink changed its layers goes out anew (#1733).
    self.publish_replans(engine);
    Ok(Flow::Continue)
  }

  fn on_timer_tick(&mut self, engine: &mut CallEngine, now: Instant) -> Result<Flow, WebCallError> {
    self.follow_upload_pacing(engine);
    engine.on_timer(now);
    self.timer_ticks = self.timer_ticks.wrapping_add(1);
    if self.timer_ticks.is_multiple_of(STATS_EVERY_TICKS) {
      self.request_path_stats();
    }
    self.judge_receive_load(engine, now);
    self.judge_captures(engine, now);
    let tick = self.watch.on_tick(now);
    if let Some(id) = tick.maybe_ping {
      self.send_ping(id)?;
    }
    if let Flow::Leave = self.on_path_verdict(engine, tick.verdict) {
      return Ok(Flow::Leave);
    }
    Ok(self.on_switch_tick(engine, now))
  }

  /// Asks for the layer a watched track's tile size calls for, when it changed.
  fn update_layer(&mut self, engine: &mut CallEngine, stream_id: MediaStreamId) {
    let Some(watched) = self.views.get_mut(&stream_id) else {
      return;
    };
    let Some(track) = self.remote_tracks.get(&stream_id) else {
      return;
    };
    let maybe_max_lines = self.qualities.get(&stream_id).copied();
    watched.wanted_layer = chosen_layer(&track.kind, &track.layers, watched.shown, maybe_max_lines);
    self.ask_for_layers(engine);
  }

  /// Asks for each watched track what its tile wants, less what this device's load holds back
  /// (#1762), where that changed.
  fn ask_for_layers(&mut self, engine: &mut CallEngine) {
    let layers = self.receive_load.layers(&self.watched_streams());
    for (stream_id, max_layer) in layers {
      if let Some(watched) = self.views.get_mut(&stream_id)
        && watched.max_layer != max_layer
      {
        debug!(
          stream_id = stream_id.0,
          max_layer,
          wanted_layer = watched.wanted_layer,
          shown = ?watched.shown,
          "video tile asks for another layer"
        );
        watched.max_layer = max_layer;
        engine.set_video_max_layer(stream_id, max_layer);
      }
    }
  }

  /// The watched tracks as this device's receive load weighs them.
  fn watched_streams(&self) -> Vec<WatchedStream> {
    self
      .views
      .iter()
      .map(|(stream_id, watched)| {
        let maybe_track = self.remote_tracks.get(stream_id);
        let kind = maybe_track.map_or(VideoTrackKind::Camera, |track| track.kind.clone());
        WatchedStream {
          stream_id: *stream_id,
          importance: Importance::new(&kind, watched.shown),
          wanted_layer: watched.wanted_layer,
          offered_layers: maybe_track
            .map(|track| track.layers.iter().map(|info| info.layer).collect())
            .unwrap_or_default(),
        }
      })
      .collect()
  }

  /// Judges whether this device decodes what it receives (#1762), every timer tick: a step asks
  /// the SFU for other layers, and the UI hears when the device starts or stops holding video
  /// back. Nothing watched, nothing to judge.
  fn judge_receive_load(&mut self, engine: &mut CallEngine, now: Instant) {
    if self.views.is_empty() {
      self.receive_load.measure_afresh(now);
    }
    let was_limited = self.receive_load.is_limited();
    if self.receive_load.evaluate(&self.watched_streams(), now) {
      info!(
        is_limited = self.receive_load.is_limited(),
        reason = ?self.receive_load.maybe_reason(),
        "received video's decoders: layers changed for this device's load"
      );
      self.ask_for_layers(engine);
    }
    let is_limited = self.receive_load.is_limited();
    if is_limited != was_limited {
      // The app dropped the call if nobody reads the events.
      let _ = self
        .events
        .unbounded_send(CallEngineEvent::DecodeLimited(is_limited));
    }
  }

  /// Judges whether each own track's capture still gives pictures (#1805), every timer tick: a
  /// stall tells the viewers (in the track's frames), the sharer and the call's crash reports,
  /// as on the desktop.
  fn judge_captures(&mut self, engine: &mut CallEngine, now: Instant) {
    for sender in &mut self.senders {
      let Some(change) = sender.judge_capture(now) else {
        continue;
      };
      let kind = sender.publication.kind.clone();
      let event = match change {
        StallChange::Stalled(window) => {
          warn!(?kind, ?window, "own capture gives no new pictures");
          engine.set_own_capture_stalled(&kind, true);
          CallEngineEvent::CaptureStalled { kind, window }
        },
        StallChange::Recovered => {
          info!(?kind, "own capture gives pictures again");
          engine.set_own_capture_stalled(&kind, false);
          CallEngineEvent::CaptureRecovered(kind)
        },
      };
      // The app may have stopped listening; the call goes on.
      let _ = self.events.unbounded_send(event);
    }
  }

  fn flush(&mut self, engine: &mut CallEngine) -> Result<(), WebCallError> {
    while let Some(output) = engine.poll_output() {
      match output {
        // While rejoining there is no session; the engine renews what matters when it is back.
        EngineOutput::Signal(message) => {
          if let Some(transport) = self.sending_transport() {
            transport.send_signal(&message)?;
          }
        },
        EngineOutput::Datagram(bytes) => {
          if let Some(transport) = self.sending_transport() {
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
          let Some(watched) = self.views.get_mut(&frame.stream_id) else {
            continue;
          };
          // No work for a picture nobody sees (#1762): its decoder closes, and a keyframe is
          // asked for once the tile shows again.
          if watched.is_hidden() {
            watched.view.close();
            watched.is_waiting_for_keyframe = true;
            continue;
          }
          let queued = watched.view.queue_size();
          let interval = watched.frame_interval.next(frame.timestamp);
          decode_video(&mut watched.view, &frame, engine);
          let late_by = self.maybe_datagram_read_at.map_or(Duration::ZERO, |read_at| {
            Instant::now().saturating_duration_since(read_at)
          });
          self.receive_load.on_frame(
            frame.stream_id,
            DecodedFrame {
              queued,
              late_by,
              interval,
            },
            Instant::now(),
          );
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
            CallEngineEvent::VideoUnpublished(_) => {
              self.on_publication(&event);
              // The steps a gone tile held go to the others (#1762).
              self.ask_for_layers(engine);
            },
            CallEngineEvent::ScreenShareTakenOver(takeover) => {
              // The app hears it as the share's end.
              self.on_screen_share_taken_over(engine, *takeover);
              continue;
            },
            CallEngineEvent::DecodableVideoCodecs(_) => self.follow_codecs(engine),
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
            CallEngineEvent::LayerBitrates { stream_id, bitrates } => {
              if let Some(sender) = self.sender_of(*stream_id)
                && let Err(gave_up) = sender.set_layer_bitrates(bitrates)
              {
                let kind = sender.publication.kind.clone();
                self.on_encoders_gave_up(engine, &kind, gave_up);
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
    // Frames out of the next input (a timer's recovered packets) weren't completed by this read.
    self.maybe_datagram_read_at = None;
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
      SessionEnd::Switched => warn!("the SFU moved the call to a session this page gave up on, rejoining"),
    }
    if let Some(transport) = self.maybe_transport.take() {
      self.lost_transports.push(transport);
    }
    // A session the SFU may have moved the call's to holds the slot as well: kept open like the lost
    // one (#1338). A try still connecting is kept when it opens (`on_switch_asked`).
    self.lost_transports.extend(self.switch.stop());
    if !engine.is_in_call() {
      self.lost_before_joining += 1;
    }
    let now = Instant::now();
    engine.connection_lost(now);
    let is_new_loss = self.maybe_lost_at.is_none();
    let lost_at = *self.maybe_lost_at.get_or_insert(now);
    let grace = engine
      .maybe_reconnect_grace()
      .unwrap_or(self.resilience.default_reconnect_grace);
    // From here on, news of the lost session is stale.
    let generation = self.watch.lose();
    let mut schedule = reconnect_schedule(lost_at, grace, end, now, &self.resilience, Jitter::random());
    // The schedule is new, the attempts before it aren't.
    schedule.count_lost_before_joining(self.lost_before_joining, now, &self.resilience);
    if is_new_loss {
      schedule.pause_first_attempt(now, self.pacer.on_lost(now, &self.resilience));
    }
    if let Some(retry_after) = self.maybe_refused_retry_after.take() {
      schedule.on_join_refused(now, retry_after, &self.resilience);
    }
    let (hurry, hurry_rx) = mpsc::unbounded();
    self.maybe_hurry = Some(hurry);
    self.spawn_reconnect(schedule, hurry_rx, generation);
    Flow::Continue
  }

  /// A signal of the current session.
  fn on_signal(&mut self, engine: &mut CallEngine, message: CallServerMessage) -> Result<Flow, WebCallError> {
    let now = Instant::now();
    if let CallServerMessage::Pong { id } = message {
      let is_pacers = [self.sending_transport(), self.maybe_transport.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(Transport::fallback)
        .any(|session| session.on_pong(id, now));
      if is_pacers {
        // The upload's pacer pinged: the answer proves the path all the same.
        self.watch.on_heard(now);
      } else {
        self.watch.on_pong(id, now);
      }
      return Ok(Flow::Continue);
    }
    self.watch.on_heard(now);
    engine.handle_signal(message, now)?;
    if let Some(retry_after) = engine.take_join_refusal() {
      // Joined this call too often (#1227), or a resumed DAVE group failed its checks: this session
      // is of no use, the next attempt waits as asked (at once after a failed resume), and the call
      // goes on.
      info!(?retry_after, "joining the call again on a new connection");
      self.maybe_refused_retry_after = Some(retry_after);
      return Ok(self.on_session_lost(engine, SessionEnd::Lost));
    }
    Ok(Flow::Continue)
  }

  /// A signal of another session than the current one: a QUIC try's answer, or the session the call
  /// moves to (held back until the old one closed). Others' are stale: a session taken for lost may
  /// still be alive and hear `connection_replaced` for this device.
  fn on_other_signal(&mut self, engine: &mut CallEngine, generation: u64, message: CallServerMessage) {
    let now = Instant::now();
    match self.switch.on_signal(generation, message, now, &self.resilience) {
      Answer::Stale | Answer::Held => {},
      // The engine's output goes to the new session from now on (`sending_transport`).
      Answer::Switched { early } => {
        for bytes in early {
          engine.handle_datagram(&bytes, now);
        }
      },
      Answer::Refused(transport) => self.sessions.close(&transport.session),
    }
  }

  /// The current session ended: the call goes on on the session the SFU moved it to, or rejoins.
  fn on_session_ended(&mut self, engine: &mut CallEngine, end: SessionEnd) -> Result<Flow, WebCallError> {
    if end == SessionEnd::Switched
      && let Some(moved) = self.switch.finish()
    {
      return self.move_to(engine, moved);
    }
    Ok(self.on_session_lost(engine, end))
  }

  /// The old session brought everything queued for it and closed: the same session goes on over QUIC
  /// (#1338), without a rejoin. What the new one brought meanwhile follows, in order.
  fn move_to(&mut self, engine: &mut CallEngine, moved: Moved<Transport>) -> Result<Flow, WebCallError> {
    let now = Instant::now();
    self.watch.moved(moved.generation, now);
    if let Some(old) = self.maybe_transport.replace(moved.transport) {
      // The SFU reads it until this, for what was sent on it before the move.
      self.sessions.close(&old.session);
    }
    self.use_transport(CallTransport::Quic);
    engine.moved_to_another_path(now);
    info!("the call moved to QUIC");
    for bytes in moved.early {
      engine.handle_datagram(&bytes, now);
    }
    for held in moved.held {
      let flow = match held {
        Held::Signal(message) => self.on_signal(engine, *message)?,
        Held::Ended(end) => self.on_session_lost(engine, end),
      };
      if self.maybe_transport.is_none() || matches!(flow, Flow::Leave) {
        // Lost (or over) on the new session too: the rest is stale.
        return Ok(flow);
      }
    }
    Ok(Flow::Continue)
  }

  /// Keeps QUIC tries planned while the call is on the fallback, and starts the one that is due;
  /// a move whose old session doesn't close in time is a loss.
  fn on_switch_tick(&mut self, engine: &mut CallEngine, now: Instant) -> Flow {
    let may_try = self.has_webtransport
      && self.call_transport == CallTransport::Fallback
      && self.maybe_transport.is_some()
      && engine.maybe_switch_secret().is_some();
    match self.switch.on_tick(may_try, now, &self.resilience) {
      Due::Nothing => Flow::Continue,
      Due::Try => {
        self.start_switch(engine);
        Flow::Continue
      },
      Due::DrainTimedOut => {
        warn!("the fallback didn't bring what was queued for it after the move to QUIC");
        self.on_session_lost(engine, SessionEnd::Lost)
      },
    }
  }

  /// A QUIC try (#1338): a session to the ticket's SFU whose first message asks to take the call's
  /// session over. It reports back as [`Input::SwitchAsked`] or [`Input::SwitchFailed`].
  fn start_switch(&mut self, engine: &CallEngine) {
    let Some(secret) = engine.maybe_switch_secret().cloned() else {
      return;
    };
    let generation = self.watch.candidate();
    if let Some(overdue) = self.switch.start(generation) {
      self.sessions.close(&overdue.session);
    }
    debug!("trying QUIC again");
    let (inputs, sessions) = (self.inputs.clone(), self.sessions.clone());
    let (sfu_url, hashes) = (
      self.ticket.sfu_url.clone(),
      self.ticket.certificate_hashes.clone(),
    );
    // A network that still swallows UDP may leave the handshake hanging.
    let connect_timeout = self.resilience.idle_timeout;
    spawn_local(async move {
      let connect = Transport::connect(&sfu_url, &hashes, &sessions, &inputs, generation);
      let input = match select(pin!(connect), pin!(sleep(connect_timeout))).await {
        Either::Left((Ok(transport), _)) => {
          match transport.send_signal(&CallClientMessage::Switch { secret }) {
            Ok(()) => Input::SwitchAsked {
              transport,
              generation,
            },
            Err(error) => {
              sessions.close(&transport.session);
              Input::SwitchFailed { generation, error }
            },
          }
        },
        Either::Left((Err(error), _)) => Input::SwitchFailed { generation, error },
        // Dropping the attempt closes its session.
        Either::Right(_) => Input::SwitchFailed {
          generation,
          error: WebCallError::Browser {
            context: "WebTransport connect",
            message: "timed out".into(),
          },
        },
      };
      if let Err(unsent) = inputs.unbounded_send(input)
        && let Input::SwitchAsked { transport, .. } = unsent.into_inner()
      {
        // The call ended while this session connected: it must not linger.
        sessions.close(&transport.session);
      }
    });
  }

  /// A QUIC try's session opened and asked to take the call's session over.
  fn on_switch_asked(&mut self, transport: Transport, generation: u64) {
    let Err(transport) = self.switch.asked(generation, transport, Instant::now()) else {
      return;
    };
    if self.maybe_lost_at.is_some() {
      // The call lost its session while this one connected: the SFU may have moved it here in that
      // moment, so it holds the slot as well. Kept open like the lost one until the call is back
      // (#1338): closing it would read as a leave, and the rejoin would be a fresh join.
      self.lost_transports.push(transport);
    } else {
      self.sessions.close(&transport.session);
    }
  }

  /// Where the engine's output goes: the session the call moves to as soon as the SFU said so,
  /// while the old one is still read up to its close.
  fn sending_transport(&self) -> Option<&Transport> {
    self.switch.maybe_moving_to().or(self.maybe_transport.as_ref())
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

  /// On the fallback, the own layers follow what its upload drains, and an own stream whose video
  /// its queue dropped starts again with a keyframe (#1848). On QUIC, the feedback decides.
  fn follow_upload_pacing(&mut self, engine: &mut CallEngine) {
    let Some(session) = self.sending_transport().and_then(Transport::fallback) else {
      engine.set_uplink_capacity(None);
      return;
    };
    engine.set_uplink_capacity(Some(session.upload_rate_bps()));
    let needs = session.take_keyframe_needs();
    for stream_id in needs {
      if let Some(sender) = self.sender_of(MediaStreamId(stream_id.0)) {
        sender.request_keyframes();
      }
    }
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
    let session = transport.session.clone();
    let inputs = self.inputs.clone();
    spawn_local(async move {
      let count = received_packets(&session).await;
      // The call ended meanwhile if nobody reads the inputs.
      let _ = inputs.unbounded_send(Input::PathPackets { reading, count });
    });
  }

  /// The call runs over this now: the UI hears of it, and the jitter buffers suit its timing.
  fn use_transport(&mut self, transport: CallTransport) {
    self.call_transport = transport;
    // QUIC sessions have the browser's measure of the round trip; the fallback's is the pings'.
    self.watch.time_round_trip(transport == CallTransport::Fallback);
    self.jitter = match transport {
      CallTransport::Quic => self.quic_jitter,
      CallTransport::Fallback => JitterBufferConfig::for_fallback(),
    };
    for remote in self.remotes.values_mut() {
      remote.jitter.set_config(self.jitter);
    }
    // The app dropped the call if nobody reads the events.
    let _ = self
      .events
      .unbounded_send(CallEngineEvent::TransportChanged(transport));
  }

  /// Back in the call on a new session: the lost ones can go.
  fn close_lost_sessions(&mut self) {
    for transport in self.lost_transports.drain(..) {
      self.sessions.close(&transport.session);
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
    let current = self.call_transport;
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
            Transport::connect_preferring_quic(&ticket, current, &resilience, &sessions, &inputs, generation)
              .await
          }
        });
        match attempt.await {
          Ok((transport, ticket)) => {
            let session = transport.session.clone();
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
        // The mute stays: it lives in the call, not in the capture.
        let result = audio.switch(&source, &sink, self.inputs).await;
        match &result {
          Ok(()) => {
            self.source = source;
            self.meter.restart();
          },
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
        let maybe_max_lines = self.qualities.get(&stream_id).copied();
        let max_layer = self.remote_tracks.get(&stream_id).map_or(TOP_LAYER, |track| {
          chosen_layer(&track.kind, &track.layers, shown, maybe_max_lines)
        });
        let view = VideoView::new(stream_id, canvas, self.inputs);
        self.views.insert(
          stream_id,
          Watched {
            view,
            shown,
            wanted_layer: max_layer,
            max_layer,
            frame_interval: FrameInterval::default(),
            is_waiting_for_keyframe: false,
            maybe_size_watch,
          },
        );
        // What this device's load holds back holds for a new tile too (#1762).
        let max_layer = self
          .receive_load
          .layers(&self.watched_streams())
          .get(&stream_id)
          .copied()
          .unwrap_or(max_layer);
        if let Some(watched) = self.views.get_mut(&stream_id) {
          watched.max_layer = max_layer;
        }
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
          // The steps it held go to the others (#1762).
          self.ask_for_layers(engine);
        }
      },
      Command::SetVideoQuality {
        stream_id,
        maybe_max_lines,
      } => {
        match maybe_max_lines {
          Some(lines) => self.qualities.insert(stream_id, lines),
          None => self.qualities.remove(&stream_id),
        };
        self.update_layer(engine, stream_id);
      },
      Command::Leave => return Ok(Flow::Leave),
      Command::HangUp => self.hang_up(),
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
    let codecs = share_codecs(engine, &plan.codecs);
    // Within the layers the SFU takes (#978): a 1080p camera's 180p only where it takes four.
    let max_layers = engine.simulcast_layer_limit();
    let sender = TrackSender::start(
      stream,
      plan,
      codecs,
      self.share_encoders,
      max_layers,
      publication,
      self.inputs,
    )
    .await?;
    // The encoders are ours: a camera follows the uplink with its layers' bitrates (#939), a
    // share with its top layer's frame rate (#1733).
    engine.publish_video_adapting_in_place(publication, kind, sender.codec().clone(), sender.layers());
    self.senders.push(sender);
    // What the others decode may have changed while the capture started.
    self.follow_codecs(engine);
    Ok(())
  }

  /// Publishes own tracks anew in another codec where the best one every viewer decodes changed
  /// (#565: a device joined that can't decode AV1, or the last one that couldn't left). The
  /// capture goes on; viewers see the track end and the new one start, with keyframes. A track
  /// whose encoders can't open in the new codec keeps its own (viewers that can't decode it see
  /// nothing until it ends).
  fn follow_codecs(&mut self, engine: &mut CallEngine) {
    let mut gave_up = Vec::new();
    for sender in &mut self.senders {
      if sender.publication.kind == VideoTrackKind::Screen {
        // A share plans its layers' codecs itself (#1733); its new layers go out below.
        if let Err(layer) = sender.follow_share_codecs(share_codecs(engine, sender.codecs())) {
          gave_up.push((sender.publication.kind.clone(), layer));
        }
        continue;
      }
      let wanted = engine.video_codec_for(sender.codecs());
      if wanted == *sender.codec() {
        continue;
      }
      let publication = self.next_publication;
      let withdrawn = sender.publication.own.id;
      match sender.switch_codec(wanted.clone(), publication, self.inputs) {
        Ok(()) => {
          self.next_publication = VideoPublicationId(publication.0.wrapping_add(1));
          debug!(kind = ?sender.publication.kind, codec = %wanted, "video track goes out in another codec");
          engine.withdraw_video(withdrawn);
          engine.publish_video_adapting_in_place(
            publication,
            sender.publication.kind.clone(),
            wanted,
            sender.layers(),
          );
        },
        Err(error) => {
          warn!(%error, codec = %wanted, "video encoders can't switch codec; the track keeps its own")
        },
      }
    }
    for (kind, layer) in gave_up {
      self.on_encoders_gave_up(engine, &kind, layer);
    }
    self.publish_replans(engine);
    self.tell_if_undecodable(engine);
  }

  /// Tells the user once per call when an own track goes out in a codec some other device
  /// doesn't decode (#1793: this browser can't encode H.264, and a phone decodes nothing else).
  fn tell_if_undecodable(&mut self, engine: &CallEngine) {
    if self.has_told_undecodable {
      return;
    }
    let maybe_undecodable = self
      .senders
      .iter()
      .find(|sender| !engine.is_decodable_by_all(sender.codec()));
    if let Some(sender) = maybe_undecodable {
      self.has_told_undecodable = true;
      warn!(kind = ?sender.publication.kind, codec = %sender.codec(), "not every device in the call decodes this video");
      let _ = self
        .events
        .unbounded_send(CallEngineEvent::OwnVideoNotDecodableByAll);
    }
  }

  /// Publishes anew the own tracks whose layers changed beyond what their encoders follow in
  /// place (#1733: a share's middle layer stopped, its top's codec changed, or the viewers'
  /// codecs did). The capture goes on; viewers see the track end and the new one start, with
  /// keyframes. A track whose new encoders don't open keeps its layers.
  fn publish_replans(&mut self, engine: &mut CallEngine) {
    for sender in &mut self.senders {
      let Some(replan) = sender.take_replan() else {
        continue;
      };
      let publication = self.next_publication;
      let withdrawn = sender.publication.own.id;
      let layers = replan.layers.len();
      match sender.rebuild(replan, publication, self.inputs) {
        Ok(()) => {
          self.next_publication = VideoPublicationId(publication.0.wrapping_add(1));
          debug!(kind = ?sender.publication.kind, codec = %sender.codec(), layers, "video track goes out with other layers");
          engine.withdraw_video(withdrawn);
          engine.publish_video_adapting_in_place(
            publication,
            sender.publication.kind.clone(),
            sender.codec().clone(),
            sender.layers(),
          );
        },
        Err(error) => warn!(%error, "new video encoders didn't open; the track keeps its layers"),
      }
    }
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
      // A stalled track that goes takes its hint with it (#1805).
      if sender.is_capture_stalled() {
        let _ = self
          .events
          .unbounded_send(CallEngineEvent::CaptureRecovered(kind.clone()));
      }
      // Also before the SFU confirmed it: its answer is then unpublished when it comes (#929).
      engine.withdraw_video(sender.publication.own.id);
      sender.stop();
    }
  }

  /// An own track ended by itself (the browser ended its capture: "Stop sharing" in its bar, an
  /// unplugged camera, a revoked permission; or its encoders gave up): it stops, and the app
  /// hears about it, so the UI doesn't show the camera or share on any more (issue #920).
  fn end_video(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind) {
    let end = if self.encoded_kinds.contains(kind) {
      OwnVideoEnd::WhileSending
    } else {
      OwnVideoEnd::BeforeFirstPicture
    };
    self.end_video_with(engine, kind, end);
  }

  fn end_video_with(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind, end: OwnVideoEnd) {
    if self.sender_of_kind(kind).is_none() {
      // Stopped already: nothing ended by itself.
      return;
    }
    self.stop_video(engine, kind);
    // The app may have stopped listening; the call goes on.
    let _ = self.events.unbounded_send(CallEngineEvent::OwnVideoEnded {
      kind: kind.clone(),
      end,
    });
  }

  /// Another device's share replaced ours (a call has one screen share at a time): the capture
  /// and its sound stop as if the browser had ended them, and the app hears who took over.
  fn on_screen_share_taken_over(&mut self, engine: &mut CallEngine, takeover: ScreenShareTakeover) {
    if self.sender_of_kind(&VideoTrackKind::Screen).is_none() {
      return;
    }
    self.stop_video(engine, &VideoTrackKind::Screen);
    // The app may have stopped listening; the call goes on.
    let _ = self.events.unbounded_send(CallEngineEvent::OwnVideoEnded {
      kind: VideoTrackKind::Screen,
      end: OwnVideoEnd::TakenOver {
        by_user_id: takeover.by_user_id,
        by_device_id: takeover.by_device_id,
      },
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
    self.end_video_with(engine, kind, OwnVideoEnd::EncodingFailed);
  }

  /// Own tracks follow their publications ([`follow_publication`]); a track that is gone loses
  /// its view.
  fn on_publication(&mut self, event: &CallEngineEvent) {
    match event {
      CallEngineEvent::VideoUnpublished(stream_id) => {
        self.views.remove(stream_id);
        self.remote_tracks.remove(stream_id);
        self.qualities.remove(stream_id);
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

  fn send_video_frame(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind, encoded: EncodedChunk) {
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
    let Some(frame) = encoded.into_frame() else {
      return;
    };
    // A late frame of the encoders a codec switch replaced is refused (`WrongCodec`); the new
    // ones start with keyframes.
    engine.send_video(stream_id, &frame, Instant::now());
  }
}

impl Transport {
  /// The fallback session, where this runs on one.
  fn fallback(&self) -> Option<&fallback::FallbackSession> {
    match &self.writers {
      Writers::Fallback(session) => Some(session),
      Writers::Quic { .. } => None,
    }
  }

  /// A session to the ticket's SFU: QUIC, and the fallback next to it if the ticket offers one and
  /// QUIC is late or fails. Without one, exactly what QUIC does.
  async fn connect_preferring_quic(
    ticket: &JoinTicket,
    current: CallTransport,
    resilience: &ResilienceConfig,
    sessions: &OpenSessions,
    inputs: &mpsc::UnboundedSender<Input>,
    generation: u64,
  ) -> Result<Self, WebCallError> {
    // No WebTransport (Safari before 26.4): the fallback is the only way, and it needn't wait.
    if !has_global(QUIC_GLOBAL) {
      let Some(fallback_url) = &ticket.maybe_fallback_url else {
        return Err(WebCallError::Unsupported("WebTransport"));
      };
      debug!("no WebTransport in this browser: the call runs on the fallback");
      return Self::connect_fallback(fallback_url, &ticket.token, sessions, inputs, generation).await;
    }
    let quic = Self::connect(
      &ticket.sfu_url,
      &ticket.certificate_hashes,
      sessions,
      inputs,
      generation,
    );
    let Some(fallback_url) = &ticket.maybe_fallback_url else {
      return quic.await;
    };
    let starts = sleep(fallback_delay(current, resilience));
    let fallback = Self::connect_fallback(fallback_url, &ticket.token, sessions, inputs, generation);
    let (transport, kind) = connect_preferring_quic(quic, starts, fallback).await?;
    if kind == CallTransport::Fallback {
      info!("QUIC didn't connect, the call runs on the fallback");
    }
    Ok(transport)
  }

  fn kind(&self) -> CallTransport {
    match &self.writers {
      Writers::Quic { .. } => CallTransport::Quic,
      Writers::Fallback(_) => CallTransport::Fallback,
    }
  }

  /// Opens the WebSocket fallback, tracked as [`Self::connect`]'s session is. The join token goes
  /// along as a subprotocol, for the SFU to check before the upgrade (#1297).
  async fn connect_fallback(
    url: &str,
    token: &str,
    sessions: &OpenSessions,
    inputs: &mpsc::UnboundedSender<Input>,
    generation: u64,
  ) -> Result<Self, WebCallError> {
    let protocols = Array::new();
    for protocol in teamcraft_proto::fallback_protocols(token) {
      protocols.push(&JsValue::from_str(&protocol));
    }
    let socket =
      web_sys::WebSocket::new_with_str_sequence(url, &protocols).map_err(browser("call socket"))?;
    let session = SessionHandle::Fallback(socket.clone());
    sessions.track(&session);
    let mut opening = OpeningSession {
      sessions,
      session: session.clone(),
      is_open: false,
    };
    let writers = Writers::Fallback(fallback::FallbackSession::open(socket, inputs, generation).await?);
    opening.is_open = true;
    Ok(Self { session, writers })
  }

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
    let session = SessionHandle::Quic(transport.clone());
    sessions.track(&session);
    let mut opening = OpeningSession {
      sessions,
      session,
      is_open: false,
    };
    let opened = Self::open(transport, inputs, generation).await?;
    opening.is_open = true;
    Ok(opened)
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
      session: SessionHandle::Quic(transport),
      writers: Writers::Quic { signaling, datagrams },
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
    match &self.writers {
      Writers::Quic { signaling, .. } => {
        let mut framed = length.to_be_bytes().to_vec();
        framed.extend_from_slice(&body);
        // Writes queue in order; a failed write surfaces as the stream closing.
        let _ = signaling.write_with_chunk(&Uint8Array::from(framed.as_slice()));
      },
      Writers::Fallback(session) => session.send_signal(&body),
    }
    Ok(())
  }

  fn send_datagram(&self, bytes: &[u8]) {
    match &self.writers {
      // Unreliable by design: the browser drops datagrams when its queue is full.
      Writers::Quic { datagrams, .. } => {
        let _ = datagrams.write_with_chunk(&Uint8Array::from(bytes));
      },
      Writers::Fallback(session) => session.send_datagram(bytes),
    }
  }
}

impl Drop for OpeningSession<'_> {
  fn drop(&mut self) {
    if !self.is_open {
      self.sessions.close(&self.session);
    }
  }
}

impl OpenSessions {
  fn track(&self, session: &SessionHandle) {
    self.0.borrow_mut().push(session.clone());
  }

  /// Closes one session as a leave.
  fn close(&self, session: &SessionHandle) {
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
fn close_session(session: &SessionHandle) {
  let session = match session {
    SessionHandle::Quic(session) => session,
    SessionHandle::Fallback(socket) => {
      fallback::close_as_leave(socket);
      return;
    },
  };
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
async fn received_packets(session: &SessionHandle) -> PacketCount {
  // A page can't see the WebSocket's frames below its messages: those count anyway.
  let SessionHandle::Quic(session) = session else {
    return PacketCount::Unsupported;
  };
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

/// A governed camera's limits as the stats show them (#1762).
#[cfg(feature = "call-view")]
/// The codecs an own track's layers may use (#1733): what every viewer decodes, and what some
/// decode as a layer in another codec where the SFU forwards such layers.
fn share_codecs(engine: &CallEngine, candidates: &[VideoCodec]) -> ShareCodecs {
  ShareCodecs {
    by_all: engine.video_codecs_for(candidates),
    by_some: engine.layer_codecs_for(candidates),
  }
}

fn camera_limits_stats(limits: video::CameraLimits) -> teamcraft_client_api::video_stats::CameraLimitsStats {
  use teamcraft_client_api::video_stats::{CameraLimitsStats, EncoderOverload, LayerLimitStats};

  use crate::camera_governor::OverloadReason;

  CameraLimitsStats {
    planned_framerate: limits.planned.framerate,
    framerate: limits.load.framerate,
    top_layers_off: limits.load.top_layers_off,
    maybe_overload: limits.maybe_reason.map(|reason| match reason {
      OverloadReason::SkippedFrames => EncoderOverload::SkippedFrames,
      OverloadReason::QueuedFrames => EncoderOverload::QueuedFrames,
      OverloadReason::SlowEncoding => EncoderOverload::SlowEncoding,
    }),
    layers: limits
      .layers
      .into_iter()
      .map(|(layer, bitrate_bps, framerate)| LayerLimitStats {
        layer,
        bitrate_bps,
        framerate,
      })
      .collect(),
  }
}

/// The session's smoothed round trip in milliseconds (`getStats().smoothedRtt`); `None` where the
/// browser has no such statistic.
#[cfg(feature = "call-view")]
async fn round_trip_ms(session: &SessionHandle) -> Option<u32> {
  let SessionHandle::Quic(session) = session else {
    return None;
  };
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
  match maybe_close_code {
    Some(SFU_GOING_AWAY_CLOSE_CODE) => SessionEnd::GoingAway,
    Some(SFU_SWITCHED_CLOSE_CODE) => SessionEnd::Switched,
    _ => SessionEnd::Lost,
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
  /// The graph at 48 kHz; at the browser's own rate where it refuses the microphone at 48 kHz
  /// ([`WebCallError::SampleRate`]).
  async fn open(
    source: &AudioSource,
    sink: &AudioSink,
    maybe_worklet_url: Option<&str>,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let at_codec_rate = Self::open_at(Some(GraphRate::CODEC), source, sink, maybe_worklet_url, inputs).await;
    let Err(WebCallError::SampleRate(message)) = at_codec_rate else {
      return at_codec_rate;
    };
    warn!(%message, "the microphone doesn't connect at 48 kHz: the call's audio runs at the browser's rate");
    Self::open_at(None, source, sink, maybe_worklet_url, inputs).await
  }

  /// A graph at `maybe_rate` (the browser's default without one) with the source and sink; a
  /// graph that doesn't start is closed again.
  async fn open_at(
    maybe_rate: Option<GraphRate>,
    source: &AudioSource,
    sink: &AudioSink,
    maybe_worklet_url: Option<&str>,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let options = AudioContextOptions::new();
    if let Some(rate) = maybe_rate {
      options.set_sample_rate(rate.0 as f32);
    }
    let context = AudioContext::new_with_context_options(&options).map_err(browser("AudioContext"))?;
    let rate = GraphRate::of(&context);
    let mut audio = Self {
      context,
      maybe_capture: None,
      maybe_playback: None,
      to_graph: RefCell::new(rate.codec_to_graph()),
    };
    if let Err(error) = audio.start(source, sink, maybe_worklet_url, inputs).await {
      audio.close();
      return Err(error);
    }
    if rate != GraphRate::CODEC {
      info!(
        rate_hz = rate.0,
        "the call's audio is resampled to and from 48 kHz"
      );
    }
    Ok(audio)
  }

  async fn start(
    &mut self,
    source: &AudioSource,
    sink: &AudioSink,
    maybe_worklet_url: Option<&str>,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<(), WebCallError> {
    load_worklet(&self.context, maybe_worklet_url).await?;
    self.switch(source, sink, inputs).await
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
      AudioSink::Discard | AudioSink::Frames => None,
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
    let maybe_resampled = self
      .to_graph
      .borrow_mut()
      .as_mut()
      .map(|resampler| resampler.process(mix));
    let samples = maybe_resampled.as_deref().unwrap_or(mix);
    node
      .port()
      .and_then(|port| port.post_message(&Float32Array::from(samples)))
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

impl GraphRate {
  const CODEC: Self = Self(SAMPLE_RATE_HZ);

  /// The rate `context` runs at; one that reports none counts as the codec's.
  fn of(context: &AudioContext) -> Self {
    Self::from_hz(context.sample_rate())
  }

  fn from_hz(rate_hz: f32) -> Self {
    // A browser's rates are whole numbers of Hz well inside u32.
    let rounded = rate_hz.round();
    if rounded.is_finite() && rounded >= 1.0 {
      Self(rounded as u32)
    } else {
      Self::CODEC
    }
  }

  /// Frames of audio between two pulses of the audio clock.
  fn clock_every_frames(self) -> u32 {
    (self.0 / CLOCK_PULSES_PER_SECOND).max(1)
  }

  /// Samples the playback worklet holds at most.
  fn max_playback_samples(self) -> u32 {
    self.0 / MAX_PLAYBACK_PER_SECOND
  }

  /// Captured samples to the codec's rate; `None` at the codec's rate.
  fn graph_to_codec(self) -> Option<MonoResampler> {
    (self != Self::CODEC).then(|| MonoResampler::between(1, self.0, SAMPLE_RATE_HZ))
  }

  /// The mix to this rate; `None` at the codec's rate.
  fn codec_to_graph(self) -> Option<MonoResampler> {
    (self != Self::CODEC).then(|| MonoResampler::between(1, SAMPLE_RATE_HZ, self.0))
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
    let every_frames = GraphRate::of(context).clock_every_frames();
    let processor_options =
      dictionary(&[("everyFrames", JsValue::from(every_frames))]).map_err(browser("clock worklet"))?;
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
    // The port outlives this clock: a pulse still queued would call the dropped closure.
    detach_port(&self.node);
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
    // A microphone the graph refuses is released again (the graph may be opened anew for it).
    Self::from_stream(context, stream.clone(), CaptureMix::FirstChannel, inputs)
      .inspect_err(|_| stop_tracks(&stream))
  }

  /// Captures an audio stream; its samples arrive as [`Input::Captured`] at 48 kHz: the first
  /// channel as the microphone's, or every channel mixed down as a shared screen's sound.
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
      .map_err(audio_source_error)?;
    let mut maybe_to_codec = GraphRate::of(context).graph_to_codec();
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
      let samples = event.data().unchecked_into::<Float32Array>().to_vec();
      let samples = match maybe_to_codec.as_mut() {
        Some(resampler) => resampler.process(&samples),
        None => samples,
      };
      // The call task may be gone; the capture stops when it drops this closure.
      let _ = sender.unbounded_send(Input::Captured { origin, samples });
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

impl Drop for Capture {
  fn drop(&mut self) {
    // The port outlives this capture: samples still queued would call the dropped closure.
    detach_port(&self.node);
  }
}

/// Unhooks a worklet node's port from its message closure before the closure is dropped. The
/// worklet may still have messages queued; without this they call a dropped closure (wasm-bindgen
/// throws "closure invoked recursively or after being dropped").
fn detach_port(node: &AudioWorkletNode) {
  // A node whose port can't be read has nothing hooked to it.
  if let Ok(port) = node.port() {
    port.set_onmessage(None);
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
      .unbounded_send(Input::Datagram {
        generation,
        bytes,
        read_at: Instant::now(),
      })
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
  let max_samples = GraphRate::of(context).max_playback_samples();
  let processor_options =
    dictionary(&[("maxSamples", JsValue::from(max_samples))]).map_err(browser("playback worklet"))?;
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
  match missing_for_calls(has_global) {
    Some(missing) => Err(WebCallError::Unsupported(missing)),
    None => Ok(()),
  }
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

/// A stream the audio graph didn't take. `NotSupportedError` is Firefox before 148 refusing a
/// microphone in a graph that doesn't run at its default device's rate (#1721).
fn audio_source_error(error: JsValue) -> WebCallError {
  let name = Reflect::get(&error, &JsValue::from_str("name"))
    .ok()
    .and_then(|name| name.as_string());
  if name.as_deref() == Some("NotSupportedError") {
    WebCallError::SampleRate(js_message(&error))
  } else {
    browser("audio source")(error)
  }
}

fn stop_tracks(stream: &MediaStream) {
  for track in stream.get_tracks().iter() {
    track.unchecked_into::<MediaStreamTrack>().stop();
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

  /// #1721: Firefox before 148 runs the call's audio graph at its device's rate.
  mod graph_rates {
    use super::*;

    #[test]
    fn at_48_khz_nothing_is_resampled() {
      let rate = GraphRate::from_hz(48_000.0);
      assert_eq!(rate, GraphRate::CODEC);
      assert!(rate.graph_to_codec().is_none() && rate.codec_to_graph().is_none());
      // 10 ms pulses and 200 ms in the playback worklet, as before.
      assert_eq!(rate.clock_every_frames(), 480);
      assert_eq!(rate.max_playback_samples(), 9_600);
    }

    #[test]
    fn a_44_1_khz_graph_sends_and_plays_48_khz_frames() {
      let rate = GraphRate::from_hz(44_100.0);
      assert_eq!(rate.clock_every_frames(), 441);
      assert_eq!(rate.max_playback_samples(), 8_820);
      // A second of the worklet's 128-sample captures is a second of 20 ms Opus frames.
      let mut to_codec = rate.graph_to_codec().expect("a resampler to 48 kHz");
      let captured: usize = (0..44_100 / 128)
        .map(|_| to_codec.process(&[0.1; 128]).len())
        .sum();
      let expected = 44_100 / 128 * 128 * 48_000 / 44_100;
      assert!(captured.abs_diff(expected) <= 1, "{captured} of {expected}");
      // And 50 mixed frames fill a second of the graph.
      let mut from_codec = rate.codec_to_graph().expect("a resampler from 48 kHz");
      let played: usize = (0..50)
        .map(|_| from_codec.process(&[0.1; FRAME_SAMPLES_PER_CHANNEL]).len())
        .sum();
      assert!(played.abs_diff(44_100) <= 1, "{played}");
    }

    #[test]
    fn a_rate_the_browser_does_not_report_counts_as_48_khz() {
      for broken in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(GraphRate::from_hz(broken), GraphRate::CODEC, "{broken}");
      }
      assert_eq!(GraphRate::from_hz(47_999.6), GraphRate::CODEC);
      assert_eq!(GraphRate::from_hz(16_000.0).clock_every_frames(), 160);
    }
  }

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

    /// #986: a microphone unplugged since it was picked gives way to the default; a refusal or
    /// a missing default does not.
    #[test]
    fn only_a_chosen_microphone_that_is_gone_gives_way_to_the_default() {
      let refused = |reason| WebCallError::MediaAccess {
        input: MediaInput::Microphone,
        reason,
        message: String::new(),
      };
      let chosen = AudioDeviceChoice::Device(AudioDeviceId::new("usb-headset"));
      assert!(is_gone_device(&chosen, &refused(MediaAccessReason::NotFound)));
      for reason in [
        MediaAccessReason::Denied,
        MediaAccessReason::InUse,
        MediaAccessReason::Other,
      ] {
        assert!(!is_gone_device(&chosen, &refused(reason)), "{reason:?}");
      }
      assert!(!is_gone_device(
        &AudioDeviceChoice::Default,
        &refused(MediaAccessReason::NotFound)
      ));
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

    /// The SFU moved the session to another one (#1338): neither the end of the call nor a loss by
    /// itself, on QUIC and on the fallback alike (whose close frame carries the same code).
    #[test]
    fn a_moved_session_closes_with_its_own_code() {
      assert_eq!(session_end(Some(SFU_SWITCHED_CLOSE_CODE)), SessionEnd::Switched);
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
      engine.connection_lost(now);
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

  mod encoded_chunks {
    use super::*;

    /// #1733: a share's AV1 encoder output went out labelled H.264, and viewers got empty frames.
    #[test]
    fn a_frame_goes_out_in_the_codec_its_encoder_encoded() {
      let chunk = EncodedChunk {
        codec: VideoCodec::Av1,
        layer: 1,
        data: vec![0x12, 0x00, 0x0a, 0x04, 0, 1, 2, 3],
        is_keyframe: true,
        timestamp_us: 1_000_000.0,
      };
      let frame = chunk.clone().into_frame().unwrap();
      assert_eq!(frame.codec, VideoCodec::Av1);
      assert_eq!(frame.layer.value(), 1);
      assert_eq!(frame.data, chunk.data);
      assert!(frame.is_keyframe);
      assert_eq!(frame.timestamp, EncodedVideoFrame::rtp_timestamp(1_000_000));
    }

    #[test]
    fn a_layer_out_of_range_is_dropped() {
      let chunk = EncodedChunk {
        codec: VideoCodec::H264,
        layer: u8::MAX,
        data: vec![0, 0, 0, 1, 0x65],
        is_keyframe: true,
        timestamp_us: 0.0,
      };
      assert!(chunk.into_frame().is_none());
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

      fn queue_size(&self) -> u32 {
        0
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

      fn queue_size(&self) -> u32 {
        0
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
        arrived_at: Instant::now(),
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
      maybe_max_simulcast_layers: Some(teamcraft_proto::MAX_SIMULCAST_LAYERS),
      maybe_switch_secret: None,
      maybe_screen_share_taken_over: None,
      maybe_retransmit_window_ms: None,
      maybe_probe_bytes: None,
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
      maybe_codec: None,
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
