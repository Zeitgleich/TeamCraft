//! Microphone capture and speaker playback with cpal, as mono 48 kHz f32.
//!
//! The backend is chosen once per process ([`audio_backend`]): on Linux PipeWire, else
//! PulseAudio (its native protocol), else raw ALSA (#1072); CoreAudio on macOS, WASAPI on Windows.
//! With PipeWire the device lists come from its registry (`audio_devices_pipewire`), named as in
//! the desktop's sound settings and identified by node name.
//!
//! Devices are opened at their own rate and format, nearest to 48 kHz, and converted here: any
//! rate is resampled, multi-channel devices are downmixed on capture and get the mono signal on
//! every channel on playback. The capture callback hands samples to the caller; playback pulls from a shared
//! [`PlaybackBuffer`] and plays silence when it runs dry.
//!
//! Devices are chosen by [`AudioDeviceId`] (from [`list_input_devices`] / [`list_output_devices`]),
//! which stays stable across restarts where the platform allows, so it can be stored as a setting.
//!
//! cpal streams are not `Send` on every platform: keep [`AudioInput`] and [`AudioOutput`] on the
//! thread that opened them and drop them to stop.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{
  Device, FromSample, Host, Sample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfigRange,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo};
use crate::system_audio::MonoResampler;

/// How PulseAudio names a sink's monitor source.
const PULSE_MONITOR_SUFFIX: &str = ".monitor";

/// Which sound system call audio goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
  PipeWire,
  PulseAudio,
  Alsa,
  CoreAudio,
  Wasapi,
  /// Another cpal host (JACK, Android's AAudio).
  Other,
}

/// A running capture stream.
pub struct AudioInput {
  _stream: OpenStream,
}

/// A running playback stream.
pub struct AudioOutput {
  _stream: OpenStream,
}

/// A cpal stream and whether we are stopping it: cpal's PipeWire host reports its own stop as
/// "Device disconnected", which is no failure worth a warning.
struct OpenStream {
  _stream: cpal::Stream,
  is_closing: Arc<AtomicBool>,
}

/// Samples waiting for the speakers, filled by the call engine and drained by the output callback.
#[derive(Debug)]
pub struct PlaybackBuffer {
  state: Mutex<PlaybackState>,
  max_samples: usize,
}

#[derive(Debug, Default)]
struct PlaybackState {
  samples: VecDeque<f32>,
  underruns: u64,
}

#[derive(Debug, Error)]
pub enum AudioDeviceError {
  #[error("no {0} device available")]
  NoDevice(&'static str),
  #[error("the {0} device is not available (unplugged?)")]
  DeviceGone(&'static str),
  #[error("the {0} device supports no usable sample format")]
  UnsupportedFormat(&'static str),
  /// The device is there but didn't start (busy, refused by the sound server, a format it
  /// rejected); `device` is its name as listed.
  #[error("the {direction} device \"{device}\" could not be opened: {reason}")]
  Open {
    direction: &'static str,
    device: String,
    reason: String,
  },
  #[error("the audio thread stopped while opening devices")]
  AudioThread,
  #[error("audio device: {0}")]
  Device(#[from] cpal::Error),
}

/// The sound system of this process, chosen on first use and logged once.
pub fn audio_backend() -> AudioBackend {
  backend_host_id().1
}

/// Ticks whenever a microphone or speaker comes, goes or becomes the default, where the backend
/// tells (PipeWire); `None` elsewhere: callers list the devices again from time to time.
pub fn device_changes() -> Option<tokio::sync::watch::Receiver<u64>> {
  #[cfg(all(feature = "pipewire", target_os = "linux"))]
  if audio_backend() == AudioBackend::PipeWire {
    return crate::audio_devices_pipewire::PipeWireDevices::shared().map(|devices| devices.subscribe());
  }
  None
}

/// Microphones, the default first.
pub fn list_input_devices() -> Result<Vec<AudioDeviceInfo>, AudioDeviceError> {
  #[cfg(all(feature = "pipewire", target_os = "linux"))]
  if let Some(devices) = pipewire_devices() {
    return Ok(devices.inputs());
  }
  let host = backend_host();
  let maybe_default = host.default_input_device();
  // PulseAudio lists every sink's monitor as a source: what the speakers play, no microphone.
  let is_pulse = audio_backend() == AudioBackend::PulseAudio;
  let devices = host.input_devices()?.filter(|device| {
    !(is_pulse
      && device
        .id()
        .is_ok_and(|id| id.id().ends_with(PULSE_MONITOR_SUFFIX)))
  });
  list_devices(devices, maybe_default, |device| {
    device
      .supported_input_configs()
      .is_ok_and(|ranges| pick_config(ranges, "input").is_ok())
  })
}

/// Speakers and headsets, the default first.
pub fn list_output_devices() -> Result<Vec<AudioDeviceInfo>, AudioDeviceError> {
  #[cfg(all(feature = "pipewire", target_os = "linux"))]
  if let Some(devices) = pipewire_devices() {
    return Ok(devices.outputs());
  }
  let host = backend_host();
  let maybe_default = host.default_output_device();
  list_devices(host.output_devices()?, maybe_default, |device| {
    device
      .supported_output_configs()
      .is_ok_and(|ranges| pick_config(ranges, "output").is_ok())
  })
}

/// Opens the chosen device with `open`; a chosen device that is gone (unplugged since it was
/// picked, or stored on another computer) gives way to the system default, so a call doesn't
/// fail to start over a stale choice (#986).
pub fn open_or_default<T>(
  choice: &AudioDeviceChoice,
  mut open: impl FnMut(&AudioDeviceChoice) -> Result<T, AudioDeviceError>,
) -> Result<T, AudioDeviceError> {
  match open(choice) {
    Err(AudioDeviceError::DeviceGone(direction)) => {
      warn!(
        direction,
        "the chosen audio device is gone; using the system default"
      );
      open(&AudioDeviceChoice::Default)
    },
    other => other,
  }
}

impl AudioInput {
  /// Opens a microphone at its own rate; `on_samples` gets mono 48 kHz samples on the audio thread.
  pub fn open(
    choice: &AudioDeviceChoice,
    on_samples: impl FnMut(&[f32]) + Send + 'static,
  ) -> Result<Self, AudioDeviceError> {
    let host = backend_host();
    let device = match choice {
      AudioDeviceChoice::Default => host.default_input_device(),
      AudioDeviceChoice::Device(id) => find_device(&host, id),
    }
    .ok_or(no_device(choice, "input"))?;
    let failed = |error: cpal::Error| open_failure(&device, "input", &error);
    let is_closing = Arc::new(AtomicBool::new(false));
    let (config, format) = pick_config(device.supported_input_configs().map_err(failed)?, "input")?;
    info!(
      device = %device_name(&device),
      channels = config.channels,
      rate_hz = config.sample_rate,
      ?format,
      "opening the microphone"
    );
    let stream = match format {
      SampleFormat::I16 => build_input::<i16>(&device, config, on_samples, Arc::clone(&is_closing)),
      SampleFormat::I32 => build_input::<i32>(&device, config, on_samples, Arc::clone(&is_closing)),
      SampleFormat::U16 => build_input::<u16>(&device, config, on_samples, Arc::clone(&is_closing)),
      SampleFormat::F64 => build_input::<f64>(&device, config, on_samples, Arc::clone(&is_closing)),
      _ => build_input::<f32>(&device, config, on_samples, Arc::clone(&is_closing)),
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;
    Ok(Self {
      _stream: OpenStream {
        _stream: stream,
        is_closing,
      },
    })
  }
}

impl AudioOutput {
  /// Opens speakers, playing what is pushed into `buffer`.
  pub fn open(choice: &AudioDeviceChoice, buffer: Arc<PlaybackBuffer>) -> Result<Self, AudioDeviceError> {
    let host = backend_host();
    let device = match choice {
      AudioDeviceChoice::Default => host.default_output_device(),
      AudioDeviceChoice::Device(id) => find_device(&host, id),
    }
    .ok_or(no_device(choice, "output"))?;
    let failed = |error: cpal::Error| open_failure(&device, "output", &error);
    let is_closing = Arc::new(AtomicBool::new(false));
    let (config, format) = pick_config(device.supported_output_configs().map_err(failed)?, "output")?;
    let stream = match format {
      SampleFormat::I16 => build_output::<i16>(&device, config, buffer, Arc::clone(&is_closing)),
      SampleFormat::I32 => build_output::<i32>(&device, config, buffer, Arc::clone(&is_closing)),
      SampleFormat::U16 => build_output::<u16>(&device, config, buffer, Arc::clone(&is_closing)),
      SampleFormat::F64 => build_output::<f64>(&device, config, buffer, Arc::clone(&is_closing)),
      _ => build_output::<f32>(&device, config, buffer, Arc::clone(&is_closing)),
    }
    .map_err(failed)?;
    stream.play().map_err(failed)?;
    Ok(Self {
      _stream: OpenStream {
        _stream: stream,
        is_closing,
      },
    })
  }
}

impl PlaybackBuffer {
  /// `max_samples` bounds the latency: older samples are dropped when the engine runs ahead.
  pub fn new(max_samples: usize) -> Self {
    Self {
      state: Mutex::new(PlaybackState::default()),
      max_samples,
    }
  }

  pub fn push(&self, samples: &[f32]) {
    let mut state = self.lock();
    state.samples.extend(samples);
    let excess = state.samples.len().saturating_sub(self.max_samples);
    state.samples.drain(..excess);
  }

  /// Fills `out`; missing samples are silence and count as an underrun.
  pub fn pull(&self, out: &mut [f32]) {
    let mut state = self.lock();
    let available = state.samples.len().min(out.len());
    for (slot, sample) in out.iter_mut().zip(state.samples.drain(..available)) {
      *slot = sample;
    }
    if available < out.len() {
      out[available..].fill(0.0);
      state.underruns += 1;
    }
  }

  pub fn len(&self) -> usize {
    self.lock().samples.len()
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  pub fn underruns(&self) -> u64 {
    self.lock().underruns
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, PlaybackState> {
    // Poisoning only means a callback panicked mid-update of a plain sample queue.
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// The devices a call can open, each name once. On Linux the ALSA host lists every PCM of a card
/// (`hw:`, `plughw:`, `front:`, `surround51:`, ...): most can't be opened in a usable format and the
/// rest repeat the card's name, so unusable ones are left out and a name is listed once (the
/// default, or the first that works).
fn list_devices(
  devices: impl Iterator<Item = Device>,
  maybe_default: Option<Device>,
  is_usable: impl Fn(&Device) -> bool,
) -> Result<Vec<AudioDeviceInfo>, AudioDeviceError> {
  let maybe_default_id = maybe_default.and_then(|device| device.id().ok());
  let mut infos: Vec<AudioDeviceInfo> = devices
    .filter_map(|device| {
      // Devices can vanish between listing and asking; skip those.
      let id = device.id().ok()?;
      let is_default = maybe_default_id.as_ref() == Some(&id);
      if !is_default && !is_usable(&device) {
        return None;
      }
      let name = device_name(&device);
      Some(AudioDeviceInfo {
        is_default,
        id: AudioDeviceId::new(id.to_string()),
        name,
      })
    })
    .collect();
  infos.sort_by_key(|info| !info.is_default);
  Ok(dedup_by_name(infos))
}

/// The host of [`audio_backend`]; a host that can't be created any more (the daemon went away
/// since) gives way to whatever cpal finds now.
fn backend_host() -> Host {
  cpal::host_from_id(backend_host_id().0).unwrap_or_else(|error| {
    warn!(%error, "the audio backend is gone; using another one");
    cpal::default_host()
  })
}

/// cpal's choice, in its order on Linux: PipeWire, PulseAudio, ALSA.
fn backend_host_id() -> (cpal::HostId, AudioBackend) {
  static BACKEND: OnceLock<(cpal::HostId, AudioBackend)> = OnceLock::new();
  *BACKEND.get_or_init(|| {
    let id = cpal::default_host().id();
    let backend = backend_of(id);
    info!(?backend, "audio backend");
    (id, backend)
  })
}

/// By cpal's id string (`pipewire`, `alsa`, ...): its variants depend on cpal's features, which
/// other crates in the build may turn on as well.
fn backend_of(id: cpal::HostId) -> AudioBackend {
  match id.to_string().as_str() {
    "pipewire" => AudioBackend::PipeWire,
    "pulseaudio" => AudioBackend::PulseAudio,
    "alsa" => AudioBackend::Alsa,
    "coreaudio" => AudioBackend::CoreAudio,
    "wasapi" => AudioBackend::Wasapi,
    _ => AudioBackend::Other,
  }
}

/// PipeWire's registry watcher when PipeWire is the backend.
#[cfg(all(feature = "pipewire", target_os = "linux"))]
fn pipewire_devices() -> Option<&'static crate::audio_devices_pipewire::PipeWireDevices> {
  (audio_backend() == AudioBackend::PipeWire)
    .then(crate::audio_devices_pipewire::PipeWireDevices::shared)
    .flatten()
}

/// A device that is there but didn't start, logged with why.
fn open_failure(device: &Device, direction: &'static str, error: &cpal::Error) -> AudioDeviceError {
  let device = device_name(device);
  warn!(direction, %device, %error, "the audio device could not be opened");
  AudioDeviceError::Open {
    direction,
    device,
    reason: error.to_string(),
  }
}

fn device_name(device: &Device) -> String {
  device
    .description()
    .map(|description| description.name().to_owned())
    .unwrap_or_else(|_| device.to_string())
}

/// `None` for ids of devices that are gone or come from another platform.
fn find_device(host: &Host, id: &AudioDeviceId) -> Option<Device> {
  let cpal_id = id.as_str().parse::<cpal::DeviceId>().ok()?;
  host.device_by_id(&cpal_id)
}

fn no_device(choice: &AudioDeviceChoice, direction: &'static str) -> AudioDeviceError {
  match choice {
    AudioDeviceChoice::Default => AudioDeviceError::NoDevice(direction),
    AudioDeviceChoice::Device(_) => AudioDeviceError::DeviceGone(direction),
  }
}

/// Sample formats a device may offer, in order of preference; everything is converted to and from
/// f32 internally. Many Linux devices (PipeWire, ALSA) offer only 16 or 32 bit integers.
const SAMPLE_FORMATS: [SampleFormat; 5] = [
  SampleFormat::F32,
  SampleFormat::I32,
  SampleFormat::I16,
  SampleFormat::U16,
  SampleFormat::F64,
];

/// The device's configuration closest to what calls run at: 48 kHz if it can, else the rate nearest
/// to it (44.1 kHz, 96 kHz, ...; resampled to and from 48 kHz), then the preferred sample format,
/// then fewer channels.
fn pick_config(
  ranges: impl Iterator<Item = SupportedStreamConfigRange>,
  direction: &'static str,
) -> Result<(StreamConfig, SampleFormat), AudioDeviceError> {
  let mut candidates: Vec<(u32, SupportedStreamConfigRange)> = ranges
    .filter(|range| SAMPLE_FORMATS.contains(&range.sample_format()))
    .filter(|range| range.max_sample_rate() > 0)
    .map(|range| {
      (
        SAMPLE_RATE_HZ.clamp(range.min_sample_rate(), range.max_sample_rate()),
        range,
      )
    })
    .collect();
  candidates.sort_by_key(|(rate_hz, range)| {
    (
      rate_hz.abs_diff(SAMPLE_RATE_HZ),
      format_rank(range.sample_format()),
      range.channels(),
    )
  });
  let (rate_hz, range) = candidates
    .into_iter()
    .next()
    .ok_or(AudioDeviceError::UnsupportedFormat(direction))?;
  let config = StreamConfig {
    channels: range.channels(),
    sample_rate: rate_hz,
    buffer_size: cpal::BufferSize::Default,
  };
  Ok((config, range.sample_format()))
}

fn format_rank(format: SampleFormat) -> usize {
  SAMPLE_FORMATS
    .iter()
    .position(|candidate| *candidate == format)
    .unwrap_or(usize::MAX)
}

/// A capture stream in the device's sample format, handing mono f32 samples to `on_samples`.
fn build_input<T>(
  device: &Device,
  config: StreamConfig,
  mut on_samples: impl FnMut(&[f32]) + Send + 'static,
  is_closing: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
  T: SizedSample,
  f32: FromSample<T>,
{
  let mut resampler = MonoResampler::new(usize::from(config.channels), config.sample_rate);
  let mut interleaved = Vec::new();
  let stream = device.build_input_stream(
    config,
    move |data: &[T], _: &cpal::InputCallbackInfo| {
      interleaved.clear();
      interleaved.extend(data.iter().map(|sample| sample.to_sample::<f32>()));
      on_samples(&resampler.process(&interleaved));
    },
    move |error| stream_error("capture", &is_closing, &error),
    None,
  )?;
  Ok(stream)
}

/// A playback stream in the device's sample format, playing the mono f32 samples of `buffer`.
fn build_output<T>(
  device: &Device,
  config: StreamConfig,
  buffer: Arc<PlaybackBuffer>,
  is_closing: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
  T: SizedSample + FromSample<f32>,
{
  let channels = usize::from(config.channels);
  let mut playback = DeviceRatePlayback::new(buffer, config.sample_rate);
  let mut mono = Vec::new();
  let stream = device.build_output_stream(
    config,
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
      mono.resize(data.len() / channels, 0.0);
      playback.fill(&mut mono);
      for (frame, sample) in data.chunks_mut(channels).zip(&mono) {
        frame.fill(sample.to_sample::<T>());
      }
    },
    move |error| stream_error("playback", &is_closing, &error),
    None,
  )?;
  Ok(stream)
}

impl Drop for OpenStream {
  fn drop(&mut self) {
    // Before the stream itself is dropped (fields drop after this).
    self.is_closing.store(true, Ordering::Relaxed);
  }
}

/// A stream error cpal reported; a device that went away mid-call reaches the user through the
/// device lists (the call falls back to the default), so this only logs.
fn stream_error(kind: &'static str, is_closing: &AtomicBool, error: &cpal::Error) {
  if is_closing.load(Ordering::Relaxed) {
    debug!(kind, %error, "audio stream error while stopping");
  } else {
    warn!(kind, %error, "audio stream error");
  }
}

/// The playback buffer's 48 kHz samples at the device's rate.
struct DeviceRatePlayback {
  buffer: Arc<PlaybackBuffer>,
  /// `None` at 48 kHz: samples go through untouched.
  maybe_resampler: Option<MonoResampler>,
  /// Converted samples not played yet (less than one chunk).
  pending: VecDeque<f32>,
  chunk: Vec<f32>,
}

impl DeviceRatePlayback {
  /// 10 ms at 48 kHz: what is pulled from the buffer per conversion.
  const CHUNK_SAMPLES: usize = SAMPLE_RATE_HZ as usize / 100;

  fn new(buffer: Arc<PlaybackBuffer>, device_rate_hz: u32) -> Self {
    let maybe_resampler =
      (device_rate_hz != SAMPLE_RATE_HZ).then(|| MonoResampler::between(1, SAMPLE_RATE_HZ, device_rate_hz));
    Self {
      buffer,
      maybe_resampler,
      pending: VecDeque::new(),
      chunk: vec![0.0; Self::CHUNK_SAMPLES],
    }
  }

  fn fill(&mut self, out: &mut [f32]) {
    let Some(resampler) = self.maybe_resampler.as_mut() else {
      self.buffer.pull(out);
      return;
    };
    while self.pending.len() < out.len() {
      self.buffer.pull(&mut self.chunk);
      self.pending.extend(resampler.process(&self.chunk));
    }
    let needed = out.len();
    for (slot, sample) in out.iter_mut().zip(self.pending.drain(..needed)) {
      *slot = sample;
    }
  }
}

/// Keeps the first device of each name (the list is sorted default first).
fn dedup_by_name(infos: Vec<AudioDeviceInfo>) -> Vec<AudioDeviceInfo> {
  let mut seen = std::collections::HashSet::new();
  infos
    .into_iter()
    .filter(|info| seen.insert(info.name.clone()))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn playback_buffer_pads_underruns_and_bounds_latency() {
    let buffer = PlaybackBuffer::new(4);
    buffer.push(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
    assert_eq!(buffer.len(), 4, "oldest samples dropped");
    let mut out = [9.0; 6];
    buffer.pull(&mut out);
    assert_eq!(out, [0.3, 0.4, 0.5, 0.6, 0.0, 0.0]);
    assert_eq!(buffer.underruns(), 1);
    assert!(buffer.is_empty());
  }

  mod formats {
    use super::*;

    #[test]
    fn integer_only_devices_are_usable() {
      let (config, format) = pick_config([range(2, SampleFormat::I16)].into_iter(), "input").unwrap();
      assert_eq!((config.channels, format), (2, SampleFormat::I16));
    }

    #[test]
    fn f32_is_preferred_then_fewer_channels() {
      let ranges = [
        range(1, SampleFormat::I16),
        range(2, SampleFormat::F32),
        range(1, SampleFormat::F32),
      ];
      let (config, format) = pick_config(ranges.into_iter(), "input").unwrap();
      assert_eq!((config.channels, format), (1, SampleFormat::F32));
    }

    #[test]
    fn devices_in_an_unknown_format_are_unusable() {
      assert!(pick_config([range(2, SampleFormat::U8)].into_iter(), "output").is_err());
    }

    #[test]
    fn a_device_without_48_khz_opens_at_its_nearest_rate() {
      let (config, _) = pick_config([fixed(44_100), fixed(96_000)].into_iter(), "input").unwrap();
      assert_eq!(config.sample_rate, 44_100);
      let (config, _) = pick_config([fixed(96_000), fixed(192_000)].into_iter(), "input").unwrap();
      assert_eq!(config.sample_rate, 96_000);
    }

    #[test]
    fn exact_48_khz_wins_over_a_better_format_at_another_rate() {
      let i16_48k = SupportedStreamConfigRange::new(
        2,
        48_000,
        48_000,
        cpal::SupportedBufferSize::Unknown,
        SampleFormat::I16,
      );
      let (config, format) = pick_config([fixed(96_000), i16_48k].into_iter(), "input").unwrap();
      assert_eq!((config.sample_rate, format), (48_000, SampleFormat::I16));
    }

    #[test]
    fn playback_on_a_44_1_khz_device_plays_at_the_right_pace() {
      let buffer = Arc::new(PlaybackBuffer::new(48_000));
      buffer.push(&vec![0.25; 4_800]);
      let mut playback = DeviceRatePlayback::new(buffer.clone(), 44_100);
      let mut out = vec![0.0; 4_410];
      playback.fill(&mut out);
      // 100 ms at the device's rate take 100 ms (4800 samples) of the 48 kHz buffer, give or
      // take one 10 ms chunk.
      let pulled = 4_800 - buffer.len();
      assert!(
        pulled.abs_diff(4_800) <= DeviceRatePlayback::CHUNK_SAMPLES,
        "pulled {pulled}"
      );
      assert!(out.iter().skip(1).all(|sample| (sample - 0.25).abs() < 1e-6));
    }

    #[test]
    fn a_gone_device_gives_way_to_the_default() {
      let chosen = AudioDeviceChoice::Device(AudioDeviceId::new("usb-headset"));
      let mut tried = Vec::new();
      let opened = open_or_default(&chosen, |choice| {
        tried.push(choice.clone());
        match choice {
          AudioDeviceChoice::Device(_) => Err(AudioDeviceError::DeviceGone("input")),
          AudioDeviceChoice::Default => Ok("default"),
        }
      });
      assert_eq!(opened.unwrap(), "default");
      assert_eq!(tried, vec![chosen, AudioDeviceChoice::Default]);
    }

    #[test]
    fn other_failures_and_the_default_are_not_retried() {
      let chosen = AudioDeviceChoice::Device(AudioDeviceId::new("busy"));
      let mut attempts = 0;
      let result: Result<(), _> = open_or_default(&chosen, |_| {
        attempts += 1;
        Err(AudioDeviceError::UnsupportedFormat("input"))
      });
      assert!(matches!(result, Err(AudioDeviceError::UnsupportedFormat(_))));
      assert_eq!(attempts, 1);
      let mut attempts = 0;
      let result: Result<(), _> = open_or_default(&AudioDeviceChoice::Default, |_| {
        attempts += 1;
        Err(AudioDeviceError::NoDevice("output"))
      });
      assert!(matches!(result, Err(AudioDeviceError::NoDevice(_))));
      assert_eq!(attempts, 1);
    }

    #[test]
    fn a_name_is_listed_once_default_first() {
      let infos = vec![
        info("a", "Headset", true),
        info("b", "Headset", false),
        info("c", "Speakers", false),
      ];
      let names: Vec<_> = dedup_by_name(infos)
        .into_iter()
        .map(|info| info.id.to_string())
        .collect();
      assert_eq!(names, ["a", "c"]);
    }

    fn fixed(rate_hz: u32) -> SupportedStreamConfigRange {
      SupportedStreamConfigRange::new(
        2,
        rate_hz,
        rate_hz,
        cpal::SupportedBufferSize::Unknown,
        SampleFormat::F32,
      )
    }

    fn range(channels: u16, format: SampleFormat) -> SupportedStreamConfigRange {
      SupportedStreamConfigRange::new(
        channels,
        8_000,
        192_000,
        cpal::SupportedBufferSize::Unknown,
        format,
      )
    }

    fn info(id: &str, name: &str, is_default: bool) -> AudioDeviceInfo {
      AudioDeviceInfo {
        id: AudioDeviceId::new(id.to_owned()),
        name: name.to_owned(),
        is_default,
      }
    }
  }
}
