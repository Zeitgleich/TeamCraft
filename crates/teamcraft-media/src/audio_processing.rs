//! Echo cancellation, noise suppression and gain control with WebRTC's audio processing module.
//!
//! The capture side (microphone) and the render side (what the speakers play) both go through the
//! processor in 10 ms frames: the render frames tell AEC3 what echo to expect. The processor is
//! thread-safe, so the capture and playback threads share one instance.
//!
//! The module is a C++ library and opt-in at build time (`--cfg teamcraft_audio_processing`, see
//! README.md). Without it [`AudioProcessor`] passes audio through unchanged and warns once, so
//! calls still work, just without echo cancellation.
//!
//! This is the only voice processing of native calls: the devices are opened plainly (cpal's HAL
//! unit on macOS, not the voice-processing I/O unit), so nothing of the platform cancels echo,
//! controls the microphone's gain or ducks other sound on top of it (#2418).

#[cfg(not(teamcraft_audio_processing))]
use std::sync::Once;

use thiserror::Error;
use tracing::info;
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

/// What the echo canceller reports about the echo it sees, for the call's debug log (#2418).
/// Everything is `None` without the module, and until the canceller has an estimate.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EchoStats {
  /// How much later the speakers' signal comes back into the microphone than it was analyzed.
  pub maybe_delay_ms: Option<u32>,
  /// Echo return loss: how much quieter the echo is than what the speakers play (dB).
  pub maybe_echo_return_loss_db: Option<f64>,
  /// Echo return loss enhancement: how much of the echo the canceller removes (dB).
  pub maybe_echo_return_loss_enhancement_db: Option<f64>,
  /// How likely what is left of the microphone after cancelling is still echo (0 to 1).
  pub maybe_residual_echo_likelihood: Option<f64>,
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
    log_config(config);
    Ok(Self { processor })
  }

  #[cfg(not(teamcraft_audio_processing))]
  pub fn new(config: AudioProcessingConfig) -> Result<Self, AudioProcessingError> {
    static WARNING: Once = Once::new();
    WARNING.call_once(|| {
      warn!("built without WebRTC audio processing: no echo cancellation, noise suppression or gain control");
    });
    log_config(config);
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

  /// The echo canceller's view after the last microphone frame.
  pub fn echo_stats(&self) -> EchoStats {
    #[cfg(teamcraft_audio_processing)]
    {
      let stats = self.processor.get_stats();
      EchoStats {
        maybe_delay_ms: stats.delay_ms,
        maybe_echo_return_loss_db: stats.echo_return_loss,
        maybe_echo_return_loss_enhancement_db: stats.echo_return_loss_enhancement,
        maybe_residual_echo_likelihood: stats.residual_echo_likelihood,
      }
    }
    #[cfg(not(teamcraft_audio_processing))]
    EchoStats::default()
  }
}

/// Once per call, so the debug log tells which processing the microphone went through.
fn log_config(config: AudioProcessingConfig) {
  info!(
    is_available = AudioProcessor::IS_AVAILABLE,
    echo_cancellation = config.has_echo_cancellation,
    noise_suppression_mode = ?config.noise_suppression,
    gain_control = config.has_gain_control,
    platform_voice_processing = false,
    "audio processing"
  );
}

#[cfg(teamcraft_audio_processing)]
fn processing_error(error: webrtc_audio_processing::Error) -> AudioProcessingError {
  AudioProcessingError::Processing(error.to_string())
}

#[cfg(teamcraft_audio_processing)]
mod webrtc {
  use webrtc_audio_processing::config::{
    AdaptiveDigital, Config, EchoCanceller, GainController, GainController2, HighPassFilter,
    NoiseSuppression, NoiseSuppressionLevel,
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
        .then(|| GainController::GainController2(gain_controller())),
      ..Config::default()
    }
  }

  /// AGC2 with its adaptive digital controller: it raises a quiet microphone towards speech level
  /// (WebRTC's defaults: up to 50 dB, at most 6 dB a second, noise kept below -50 dBFS), then the
  /// limiter catches peaks. `GainController2::default()` alone leaves the adaptive controller
  /// out: AGC2 is then only the limiter at 0 dB, and a quiet microphone (a MacBook's built-in one
  /// through the plain HAL unit, #2418) reaches the others as quiet as it is, under their louder
  /// voices.
  fn gain_controller() -> GainController2 {
    GainController2 {
      adaptive_digital: Some(AdaptiveDigital::default()),
      ..GainController2::default()
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

  /// #2418: gain control is AGC2 with its adaptive controller, not the bare limiter that
  /// `GainController2::default()` is.
  #[cfg(teamcraft_audio_processing)]
  #[test]
  fn gain_control_adapts_to_the_microphone() {
    use webrtc_audio_processing::config::GainController;

    let config = webrtc::apm_config(AudioProcessingConfig::default());
    let Some(GainController::GainController2(agc2)) = config.gain_controller else {
      panic!("AGC2 expected, got {:?}", config.gain_controller);
    };
    assert!(agc2.adaptive_digital.is_some(), "{agc2:?}");
    let without = webrtc::apm_config(AudioProcessingConfig {
      has_gain_control: false,
      ..AudioProcessingConfig::default()
    });
    assert_eq!(without.gain_controller, None);
  }

  /// #2418: a voice that reaches the microphone quietly (about -45 dBFS) leaves louder: with only
  /// the limiter it left as quiet as it came.
  #[cfg(teamcraft_audio_processing)]
  #[test]
  fn a_quiet_voice_is_raised() {
    let processor = AudioProcessor::new(AudioProcessingConfig {
      has_echo_cancellation: false,
      noise_suppression: NoiseSuppressionStrength::Off,
      has_gain_control: true,
    })
    .unwrap();
    let mut input_energy = 0.0;
    let mut output_energy = 0.0;
    // Ten seconds; the last five are measured, after the gain had time to adapt.
    for index in 0..1000 {
      let mut frame = voice_like_frame(index, 0.008);
      let before = energy(&frame);
      processor.process_capture(&mut frame).unwrap();
      if index >= 500 {
        input_energy += before;
        output_energy += energy(&frame);
      }
    }
    let gain_db = 10.0 * (output_energy / input_energy).log10();
    assert!(gain_db > 10.0, "only {gain_db:.1} dB gain");
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

  #[cfg(teamcraft_audio_processing)]
  fn energy(samples: &[f32]) -> f64 {
    samples.iter().map(|sample| f64::from(*sample).powi(2)).sum()
  }

  /// 10 ms of something a voice activity detector takes for speech: a 140 Hz voice with its
  /// harmonics, two formant-like peaks, its pitch wandering and syllables four times a second.
  #[cfg(teamcraft_audio_processing)]
  fn voice_like_frame(index: usize, amplitude: f32) -> Vec<f32> {
    use std::f32::consts::PI;

    use crate::audio_codec::SAMPLE_RATE_HZ;

    (0..PROCESSING_FRAME_SAMPLES)
      .map(|i| {
        let t = (index * PROCESSING_FRAME_SAMPLES + i) as f32 / SAMPLE_RATE_HZ as f32;
        let pitch_hz = 140.0 + 20.0 * (2.0 * PI * 0.7 * t).sin();
        let syllable = (2.0 * PI * 4.0 * t).sin().max(0.0);
        let voice: f32 = (1..=20)
          .map(|harmonic| {
            let frequency_hz = pitch_hz * harmonic as f32;
            let formants = 1.0 / (1.0 + ((frequency_hz - 600.0) / 200.0).powi(2))
              + 0.6 / (1.0 + ((frequency_hz - 1800.0) / 300.0).powi(2));
            formants * (2.0 * PI * frequency_hz * t).sin()
          })
          .sum();
        amplitude * syllable * voice
      })
      .collect()
  }
}
