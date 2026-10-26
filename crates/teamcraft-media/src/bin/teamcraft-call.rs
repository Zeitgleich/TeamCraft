//! Headless call client: joins a call through the SFU, sends a test tone (or the microphone) and
//! decodes what the others send. Used by automated tests and for manual checks.
//!
//! ```text
//! teamcraft-call --sfu https://127.0.0.1:4433/call --token dev:1:1:1 --user 1 --device 1 \
//!   [--cert-hash <64 hex chars>] [--source tone:440 | mic[:<id>] | silence] \
//!   [--sink discard | speakers[:<id>]] [--seconds 10] [--roster 2,3] [--muted] [--watch-video]
//!   [--send-video]
//! teamcraft-call --list-devices
//! ```
//!
//! Ends after `--seconds` or on Ctrl-C and logs the call statistics.

use std::time::Duration;

use anyhow::{Context, bail};
use teamcraft_media::audio_device::{list_input_devices, list_output_devices};
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::video::{VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::{CaptureFormat, TestPatternCapture};
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{AudioCodec, DeviceId, MediaCapabilities, UserId, VideoCodec, VideoTrackKind};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();
  if std::env::args().nth(1).as_deref() == Some("--list-devices") {
    log_devices("input", &list_input_devices()?);
    log_devices("output", &list_output_devices()?);
    return Ok(());
  }
  let options = Options::parse(std::env::args().skip(1))?;
  let mut call = NativeCall::join(options.config)
    .await
    .context("joining the call")?;
  if options.is_muted {
    call.set_muted(true);
  }
  let decoded = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
  let mut is_video_pending = options.is_sending_video;
  let deadline = async {
    match options.maybe_duration {
      Some(duration) => tokio::time::sleep(duration).await,
      None => {
        // Ctrl-C failing to register just means the call runs until the SFU ends it.
        let _ = tokio::signal::ctrl_c().await;
      },
    }
  };
  tokio::pin!(deadline);
  loop {
    tokio::select! {
      () = &mut deadline => break,
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => {
          log_event(&event);
          if is_video_pending && matches!(event, CallEngineEvent::Secured { .. }) {
            is_video_pending = false;
            call.start_video(test_pattern_camera()).await.context("starting video")?;
          }
          if let (true, CallEngineEvent::VideoPublished(track)) = (options.is_watching_video, &event)
            && track.device_id != options.device_id
          {
            let counter = std::sync::Arc::clone(&decoded);
            call.watch_video(track.stream_id, None, Box::new(move |frame| {
              let count = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
              if count % 30 == 1 {
                let resolution = frame.resolution();
                info!(count, width = resolution.width, height = resolution.height, "decoded video");
              }
            }));
          }
        },
        None => break,
      },
    }
  }
  let stats = call.leave().await?;
  info!(
    frames_sent = stats.frames_sent,
    frames_received = stats.frames_received,
    audible_frames_played = stats.audible_frames_played,
    privacy_code = stats.maybe_privacy_code.as_deref().unwrap_or("-"),
    video_frames_decoded = decoded.load(std::sync::atomic::Ordering::Relaxed),
    "call statistics"
  );
  Ok(())
}

/// A 720p test pattern as a camera with three simulcast layers.
fn test_pattern_camera() -> VideoSendConfig {
  let format = CaptureFormat {
    resolution: VideoResolution {
      width: 1280,
      height: 720,
    },
    framerate: 30,
  };
  VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture: Box::new(TestPatternCapture::new(format)),
    layers: simulcast_layers(format.resolution, format.framerate, 1_500_000, 3),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_idle_refresh: None,
  }
}

fn log_devices(direction: &str, devices: &[AudioDeviceInfo]) {
  for device in devices {
    info!(direction, id = %device.id, name = %device.name, is_default = device.is_default, "audio device");
  }
}

struct Options {
  config: NativeCallConfig,
  maybe_duration: Option<Duration>,
  is_muted: bool,
  /// Decode other devices' video and log it.
  is_watching_video: bool,
  /// Publish a test pattern as camera once the call is secured.
  is_sending_video: bool,
  device_id: DeviceId,
}

impl Options {
  fn parse(mut args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
    let (mut maybe_sfu, mut maybe_token, mut maybe_user, mut maybe_device) = (None, None, None, None);
    let mut maybe_certificate_hash = None;
    let mut source = AudioSource::Tone {
      frequency_hz: 440.0,
      amplitude: 0.3,
    };
    let mut sink = AudioSink::Discard;
    let mut maybe_duration = None;
    let mut maybe_trusted_roster = None;
    let mut is_muted = false;
    let mut is_watching_video = false;
    let mut is_sending_video = false;
    while let Some(flag) = args.next() {
      if flag == "--muted" {
        is_muted = true;
        continue;
      }
      if flag == "--watch-video" {
        is_watching_video = true;
        continue;
      }
      if flag == "--send-video" {
        is_sending_video = true;
        continue;
      }
      let value = args.next().with_context(|| format!("{flag} needs a value"))?;
      match flag.as_str() {
        "--sfu" => maybe_sfu = Some(value),
        "--token" => maybe_token = Some(value),
        "--user" => maybe_user = Some(value.parse::<UserId>().context("--user")?),
        "--device" => maybe_device = Some(value.parse::<DeviceId>().context("--device")?),
        "--cert-hash" => maybe_certificate_hash = Some(parse_hash(&value)?),
        "--source" => source = parse_source(&value)?,
        "--sink" => sink = parse_sink(&value)?,
        "--seconds" => maybe_duration = Some(Duration::from_secs(value.parse().context("--seconds")?)),
        "--roster" => {
          let devices = value
            .split(',')
            .map(|id| id.parse::<DeviceId>())
            .collect::<Result<Vec<_>, _>>()
            .context("--roster")?;
          maybe_trusted_roster = Some(devices);
        },
        other => bail!("unknown option {other}"),
      }
    }
    let device_id = maybe_device.context("--device is required")?;
    let engine = CallEngineConfig {
      user_id: maybe_user.context("--user is required")?,
      device_id,
      capabilities: MediaCapabilities {
        audio_codecs: vec![AudioCodec::Opus],
        video_encoders: vec![VideoCodec::H264],
        video_decoders: vec![VideoCodec::H264],
        max_simulcast_layers: 3,
      },
      bwe: BweConfig::default(),
    };
    let mut config = NativeCallConfig::new(
      maybe_sfu.context("--sfu is required")?,
      maybe_token.context("--token is required")?,
      engine,
    );
    config.maybe_certificate_hash = maybe_certificate_hash;
    if matches!(source, AudioSource::Microphone(_)) {
      config.maybe_processing = Some(teamcraft_media::audio_processing::AudioProcessingConfig::default());
    }
    config.source = source;
    config.sink = sink;
    config.maybe_trusted_roster = maybe_trusted_roster;
    Ok(Self {
      config,
      maybe_duration,
      is_muted,
      is_watching_video,
      is_sending_video,
      device_id,
    })
  }
}

fn parse_source(value: &str) -> anyhow::Result<AudioSource> {
  Ok(match value.split_once(':') {
    Some(("tone", frequency)) => AudioSource::Tone {
      frequency_hz: frequency.parse().context("tone frequency")?,
      amplitude: 0.3,
    },
    None if value == "tone" => AudioSource::Tone {
      frequency_hz: 440.0,
      amplitude: 0.3,
    },
    None if value == "mic" => AudioSource::Microphone(AudioDeviceChoice::Default),
    Some(("mic", id)) => AudioSource::Microphone(AudioDeviceChoice::Device(AudioDeviceId::new(id))),
    None if value == "silence" => AudioSource::Silence,
    _ => bail!("--source must be tone[:hz], mic[:<device id>] or silence"),
  })
}

fn parse_sink(value: &str) -> anyhow::Result<AudioSink> {
  match value {
    "discard" => Ok(AudioSink::Discard),
    "speakers" => Ok(AudioSink::Speakers(AudioDeviceChoice::Default)),
    _ => match value.strip_prefix("speakers:") {
      Some(id) => Ok(AudioSink::Speakers(AudioDeviceChoice::Device(
        AudioDeviceId::new(id),
      ))),
      None => bail!("--sink must be discard, speakers or speakers:<device id>"),
    },
  }
}

fn parse_hash(value: &str) -> anyhow::Result<[u8; 32]> {
  let cleaned: String = value.chars().filter(|c| *c != ':').collect();
  if cleaned.len() != 64 {
    bail!("--cert-hash needs 32 bytes as hex");
  }
  let mut hash = [0u8; 32];
  for (index, byte) in hash.iter_mut().enumerate() {
    *byte = u8::from_str_radix(&cleaned[2 * index..2 * index + 2], 16).context("--cert-hash")?;
  }
  Ok(hash)
}

fn log_event(event: &CallEngineEvent) {
  match event {
    // Audio frames are internal to the driver; never logged.
    CallEngineEvent::AudioFrame { .. } => {},
    CallEngineEvent::Secured { epoch, privacy_code } => info!(epoch, privacy_code, "call secured"),
    other => info!(event = ?other, "call event"),
  }
}
