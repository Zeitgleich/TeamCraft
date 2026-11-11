//! The sound of a phone's screen share: the app captures what other apps play (Android's
//! `AudioPlaybackCapture`, which leaves out the call's own playback and apps that opt out) and
//! pushes it here as 16-bit mono PCM at 48 kHz; the call sends it as the share's second audio
//! stream, as the desktop does with its system audio.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use teamcraft_media::system_audio::{SystemAudioCapture, SystemAudioError, SystemAudioSender};

/// The share's sound as a system audio capture for the call: the app pushes samples,
/// [`SystemAudioCapture::start`] hands over the queue they go to.
pub(crate) struct AppScreenAudio {
  slot: ScreenAudioSlot,
  /// This capture's start, while its queue is the slot's.
  maybe_start: Option<u64>,
}

/// Where pushed samples go while the share's sound is sent.
#[derive(Clone, Default)]
pub(crate) struct ScreenAudioSlot {
  inner: Arc<Mutex<SlotState>>,
}

/// The queue of the capture that started last, numbered by its start.
#[derive(Default)]
struct SlotState {
  starts: u64,
  maybe_current: Option<(u64, SystemAudioSender)>,
}

impl AppScreenAudio {
  pub(crate) fn new(slot: ScreenAudioSlot) -> Self {
    Self {
      slot,
      maybe_start: None,
    }
  }
}

impl SystemAudioCapture for AppScreenAudio {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    let mut state = self.slot.lock();
    state.starts += 1;
    state.maybe_current = Some((state.starts, samples));
    self.maybe_start = Some(state.starts);
    Ok(())
  }

  /// Leaves the slot to a capture that started since (a replacing one starts first).
  fn stop(&mut self) {
    let Some(start) = self.maybe_start.take() else {
      return;
    };
    let mut state = self.slot.lock();
    if state
      .maybe_current
      .as_ref()
      .is_some_and(|(current, _)| *current == start)
    {
      state.maybe_current = None;
    }
  }
}

impl ScreenAudioSlot {
  /// Queues 16-bit little-endian mono samples at 48 kHz; `false` while the sound isn't sent.
  pub(crate) fn push(&self, pcm16: &[u8]) -> bool {
    let Some(sender) = self
      .lock()
      .maybe_current
      .as_ref()
      .map(|(_, sender)| sender.clone())
    else {
      return false;
    };
    sender.send(pcm16_to_f32(pcm16))
  }

  pub(crate) fn clear(&self) {
    self.lock().maybe_current = None;
  }

  fn lock(&self) -> MutexGuard<'_, SlotState> {
    // Each update is a single assignment; a poisoned lock holds a consistent value.
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Samples in -1.0..1.0; a trailing odd byte is dropped.
fn pcm16_to_f32(pcm16: &[u8]) -> Vec<f32> {
  pcm16
    .as_chunks::<2>()
    .0
    .iter()
    .map(|pair| f32::from(i16::from_le_bytes(*pair)) / 32_768.0)
    .collect()
}

#[cfg(test)]
mod tests {
  use teamcraft_media::system_audio::system_audio_queue;

  use super::*;

  #[test]
  fn pcm_becomes_float_samples() {
    let pcm = [0x00, 0x00, 0xff, 0x7f, 0x00, 0x80, 0x01];
    assert_eq!(pcm16_to_f32(&pcm), vec![0.0, 32_767.0 / 32_768.0, -1.0]);
  }

  #[test]
  fn samples_go_out_only_while_the_sound_is_sent() {
    let slot = ScreenAudioSlot::default();
    assert!(!slot.push(&[0, 0]));
    let mut capture = AppScreenAudio::new(slot.clone());
    let (sender, mut receiver) = system_audio_queue();
    capture.start(sender).unwrap();
    assert!(slot.push(&[0, 0x40]));
    assert_eq!(receiver.try_recv().unwrap(), vec![0.5]);
    capture.stop();
    assert!(!slot.push(&[0, 0]));
  }

  #[test]
  fn a_replaced_capture_leaves_the_slot_to_its_successor() {
    let slot = ScreenAudioSlot::default();
    let mut old = AppScreenAudio::new(slot.clone());
    let (old_sender, _old_receiver) = system_audio_queue();
    old.start(old_sender).unwrap();
    let mut new = AppScreenAudio::new(slot.clone());
    let (new_sender, mut new_receiver) = system_audio_queue();
    new.start(new_sender).unwrap();
    old.stop();
    assert!(slot.push(&[0, 0]));
    assert_eq!(new_receiver.try_recv().unwrap(), vec![0.0]);
    slot.clear();
    assert!(!slot.push(&[0, 0]));
  }
}
