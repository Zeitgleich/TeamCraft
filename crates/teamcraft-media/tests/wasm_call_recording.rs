//! The browser's call recorder (#2114) in a real browser: WebCodecs encodes, the sealed file grows
//! in the origin private file system (or memory), and the take decrypts to an Ogg file of the
//! played length. Needs a browser (Node has no WebCodecs): CI's browser tests run it in headless
//! Chromium.

#![cfg(all(target_arch = "wasm32", target_os = "unknown", feature = "web"))]
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use js_sys::Uint8Array;
use teamcraft_media::audio_frame::{FRAME_SAMPLES_PER_CHANNEL, SAMPLE_RATE_HZ};
use teamcraft_media::call_web::recording::{RecordingError, RecordingFolder, WebCallRecorder};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn played_frames_and_the_own_voice_make_one_file_of_the_played_length() {
  let folder = RecordingFolder::new();
  let recorder = WebCallRecorder::start(&folder, Duration::from_secs(60), Box::new(|| {}))
    .await
    .unwrap();
  let sink = recorder.sink();
  for index in 0..100 {
    sink.push_own(&tone(index, 660.0));
    sink.push_playback(&tone(index, 440.0));
    // The encoder works off the page's event loop, as in a call (one frame per 20 ms there).
    if index % 10 == 9 {
      yield_now().await;
    }
  }
  assert!(!sink.is_ended());
  let take = recorder.finish().await.unwrap();
  assert_eq!(take.duration_ms, 2000, "one packet out per frame played");
  assert!(!take.is_at_limit && !take.is_at_memory_limit);
  let file = take.decrypt(&folder).await.unwrap();
  let bytes = bytes_of(&file).await;
  assert_eq!(&bytes[..4], b"OggS");
  assert_eq!(bytes.len() as u64, take.size_bytes);
  // The end of the stream is marked on the last page.
  let last_page = bytes.windows(4).rposition(|window| window == b"OggS").unwrap();
  assert_eq!(bytes[last_page + 5] & 0x04, 0x04);
  take.delete().await;
  folder.clear().await;
}

/// "Save as file…" (#2116) decrypts a copy and keeps the take: it must decrypt again for posting,
/// to the same file, and the copies must not replace each other.
#[wasm_bindgen_test]
async fn a_take_saved_as_a_file_still_decrypts_for_posting() {
  let folder = RecordingFolder::new();
  let recorder = WebCallRecorder::start(&folder, Duration::from_secs(60), Box::new(|| {}))
    .await
    .unwrap();
  let sink = recorder.sink();
  for index in 0..50 {
    sink.push_playback(&tone(index, 440.0));
    if index % 10 == 9 {
      yield_now().await;
    }
  }
  let take = recorder.finish().await.unwrap();
  let saved = take.decrypt(&folder).await.unwrap();
  let posted = take.decrypt(&folder).await.unwrap();
  let (saved, posted) = (bytes_of(&saved).await, bytes_of(&posted).await);
  assert_eq!(&saved[..4], b"OggS");
  assert_eq!(saved.len() as u64, take.size_bytes);
  assert_eq!(saved, posted);
  take.delete().await;
  folder.clear().await;
}

#[wasm_bindgen_test]
async fn it_stops_at_its_maximum_length_and_says_so() {
  let folder = RecordingFolder::new();
  let ended = Rc::new(Cell::new(0));
  let on_end = {
    let ended = Rc::clone(&ended);
    Box::new(move || ended.set(ended.get() + 1))
  };
  let recorder = WebCallRecorder::start(&folder, Duration::from_millis(200), on_end)
    .await
    .unwrap();
  let sink = recorder.sink();
  for index in 0..30 {
    sink.push_playback(&tone(index, 440.0));
    yield_now().await;
  }
  for _ in 0..100 {
    if sink.is_ended() {
      break;
    }
    yield_now().await;
  }
  assert!(sink.is_ended());
  assert_eq!(ended.get(), 1, "called back once");
  let take = recorder.finish().await.unwrap();
  // The ceiling of a policy: 200 ms here, more frames played are not recorded.
  assert_eq!(take.duration_ms, 200);
  assert!(take.is_at_limit || take.is_at_memory_limit);
  take.delete().await;
  folder.clear().await;
}

#[wasm_bindgen_test]
async fn an_empty_or_cancelled_recording_keeps_nothing() {
  let folder = RecordingFolder::new();
  let recorder = WebCallRecorder::start(&folder, Duration::from_secs(60), Box::new(|| {}))
    .await
    .unwrap();
  assert!(matches!(recorder.finish().await, Err(RecordingError::Empty)));
  let recorder = WebCallRecorder::start(&folder, Duration::from_secs(60), Box::new(|| {}))
    .await
    .unwrap();
  recorder.sink().push_playback(&tone(0, 440.0));
  recorder.cancel();
  folder.clear().await;
}

#[wasm_bindgen_test]
async fn a_frame_of_another_length_is_skipped() {
  let folder = RecordingFolder::new();
  let recorder = WebCallRecorder::start(&folder, Duration::from_secs(60), Box::new(|| {}))
    .await
    .unwrap();
  let sink = recorder.sink();
  sink.push_playback(&[0.0; 480]);
  for index in 0..10 {
    sink.push_playback(&tone(index, 440.0));
  }
  let take = recorder.finish().await.unwrap();
  assert_eq!(take.duration_ms, 200);
  take.delete().await;
  folder.clear().await;
}

fn tone(index: usize, frequency_hz: f32) -> Vec<f32> {
  (0..FRAME_SAMPLES_PER_CHANNEL)
    .map(|sample| {
      let at = (index * FRAME_SAMPLES_PER_CHANNEL + sample) as f32 / SAMPLE_RATE_HZ as f32;
      0.3 * (std::f32::consts::TAU * frequency_hz * at).sin()
    })
    .collect()
}

async fn bytes_of(blob: &web_sys::Blob) -> Vec<u8> {
  let buffer = JsFuture::from(blob.array_buffer()).await.unwrap();
  Uint8Array::new(&buffer).to_vec()
}

/// Lets the browser run its tasks (the encoder's outputs, the file's writes).
async fn yield_now() {
  let promise = js_sys::Promise::new(&mut |resolve, _| {
    let window = web_sys::window().unwrap();
    window
      .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 5)
      .unwrap();
  });
  JsFuture::from(promise).await.unwrap();
}
