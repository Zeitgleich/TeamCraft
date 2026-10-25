//! Picks the camera mode closest to what a call wants from the modes a device offers, so opening a
//! camera never fails just because the preferred pixel format or size is missing (#516: AVFoundation
//! cameras don't offer NV12 1280×720@30). Pure logic, shared by the platform backends.

use crate::capture_convert::PixelFormat;
use crate::video_capture::CaptureFormat;

/// Pixel formats [`crate::capture_convert::RawPicture::to_i420`] converts, cheapest first. Planar
/// and semi-planar YUV need the least work, packed YUV a repack, RGB a colour conversion, MJPEG a
/// decode. (UYVY is not converted yet.)
const PIXEL_FORMAT_PREFERENCE: [PixelFormat; 6] = [
  PixelFormat::Nv12,
  PixelFormat::I420,
  PixelFormat::Yuyv,
  PixelFormat::Bgrx,
  PixelFormat::Rgbx,
  PixelFormat::Mjpeg,
];

/// One mode a camera reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferedMode {
  pub pixel_format: PixelFormat,
  pub width: u32,
  pub height: u32,
  pub framerate: u32,
}

/// The best offered mode for `wanted`: closest resolution first, then closest frame rate (the
/// higher one on a tie), then the cheapest pixel format to convert. Modes in formats we can't
/// convert are ignored; `None` if nothing is left.
pub fn choose_mode(offered: &[OfferedMode], wanted: CaptureFormat) -> Option<OfferedMode> {
  let wanted_width = i64::from(wanted.resolution.width);
  let wanted_height = i64::from(wanted.resolution.height);
  let wanted_rate = i64::from(wanted.framerate);
  offered
    .iter()
    .filter_map(|mode| {
      let rank = PIXEL_FORMAT_PREFERENCE
        .iter()
        .position(|format| *format == mode.pixel_format)?;
      let resolution_distance =
        (i64::from(mode.width) - wanted_width).pow(2) + (i64::from(mode.height) - wanted_height).pow(2);
      let rate = i64::from(mode.framerate);
      Some((
        (resolution_distance, (rate - wanted_rate).abs(), -rate, rank),
        *mode,
      ))
    })
    .min_by_key(|(key, _)| *key)
    .map(|(_, mode)| mode)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::VideoResolution;

  const WANTED: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1280,
      height: 720,
    },
    framerate: 30,
  };

  #[test]
  fn exact_match_in_another_format_beats_nothing() {
    let offered = [
      mode(PixelFormat::Yuyv, 1280, 720, 30),
      mode(PixelFormat::Yuyv, 640, 480, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn prefers_nv12_when_everything_else_ties() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Nv12, 1280, 720, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  #[test]
  fn closest_resolution_wins_over_frame_rate() {
    let offered = [
      mode(PixelFormat::Nv12, 1920, 1080, 30),
      mode(PixelFormat::Nv12, 1280, 960, 15),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  #[test]
  fn closest_frame_rate_then_higher_on_a_tie() {
    let offered = [
      mode(PixelFormat::Nv12, 1280, 720, 15),
      mode(PixelFormat::Nv12, 1280, 720, 20),
      mode(PixelFormat::Nv12, 1280, 720, 40),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[2]));
  }

  #[test]
  fn empty_list_gives_nothing() {
    assert_eq!(choose_mode(&[], WANTED), None);
  }

  #[test]
  fn only_mjpeg() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Mjpeg, 640, 480, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_vga_at_15() {
    let offered = [mode(PixelFormat::Yuyv, 640, 480, 15)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_4k() {
    let offered = [mode(PixelFormat::Nv12, 3840, 2160, 30)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn only_1080p_at_60() {
    let offered = [mode(PixelFormat::Mjpeg, 1920, 1080, 60)];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[0]));
  }

  #[test]
  fn raw_rgb_is_chosen_when_nothing_cheaper_exists() {
    let offered = [
      mode(PixelFormat::Mjpeg, 1280, 720, 30),
      mode(PixelFormat::Bgrx, 1280, 720, 30),
    ];
    assert_eq!(choose_mode(&offered, WANTED), Some(offered[1]));
  }

  fn mode(pixel_format: PixelFormat, width: u32, height: u32, framerate: u32) -> OfferedMode {
    OfferedMode {
      pixel_format,
      width,
      height,
      framerate,
    }
  }
}
