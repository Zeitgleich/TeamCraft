//! Voice messages another member sent (#204), decoded where the platform's player can't read Ogg
//! or WebM (iOS, the macOS webview). Whatever the bytes, decoding must not panic, and what comes
//! out stays within what the input's size can plausibly hold, so a small file can't fill a disk.
#![no_main]

use std::io::Cursor;
use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use teamcraft_media::voice_decode::{DECODED_RATE_HZ, decode_bytes, plausible_length};

/// The workspace's longest voice message, the cap the apps pass.
const MAX: Duration = Duration::from_secs(60 * 60);

fuzz_target!(|data: &[u8]| {
  let mut wav = Cursor::new(Vec::new());
  let Ok(length) = decode_bytes(data.to_vec(), &mut wav, MAX) else {
    return;
  };
  let bound = plausible_length(data.len() as u64).min(MAX);
  assert!(length <= bound, "{length:?} from {} bytes", data.len());
  let samples_bound = bound.as_secs() * u64::from(DECODED_RATE_HZ);
  assert!(wav.get_ref().len() as u64 <= 44 + 2 * samples_bound);
});
