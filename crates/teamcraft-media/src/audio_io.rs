//! Where call audio comes from and goes to, shared by the native and the browser driver.
//!
//! Device ids are platform strings (cpal's `host:device` on desktop, `MediaDeviceInfo.deviceId` in
//! browsers): opaque to the app, stable enough to store as a setting.

use std::fmt;

use crate::audio_frame::SAMPLE_RATE_HZ;

/// What the call sends.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioSource {
  Silence,
  /// A sine tone (automated tests, "can you hear me" checks).
  Tone {
    frequency_hz: f32,
    amplitude: f32,
  },
  Microphone(AudioDeviceChoice),
}

/// Where received audio goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioSink {
  /// Decoded and mixed, then dropped (headless clients); still counted in the call statistics.
  Discard,
  Speakers(AudioDeviceChoice),
}

/// Identifies an audio device on this machine; opaque to callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioDeviceId(String);

/// A device to show in a picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
  pub id: AudioDeviceId,
  /// Human-readable name from the platform.
  pub name: String,
  pub is_default: bool,
}

/// Which device to open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AudioDeviceChoice {
  /// The system default, following the platform's choice at open time.
  #[default]
  Default,
  Device(AudioDeviceId),
}

impl AudioDeviceId {
  /// Wraps a stored id (from [`AudioDeviceId::as_str`] of an earlier listing).
  pub fn new(id: impl Into<String>) -> Self {
    Self(id.into())
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl fmt::Display for AudioDeviceId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str(&self.0)
  }
}

/// A sine tone as mono 48 kHz samples, for test clients.
pub fn sine_tone(frequency_hz: f32, amplitude: f32, start_sample: u64, samples: usize) -> Vec<f32> {
  (0..samples as u64)
    .map(|offset| {
      let t = (start_sample + offset) as f32 / SAMPLE_RATE_HZ as f32;
      amplitude * (2.0 * std::f32::consts::PI * frequency_hz * t).sin()
    })
    .collect()
}
