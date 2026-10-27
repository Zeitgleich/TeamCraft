//! Native driver for [`CallEngine`]: WebTransport to the SFU (wtransport), audio devices or test
//! signals, Opus, jitter buffers and mixing, on a tokio runtime.
//!
//! The desktop app and the headless `teamcraft-call` client use it. Device streams (cpal) are not
//! `Send` everywhere, so they live on a dedicated audio thread that exchanges samples with the call
//! task over channels.
//!
//! Network changes (ARCHITECTURE.md §6.5): mobile apps report them with
//! [`NativeCall::network_changed`], on desktops the driver notices a new route to the SFU itself.
//! The driver then moves the QUIC connection to a fresh wildcard socket (`rebind`): quinn switches
//! to a new connection ID and probes, the SFU validates the new path, and the call continues;
//! nothing else changes. If the path then stays silent, or it breaks in any other way, or the SFU
//! goes away (a restart, a deploy), the driver gets a fresh join ticket and rejoins on a new
//! connection within the SFU's grace period; devices, mute state and tracks carry over.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle as ThreadHandle;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};
use teamcraft_net::{ATTEMPT_DELAY, RaceError, interleave, race};
use teamcraft_proto::{
  CallClientMessage, CallServerMessage, DeviceId, MediaStreamId, SFU_GOING_AWAY_CLOSE_CODE, VideoCodec,
  VideoLayerInfo, VideoTrackKind,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use wtransport::config::ClientConfigBuilder;
use wtransport::config::states::{WantsRootStore, WantsTransportConfigClient};
use wtransport::datagram::Datagram;
use wtransport::endpoint::endpoint_side::Client;
use wtransport::error::{ConnectionError, StreamReadExactError};
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};

use crate::audio_codec::{FRAME_SAMPLES_PER_CHANNEL, OpusEncoderConfig};
use crate::audio_device::{AudioDeviceError, AudioInput, AudioOutput, PlaybackBuffer};
use crate::audio_io::{AudioSink, AudioSource, sine_tone};
use crate::audio_pipeline::{AudioPipelineError, CapturePipeline, PlaybackMixer};
use crate::call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use crate::call_cues::{CallCue, CuePlayer, StatusCues};
use crate::call_engine::{
  AudioOrigin, CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
  VideoSendOutcome, VideoStats,
};
use crate::call_resilience::{
  JoinTicket, JoinTicketError, PathMonitor, PathState, ReconnectSchedule, ResilienceConfig, rejoin_attempt,
};
use crate::jitter::JitterBufferConfig;
use crate::sfu_address::{PinnedResolver, sfu_addresses};
use crate::system_audio::{SystemAudioCapture, SystemAudioError, SystemAudioReceiver, system_audio_queue};
use crate::video::EncodedVideoFrame;
use crate::video_capture::CaptureError;
use crate::video_pipeline::{
  FirstPicture, FrameSink, KeyframeNeeded, RepeatedWarning, VideoReceiver, VideoSendConfig, VideoSender,
  VideoStartError,
};

/// Audio frame period.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Feedback and allocation period.
const TIMER_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound of one signaling message.
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
/// At most this much audio waits for the speakers (latency bound).
const MAX_PLAYBACK_BUFFER_SAMPLES: usize = 48_000 / 5;
/// QUIC application close code of a deliberate leave.
const LEAVE: u32 = 0;
/// How long a broken signaling stream waits for its connection's close reason.
const STREAM_CLOSE_WAIT: Duration = Duration::from_millis(200);
/// How often the route toward the SFU is looked up (see [`RouteWatch`]).
const ROUTE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How often the video counters are logged (debug) while own or others' video flows.
const VIDEO_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// A running call.
pub struct NativeCall {
  commands: mpsc::UnboundedSender<Command>,
  events: mpsc::UnboundedReceiver<CallEngineEvent>,
  task: JoinHandle<Result<CallStats, NativeCallError>>,
}

/// How to reach the SFU and what to send and play.
#[derive(Debug, Clone)]
pub struct NativeCallConfig {
  /// WebTransport URL from the join token response, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  /// SHA-256s of the SFU's self-signed certificates, from the join token response; every rejoin
  /// uses its own fresh ticket's instead ([`JoinTicket::certificate_hashes`]).
  pub certificate_hashes: Vec<[u8; 32]>,
  /// A self-signed SFU certificate accepted on every connection besides the ticket's (development
  /// tools, tests). With neither, the system's trusted roots apply.
  pub maybe_certificate_hash: Option<[u8; 32]>,
  pub token: String,
  pub engine: CallEngineConfig,
  pub source: AudioSource,
  pub sink: AudioSink,
  pub maybe_trusted_roster: Option<Vec<DeviceId>>,
  /// Binds DAVE keys to device identities (e2ee.md §8); `None` only for development tools.
  pub maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  pub opus: OpusEncoderConfig,
  pub jitter: JitterBufferConfig,
  /// Echo cancellation, noise suppression and gain control for the microphone.
  pub maybe_processing: Option<crate::audio_processing::AudioProcessingConfig>,
  /// Keep-alives, loss detection and rejoin timing.
  pub resilience: ResilienceConfig,
  /// Fresh join tickets for rejoining after a lost connection. Without one the first ticket is
  /// used again, which works only while it is valid (development tools).
  pub maybe_ticket_source: Option<Arc<dyn JoinTicketSource>>,
  /// Which call status sounds (reconnecting, reconnected, connection lost) play.
  pub status_cues: StatusCues,
  /// The microphone is muted from the first frame (the user's choice before joining, or a call
  /// that moves on while muted): nothing of it is sent before a later unmute.
  pub is_muted_at_start: bool,
}

/// Gets a fresh join ticket from `teamcraft-server` (a new token, possibly another SFU URL).
pub trait JoinTicketSource: Send + Sync {
  fn fetch(&self) -> TicketFuture;
}

/// A join ticket on its way.
pub type TicketFuture = Pin<Box<dyn Future<Output = Result<JoinTicket, JoinTicketError>> + Send>>;

impl std::fmt::Debug for dyn JoinTicketSource {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("JoinTicketSource")
  }
}

#[derive(Debug, Error)]
pub enum NativeCallError {
  #[error("connecting to the SFU failed: {0}")]
  Connect(String),
  #[error("signaling stream: {0}")]
  Signaling(String),
  #[error(transparent)]
  Engine(#[from] CallEngineError),
  #[error(transparent)]
  Audio(#[from] AudioPipelineError),
  #[error(transparent)]
  Device(#[from] AudioDeviceError),
  #[error(transparent)]
  Capture(#[from] CaptureError),
  #[error(transparent)]
  VideoStart(#[from] VideoStartError),
  #[error(transparent)]
  SystemAudio(#[from] SystemAudioError),
  #[error(transparent)]
  Ticket(#[from] JoinTicketError),
  #[error("the connection to the call was lost and could not be restored in time")]
  ConnectionLost,
  #[error("the call task ended unexpectedly")]
  TaskEnded,
}

enum Command {
  Mute(bool),
  Deafen(bool),
  MoveHere,
  SwitchAudio {
    source: AudioSource,
    sink: AudioSink,
    done: oneshot::Sender<Result<(), AudioDeviceError>>,
  },
  Video(VideoCommand),
  /// Starts sending a shared screen's sound from `capture`, replacing a running one.
  StartScreenAudio(
    Box<dyn SystemAudioCapture>,
    oneshot::Sender<Result<(), SystemAudioError>>,
  ),
  StopScreenAudio,
  NetworkChanged,
  SetStatusCues(StatusCues),
  Leave,
}

/// The shared screen's sound being captured and encoded.
struct ScreenAudio {
  capture: Box<dyn SystemAudioCapture>,
  samples: SystemAudioReceiver,
  pipeline: CapturePipeline,
}

/// Video requests the app makes; frames come from the app's encoder (capture and codecs are
/// platform specific).
enum VideoCommand {
  Publish {
    kind: VideoTrackKind,
    codec: VideoCodec,
    layers: Vec<VideoLayerInfo>,
  },
  Unpublish(MediaStreamId),
  Subscribe {
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
  },
  Send {
    stream_id: MediaStreamId,
    frame: EncodedVideoFrame,
  },
  /// Capture, encode and publish a track (camera or screen); the answer brings the wait for
  /// its first picture.
  Start(
    VideoSendConfig,
    oneshot::Sender<Result<FirstPicture, CaptureError>>,
  ),
  Stop(VideoTrackKind),
  Watch {
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
    sink: FrameSink,
  },
  Unwatch(MediaStreamId),
  /// A received track's decoder failed: ask for a keyframe (rate-limited by the engine).
  RequestKeyframe(MediaStreamId),
}

/// The driver's video: own tracks being captured and received tracks being decoded.
#[derive(Default)]
struct VideoState {
  /// Own tracks; the stream id arrives with `VideoPublished`.
  senders: Vec<(VideoSender, Option<MediaStreamId>)>,
  receivers: HashMap<MediaStreamId, VideoReceiver>,
  /// The call task's own command queue, for the decoder threads' keyframe requests. Weak: it
  /// must not keep the call running once the app dropped its handle.
  maybe_commands: Option<mpsc::WeakUnboundedSender<Command>>,
  /// What others' tracks show, for counting what arrives per kind.
  remote_kinds: HashMap<MediaStreamId, VideoTrackKind>,
  counts: VideoCounts,
  send_warning: RepeatedWarning,
  /// When the counters were last looked at, and what they were.
  maybe_last_log: Option<(Instant, VideoCounts)>,
}

/// Where an own track's pictures stopped during one [`VIDEO_LOG_INTERVAL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoStall {
  /// The camera delivered no frame (a screen only delivers when something changes).
  Capture,
  /// Frames came, but no encoder gave a picture.
  Encoder,
  /// Pictures were encoded but none left: the track not confirmed, the call not secured, or
  /// sending failed.
  Sending,
}

/// Video counters of the call: what the stopped own tracks captured and encoded, and what was
/// sent and received.
#[derive(Debug, Clone, Copy, Default)]
struct VideoCounts {
  camera: VideoStats,
  screen: VideoStats,
}

/// The audio thread's handles: samples from the microphone and the speakers' buffer.
struct AudioDevices {
  maybe_capture: Option<mpsc::UnboundedReceiver<Vec<f32>>>,
  maybe_playback: Option<Arc<PlaybackBuffer>>,
  stop: std_mpsc::Sender<()>,
  thread: ThreadHandle<()>,
}

impl NativeCallConfig {
  /// A call that sends silence and discards what it receives; set the fields as needed.
  pub fn new(sfu_url: String, token: String, engine: CallEngineConfig) -> Self {
    Self {
      sfu_url,
      certificate_hashes: Vec::new(),
      maybe_certificate_hash: None,
      token,
      engine,
      source: AudioSource::Silence,
      sink: AudioSink::Discard,
      maybe_trusted_roster: None,
      maybe_authenticator: None,
      opus: OpusEncoderConfig::default(),
      jitter: JitterBufferConfig::default(),
      maybe_processing: None,
      resilience: ResilienceConfig::default(),
      maybe_ticket_source: None,
      status_cues: StatusCues::default(),
      is_muted_at_start: false,
    }
  }

  /// A call whose DAVE keys are bound to device identities (e2ee.md §8): only the
  /// authenticator's devices are trusted, and this device signs its own key.
  pub fn device_bound(
    sfu_url: String,
    token: String,
    engine: CallEngineConfig,
    authenticator: DeviceCallKeyAuthenticator,
  ) -> Self {
    let own_device = engine.device_id;
    let roster = authenticator
      .device_ids()
      .filter(|device| *device != own_device)
      .collect();
    let mut config = Self::new(sfu_url, token, engine);
    config.maybe_trusted_roster = Some(roster);
    config.maybe_authenticator = Some(Arc::new(authenticator));
    config
  }
}

impl NativeCall {
  /// Connects, joins and starts sending and playing in the background.
  pub async fn join(config: NativeCallConfig) -> Result<Self, NativeCallError> {
    let hashes = accepted_hashes(config.maybe_certificate_hash, &config.certificate_hashes);
    let link = Link::open(&config.sfu_url, &hashes, &config.resilience).await?;
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (event_tx, events) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_call(
      config,
      link,
      (commands.downgrade(), command_rx),
      event_tx,
    ));
    Ok(Self {
      commands,
      events,
      task,
    })
  }

  pub fn set_muted(&self, is_muted: bool) {
    // A closed channel means the call already ended; nothing to mute.
    let _ = self.commands.send(Command::Mute(is_muted));
  }

  pub fn set_deafened(&self, is_deafened: bool) {
    let _ = self.commands.send(Command::Deafen(is_deafened));
  }

  /// Takes the call over from the account's other devices.
  pub fn move_here(&self) {
    let _ = self.commands.send(Command::MoveHere);
  }

  /// Publishes a video track; [`CallEngineEvent::VideoPublished`] for this device brings the stream
  /// id for [`NativeCall::send_video`]. Screen shares use a single layer.
  pub fn publish_video(&self, kind: VideoTrackKind, codec: VideoCodec, layers: Vec<VideoLayerInfo>) {
    self.video(VideoCommand::Publish { kind, codec, layers });
  }

  pub fn unpublish_video(&self, stream_id: MediaStreamId) {
    self.video(VideoCommand::Unpublish(stream_id));
  }

  /// Receives another device's track up to `max_layer` (`None` stops); frames arrive as
  /// [`CallEngineEvent::VideoFrame`].
  pub fn subscribe_video(&self, stream_id: MediaStreamId, maybe_max_layer: Option<u8>) {
    self.video(VideoCommand::Subscribe {
      stream_id,
      maybe_max_layer,
    });
  }

  /// Captures, encodes (simulcast per `config.layers`) and publishes a track; replaces a running
  /// track of the same kind. Frames flow once the SFU confirmed the track. Returns once the
  /// capture started; see [`NativeCall::start_video_checked`] to learn whether pictures come.
  pub async fn start_video(&self, config: VideoSendConfig) -> Result<(), NativeCallError> {
    self.start_video_checked(config).await.map(drop)
  }

  /// [`NativeCall::start_video`], with the wait for the track's first encoded picture: awaiting
  /// it tells a camera or screen capture that delivers nothing, or pictures no encoder takes,
  /// from a working one; the track then ends by itself after
  /// [`crate::video_pipeline::FIRST_PICTURE_TIMEOUT`]. Dropping it lets the track wait as long as
  /// it needs (an app that pushes the frames itself).
  pub async fn start_video_checked(&self, config: VideoSendConfig) -> Result<FirstPicture, NativeCallError> {
    let (done, done_rx) = oneshot::channel();
    self.video(VideoCommand::Start(config, done));
    Ok(done_rx.await.map_err(|_| NativeCallError::TaskEnded)??)
  }

  pub fn stop_video(&self, kind: VideoTrackKind) {
    self.video(VideoCommand::Stop(kind));
  }

  /// Receives and decodes another device's track; decoded frames go to `sink` on a decoder
  /// thread.
  pub fn watch_video(&self, stream_id: MediaStreamId, maybe_max_layer: Option<u8>, sink: FrameSink) {
    self.video(VideoCommand::Watch {
      stream_id,
      maybe_max_layer,
      sink,
    });
  }

  pub fn unwatch_video(&self, stream_id: MediaStreamId) {
    self.video(VideoCommand::Unwatch(stream_id));
  }

  /// The app's own decoder of a subscribed track failed: its deltas are dropped until the next
  /// keyframe, which is asked for (at most once a second). Tracks decoded with
  /// [`NativeCall::watch_video`] do this by themselves.
  pub fn request_keyframe(&self, stream_id: MediaStreamId) {
    self.video(VideoCommand::RequestKeyframe(stream_id));
  }

  /// Sends one encoded frame of an own track.
  pub fn send_video(&self, stream_id: MediaStreamId, frame: EncodedVideoFrame) {
    self.video(VideoCommand::Send { stream_id, frame });
  }

  /// Sends the shared screen's sound from `capture` as a second audio stream (not affected by
  /// muting the microphone); replaces a running one.
  pub async fn start_screen_audio(
    &self,
    capture: Box<dyn SystemAudioCapture>,
  ) -> Result<(), NativeCallError> {
    let (done, done_rx) = oneshot::channel();
    self
      .commands
      .send(Command::StartScreenAudio(capture, done))
      .map_err(|_| NativeCallError::TaskEnded)?;
    Ok(done_rx.await.map_err(|_| NativeCallError::TaskEnded)??)
  }

  pub fn stop_screen_audio(&self) {
    // A closed channel means the call already ended.
    let _ = self.commands.send(Command::StopScreenAudio);
  }

  /// Switches microphone and speakers (or test signals) mid-call. On error the previous devices
  /// keep running.
  pub async fn set_audio(&self, source: AudioSource, sink: AudioSink) -> Result<(), NativeCallError> {
    let (done, done_rx) = oneshot::channel();
    self
      .commands
      .send(Command::SwitchAudio { source, sink, done })
      .map_err(|_| NativeCallError::TaskEnded)?;
    Ok(done_rx.await.map_err(|_| NativeCallError::TaskEnded)??)
  }

  /// The system reported a network change (Android `ConnectivityManager`, iOS `NWPathMonitor`, an
  /// interface or route change on desktops). The connection then has to show it migrated to the
  /// new path quickly, or the call rejoins on a new connection; while rejoining, the next attempt
  /// starts at once.
  pub fn network_changed(&self) {
    // A closed channel means the call already ended.
    let _ = self.commands.send(Command::NetworkChanged);
  }

  /// Which call status sounds play from now on.
  pub fn set_status_cues(&self, status_cues: StatusCues) {
    // A closed channel means the call already ended.
    let _ = self.commands.send(Command::SetStatusCues(status_cues));
  }

  fn video(&self, command: VideoCommand) {
    // A closed channel means the call already ended.
    let _ = self.commands.send(Command::Video(command));
  }

  /// The next event, or `None` once the call ended.
  pub async fn next_event(&mut self) -> Option<CallEngineEvent> {
    self.events.recv().await
  }

  /// Leaves the call and returns its statistics.
  pub async fn leave(self) -> Result<CallStats, NativeCallError> {
    let _ = self.commands.send(Command::Leave);
    self.task.await.map_err(|_| NativeCallError::TaskEnded)?
  }
}

/// The system's trusted roots. Android keeps them where rustls-native-certs doesn't look, so it
/// uses the Mozilla roots (webpki-roots), like the client core's HTTPS on Android.
#[cfg(not(target_os = "android"))]
fn with_trusted_roots(
  builder: ClientConfigBuilder<WantsRootStore>,
) -> ClientConfigBuilder<WantsTransportConfigClient> {
  builder.with_native_certs()
}

#[cfg(target_os = "android")]
fn with_trusted_roots(
  builder: ClientConfigBuilder<WantsRootStore>,
) -> ClientConfigBuilder<WantsTransportConfigClient> {
  let roots = wtransport::tls::rustls::RootCertStore {
    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
  };
  builder.with_custom_tls(wtransport::tls::client::build_default_tls_config(
    Arc::new(roots),
    None,
  ))
}

/// The call task: drives the engine from the network, the audio thread and timers, and gets the
/// call back onto a new connection when the current one is lost.
async fn run_call(
  config: NativeCallConfig,
  link: Link,
  (own_commands, mut commands): (
    mpsc::WeakUnboundedSender<Command>,
    mpsc::UnboundedReceiver<Command>,
  ),
  events: mpsc::UnboundedSender<CallEngineEvent>,
) -> Result<CallStats, NativeCallError> {
  let start = Instant::now();
  let mut engine = CallEngine::new(config.engine.clone(), start);
  if let Some(roster) = &config.maybe_trusted_roster {
    engine.set_trusted_roster(roster.iter().copied());
  }
  if let Some(authenticator) = &config.maybe_authenticator {
    engine.set_authenticator(Arc::clone(authenticator));
  }
  let maybe_processor = config
    .maybe_processing
    .map(crate::audio_processing::AudioProcessor::new)
    .transpose()
    .map_err(AudioPipelineError::from)?
    .map(Arc::new);
  let mut capture = CapturePipeline::new(config.opus)?;
  let mut mixer = PlaybackMixer::new(config.jitter);
  if let Some(processor) = &maybe_processor {
    capture = capture.with_processor(Arc::clone(processor));
    mixer = mixer.with_processor(Arc::clone(processor));
  }
  if config.is_muted_at_start {
    engine.set_muted(true);
    capture.set_muted(true);
  }
  let mut source = config.source.clone();
  let mut devices = AudioDevices::open(&source, &config.sink)?;
  let mut sinks = Sinks {
    events,
    mixer,
    stats: CallStats::default(),
    video: VideoState {
      maybe_commands: Some(own_commands),
      ..VideoState::default()
    },
    cues: CuePlayer::new(config.status_cues),
  };
  let mut connection = CallConnection::new(&config, link, start);

  let mut tone_position = 0u64;
  let mut frame_tick = tokio::time::interval(FRAME_INTERVAL);
  let mut timer_tick = tokio::time::interval(TIMER_INTERVAL);
  engine.join(config.token.clone());
  let mut maybe_screen_audio: Option<ScreenAudio> = None;
  let (encoded_tx, mut encoded_rx) = mpsc::unbounded_channel::<(VideoTrackKind, EncodedVideoFrame)>();
  sinks.flush(&mut engine, &mut capture, connection.maybe_link.as_ref())?;

  let result = loop {
    tokio::select! {
      event = connection.next_event() => match event {
        ConnectionEvent::Input(LinkInput::Signal(message)) => {
          if let Err(error) = engine.handle_signal(message, Instant::now()) {
            break Err(error.into());
          }
        },
        ConnectionEvent::Input(LinkInput::Datagram(datagram)) => {
          engine.handle_datagram(&datagram.payload(), Instant::now());
        },
        ConnectionEvent::Input(LinkInput::Ended(LinkEnd::Closed)) => break Ok(()),
        ConnectionEvent::Input(LinkInput::Ended(LinkEnd::Lost)) => connection.lose(&mut engine, Instant::now()),
        ConnectionEvent::Input(LinkInput::Ended(LinkEnd::GoingAway)) => {
          connection.lose_to_going_away(&mut engine, Instant::now());
        },
        ConnectionEvent::AttemptDue => connection.start_attempt(),
        ConnectionEvent::AttemptFinished(Ok((link, ticket))) => {
          connection.resume(&mut engine, link, ticket, Instant::now());
        },
        ConnectionEvent::AttemptFinished(Err(error)) => connection.on_failed_attempt(&error, Instant::now()),
      },
      maybe_samples = recv_capture(&mut devices.maybe_capture) => {
        if let Some(samples) = maybe_samples {
          for frame in capture.push(&samples)? {
            engine.send_audio(&frame, Instant::now());
            sinks.stats.frames_sent += 1;
          }
        }
      },
      _ = frame_tick.tick() => {
        if let AudioSource::Tone { frequency_hz, amplitude } = source {
          let samples = sine_tone(frequency_hz, amplitude, tone_position, FRAME_SAMPLES_PER_CHANNEL);
          tone_position += FRAME_SAMPLES_PER_CHANNEL as u64;
          for frame in capture.push(&samples)? {
            engine.send_audio(&frame, Instant::now());
            sinks.stats.frames_sent += 1;
          }
        }
        let mix = sinks.next_playback_frame()?;
        if let Some(buffer) = &devices.maybe_playback {
          buffer.push(&mix);
        }
      },
      maybe_samples = recv_screen_audio(&mut maybe_screen_audio) => {
        match (maybe_samples, maybe_screen_audio.as_mut()) {
          (Some(samples), Some(screen_audio)) => {
            sinks.mixer.push_echo_reference(&samples);
            for frame in screen_audio.pipeline.push(&samples)? {
              engine.send_screen_audio(&frame, Instant::now());
            }
          },
          // The sound capture ended on its own (the audio device or portal went away).
          _ => stop_screen_audio(&mut engine, &mut maybe_screen_audio),
        }
      },
      _ = timer_tick.tick() => {
        let now = Instant::now();
        engine.on_timer(now);
        if connection.is_out_of_time(now) {
          break Err(NativeCallError::ConnectionLost);
        }
        connection.watch_path(&mut engine, now);
        // A capture the system ended (the share stopped from the OS indicator or menu bar) ends
        // its track, and a screen share takes the computer's sound with it: nothing keeps sending
        // after the user saw the share end.
        for kind in sinks.video.ended_kinds() {
          sinks.video.stop(&mut engine, &kind);
          if kind == VideoTrackKind::Screen {
            stop_screen_audio(&mut engine, &mut maybe_screen_audio);
          }
        }
        // The SFU stopped our screen sound (a host's "mute all"): stop capturing it too.
        if maybe_screen_audio.is_some() && !engine.is_sharing_screen_audio() {
          stop_screen_audio(&mut engine, &mut maybe_screen_audio);
        }
        sinks.video.log_counts(now);
      },
      Some((kind, frame)) = encoded_rx.recv() => sinks.video.send(&mut engine, &kind, &frame, Instant::now()),
      maybe_command = commands.recv() => {
        match maybe_command {
          Some(Command::Mute(is_muted)) => {
            engine.set_muted(is_muted);
            capture.set_muted(is_muted);
          },
          Some(Command::MoveHere) => engine.move_here(),
          Some(Command::Deafen(is_deafened)) => {
            engine.set_deafened(is_deafened);
            sinks.mixer.set_deafened(is_deafened);
          },
          Some(Command::SwitchAudio { source: new_source, sink, done }) => {
            // Open the new devices before closing the old ones, so a failure changes nothing.
            let result = AudioDevices::open(&new_source, &sink).map(|new_devices| {
              std::mem::replace(&mut devices, new_devices).close();
              source = new_source;
            });
            if let Err(error) = &result {
              warn!(%error, "switching audio devices failed");
            }
            // The caller may have given up waiting.
            let _ = done.send(result);
          },
          Some(Command::Video(command)) => handle_video(&mut engine, &mut sinks.video, &encoded_tx, command),
          Some(Command::StartScreenAudio(capture, done)) => {
            stop_screen_audio(&mut engine, &mut maybe_screen_audio);
            let result = start_screen_audio(capture).map(|screen_audio| {
              maybe_screen_audio = Some(screen_audio);
              engine.publish_screen_audio();
            });
            if let Err(error) = &result {
              warn!(%error, "sharing the computer's sound failed");
            }
            // The caller may have given up waiting.
            let _ = done.send(result);
          },
          Some(Command::StopScreenAudio) => stop_screen_audio(&mut engine, &mut maybe_screen_audio),
          Some(Command::NetworkChanged) => connection.on_network_changed(Instant::now()),
          Some(Command::SetStatusCues(status_cues)) => sinks.cues.set_setting(status_cues),
          Some(Command::Leave) | None => break Ok(()),
        }
      },
    }
    if let Err(error) = sinks.flush(&mut engine, &mut capture, connection.maybe_link.as_ref()) {
      break Err(error);
    }
    // Refused (locked call, removed user), ended by the host, or taken over by another
    // connection of this device: nothing to rejoin.
    if engine.is_over() {
      break Ok(());
    }
    connection.settle(&engine);
  };

  if let Some(mut screen_audio) = maybe_screen_audio {
    screen_audio.capture.stop();
  }
  if matches!(result, Err(NativeCallError::ConnectionLost)) {
    sinks.play_to_end(CallCue::ConnectionLost, &devices).await;
  }
  devices.close();
  connection.close();
  let mut stats = sinks.stats;
  let video = sinks.video.totals();
  (stats.camera, stats.screen) = (video.camera, video.screen);
  stats.maybe_privacy_code = engine.privacy_code();
  info!(?stats, "call ended");
  result.map(|()| stats)
}

/// The call's connection to the SFU: the current link, and the way back after losing it.
struct CallConnection {
  maybe_link: Option<Link>,
  /// Lost links, kept open until the call is back: dropping one closes it, which the SFU would
  /// take for a deliberate leave and free the slot (it keeps it only for lost connections).
  lost_links: Vec<Link>,
  monitor: PathMonitor,
  route: RouteWatch,
  maybe_schedule: Option<ReconnectSchedule>,
  maybe_attempt: Option<JoinHandle<Result<(Link, JoinTicket), NativeCallError>>>,
  /// The ticket of the current (or last) connection.
  ticket: JoinTicket,
  /// Accepted on every connection besides the ticket's certificates.
  maybe_pinned_hash: Option<[u8; 32]>,
  resilience: ResilienceConfig,
  maybe_ticket_source: Option<Arc<dyn JoinTicketSource>>,
}

/// Notices when the local address the system would send from toward the SFU changes (another
/// interface, a new network), for platforms that don't tell the app (desktops). Mobile apps report
/// changes themselves, earlier.
#[derive(Debug)]
struct RouteWatch {
  maybe_local_ip: Option<IpAddr>,
  maybe_checked_at: Option<Instant>,
}

/// One QUIC connection with its signaling stream; reader tasks feed [`LinkInput`]s.
struct Link {
  /// The connection's own endpoint, for moving it to a new socket.
  endpoint: Endpoint<Client>,
  connection: Connection,
  signal_out: mpsc::UnboundedSender<CallClientMessage>,
  inputs: mpsc::UnboundedReceiver<LinkInput>,
  tasks: Vec<JoinHandle<()>>,
}

/// What happened to the call's connection.
enum ConnectionEvent {
  Input(LinkInput),
  /// Time for the next rejoin attempt.
  AttemptDue,
  AttemptFinished(Result<(Link, JoinTicket), NativeCallError>),
}

/// What arrives on a link.
enum LinkInput {
  Signal(CallServerMessage),
  Datagram(Datagram),
  Ended(LinkEnd),
}

/// How a link ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkEnd {
  /// The SFU closed it on purpose (host control, rejected join): the call is over.
  Closed,
  /// It broke (timeout, reset, silent path): rejoin.
  Lost,
  /// The SFU is shutting down (a restart, a deploy): rejoin, after a random part of a short
  /// spread so the restarted SFU doesn't get every client at once.
  GoingAway,
}

impl CallConnection {
  fn new(config: &NativeCallConfig, link: Link, now: Instant) -> Self {
    Self {
      maybe_link: Some(link),
      lost_links: Vec::new(),
      monitor: PathMonitor::new(now),
      route: RouteWatch::new(),
      maybe_schedule: None,
      maybe_attempt: None,
      ticket: JoinTicket {
        sfu_url: config.sfu_url.clone(),
        token: config.token.clone(),
        certificate_hashes: config.certificate_hashes.clone(),
      },
      maybe_pinned_hash: config.maybe_certificate_hash,
      resilience: config.resilience,
      maybe_ticket_source: config.maybe_ticket_source.clone(),
    }
  }

  /// The next input of the current link, or else the next step of rejoining; pending while
  /// there is nothing to wait for.
  async fn next_event(&mut self) -> ConnectionEvent {
    if let Some(link) = &mut self.maybe_link {
      let input = link
        .inputs
        .recv()
        .await
        .unwrap_or(LinkInput::Ended(LinkEnd::Lost));
      return ConnectionEvent::Input(input);
    }
    if let Some(attempt) = &mut self.maybe_attempt {
      let result = attempt.await.unwrap_or(Err(NativeCallError::TaskEnded));
      self.maybe_attempt = None;
      return ConnectionEvent::AttemptFinished(result);
    }
    match &self.maybe_schedule {
      Some(schedule) => {
        tokio::time::sleep_until(tokio::time::Instant::from_std(schedule.next_attempt_at())).await;
        ConnectionEvent::AttemptDue
      },
      None => std::future::pending().await,
    }
  }

  /// The link broke: media pauses and rejoining starts.
  fn lose(&mut self, engine: &mut CallEngine, now: Instant) {
    let Some(link) = self.maybe_link.take() else {
      return;
    };
    warn!("lost the connection to the call, rejoining");
    link.stop_reading();
    self.lost_links.push(link);
    engine.connection_lost();
    if self.maybe_schedule.is_none() {
      let grace = engine
        .maybe_reconnect_grace()
        .unwrap_or(self.resilience.default_reconnect_grace);
      self.maybe_schedule = Some(ReconnectSchedule::new(now, grace));
    }
  }

  /// The SFU is going away: rejoin like after a loss, the first attempt spread a little.
  fn lose_to_going_away(&mut self, engine: &mut CallEngine, now: Instant) {
    info!("the SFU is going away, rejoining");
    self.lose(engine, now);
    if let Some(schedule) = &mut self.maybe_schedule {
      schedule.spread_first_attempt(now, &self.resilience);
    }
  }

  /// Reads the path's packet count; a path silent for too long (or after a network change) is
  /// lost.
  fn watch_path(&mut self, engine: &mut CallEngine, now: Instant) {
    let Some(link) = &self.maybe_link else {
      return;
    };
    let received = link.connection.quic_connection().stats().udp_rx.datagrams;
    self.monitor.on_received_packets(received, now);
    if self.route.is_due(now) {
      let maybe_local_ip = local_route_to(link.connection.remote_address());
      if self.route.on_route(maybe_local_ip, now) {
        debug!("the route to the SFU changed");
        self.migrate(now);
      }
    }
    if self.monitor.check(now, &self.resilience) == PathState::Dead {
      self.lose(engine, now);
    }
  }

  fn on_network_changed(&mut self, now: Instant) {
    debug!("network changed");
    if self.maybe_link.is_some() {
      self.migrate(now);
    } else if let Some(schedule) = &mut self.maybe_schedule {
      // A network is back: an attempt still waiting on the old one starts over on it, without
      // waiting for the back-off.
      if let Some(attempt) = self.maybe_attempt.take() {
        attempt.abort();
      }
      schedule.hurry(now);
    }
  }

  /// Moves the connection to a fresh socket for the new network; the path then has to show it
  /// works within the handover timeout. A socket that can't be bound (no network at all right
  /// now) leaves the old one in place: the handover check decides.
  fn migrate(&mut self, now: Instant) {
    if let Some(link) = &self.maybe_link {
      match link.rebind() {
        Ok(local_addr) => debug!(%local_addr, "moved the call connection to a new socket"),
        Err(error) => warn!(%error, "could not move the call connection to a new socket"),
      }
    }
    self.monitor.on_network_changed(now, &self.resilience);
  }

  fn start_attempt(&mut self) {
    let maybe_fetch = self.maybe_ticket_source.as_ref().map(|source| source.fetch());
    let fallback = self.ticket.clone();
    let maybe_pinned_hash = self.maybe_pinned_hash;
    let resilience = self.resilience;
    self.maybe_attempt = Some(tokio::spawn(rejoin_attempt(
      maybe_fetch,
      fallback,
      move |ticket: JoinTicket| async move {
        let hashes = accepted_hashes(maybe_pinned_hash, &ticket.certificate_hashes);
        Link::open(&ticket.sfu_url, &hashes, &resilience).await
      },
    )));
  }

  /// A new connection is up: rejoin on it.
  fn resume(&mut self, engine: &mut CallEngine, link: Link, ticket: JoinTicket, now: Instant) {
    info!(attempts = self.attempts(), "connected again, rejoining the call");
    self.maybe_link = Some(link);
    self.monitor = PathMonitor::new(now);
    self.route = RouteWatch::new();
    engine.rejoin(ticket.token.clone(), now);
    self.ticket = ticket;
  }

  fn on_failed_attempt(&mut self, error: &NativeCallError, now: Instant) {
    debug!(%error, "rejoin attempt failed");
    if let Some(schedule) = &mut self.maybe_schedule {
      schedule.on_failed_attempt(now, &self.resilience);
    }
  }

  /// Once the engine is back in the call, the rejoin is over.
  fn settle(&mut self, engine: &CallEngine) {
    if self.maybe_schedule.is_some() && self.maybe_link.is_some() && engine.is_in_call() {
      self.maybe_schedule = None;
      // The slot is on the new connection now; the SFU no longer cares about the old ones.
      self.lost_links.clear();
      info!("back in the call");
    }
  }

  /// Rejoining took longer than the SFU keeps the slot.
  fn is_out_of_time(&self, now: Instant) -> bool {
    self
      .maybe_schedule
      .as_ref()
      .is_some_and(|schedule| schedule.has_expired(now))
  }

  fn attempts(&self) -> u32 {
    self
      .maybe_schedule
      .as_ref()
      .map_or(0, ReconnectSchedule::attempts)
  }

  /// Leaves: the SFU frees the slot at once.
  fn close(self) {
    if let Some(attempt) = self.maybe_attempt {
      attempt.abort();
    }
    for link in self.maybe_link.into_iter().chain(self.lost_links) {
      link.stop_reading();
      link.connection.close(LEAVE.into(), b"leave");
    }
  }
}

impl RouteWatch {
  fn new() -> Self {
    Self {
      maybe_local_ip: None,
      maybe_checked_at: None,
    }
  }

  fn is_due(&self, now: Instant) -> bool {
    self
      .maybe_checked_at
      .is_none_or(|checked_at| now.saturating_duration_since(checked_at) >= ROUTE_CHECK_INTERVAL)
  }

  /// Records the local address of the current route; returns whether it changed from a known one
  /// (no route at all counts as a change too: the network went away).
  fn on_route(&mut self, maybe_local_ip: Option<IpAddr>, now: Instant) -> bool {
    let is_first = self.maybe_checked_at.is_none();
    self.maybe_checked_at = Some(now);
    let has_changed = !is_first && maybe_local_ip != self.maybe_local_ip;
    self.maybe_local_ip = maybe_local_ip;
    has_changed
  }
}

/// The local address the system would send from toward `remote`: connecting a UDP socket picks
/// the route without sending anything.
fn local_route_to(remote: SocketAddr) -> Option<IpAddr> {
  // Dual-stack sockets show IPv4 peers as IPv4-mapped; look the route up in their own family.
  let remote = SocketAddr::new(remote.ip().to_canonical(), remote.port());
  let unspecified: IpAddr = match remote {
    SocketAddr::V4(_) => Ipv4Addr::UNSPECIFIED.into(),
    SocketAddr::V6(_) => Ipv6Addr::UNSPECIFIED.into(),
  };
  let socket = UdpSocket::bind(SocketAddr::new(unspecified, 0)).ok()?;
  socket.connect(remote).ok()?;
  Some(socket.local_addr().ok()?.ip())
}

impl Link {
  /// Connects to the SFU and opens the signaling stream.
  async fn open(
    sfu_url: &str,
    certificate_hashes: &[[u8; 32]],
    resilience: &ResilienceConfig,
  ) -> Result<Self, NativeCallError> {
    let (endpoint, connection) = connect(sfu_url, certificate_hashes, resilience).await?;
    let (send, recv) = connection
      .open_bi()
      .await
      .map_err(|error| NativeCallError::Connect(error.to_string()))?
      .await
      .map_err(|error| NativeCallError::Connect(error.to_string()))?;
    let (signal_out, signal_out_rx) = mpsc::unbounded_channel::<CallClientMessage>();
    let (input_tx, inputs) = mpsc::unbounded_channel();
    let tasks = vec![
      tokio::spawn(write_signaling(send, signal_out_rx)),
      tokio::spawn(read_signaling(recv, connection.clone(), input_tx.clone())),
      tokio::spawn(read_datagrams(connection.clone(), input_tx)),
    ];
    Ok(Self {
      endpoint,
      connection,
      signal_out,
      inputs,
      tasks,
    })
  }

  /// Switches the connection to a fresh wildcard socket of the current one's family (dual-stack
  /// for IPv6, as the first one); returns its local address.
  fn rebind(&self) -> std::io::Result<SocketAddr> {
    let current = self.endpoint.local_addr()?;
    let socket = bind_wildcard(current.is_ipv6())?;
    let local_addr = socket.local_addr()?;
    self.endpoint.rebind(socket)?;
    Ok(local_addr)
  }

  fn stop_reading(&self) {
    for task in &self.tasks {
      task.abort();
    }
  }
}

/// Races the QUIC handshake over the SFU's addresses (Happy Eyeballs, issue #420): a broken IPv6
/// route costs one attempt delay before IPv4 is tried.
async fn connect(
  sfu_url: &str,
  certificate_hashes: &[[u8; 32]],
  resilience: &ResilienceConfig,
) -> Result<(Endpoint<Client>, Connection), NativeCallError> {
  let addresses = sfu_addresses(sfu_url)
    .await
    .map_err(|error| NativeCallError::Connect(error.to_string()))?;
  race(interleave(addresses), ATTEMPT_DELAY, |address| {
    connect_to(sfu_url, address, certificate_hashes, resilience)
  })
  .await
  .map_err(|error| match error {
    RaceError::NoAddress => NativeCallError::Connect("the SFU's host has no address".into()),
    RaceError::Failed(error) => error,
  })
}

/// One attempt at `address`. A socket bound to the wildcard address (it follows route changes,
/// which lets QUIC migrate), with keep-alives for carrier NATs. Self-signed certificates are
/// accepted by their hashes; without any, the trusted roots decide.
async fn connect_to(
  sfu_url: &str,
  address: SocketAddr,
  certificate_hashes: &[[u8; 32]],
  resilience: &ResilienceConfig,
) -> Result<(Endpoint<Client>, Connection), NativeCallError> {
  let builder = ClientConfig::builder().with_bind_default();
  let builder = if certificate_hashes.is_empty() {
    with_trusted_roots(builder)
  } else {
    builder.with_server_certificate_hashes(certificate_hashes.iter().copied().map(Sha256Digest::new))
  };
  let client_config = builder
    .dns_resolver(PinnedResolver(address))
    .keep_alive_interval(Some(resilience.keep_alive_interval))
    .max_idle_timeout(Some(resilience.idle_timeout))
    .map_err(|error| NativeCallError::Connect(error.to_string()))?
    .build();
  let endpoint =
    Endpoint::client(client_config).map_err(|error| NativeCallError::Connect(error.to_string()))?;
  let connection = endpoint
    .connect(sfu_url)
    .await
    .map_err(|error| NativeCallError::Connect(error.to_string()))?;
  Ok((endpoint, connection))
}

/// The certificates a connection accepts: the pinned one (development tools) and the ticket's,
/// each once.
fn accepted_hashes(maybe_pinned: Option<[u8; 32]>, ticket_hashes: &[[u8; 32]]) -> Vec<[u8; 32]> {
  let mut hashes: Vec<[u8; 32]> = maybe_pinned.into_iter().collect();
  for hash in ticket_hashes {
    if !hashes.contains(hash) {
      hashes.push(*hash);
    }
  }
  hashes
}

/// An unspecified-address UDP socket on a free port, like wtransport's default client binding:
/// IPv6 with dual-stack on (Windows defaults to IPv6-only), or IPv4.
fn bind_wildcard(is_ipv6: bool) -> std::io::Result<UdpSocket> {
  let (domain, address) = if is_ipv6 {
    (Domain::IPV6, SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0))
  } else {
    (Domain::IPV4, SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0))
  };
  let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
  if is_ipv6 {
    socket.set_only_v6(false)?;
  }
  socket.bind(&address.into())?;
  Ok(socket.into())
}

/// A deliberate close by the SFU ends the call, unless the SFU is going away; anything else is a
/// loss to recover from.
fn link_end(error: &ConnectionError) -> LinkEnd {
  match error {
    ConnectionError::ApplicationClosed(close) => application_close_end(close.code().into_inner()),
    ConnectionError::LocallyClosed | ConnectionError::LocalH3Error(_) => LinkEnd::Closed,
    ConnectionError::ConnectionClosed(_)
    | ConnectionError::TimedOut
    | ConnectionError::QuicProto(_)
    | ConnectionError::CidsExhausted => LinkEnd::Lost,
  }
}

/// How a close by the SFU with this application code ends the link.
fn application_close_end(code: u64) -> LinkEnd {
  if code == u64::from(SFU_GOING_AWAY_CLOSE_CODE) {
    LinkEnd::GoingAway
  } else {
    LinkEnd::Closed
  }
}

fn start_screen_audio(mut capture: Box<dyn SystemAudioCapture>) -> Result<ScreenAudio, SystemAudioError> {
  let pipeline = CapturePipeline::new(OpusEncoderConfig::screen_audio())
    .map_err(|error| SystemAudioError::Device(error.to_string()))?;
  let (sender, samples) = system_audio_queue();
  capture.start(sender)?;
  Ok(ScreenAudio {
    capture,
    samples,
    pipeline,
  })
}

fn stop_screen_audio(engine: &mut CallEngine, maybe_screen_audio: &mut Option<ScreenAudio>) {
  if let Some(mut screen_audio) = maybe_screen_audio.take() {
    screen_audio.capture.stop();
    engine.unpublish_screen_audio();
  }
}

fn handle_video(
  engine: &mut CallEngine,
  video: &mut VideoState,
  encoded: &mpsc::UnboundedSender<(VideoTrackKind, EncodedVideoFrame)>,
  command: VideoCommand,
) {
  match command {
    VideoCommand::Start(config, done) => {
      video.stop(engine, &config.kind);
      let kind = config.kind.clone();
      let result = VideoSender::start(config, encoded.clone())
        .map(|(sender, first_picture)| {
          engine.publish_video(sender.kind.clone(), VideoCodec::H264, sender.layers.clone());
          video.senders.push((sender, None));
          first_picture
        })
        .inspect_err(|error| warn!(%error, ?kind, "video capture not started"));
      // The caller may have given up waiting.
      let _ = done.send(result);
    },
    VideoCommand::Stop(kind) => video.stop(engine, &kind),
    VideoCommand::Watch {
      stream_id,
      maybe_max_layer,
      sink,
    } => match VideoReceiver::start(sink, video.keyframe_requester(stream_id)) {
      Ok(receiver) => {
        video.receivers.insert(stream_id, receiver);
        engine.subscribe_video(stream_id, maybe_max_layer.or(Some(u8::MAX)));
      },
      Err(error) => warn!(%error, "cannot decode video"),
    },
    VideoCommand::Unwatch(stream_id) => {
      if video.receivers.remove(&stream_id).is_some() {
        engine.subscribe_video(stream_id, None);
      }
    },
    VideoCommand::RequestKeyframe(stream_id) => engine.request_keyframe(stream_id, Instant::now()),
    VideoCommand::Publish { kind, codec, layers } => engine.publish_video(kind, codec, layers),
    VideoCommand::Unpublish(stream_id) => engine.unpublish_video(stream_id),
    VideoCommand::Subscribe {
      stream_id,
      maybe_max_layer,
    } => engine.subscribe_video(stream_id, maybe_max_layer),
    VideoCommand::Send { stream_id, frame } => {
      // Frames the app encodes itself; what becomes of each is its own business.
      let _ = engine.send_video(stream_id, &frame, Instant::now());
    },
  }
}

/// Where engine outputs go besides the network.
struct Sinks {
  events: mpsc::UnboundedSender<CallEngineEvent>,
  mixer: PlaybackMixer,
  stats: CallStats,
  video: VideoState,
  /// Status sounds, mixed into the playback (never sent).
  cues: CuePlayer,
}

impl Sinks {
  /// The next 20 ms for the speakers: the call's mix with the status sounds on top. The sounds
  /// play even when deafened (they are status, not call audio) and reach the echo canceller as
  /// far-end signal, so the microphone doesn't send them.
  fn next_playback_frame(&mut self) -> Result<Vec<f32>, NativeCallError> {
    let cue = self.cues.next_samples(FRAME_SAMPLES_PER_CHANNEL);
    let has_cue = cue.iter().any(|sample| *sample != 0.0);
    if has_cue {
      self.mixer.push_echo_reference(&cue);
    }
    let mut mix = self.mixer.next_frame()?;
    if mix.iter().any(|sample| sample.abs() > 0.01) {
      self.stats.audible_frames_played += 1;
    }
    if has_cue {
      for (sample, cue_sample) in mix.iter_mut().zip(cue) {
        *sample = (*sample + cue_sample).clamp(-1.0, 1.0);
      }
    }
    Ok(mix)
  }

  /// Plays `cue` out (in real time) before the devices close.
  async fn play_to_end(&mut self, cue: CallCue, devices: &AudioDevices) {
    self.cues.play(cue);
    let mut frame_tick = tokio::time::interval(FRAME_INTERVAL);
    while self.cues.is_playing(cue) {
      frame_tick.tick().await;
      let samples = self.cues.next_samples(FRAME_SAMPLES_PER_CHANNEL);
      if let Some(buffer) = &devices.maybe_playback {
        buffer.push(&samples);
      }
    }
    // What is still buffered for the speakers.
    tokio::time::sleep(FRAME_INTERVAL * 5).await;
  }

  /// Sends what the engine produced. Without a link (rejoining), signaling and media are
  /// dropped: the engine renews what matters when it is back.
  fn flush(
    &mut self,
    engine: &mut CallEngine,
    capture: &mut CapturePipeline,
    maybe_link: Option<&Link>,
  ) -> Result<(), NativeCallError> {
    while let Some(output) = engine.poll_output() {
      match output {
        EngineOutput::Signal(message) => {
          if let Some(link) = maybe_link
            && link.signal_out.send(message).is_err()
          {
            // The writer stopped with the connection; the reader reports the loss.
            debug!("signaling writer stopped");
          }
        },
        EngineOutput::Datagram(bytes) => {
          // Unreliable by design: a full send buffer drops the frame.
          if let Some(link) = maybe_link
            && let Err(error) = link.connection.send_datagram(&bytes)
          {
            debug!(%error, "datagram dropped");
          }
        },
        EngineOutput::Event(CallEngineEvent::AudioFrame {
          sender,
          origin,
          sequence_number,
          opus,
        }) => {
          self.stats.frames_received += 1;
          self.mixer.push(sender, origin, sequence_number, opus)?;
        },
        EngineOutput::Event(CallEngineEvent::VideoFrame(frame)) => {
          self.video.count_received(frame.stream_id);
          // Watched tracks are decoded for the app's sink; tracks the app subscribed to itself
          // (subscribe_video) arrive encoded as events.
          match self.video.receivers.get(&frame.stream_id) {
            Some(receiver) => receiver.push(frame),
            None => {
              let _ = self.events.send(CallEngineEvent::VideoFrame(frame));
            },
          }
        },
        EngineOutput::Event(event) => {
          match &event {
            CallEngineEvent::ParticipantLeft(device_id) => self.mixer.remove(*device_id),
            CallEngineEvent::ScreenAudioChanged {
              device_id,
              is_shared: false,
            } => self.mixer.remove_stream(*device_id, AudioOrigin::Screen),
            CallEngineEvent::VideoPublished(track) if track.device_id == engine.device_id() => {
              self.video.on_published(&track.kind, track.stream_id);
            },
            CallEngineEvent::VideoPublished(track) => {
              self
                .video
                .remote_kinds
                .insert(track.stream_id, track.kind.clone());
            },
            CallEngineEvent::VideoUnpublished(stream_id) => {
              self.video.receivers.remove(stream_id);
              self.video.remote_kinds.remove(stream_id);
              // An own track the SFU dropped (rejoin without the slot) is published again.
              self.video.on_unpublished(*stream_id);
            },
            CallEngineEvent::KeyframeRequested { stream_id, layer } => {
              if let Some(sender) = self.video.sender_of(*stream_id) {
                sender.request_keyframe(*layer);
              }
            },
            CallEngineEvent::LayersNeeded { stream_id, layers } => {
              if let Some(sender) = self.video.sender_of(*stream_id) {
                sender.set_needed_layers(layers.clone());
              }
            },
            CallEngineEvent::UplinkEstimate { loss_percent, .. } => {
              capture.set_measured_loss_percent(*loss_percent)?;
            },
            _ => {},
          }
          self.cues.on_event(&event);
          // The app may have stopped listening; the call goes on.
          let _ = self.events.send(event);
        },
      }
    }
    Ok(())
  }
}

impl VideoState {
  /// What a decoder thread of `stream_id` calls when it needs a keyframe: a command to the call
  /// task (nothing once the call ended).
  fn keyframe_requester(&self, stream_id: MediaStreamId) -> KeyframeNeeded {
    let maybe_commands = self.maybe_commands.clone();
    Box::new(move || {
      if let Some(commands) = maybe_commands
        .as_ref()
        .and_then(mpsc::WeakUnboundedSender::upgrade)
      {
        // A closed queue means the call ended meanwhile.
        let _ = commands.send(Command::Video(VideoCommand::RequestKeyframe(stream_id)));
      }
    })
  }

  /// Hands an own picture to the engine and counts what became of it.
  fn send(
    &mut self,
    engine: &mut CallEngine,
    kind: &VideoTrackKind,
    frame: &EncodedVideoFrame,
    now: Instant,
  ) {
    let outcome = match self.stream_of(kind) {
      Some(stream_id) => engine.send_video(stream_id, frame, now),
      None => VideoSendOutcome::NotPublished,
    };
    let Some(stats) = self.counts.of_mut(kind) else {
      return;
    };
    match outcome {
      VideoSendOutcome::Sent => stats.pictures_sent += 1,
      VideoSendOutcome::LayerPaused => stats.pictures_paused += 1,
      // Expected for a moment at the start; `log_counts` warns when it lasts.
      VideoSendOutcome::NotPublished | VideoSendOutcome::NotSecured => {
        stats.pictures_dropped += 1;
        debug!(?kind, ?outcome, "video picture not sent");
      },
      VideoSendOutcome::Failed => {
        stats.pictures_dropped += 1;
        self
          .send_warning
          .warn(|suppressed| warn!(?kind, suppressed, "video picture not sent"));
      },
    }
  }

  fn count_received(&mut self, stream_id: MediaStreamId) {
    if let Some(kind) = self.remote_kinds.get(&stream_id)
      && let Some(stats) = self.counts.of_mut(kind)
    {
      stats.pictures_received += 1;
    }
  }

  /// The counters so far: stopped tracks, running ones and what was sent and received.
  fn totals(&self) -> VideoCounts {
    let mut totals = VideoCounts {
      camera: self.counts.camera,
      screen: self.counts.screen,
    };
    for (sender, _) in &self.senders {
      if let Some(stats) = totals.of_mut(&sender.kind) {
        let progress = sender.progress();
        stats.frames_captured += progress.frames_captured;
        stats.pictures_encoded += progress.pictures_encoded;
        stats.frames_black += progress.frames_black;
      }
    }
    totals
  }

  /// Logs the counters every [`VIDEO_LOG_INTERVAL`] while there is video, and warns when an own
  /// track's pictures stopped somewhere during it (capture, encoder, sending), so a log shows
  /// where they stop.
  fn log_counts(&mut self, now: Instant) {
    if self.senders.is_empty() && self.remote_kinds.is_empty() {
      self.maybe_last_log = None;
      return;
    }
    let Some((last, previous)) = self.maybe_last_log else {
      self.maybe_last_log = Some((now, self.totals()));
      return;
    };
    if now.saturating_duration_since(last) < VIDEO_LOG_INTERVAL {
      return;
    }
    let totals = self.totals();
    debug!(camera = ?totals.camera, screen = ?totals.screen, "video counters");
    for (sender, _) in &self.senders {
      let (Some(before), Some(after)) = (previous.of(&sender.kind), totals.of(&sender.kind)) else {
        continue;
      };
      if let Some(stall) = video_stall(&sender.kind, before, after) {
        warn!(kind = ?sender.kind, ?stall, interval = ?VIDEO_LOG_INTERVAL, stats = ?after, "own video stalled");
      }
    }
    self.maybe_last_log = Some((now, totals));
  }

  fn stream_of(&self, kind: &VideoTrackKind) -> Option<MediaStreamId> {
    self
      .senders
      .iter()
      .find(|(sender, _)| sender.kind == *kind)
      .and_then(|(_, maybe_stream)| *maybe_stream)
  }

  fn sender_of(&self, stream_id: MediaStreamId) -> Option<&VideoSender> {
    self
      .senders
      .iter()
      .find(|(_, maybe_stream)| *maybe_stream == Some(stream_id))
      .map(|(sender, _)| sender)
  }

  fn on_unpublished(&mut self, stream_id: MediaStreamId) {
    for (_, maybe_stream) in &mut self.senders {
      if *maybe_stream == Some(stream_id) {
        *maybe_stream = None;
      }
    }
  }

  fn on_published(&mut self, kind: &VideoTrackKind, stream_id: MediaStreamId) {
    if let Some((_, maybe_stream)) = self
      .senders
      .iter_mut()
      .find(|(sender, maybe_stream)| sender.kind == *kind && maybe_stream.is_none())
    {
      *maybe_stream = Some(stream_id);
    }
  }

  /// Kinds of own tracks whose capture ended by itself.
  fn ended_kinds(&self) -> Vec<VideoTrackKind> {
    self
      .senders
      .iter()
      .filter(|(sender, _)| sender.has_ended())
      .map(|(sender, _)| sender.kind.clone())
      .collect()
  }

  /// Stops capturing a track of `kind` and unpublishes it.
  fn stop(&mut self, engine: &mut CallEngine, kind: &VideoTrackKind) {
    let (stopped, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.senders)
      .into_iter()
      .partition(|(sender, _)| sender.kind == *kind);
    self.senders = kept;
    for (sender, maybe_stream) in stopped {
      if let Some(stream_id) = maybe_stream {
        engine.unpublish_video(stream_id);
      }
      let kind = sender.kind.clone();
      // Read after the thread stopped: nothing changes the counters any more.
      let progress = sender.finish();
      if let Some(stats) = self.counts.of_mut(&kind) {
        stats.frames_captured += progress.frames_captured;
        stats.pictures_encoded += progress.pictures_encoded;
        stats.frames_black += progress.frames_black;
      }
    }
  }
}

/// Whether an own track of `kind` stalled between two readings of its counters.
fn video_stall(kind: &VideoTrackKind, before: &VideoStats, after: &VideoStats) -> Option<VideoStall> {
  let captured = after.frames_captured.saturating_sub(before.frames_captured);
  let encoded = after.pictures_encoded.saturating_sub(before.pictures_encoded);
  let sent = after.pictures_sent.saturating_sub(before.pictures_sent);
  let dropped = after.pictures_dropped.saturating_sub(before.pictures_dropped);
  if captured == 0 {
    return (*kind == VideoTrackKind::Camera).then_some(VideoStall::Capture);
  }
  if encoded == 0 {
    return Some(VideoStall::Encoder);
  }
  (sent == 0 && dropped > 0).then_some(VideoStall::Sending)
}

impl VideoCounts {
  fn of(&self, kind: &VideoTrackKind) -> Option<&VideoStats> {
    match kind {
      VideoTrackKind::Camera => Some(&self.camera),
      VideoTrackKind::Screen => Some(&self.screen),
      _ => None,
    }
  }

  fn of_mut(&mut self, kind: &VideoTrackKind) -> Option<&mut VideoStats> {
    match kind {
      VideoTrackKind::Camera => Some(&mut self.camera),
      VideoTrackKind::Screen => Some(&mut self.screen),
      _ => None,
    }
  }
}

impl AudioDevices {
  /// Starts the audio thread with the devices the source and sink need.
  fn open(source: &AudioSource, sink: &AudioSink) -> Result<Self, AudioDeviceError> {
    let (capture_tx, capture_rx) = mpsc::unbounded_channel();
    let maybe_output_choice = match sink {
      AudioSink::Speakers(choice) => Some(choice.clone()),
      AudioSink::Discard => None,
    };
    let maybe_input_choice = match source {
      AudioSource::Microphone(choice) => Some(choice.clone()),
      AudioSource::Silence | AudioSource::Tone { .. } => None,
    };
    let maybe_playback = maybe_output_choice
      .is_some()
      .then(|| Arc::new(PlaybackBuffer::new(MAX_PLAYBACK_BUFFER_SAMPLES)));
    let playback_for_thread = maybe_playback.clone();
    let wants_microphone = maybe_input_choice.is_some();
    let (stop, stop_rx) = std_mpsc::channel::<()>();
    let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), AudioDeviceError>>();
    let thread = std::thread::spawn(move || {
      let opened = (|| -> Result<(Option<AudioInput>, Option<AudioOutput>), AudioDeviceError> {
        let maybe_input = match &maybe_input_choice {
          Some(choice) => Some(AudioInput::open(choice, move |samples| {
            // The call task may be gone; the device thread stops right after.
            let _ = capture_tx.send(samples.to_vec());
          })?),
          None => None,
        };
        let maybe_output = match (&maybe_output_choice, playback_for_thread) {
          (Some(choice), Some(buffer)) => Some(AudioOutput::open(choice, buffer)?),
          _ => None,
        };
        Ok((maybe_input, maybe_output))
      })();
      match opened {
        Ok(streams) => {
          let _ = ready_tx.send(Ok(()));
          // Keep the streams alive until the call ends.
          let _ = stop_rx.recv();
          drop(streams);
        },
        Err(error) => {
          let _ = ready_tx.send(Err(error));
        },
      }
    });
    ready_rx.recv().map_err(|_| AudioDeviceError::AudioThread)??;
    Ok(Self {
      maybe_capture: wants_microphone.then_some(capture_rx),
      maybe_playback,
      stop,
      thread,
    })
  }

  fn close(self) {
    let _ = self.stop.send(());
    if self.thread.join().is_err() {
      warn!("audio thread panicked");
    }
  }
}

/// Next microphone chunk, or pending forever without a microphone.
async fn recv_capture(maybe_capture: &mut Option<mpsc::UnboundedReceiver<Vec<f32>>>) -> Option<Vec<f32>> {
  match maybe_capture {
    Some(receiver) => receiver.recv().await,
    None => std::future::pending().await,
  }
}

async fn recv_screen_audio(maybe_screen_audio: &mut Option<ScreenAudio>) -> Option<Vec<f32>> {
  match maybe_screen_audio {
    Some(screen_audio) => screen_audio.samples.recv().await,
    None => std::future::pending().await,
  }
}

async fn write_signaling(mut send: SendStream, mut messages: mpsc::UnboundedReceiver<CallClientMessage>) {
  while let Some(message) = messages.recv().await {
    let Ok(body) = serde_json::to_vec(&message) else {
      warn!("could not encode a signaling message");
      continue;
    };
    let Ok(length) = u32::try_from(body.len()) else {
      continue;
    };
    let mut framed = length.to_be_bytes().to_vec();
    framed.extend_from_slice(&body);
    if let Err(error) = send.write_all(&framed).await {
      debug!(%error, "signaling write failed");
      return;
    }
  }
}

/// Reads framed signaling messages; reports how the link ended when the stream does.
async fn read_signaling(
  mut recv: RecvStream,
  connection: Connection,
  inputs: mpsc::UnboundedSender<LinkInput>,
) {
  let end = loop {
    let mut length = [0u8; 4];
    if let Err(error) = recv.read_exact(&mut length).await {
      break stream_end(&error, &connection).await;
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_SIGNALING_BYTES {
      warn!(length, "signaling message too large");
      break LinkEnd::Lost;
    }
    let mut body = vec![0u8; length];
    if let Err(error) = recv.read_exact(&mut body).await {
      break stream_end(&error, &connection).await;
    }
    match serde_json::from_slice::<CallServerMessage>(&body) {
      Ok(message) => {
        if inputs.send(LinkInput::Signal(message)).is_err() {
          return;
        }
      },
      Err(error) => warn!(%error, "invalid signaling message"),
    }
  };
  // The call task may be gone.
  let _ = inputs.send(LinkInput::Ended(end));
}

/// The SFU finishes the signaling stream only on purpose (a refused join); a broken stream goes
/// with its connection, whose close reason tells a loss from a deliberate close.
async fn stream_end(error: &StreamReadExactError, connection: &Connection) -> LinkEnd {
  if matches!(error, StreamReadExactError::FinishedEarly(_)) {
    return LinkEnd::Closed;
  }
  match tokio::time::timeout(STREAM_CLOSE_WAIT, connection.closed()).await {
    Ok(error) => link_end(&error),
    // A connection without its signaling stream is of no use either.
    Err(_) => LinkEnd::Lost,
  }
}

async fn read_datagrams(connection: Connection, inputs: mpsc::UnboundedSender<LinkInput>) {
  loop {
    match connection.receive_datagram().await {
      Ok(datagram) => {
        if inputs.send(LinkInput::Datagram(datagram)).is_err() {
          return;
        }
      },
      Err(error) => {
        // The call task may be gone.
        let _ = inputs.send(LinkInput::Ended(link_end(&error)));
        return;
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod video_stalls {
    use super::*;

    #[test]
    fn flowing_video_is_no_stall() {
      let before = stats(100, 300, 290, 10);
      let after = stats(400, 1200, 1190, 10);
      assert_eq!(video_stall(&VideoTrackKind::Camera, &before, &after), None);
    }

    #[test]
    fn a_camera_without_frames_stalls_but_a_still_screen_does_not() {
      let before = stats(100, 300, 300, 0);
      assert_eq!(
        video_stall(&VideoTrackKind::Camera, &before, &before),
        Some(VideoStall::Capture)
      );
      // Screen captures deliver frames only when something on the screen changes.
      assert_eq!(video_stall(&VideoTrackKind::Screen, &before, &before), None);
    }

    #[test]
    fn frames_without_pictures_are_an_encoder_stall() {
      let before = stats(100, 300, 300, 0);
      let after = stats(400, 300, 300, 0);
      assert_eq!(
        video_stall(&VideoTrackKind::Screen, &before, &after),
        Some(VideoStall::Encoder)
      );
    }

    #[test]
    fn pictures_that_never_leave_are_a_send_stall_but_paused_layers_are_not() {
      let before = stats(100, 300, 0, 300);
      let after = stats(400, 1200, 0, 1200);
      assert_eq!(
        video_stall(&VideoTrackKind::Camera, &before, &after),
        Some(VideoStall::Sending)
      );
      // Nobody watches: every picture is paused, which is no failure.
      let mut paused = stats(400, 1200, 0, 300);
      paused.pictures_paused = 900;
      assert_eq!(video_stall(&VideoTrackKind::Camera, &before, &paused), None);
    }

    fn stats(captured: u64, encoded: u64, sent: u64, dropped: u64) -> VideoStats {
      VideoStats {
        frames_captured: captured,
        pictures_encoded: encoded,
        pictures_sent: sent,
        pictures_dropped: dropped,
        ..VideoStats::default()
      }
    }
  }

  /// Issue #420 for calls: nothing answers QUIC on the SFU's IPv6 address (the handshake is
  /// swallowed, as on a broken route), IPv4 works. The call connects over IPv4.
  #[tokio::test]
  async fn the_sfu_handshake_falls_back_to_ipv4() {
    let identity = wtransport::Identity::self_signed(["localhost"]).unwrap();
    let hash: [u8; 32] = *identity.certificate_chain().as_slice()[0].hash().as_ref();
    let config = wtransport::ServerConfig::builder()
      .with_bind_address(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
      .with_identity(identity)
      .build();
    let server = Endpoint::server(config).unwrap();
    let port = server.local_addr().unwrap().port();
    tokio::spawn(async move {
      let mut sessions = Vec::new();
      loop {
        if let Ok(request) = server.accept().await.await
          && let Ok(connection) = request.accept().await
        {
          sessions.push(connection);
        }
      }
    });
    // Nothing listens for UDP on [::1] at this port. The kernel answers with ICMP "port
    // unreachable", but the client's socket is unconnected and quinn doesn't act on ICMP errors,
    // so the handshake just waits, like on a route that swallows packets. Should a platform report
    // the error and fail the attempt at once, IPv4 starts right away and the elapsed-time check
    // below would fail: that would be a change in the test's assumption, not in the racing.
    let silent_v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let url = format!("https://localhost:{port}/call");
    let resilience = ResilienceConfig::default();
    let hashes = [hash];
    let started = Instant::now();
    let racing = race(vec![silent_v6, v4], ATTEMPT_DELAY, |address| {
      connect_to(&url, address, &hashes, &resilience)
    });
    let (_endpoint, connection) = tokio::time::timeout(Duration::from_secs(5), racing)
      .await
      .expect("fell back to IPv4 within the timeout")
      .unwrap();
    assert_eq!(connection.remote_address().ip().to_canonical(), v4.ip());
    assert!(started.elapsed() >= ATTEMPT_DELAY, "IPv6 was not tried first");
  }

  #[test]
  fn a_new_local_address_toward_the_sfu_is_a_network_change() {
    let start = Instant::now();
    let mut route = RouteWatch::new();
    let wifi: IpAddr = "192.0.2.10".parse().unwrap();
    let mobile: IpAddr = "2001:db8::7".parse().unwrap();
    assert!(route.is_due(start));
    assert!(
      !route.on_route(Some(wifi), start),
      "the first lookup is the baseline"
    );
    assert!(!route.is_due(start + Duration::from_millis(500)));
    assert!(!route.on_route(Some(wifi), start + ROUTE_CHECK_INTERVAL));
    assert!(route.on_route(Some(mobile), start + 2 * ROUTE_CHECK_INTERVAL));
    assert!(
      route.on_route(None, start + 3 * ROUTE_CHECK_INTERVAL),
      "no route at all"
    );
  }

  #[test]
  fn only_the_sfu_going_away_is_a_close_to_rejoin_after() {
    assert_eq!(
      application_close_end(u64::from(SFU_GOING_AWAY_CLOSE_CODE)),
      LinkEnd::GoingAway
    );
    // A leave, a host's removal or end, a replaced connection: final.
    for code in [0, 0x4801, 0x4802, 0x4804] {
      assert_eq!(application_close_end(code), LinkEnd::Closed, "{code:#x}");
    }
  }

  #[test]
  fn a_connection_accepts_the_pinned_and_the_tickets_certificates() {
    assert!(accepted_hashes(None, &[]).is_empty(), "trusted roots");
    assert_eq!(accepted_hashes(None, &[[1; 32], [2; 32]]), vec![[1; 32], [2; 32]]);
    assert_eq!(
      accepted_hashes(Some([1; 32]), &[[1; 32], [3; 32]]),
      vec![[1; 32], [3; 32]]
    );
  }

  #[test]
  fn fresh_sockets_keep_the_family_and_ipv6_is_dual_stack() {
    let v6 = bind_wildcard(true).unwrap();
    assert!(v6.local_addr().unwrap().is_ipv6());
    assert!(!Socket::from(v6).only_v6().unwrap());
    let v4 = bind_wildcard(false).unwrap();
    assert!(v4.local_addr().unwrap().is_ipv4());
  }

  #[test]
  fn loopback_peers_are_reached_from_loopback() {
    let v6 = local_route_to(SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 4433));
    assert_eq!(v6, Some(Ipv6Addr::LOCALHOST.into()));
    // An IPv4 peer as a dual-stack socket reports it: looked up as IPv4.
    let mapped = SocketAddr::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped().into(), 4433);
    assert_eq!(local_route_to(mapped), Some(Ipv4Addr::LOCALHOST.into()));
  }
}
