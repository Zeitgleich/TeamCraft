//! Native driver for [`CallEngine`]: WebTransport to the SFU (wtransport), audio devices or test
//! signals, Opus, jitter buffers and mixing, on a tokio runtime.
//!
//! The desktop app and the headless `teamcraft-call` client use it. Device streams (cpal) are not
//! `Send` everywhere, so they live on a dedicated audio thread that exchanges samples with the call
//! task over channels.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle as ThreadHandle;
use std::time::{Duration, Instant};

use teamcraft_proto::{
  CallClientMessage, CallServerMessage, DeviceId, MediaStreamId, VideoCodec, VideoLayerInfo, VideoTrackKind,
};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};
use wtransport::config::ClientConfigBuilder;
use wtransport::config::states::{WantsRootStore, WantsTransportConfigClient};
use wtransport::tls::Sha256Digest;
use wtransport::{ClientConfig, Connection, Endpoint, RecvStream, SendStream};

use crate::audio_codec::{FRAME_SAMPLES_PER_CHANNEL, OpusEncoderConfig};
use crate::audio_device::{AudioDeviceError, AudioInput, AudioOutput, PlaybackBuffer};
use crate::audio_io::{AudioSink, AudioSource, sine_tone};
use crate::audio_pipeline::{AudioPipelineError, CapturePipeline, PlaybackMixer};
use crate::call_auth::{CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use crate::call_engine::{
  CallEngine, CallEngineConfig, CallEngineError, CallEngineEvent, CallStats, EngineOutput,
};
use crate::jitter::JitterBufferConfig;
use crate::video::EncodedVideoFrame;
use crate::video_capture::CaptureError;
use crate::video_pipeline::{FrameSink, VideoReceiver, VideoSendConfig, VideoSender};

/// Audio frame period.
const FRAME_INTERVAL: Duration = Duration::from_millis(20);
/// Feedback and allocation period.
const TIMER_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound of one signaling message.
const MAX_SIGNALING_BYTES: usize = 1024 * 1024;
/// At most this much audio waits for the speakers (latency bound).
const MAX_PLAYBACK_BUFFER_SAMPLES: usize = 48_000 / 5;

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
  /// Accept the SFU's self-signed certificate by its SHA-256 hash (development); `None` uses the
  /// system's trusted roots.
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
  #[error("the call task ended unexpectedly")]
  TaskEnded,
}

enum Command {
  Mute(bool),
  Deafen(bool),
  SwitchAudio {
    source: AudioSource,
    sink: AudioSink,
    done: oneshot::Sender<Result<(), AudioDeviceError>>,
  },
  Video(VideoCommand),
  Leave,
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
  /// Capture, encode and publish a track (camera or screen).
  Start(VideoSendConfig, oneshot::Sender<Result<(), CaptureError>>),
  Stop(VideoTrackKind),
  Watch {
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
    sink: FrameSink,
  },
  Unwatch(MediaStreamId),
}

/// The driver's video: own tracks being captured and received tracks being decoded.
#[derive(Default)]
struct VideoState {
  /// Own tracks; the stream id arrives with `VideoPublished`.
  senders: Vec<(VideoSender, Option<MediaStreamId>)>,
  receivers: HashMap<MediaStreamId, VideoReceiver>,
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
    let connection = connect(&config).await?;
    let (send, recv) = connection
      .open_bi()
      .await
      .map_err(|error| NativeCallError::Connect(error.to_string()))?
      .await
      .map_err(|error| NativeCallError::Connect(error.to_string()))?;
    let (commands, command_rx) = mpsc::unbounded_channel();
    let (event_tx, events) = mpsc::unbounded_channel();
    let task = tokio::spawn(run_call(config, connection, send, recv, command_rx, event_tx));
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
  /// track of the same kind. Frames flow once the SFU confirmed the track.
  pub async fn start_video(&self, config: VideoSendConfig) -> Result<(), NativeCallError> {
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

  /// Sends one encoded frame of an own track.
  pub fn send_video(&self, stream_id: MediaStreamId, frame: EncodedVideoFrame) {
    self.video(VideoCommand::Send { stream_id, frame });
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

async fn connect(config: &NativeCallConfig) -> Result<Connection, NativeCallError> {
  let builder = ClientConfig::builder().with_bind_default();
  let client_config = match config.maybe_certificate_hash {
    Some(hash) => builder
      .with_server_certificate_hashes([Sha256Digest::new(hash)])
      .build(),
    None => with_trusted_roots(builder).build(),
  };
  let endpoint =
    Endpoint::client(client_config).map_err(|error| NativeCallError::Connect(error.to_string()))?;
  endpoint
    .connect(&config.sfu_url)
    .await
    .map_err(|error| NativeCallError::Connect(error.to_string()))
}

/// The call task: drives the engine from the network, the audio thread and timers.
async fn run_call(
  config: NativeCallConfig,
  connection: Connection,
  send: SendStream,
  recv: RecvStream,
  mut commands: mpsc::UnboundedReceiver<Command>,
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
  let mut source = config.source.clone();
  let mut devices = AudioDevices::open(&source, &config.sink)?;
  let (signal_out, signal_out_rx) = mpsc::unbounded_channel::<CallClientMessage>();
  let writer = tokio::spawn(write_signaling(send, signal_out_rx));
  let (signal_in, mut signal_in_rx) = mpsc::unbounded_channel::<CallServerMessage>();
  let reader = tokio::spawn(read_signaling(recv, signal_in));

  let mut stats = CallStats::default();
  let mut tone_position = 0u64;
  let mut frame_tick = tokio::time::interval(FRAME_INTERVAL);
  let mut timer_tick = tokio::time::interval(TIMER_INTERVAL);
  engine.join(config.token.clone());
  let mut video = VideoState::default();
  let (encoded_tx, mut encoded_rx) = mpsc::unbounded_channel::<(VideoTrackKind, EncodedVideoFrame)>();
  let mut io = DriverIo {
    connection: &connection,
    signal_out: &signal_out,
    events: &events,
    mixer: &mut mixer,
    stats: &mut stats,
    video: &mut video,
  };
  io.flush(&mut engine)?;

  let result = loop {
    tokio::select! {
      maybe_message = signal_in_rx.recv() => {
        let Some(message) = maybe_message else {
          break Ok(());
        };
        if let Err(error) = engine.handle_signal(message, Instant::now()) {
          break Err(error.into());
        }
      },
      datagram = connection.receive_datagram() => {
        match datagram {
          Ok(datagram) => engine.handle_datagram(&datagram.payload(), Instant::now()),
          Err(error) => break Err(NativeCallError::Signaling(error.to_string())),
        }
      },
      maybe_samples = recv_capture(&mut devices.maybe_capture) => {
        if let Some(samples) = maybe_samples {
          for frame in capture.push(&samples)? {
            engine.send_audio(&frame, Instant::now());
            io.stats.frames_sent += 1;
          }
        }
      },
      _ = frame_tick.tick() => {
        if let AudioSource::Tone { frequency_hz, amplitude } = source {
          let samples = sine_tone(frequency_hz, amplitude, tone_position, FRAME_SAMPLES_PER_CHANNEL);
          tone_position += FRAME_SAMPLES_PER_CHANNEL as u64;
          for frame in capture.push(&samples)? {
            engine.send_audio(&frame, Instant::now());
            io.stats.frames_sent += 1;
          }
        }
        let mix = io.mixer.next_frame()?;
        if mix.iter().any(|sample| sample.abs() > 0.01) {
          io.stats.audible_frames_played += 1;
        }
        if let Some(buffer) = &devices.maybe_playback {
          buffer.push(&mix);
        }
      },
      _ = timer_tick.tick() => engine.on_timer(Instant::now()),
      Some((kind, frame)) = encoded_rx.recv() => {
        if let Some(stream_id) = io.video.stream_of(&kind) {
          engine.send_video(stream_id, &frame, Instant::now());
        }
      },
      maybe_command = commands.recv() => {
        match maybe_command {
          Some(Command::Mute(is_muted)) => {
            engine.set_muted(is_muted);
            capture.set_muted(is_muted);
          },
          Some(Command::Deafen(is_deafened)) => {
            engine.set_deafened(is_deafened);
            io.mixer.set_deafened(is_deafened);
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
          Some(Command::Video(command)) => handle_video(&mut engine, io.video, &encoded_tx, command),
          Some(Command::Leave) | None => break Ok(()),
        }
      },
    }
    if let Err(error) = io.flush(&mut engine) {
      break Err(error);
    }
  };

  devices.close();
  reader.abort();
  writer.abort();
  connection.close(0u32.into(), b"leave");
  stats.maybe_privacy_code = engine.privacy_code();
  info!(?stats, "call ended");
  result.map(|()| stats)
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
      let result = VideoSender::start(config, encoded.clone()).map(|sender| {
        engine.publish_video(sender.kind.clone(), VideoCodec::H264, sender.layers.clone());
        video.senders.push((sender, None));
      });
      // The caller may have given up waiting.
      let _ = done.send(result);
    },
    VideoCommand::Stop(kind) => video.stop(engine, &kind),
    VideoCommand::Watch {
      stream_id,
      maybe_max_layer,
      sink,
    } => match VideoReceiver::start(sink) {
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
    VideoCommand::Publish { kind, codec, layers } => engine.publish_video(kind, codec, layers),
    VideoCommand::Unpublish(stream_id) => engine.unpublish_video(stream_id),
    VideoCommand::Subscribe {
      stream_id,
      maybe_max_layer,
    } => engine.subscribe_video(stream_id, maybe_max_layer),
    VideoCommand::Send { stream_id, frame } => engine.send_video(stream_id, &frame, Instant::now()),
  }
}

/// Where engine outputs go.
struct DriverIo<'a> {
  connection: &'a Connection,
  signal_out: &'a mpsc::UnboundedSender<CallClientMessage>,
  events: &'a mpsc::UnboundedSender<CallEngineEvent>,
  mixer: &'a mut PlaybackMixer,
  stats: &'a mut CallStats,
  video: &'a mut VideoState,
}

impl DriverIo<'_> {
  fn flush(&mut self, engine: &mut CallEngine) -> Result<(), NativeCallError> {
    while let Some(output) = engine.poll_output() {
      match output {
        EngineOutput::Signal(message) => {
          self
            .signal_out
            .send(message)
            .map_err(|_| NativeCallError::Signaling("writer stopped".into()))?;
        },
        EngineOutput::Datagram(bytes) => {
          // Unreliable by design: a full send buffer drops the frame.
          if let Err(error) = self.connection.send_datagram(&bytes) {
            debug!(%error, "datagram dropped");
          }
        },
        EngineOutput::Event(CallEngineEvent::AudioFrame {
          sender,
          sequence_number,
          opus,
        }) => {
          self.stats.frames_received += 1;
          self.mixer.push(sender, sequence_number, opus)?;
        },
        EngineOutput::Event(CallEngineEvent::VideoFrame(frame)) => {
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
            CallEngineEvent::VideoPublished(track) if track.device_id == engine.device_id() => {
              self.video.on_published(&track.kind, track.stream_id);
            },
            CallEngineEvent::VideoUnpublished(stream_id) => {
              self.video.receivers.remove(stream_id);
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
            _ => {},
          }
          // The app may have stopped listening; the call goes on.
          let _ = self.events.send(event);
        },
      }
    }
    Ok(())
  }
}

impl VideoState {
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

  fn on_published(&mut self, kind: &VideoTrackKind, stream_id: MediaStreamId) {
    if let Some((_, maybe_stream)) = self
      .senders
      .iter_mut()
      .find(|(sender, maybe_stream)| sender.kind == *kind && maybe_stream.is_none())
    {
      *maybe_stream = Some(stream_id);
    }
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
      drop(sender);
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

async fn read_signaling(mut recv: RecvStream, messages: mpsc::UnboundedSender<CallServerMessage>) {
  loop {
    let mut length = [0u8; 4];
    if recv.read_exact(&mut length).await.is_err() {
      return;
    }
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_SIGNALING_BYTES {
      warn!(length, "signaling message too large");
      return;
    }
    let mut body = vec![0u8; length];
    if recv.read_exact(&mut body).await.is_err() {
      return;
    }
    match serde_json::from_slice::<CallServerMessage>(&body) {
      Ok(message) => {
        if messages.send(message).is_err() {
          return;
        }
      },
      Err(error) => warn!(%error, "invalid signaling message"),
    }
  }
}
