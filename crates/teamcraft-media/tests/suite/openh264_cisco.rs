//! Cisco's prebuilt OpenH264 end to end: install a download (`TEAMCRAFT_OPENH264_DOWNLOAD`, the
//! `.bz2` from `ciscobinary.openh264.org` for this platform), then encode and decode with it.
//! Skipped without the variable: tests don't download.

#![cfg(feature = "native")]
// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use teamcraft_media::openh264_library::{self, OpenH264Binary};
use teamcraft_media::video::{VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::test_pattern;
use teamcraft_media::video_codec::{
  OpenH264Decoder, OpenH264Encoder, VideoContent, VideoDecoder, VideoEncoder,
};

const SIZE: VideoResolution = VideoResolution {
  width: 640,
  height: 360,
};

#[test]
fn ciscos_binary_installs_and_codes() {
  let Ok(download) = std::env::var("TEAMCRAFT_OPENH264_DOWNLOAD") else {
    return;
  };
  let compressed = std::fs::read(download).unwrap();
  let directory = std::env::temp_dir().join(format!("teamcraft-openh264-{}", std::process::id()));
  let binary = OpenH264Binary::for_this_platform().unwrap();

  let path = binary.install(&directory, &compressed).unwrap();
  assert!(path.exists());
  assert!(openh264_library::is_available());
  assert!(
    binary.use_installed(&directory),
    "a cached install is found again"
  );

  let layer = simulcast_layers(SIZE, 30, 1_000_000, 1)[0];
  let mut encoder = OpenH264Encoder::new(&layer, VideoContent::Camera).unwrap();
  let mut decoder = OpenH264Decoder::new().unwrap();
  let mut decoded = 0;
  for index in 0..5u64 {
    let frame = test_pattern(SIZE, index, index * 33_333).unwrap();
    let picture = encoder.encode(&frame, false).unwrap().unwrap();
    if decoder
      .decode(&picture.data, frame.timestamp_us())
      .unwrap()
      .is_some()
    {
      decoded += 1;
    }
  }
  assert!(decoded >= 4, "decoded {decoded} of 5 frames");
  let _ = std::fs::remove_dir_all(&directory);
}
