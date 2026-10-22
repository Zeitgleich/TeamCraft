//! Echo cancellation, noise suppression and gain control with WebRTC's audio processing module.
//!
//! The capture side (microphone) and the render side (what the speakers play) both go through the
//! processor in 10 ms frames: the render frames tell AEC3 what echo to expect. The processor is
//! thread-safe, so the capture and playback threads share one instance.
//!
//! The module is a C++ library and opt-in at build time (`--cfg teamcraft_audio_processing`, see
//! README.md). Without it [`AudioProcessor`] passes audio through unchanged and warns once, so
//! calls still work, just without echo cancellation.

#[cfg(not(teamcraft_audio_processing))]
use std::sync::Once;

use thiserror::Error;
#[cfg(not(teamcraft_audio_processing))]
use tracing::warn;

#[cfg(teamcraft_audio_processing)]
use crate::audio_codec::SAMPLE_RATE_HZ;

/// Samples (mono) per 10 ms processing frame at 48 kHz.
pub const PROCESSING_FRAME_SAMPLES: usize = 480;

/// Audio processing for one call.
#[derive(Debug)]
pub struct AudioProcessor {
  #[cfg(teamcraft_audio_processing)]
  processor: webrtc_audio_processing::Processor,
}

/// Which parts of the processing are on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioProcessingConfig {
  pub has_echo_cancellation: bool,
  pub noise_suppression: NoiseSuppressionStrength,
  pub has_gain_control: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseSuppressionStrength {
  Off,
  Low,
  Moderate,
  High,
}

#[derive(Debug, Error)]
pub enum AudioProcessingError {
  #[error("audio processing: {0}")]
  Processing(String),
  #[error("processing frames must have {PROCESSING_FRAME_SAMPLES} samples, got {0}")]
  WrongFrameSize(usize),
}

impl Default for AudioProcessingConfig {
  fn default() -> Self {
    Self {
      has_echo_cancellation: true,
      noise_suppression: NoiseSuppressionStrength::Moderate,
      has_gain_control: true,
    }
  }
}

impl AudioProcessor {
  /// Whether this build contains the WebRTC module (otherwise audio passes through).
  pub const IS_AVAILABLE: bool = cfg!(teamcraft_audio_processing);

  #[cfg(teamcraft_audio_processing)]
  pub fn new(config: AudioProcessingConfig) -> Result<Self, AudioProcessingError> {
    let processor = webrtc_audio_processing::Processor::new(SAMPLE_RATE_HZ).map_err(processing_error)?;
    processor.set_config(webrtc::apm_config(config));
    Ok(Self { processor })
  }

  #[cfg(not(teamcraft_audio_processing))]
  pub fn new(_config: AudioProcessingConfig) -> Result<Self, AudioProcessingError> {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
      warn!("built without WebRTC audio processing: no echo cancellation, noise suppression or gain control");
    });
    Ok(Self {})
  }

  /// Cleans one 10 ms microphone frame in place (mono, f32 in -1..1).
  pub fn process_capture(&self, frame: &mut [f32]) -> Result<(), AudioProcessingError> {
    ensure_frame_size(frame.len())?;
    #[cfg(teamcraft_audio_processing)]
    self
      .processor
      .process_capture_frame([frame])
      .map_err(processing_error)?;
    Ok(())
  }

  /// Tells the echo canceller what the speakers play (one 10 ms mono frame).
  pub fn analyze_render(&self, frame: &[f32]) -> Result<(), AudioProcessingError> {
    ensure_frame_size(frame.len())?;
    #[cfg(teamcraft_audio_processing)]
    self
      .processor
      .analyze_render_frame([frame])
      .map_err(processing_error)?;
    Ok(())
  }

  /// While muted, gain control must not adapt to the silence.
  pub fn set_capture_muted(&self, is_muted: bool) {
    #[cfg(teamcraft_audio_processing)]
    self.processor.set_output_will_be_muted(is_muted);
    #[cfg(not(teamcraft_audio_processing))]
    let _ = is_muted;
  }
}

#[cfg(teamcraft_audio_processing)]
fn processing_error(error: webrtc_audio_processing::Error) -> AudioProcessingError {
  AudioProcessingError::Processing(error.to_string())
}

#[cfg(teamcraft_audio_processing)]
mod webrtc {
  use webrtc_audio_processing::config::{
    Config, EchoCanceller, GainController, GainController2, HighPassFilter, NoiseSuppression,
    NoiseSuppressionLevel,
  };

  use super::{AudioProcessingConfig, NoiseSuppressionStrength};

  pub(super) fn apm_config(config: AudioProcessingConfig) -> Config {
    let level = match config.noise_suppression {
      NoiseSuppressionStrength::Off => None,
      NoiseSuppressionStrength::Low => Some(NoiseSuppressionLevel::Low),
      NoiseSuppressionStrength::Moderate => Some(NoiseSuppressionLevel::Moderate),
      NoiseSuppressionStrength::High => Some(NoiseSuppressionLevel::High),
    };
    Config {
      high_pass_filter: Some(HighPassFilter::default()),
      echo_canceller: config.has_echo_cancellation.then(EchoCanceller::default),
      noise_suppression: level.map(|level| NoiseSuppression {
        level,
        ..NoiseSuppression::default()
      }),
      gain_controller: config
        .has_gain_control
        .then(|| GainController::GainController2(GainController2::default())),
      ..Config::default()
    }
  }
}

fn ensure_frame_size(len: usize) -> Result<(), AudioProcessingError> {
  if len == PROCESSING_FRAME_SAMPLES {
    Ok(())
  } else {
    Err(AudioProcessingError::WrongFrameSize(len))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn wrong_frame_size_is_rejected() {
    let processor = AudioProcessor::new(AudioProcessingConfig::default()).unwrap();
    assert!(matches!(
      processor.process_capture(&mut [0.0; 960]),
      Err(AudioProcessingError::WrongFrameSize(960))
    ));
    assert!(processor.analyze_render(&[0.0; 10]).is_err());
  }

  #[cfg(not(teamcraft_audio_processing))]
  #[test]
  fn without_the_module_audio_passes_through() {
    let processor = AudioProcessor::new(AudioProcessingConfig::default()).unwrap();
    let mut frame: Vec<f32> = (0..PROCESSING_FRAME_SAMPLES).map(|i| i as f32 / 1000.0).collect();
    let original = frame.clone();
    processor.analyze_render(&frame).unwrap();
    processor.process_capture(&mut frame).unwrap();
    assert_eq!(frame, original);
  }

  #[cfg(teamcraft_audio_processing)]
  #[test]
  fn echo_of_the_far_end_is_removed() {
    use std::f32::consts::PI;

    use crate::audio_codec::SAMPLE_RATE_HZ;

    let tone_frame = |index: usize| -> Vec<f32> {
      (0..PROCESSING_FRAME_SAMPLES)
        .map(|i| {
          let t = (index * PROCESSING_FRAME_SAMPLES + i) as f32 / SAMPLE_RATE_HZ as f32;
          0.5 * (2.0 * PI * 300.0 * t).sin()
        })
        .collect()
    };
    let energy = |samples: &[f32]| -> f64 { samples.iter().map(|sample| f64::from(*sample).powi(2)).sum() };
    let processor = AudioProcessor::new(AudioProcessingConfig {
      noise_suppression: NoiseSuppressionStrength::Off,
      has_gain_control: false,
      ..AudioProcessingConfig::default()
    })
    .unwrap();
    // Far end: a 300 Hz tone; the microphone hears it back at half amplitude, 20 ms late.
    let delay_frames = 2;
    let frames: Vec<Vec<f32>> = (0..600).map(tone_frame).collect();
    let mut residual_energy = 0.0;
    let mut echo_energy = 0.0;
    for (index, render) in frames.iter().enumerate() {
      processor.analyze_render(render).unwrap();
      let mut capture: Vec<f32> = match index.checked_sub(delay_frames) {
        Some(earlier) => frames[earlier].iter().map(|sample| sample * 0.5).collect(),
        None => vec![0.0; PROCESSING_FRAME_SAMPLES],
      };
      let before = energy(&capture);
      processor.process_capture(&mut capture).unwrap();
      // Measure the last second, after the canceller converged.
      if index >= 500 {
        echo_energy += before;
        residual_energy += energy(&capture);
      }
    }
    let attenuation_db = 10.0 * (echo_energy / residual_energy.max(1e-12)).log10();
    assert!(
      attenuation_db > 20.0,
      "only {attenuation_db:.1} dB echo attenuation"
    );
  }
}
