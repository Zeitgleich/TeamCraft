//! Motion Photos another app wrote (issue #2181): the sending phone looks for a video behind the
//! JPEG of a picture it was handed. Whatever the bytes, finding it must not panic, and what it finds
//! lies inside the file, the picture first and the video after it, starting with an `ftyp` box.
//! Blanking the clip's metadata (any bytes, not only found clips) never panics or changes a length.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_core::motion_photo::{locate, strip_metadata};

fuzz_target!(|data: &[u8]| {
  let mut any = data.to_vec();
  strip_metadata(&mut any);
  assert_eq!(any.len(), data.len());
  let Some(parts) = locate(data) else {
    return;
  };
  let len = data.len() as u64;
  assert_eq!(parts.still.start, 0);
  assert!(parts.still.end <= parts.video.start);
  assert!(parts.video.start < parts.video.end && parts.video.end <= len);
  let start = parts.video.start as usize;
  assert_eq!(&data[start + 4..start + 8], b"ftyp");
  let mut clip = data[start..parts.video.end as usize].to_vec();
  strip_metadata(&mut clip);
  assert_eq!(&clip[..8], &data[start..start + 8], "the ftyp box stays");
});
