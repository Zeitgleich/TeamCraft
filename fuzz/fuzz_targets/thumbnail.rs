//! Pictures from untrusted senders, as clients and the integration bridge decode them for their
//! thumbnails and sizes: no input may panic or allocate without bound, and a thumbnail that comes
//! out fits the message's limit.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_core::thumbnail::{MAX_THUMBNAIL_BYTES, image_dimensions, thumbnail_within};

const MEDIA_TYPES: [&str; 8] = [
  "image/png",
  "image/jpeg",
  "image/gif",
  "image/webp",
  "image/bmp",
  "image/x-icon",
  "image/svg+xml",
  "image/unknown",
];
/// The bridge's default limit (`files.max_thumbnail_pixels`) divided by four: the fuzzer's memory
/// limit holds a few decodes of this size.
const MAX_PIXELS: u64 = 4_000_000;

fuzz_target!(|data: &[u8]| {
  let Some((&selector, picture)) = data.split_first() else {
    return;
  };
  let media_type = MEDIA_TYPES[usize::from(selector) % MEDIA_TYPES.len()];
  let _ = image_dimensions(picture, media_type);
  if let Some(thumbnail) = thumbnail_within(picture, media_type, MAX_PIXELS) {
    assert!(thumbnail.len() <= MAX_THUMBNAIL_BYTES, "{} bytes", thumbnail.len());
  }
});
