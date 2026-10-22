//! System audio on Windows: WASAPI process loopback of everything except this process tree.
//!
//! Windows 10 2004 and later can capture the mix of all applications while leaving out one
//! process and its children (`PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`); with our own
//! process id that drops the call's playback and the WebView's sound. The capture runs on its own
//! thread in a COM multi-threaded apartment and asks for 48 kHz float stereo, which WASAPI
//! converts to.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use tracing::{debug, info, warn};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::system_audio::{MonoResampler, SystemAudioCapture, SystemAudioError, SystemAudioSender};

const CHANNELS: usize = 2;
const BYTES_PER_SAMPLE: usize = 4;
/// How long one wait for captured audio may take before the stop flag is checked again.
const WAIT_MS: u32 = 100;

/// The computer's sound minus this app's, through WASAPI process loopback.
#[derive(Default)]
pub struct WasapiSystemAudio {
  maybe_running: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
}

impl WasapiSystemAudio {
  pub fn new() -> Self {
    Self::default()
  }
}

impl SystemAudioCapture for WasapiSystemAudio {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    self.stop();
    let is_stopped = Arc::new(AtomicBool::new(false));
    let (ready, ready_rx) = std_mpsc::channel::<Result<(), SystemAudioError>>();
    let stop_flag = Arc::clone(&is_stopped);
    let thread = std::thread::Builder::new()
      .name("wasapi-system-audio".into())
      .spawn(move || {
        if let Err(error) = run_capture(&samples, &stop_flag, &ready) {
          // Before the stream ran, this is the answer to `start`; afterwards nobody listens.
          if ready.send(Err(error)).is_err() {
            debug!("system audio capture ended");
          }
        }
      })
      .map_err(|error| SystemAudioError::Device(error.to_string()))?;
    match ready_rx.recv() {
      Ok(Ok(())) => {
        self.maybe_running = Some((is_stopped, thread));
        Ok(())
      },
      Ok(Err(error)) => {
        let _ = thread.join();
        Err(error)
      },
      Err(_) => Err(SystemAudioError::Device("the capture thread ended".into())),
    }
  }

  fn stop(&mut self) {
    if let Some((is_stopped, thread)) = self.maybe_running.take() {
      is_stopped.store(true, Ordering::Relaxed);
      if thread.join().is_err() {
        warn!("WASAPI system audio thread panicked");
      }
    }
  }
}

impl Drop for WasapiSystemAudio {
  fn drop(&mut self) {
    self.stop();
  }
}

/// Opens the loopback, reports through `ready`, then captures until stopped or the consumer is
/// gone.
fn run_capture(
  samples: &SystemAudioSender,
  is_stopped: &AtomicBool,
  ready: &std_mpsc::Sender<Result<(), SystemAudioError>>,
) -> Result<(), SystemAudioError> {
  initialize_mta()
    .ok()
    .map_err(|error| SystemAudioError::Device(format!("COM: {error}")))?;
  let format = WaveFormat::new(
    32,
    32,
    &SampleType::Float,
    SAMPLE_RATE_HZ as usize,
    CHANNELS,
    None,
  );
  // `false`: everything except our process tree (the crate's name for the flag is misleading;
  // it selects PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE).
  let mut client = AudioClient::new_application_loopback_client(std::process::id(), false)
    // Older Windows versions have no process loopback.
    .map_err(|_| SystemAudioError::Unsupported)?;
  let mode = StreamMode::EventsShared {
    autoconvert: true,
    buffer_duration_hns: 0,
  };
  client
    .initialize_client(&format, &Direction::Capture, &mode)
    .map_err(wasapi_error)?;
  let event = client.set_get_eventhandle().map_err(wasapi_error)?;
  let capture = client.get_audiocaptureclient().map_err(wasapi_error)?;
  client.start_stream().map_err(wasapi_error)?;
  // `start` waits for this; it may have given up already.
  let _ = ready.send(Ok(()));
  info!("capturing system audio");

  let mut resampler = MonoResampler::new(CHANNELS, SAMPLE_RATE_HZ);
  let mut bytes: VecDeque<u8> = VecDeque::new();
  while !is_stopped.load(Ordering::Relaxed) {
    // A timeout only means silence: the loopback delivers nothing while nothing plays.
    if event.wait_for_event(WAIT_MS).is_err() {
      continue;
    }
    if capture.get_next_packet_size().map_err(wasapi_error)?.unwrap_or(0) == 0 {
      continue;
    }
    capture
      .read_from_device_to_deque(&mut bytes)
      .map_err(wasapi_error)?;
    let whole = bytes.len() - bytes.len() % (CHANNELS * BYTES_PER_SAMPLE);
    let interleaved: Vec<f32> = bytes
      .drain(..whole)
      .collect::<Vec<u8>>()
      .chunks_exact(BYTES_PER_SAMPLE)
      .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
      .collect();
    if !samples.send(resampler.process(&interleaved)) {
      break;
    }
  }
  if let Err(error) = client.stop_stream() {
    debug!(%error, "loopback stream not stopped cleanly");
  }
  Ok(())
}

fn wasapi_error(error: wasapi::WasapiError) -> SystemAudioError {
  SystemAudioError::Device(error.to_string())
}
