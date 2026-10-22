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

mod video;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use js_sys::{Array, Float32Array, Function, Object, Reflect, Uint8Array};
use teamcraft_proto::{
  CallClientMessage, CallServerMessage, DeviceId, MediaStreamId, VideoCodec, VideoTrackKind,
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

use self::video::{TrackSender, VideoView};

use crate::audio_frame::{EncodedAudioFrame, FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use crate::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource, sine_tone};
use crate::audio_level::{DEFAULT_VOICE_THRESHOLD_DBOV, measure_audio_level};
use crate::call_auth::CallKeyAuthenticator;
use crate::call_engine::{
  AudioOrigin, CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
};
use crate::datagram::SimulcastLayer;
use crate::jitter::{JitterBuffer, JitterBufferConfig, Playout};
use crate::rtp::AudioLevel;
use crate::video::EncodedVideoFrame;
use crate::web_api::{
  AudioData, AudioDecoder, AudioEncoder, EncodedAudioChunk, WORKLET_SOURCE, WebTransport,
  WebTransportBidirectionalStream, dictionary, has_global,
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
/// Mixed frames above this amplitude count as audible in [`CallStats`].
const AUDIBLE_AMPLITUDE: f32 = 0.01;

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
  /// Accept a self-signed SFU certificate by its SHA-256 hash (`serverCertificateHashes`,
  /// development); `None` uses the browser's trusted roots.
  pub maybe_certificate_hash: Option<[u8; 32]>,
  pub token: String,
  pub engine: CallEngineConfig,
  pub source: AudioSource,
  pub sink: AudioSink,
  pub maybe_trusted_roster: Option<Vec<DeviceId>>,
  /// Binds DAVE keys to device identities (e2ee.md §8); `None` only for development pages.
  pub maybe_authenticator: Option<Arc<dyn CallKeyAuthenticator>>,
  pub jitter: JitterBufferConfig,
}

/// The browser's microphones and speakers. Labels are empty until the page has microphone
/// permission (browser privacy rule).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BrowserAudioDevices {
  pub inputs: Vec<AudioDeviceInfo>,
  pub outputs: Vec<AudioDeviceInfo>,
}

#[derive(Debug, Error)]
pub enum WebCallError {
  #[error("{context} failed: {message}")]
  Browser { context: &'static str, message: String },
  #[error("this browser lacks {0}")]
  Unsupported(&'static str),
  #[error(transparent)]
  Engine(#[from] CallEngineError),
  #[error("the call ended")]
  Ended,
}

/// Everything the call task reacts to.
enum Input {
  Signal(CallServerMessage),
  SignalingClosed,
  Datagram(Vec<u8>),
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
  Command(Command),
}

enum Command {
  Mute(bool),
  Deafen(bool),
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
  Leave,
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
    let audio = WebAudio::open(&config.source, &config.sink, &inputs).await?;
    let transport = Transport::connect(&config, &inputs).await?;
    let (event_tx, events) = mpsc::unbounded();
    let (finished_tx, finished) = oneshot::channel();
    let task_inputs = inputs.clone();
    spawn_local(async move {
      let result = run_call(config, transport, audio, task_inputs, input_rx, event_tx).await;
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
  pub async fn start_screen_share(&self) -> Result<(), WebCallError> {
    let stream = video::capture_screen().await?;
    self.share_video_stream(stream).await
  }

  /// Shares any video stream as the screen share (e.g. a canvas' `captureStream()`), replacing a
  /// running one.
  pub async fn share_video_stream(&self, stream: MediaStream) -> Result<(), WebCallError> {
    self.start_video(stream, video::screen_plan()).await
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

  fn command(&self, command: Command) -> Result<(), WebCallError> {
    self
      .inputs
      .unbounded_send(Input::Command(command))
      .map_err(|_| WebCallError::Ended)
  }

  /// The next event, or `None` once the call ended.
  pub async fn next_event(&mut self) -> Option<CallEngineEvent> {
    self.events.next().await
  }

  /// Leaves the call and returns its statistics.
  pub async fn leave(self) -> Result<CallStats, WebCallError> {
    let _ = self.inputs.unbounded_send(Input::Command(Command::Leave));
    self.finished.await.map_err(|_| WebCallError::Ended)?
  }
}

/// Lists microphones and speakers (`enumerateDevices`), defaults first.
pub async fn list_audio_devices() -> Result<BrowserAudioDevices, WebCallError> {
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
  let mut devices = BrowserAudioDevices::default();
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
      _ => {},
    }
  }
  devices.inputs.sort_by_key(|device| !device.is_default);
  devices.outputs.sort_by_key(|device| !device.is_default);
  Ok(devices)
}

/// The call task: drives the engine from everything that arrives on `inputs`.
async fn run_call(
  config: WebCallConfig,
  transport: Transport,
  mut audio: WebAudio,
  inputs: mpsc::UnboundedSender<Input>,
  mut input_rx: mpsc::UnboundedReceiver<Input>,
  events: mpsc::UnboundedSender<CallEngineEvent>,
) -> Result<CallStats, WebCallError> {
  let mut engine = CallEngine::new(config.engine.clone(), Instant::now());
  if let Some(roster) = &config.maybe_trusted_roster {
    engine.set_trusted_roster(roster.iter().copied());
  }
  if let Some(authenticator) = &config.maybe_authenticator {
    engine.set_authenticator(Arc::clone(authenticator));
  }
  let mut state = CallState {
    transport: &transport,
    inputs: &inputs,
    events: &events,
    jitter: config.jitter,
    encoder: OpusEncoder::new(&inputs)?,
    remotes: BTreeMap::new(),
    source: config.source.clone(),
    tone_position: 0,
    is_muted: false,
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
  };

  audio.close();
  transport.close();
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
  transport: &'a Transport,
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
      Input::Signal(message) => engine.handle_signal(message, Instant::now())?,
      Input::SignalingClosed => return Ok(Flow::Leave),
      Input::Datagram(bytes) => engine.handle_datagram(&bytes, Instant::now()),
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
      Input::FrameTick => self.on_frame_tick(audio)?,
      Input::TimerTick => engine.on_timer(Instant::now()),
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
      Input::Command(command) => return self.on_command(command, engine, audio).await,
    }
    Ok(Flow::Continue)
  }

  fn flush(&mut self, engine: &mut CallEngine) -> Result<(), WebCallError> {
    while let Some(output) = engine.poll_output() {
      match output {
        EngineOutput::Signal(message) => self.transport.send_signal(&message)?,
        EngineOutput::Datagram(bytes) => self.transport.send_datagram(&bytes),
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
            _ => {},
          }
          // The app may have stopped listening; the call goes on.
          let _ = self.events.unbounded_send(event);
        },
      }
    }
    Ok(())
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
      return Ok(());
    }
    for sample in &mut mix {
      *sample = sample.clamp(-1.0, 1.0);
    }
    if mix.iter().any(|sample| sample.abs() > AUDIBLE_AMPLITUDE) {
      self.stats.audible_frames_played += 1;
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
        let view = VideoView::new(canvas, self.inputs);
        self.views.insert(stream_id, view);
        engine.subscribe_video(stream_id, Some(0));
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
  async fn connect(
    config: &WebCallConfig,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let options = Object::new();
    if let Some(hash) = config.maybe_certificate_hash {
      let entry = dictionary(&[
        ("algorithm", JsValue::from_str("sha-256")),
        ("value", Uint8Array::from(&hash[..]).into()),
      ])
      .map_err(browser("WebTransport options"))?;
      Reflect::set(
        &options,
        &JsValue::from_str("serverCertificateHashes"),
        &Array::of1(&entry),
      )
      .map_err(browser("WebTransport options"))?;
    }
    let transport = WebTransport::new(&config.sfu_url, &options).map_err(browser("WebTransport"))?;
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
    let datagrams = datagram_stream
      .writable()
      .get_writer()
      .map_err(browser("datagram stream"))?;
    spawn_local(read_signaling(stream.readable(), inputs.clone()));
    spawn_local(read_datagrams(datagram_stream.readable(), inputs.clone()));
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

  fn close(&self) {
    self.transport.close();
  }
}

impl WebAudio {
  async fn open(
    source: &AudioSource,
    sink: &AudioSink,
    inputs: &mpsc::UnboundedSender<Input>,
  ) -> Result<Self, WebCallError> {
    let options = AudioContextOptions::new();
    options.set_sample_rate(SAMPLE_RATE_HZ as f32);
    let context = AudioContext::new_with_context_options(&options).map_err(browser("AudioContext"))?;
    load_worklet(&context).await?;
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
    let track_constraints = MediaTrackConstraints::new();
    track_constraints.set_echo_cancellation(&JsValue::TRUE);
    track_constraints.set_noise_suppression(&JsValue::TRUE);
    track_constraints.set_auto_gain_control(&JsValue::TRUE);
    track_constraints.set_channel_count(&JsValue::from(1));
    if let AudioDeviceChoice::Device(id) = choice {
      let exact =
        dictionary(&[("exact", JsValue::from_str(id.as_str()))]).map_err(browser("getUserMedia"))?;
      track_constraints.set_device_id(&exact);
    }
    let constraints = MediaStreamConstraints::new();
    constraints.set_audio(&track_constraints);
    let promise = window()?
      .navigator()
      .media_devices()
      .map_err(browser("mediaDevices"))?
      .get_user_media_with_constraints(&constraints)
      .map_err(browser("getUserMedia"))?;
    let stream: MediaStream = JsFuture::from(promise)
      .await
      .map_err(browser("getUserMedia"))?
      .unchecked_into();
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
    let config = dictionary(&[
      ("codec", JsValue::from_str("opus")),
      ("sampleRate", JsValue::from(SAMPLE_RATE_HZ)),
      ("numberOfChannels", JsValue::from(1)),
      ("bitrate", JsValue::from(OPUS_BITRATE_BPS)),
    ])
    .map_err(browser("AudioEncoder"))?;
    encoder.configure(&config).map_err(browser("AudioEncoder"))?;
    Ok(Self {
      encoder,
      pending: Vec::with_capacity(2 * FRAME_SAMPLES_PER_CHANNEL),
      levels: VecDeque::new(),
      timestamp_us: 0.0,
      _output: output,
      _error: error,
    })
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

async fn read_signaling(stream: ReadableStream, inputs: mpsc::UnboundedSender<Input>) {
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
          if inputs.unbounded_send(Input::Signal(message)).is_err() {
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
  let _ = inputs.unbounded_send(Input::SignalingClosed);
}

async fn read_datagrams(stream: ReadableStream, inputs: mpsc::UnboundedSender<Input>) {
  let result = read_chunks(stream, |chunk| {
    inputs.unbounded_send(Input::Datagram(chunk)).is_ok()
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
async fn load_worklet(context: &AudioContext) -> Result<(), WebCallError> {
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
