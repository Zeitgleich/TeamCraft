//! Motion Photos another app wrote (issue #2181): the sending phone looks for a video behind the
//! JPEG of a picture it was handed. Whatever the bytes, finding it must not panic, and what it finds
//! lies inside the file, the picture first and the video after it, starting with an `ftyp` box.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_core::motion_photo::locate;

fuzz_target!(|data: &[u8]| {
  let Some(parts) = locate(data) else {
    return;
  };
  let len = data.len() as u64;
  assert_eq!(parts.still.start, 0);
  assert!(parts.still.end <= parts.video.start);
  assert!(parts.video.start < parts.video.end && parts.video.end <= len);
  let start = parts.video.start as usize;
  assert_eq!(&data[start + 4..start + 8], b"ftyp");
});
