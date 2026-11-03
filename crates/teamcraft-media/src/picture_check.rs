//! What a picture shows, cheaply enough to look at every frame of a call: black, one flat colour
//! (a zeroed buffer is dark green in I420) or content. Calls use it to notice a camera or screen
//! that sends only black (issue #683: on macOS a screen capture shows black until the app restarts
//! after the Screen Recording permission was granted), tests to check that video survives
//! capture, conversion, encoding and decoding with its content.
//!
//! The check samples a grid of at most [`MAX_SAMPLES_PER_SIDE`]² pixels (about 4000), a few
//! microseconds per frame at any resolution.

use crate::video::VideoFrame;

/// Grid points per side of the picture that are sampled.
const MAX_SAMPLES_PER_SIDE: usize = 64;
/// Brightest luma (BT.601 limited range: black is 16) a black picture has in nearly all samples;
/// the darkest grey a person still sees as "black" on a screen.
const BLACK_LUMA: u8 = 32;
/// How far a black picture's chroma may be from grey (128): black, not dark green or purple.
const NEUTRAL_CHROMA_TOLERANCE: u8 = 16;
/// Luma spread (between the 2nd and 98th percentile of the samples) below which a picture is one
/// flat colour.
const FLAT_LUMA_SPREAD: u8 = 6;
/// Percentile of samples ignored at either end: a mouse pointer or a status light on a black
/// picture doesn't make it content.
const OUTLIER_PERCENT: usize = 2;

/// What a picture shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureContent {
  /// (Nearly) black everywhere.
  Black,
  /// One colour that isn't black: a cleared or zeroed buffer (dark green), a grey placeholder.
  Flat,
  /// Anything with detail.
  Detailed,
}

/// Luma and chroma statistics of a picture's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PictureStats {
  pub mean_luma: u8,
  /// Luma of the darkest and brightest samples, without the outliers at either end.
  pub low_luma: u8,
  pub high_luma: u8,
  pub mean_cb: u8,
  pub mean_cr: u8,
}

impl PictureStats {
  /// Samples `frame` on a grid.
  pub fn of(frame: &VideoFrame) -> Self {
    let resolution = frame.resolution();
    let (width, height) = (usize::from(resolution.width), usize::from(resolution.height));
    let (luma, cb, cr) = frame.planes();
    let chroma_width = width / 2;
    let mut histogram = [0usize; 256];
    let (mut luma_sum, mut cb_sum, mut cr_sum, mut count) = (0usize, 0usize, 0usize, 0usize);
    for y in grid(height) {
      for x in grid(width) {
        let value = luma[y * width + x];
        histogram[usize::from(value)] += 1;
        luma_sum += usize::from(value);
        let chroma_index = (y / 2) * chroma_width + x / 2;
        cb_sum += usize::from(cb[chroma_index]);
        cr_sum += usize::from(cr[chroma_index]);
        count += 1;
      }
    }
    // Invariant: frames have even, non-zero sides, so the grid has at least one point.
    let count = count.max(1);
    let outliers = count * OUTLIER_PERCENT / 100;
    Self {
      mean_luma: mean(luma_sum, count),
      low_luma: percentile(&histogram, outliers),
      high_luma: percentile(&histogram, count - 1 - outliers),
      mean_cb: mean(cb_sum, count),
      mean_cr: mean(cr_sum, count),
    }
  }

  pub fn content(&self) -> PictureContent {
    let is_neutral = self.mean_cb.abs_diff(128) <= NEUTRAL_CHROMA_TOLERANCE
      && self.mean_cr.abs_diff(128) <= NEUTRAL_CHROMA_TOLERANCE;
    if self.high_luma <= BLACK_LUMA && is_neutral {
      PictureContent::Black
    } else if self.high_luma - self.low_luma <= FLAT_LUMA_SPREAD {
      PictureContent::Flat
    } else {
      PictureContent::Detailed
    }
  }
}

/// What `frame` shows ([`PictureStats::of`] and [`PictureStats::content`]).
pub fn picture_content(frame: &VideoFrame) -> PictureContent {
  PictureStats::of(frame).content()
}

/// Sample positions along a side of `length` pixels: evenly spread, at most
/// [`MAX_SAMPLES_PER_SIDE`], the first half a step in.
fn grid(length: usize) -> impl Iterator<Item = usize> {
  let samples = length.clamp(1, MAX_SAMPLES_PER_SIDE);
  (0..samples).map(move |index| ((2 * index + 1) * length / (2 * samples)).min(length.saturating_sub(1)))
}

/// The value below which `rank` samples (0-based) lie.
fn percentile(histogram: &[usize; 256], rank: usize) -> u8 {
  let mut seen = 0;
  for (value, count) in histogram.iter().enumerate() {
    seen += count;
    if seen > rank {
      // `value` indexes a 256-entry array.
      return value as u8;
    }
  }
  u8::MAX
}

fn mean(sum: usize, count: usize) -> u8 {
  u8::try_from((sum + count / 2) / count).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::video::VideoResolution;

  const SIZE: VideoResolution = VideoResolution {
    width: 320,
    height: 180,
  };

  #[test]
  fn limited_and_full_range_black_are_black() {
    assert_eq!(picture_content(&flat(16, 128, 128)), PictureContent::Black);
    assert_eq!(picture_content(&flat(0, 128, 128)), PictureContent::Black);
    assert_eq!(picture_content(&flat(30, 126, 131)), PictureContent::Black);
  }

  #[test]
  fn a_pointer_on_black_is_still_black() {
    let mut frame = flat_data(16, 128, 128);
    // A 16x16 white pointer in the middle.
    for y in 80..96 {
      for x in 150..166 {
        frame[y * usize::from(SIZE.width) + x] = 235;
      }
    }
    let frame = VideoFrame::new(SIZE, 0, frame).unwrap();
    assert_eq!(picture_content(&frame), PictureContent::Black);
  }

  #[test]
  fn zeroed_buffers_and_grey_are_flat_not_black() {
    // All zero bytes: dark green on screen.
    assert_eq!(picture_content(&flat(0, 0, 0)), PictureContent::Flat);
    assert_eq!(picture_content(&flat(128, 128, 128)), PictureContent::Flat);
    assert_eq!(picture_content(&flat(235, 128, 128)), PictureContent::Flat);
  }

  #[test]
  fn gradients_and_dark_detail_are_detailed() {
    // A gradient over the whole luma range.
    assert_eq!(picture_content(&luma_ramp(16, 219)), PictureContent::Detailed);
    // A dim room: luma 40..100.
    assert_eq!(picture_content(&luma_ramp(40, 60)), PictureContent::Detailed);
  }

  #[test]
  fn statistics_of_a_known_picture() {
    let stats = PictureStats::of(&flat(100, 90, 160));
    assert_eq!(
      stats,
      PictureStats {
        mean_luma: 100,
        low_luma: 100,
        high_luma: 100,
        mean_cb: 90,
        mean_cr: 160,
      }
    );
  }

  #[test]
  fn tiny_pictures_are_sampled_whole() {
    assert_eq!(grid(2).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(grid(128).count(), MAX_SAMPLES_PER_SIDE);
    assert!(grid(4096).all(|position| position < 4096));
  }

  /// Luma rising from `low` by up to `range` across each row, grey chroma.
  fn luma_ramp(low: u8, range: u8) -> VideoFrame {
    let (width, height) = (usize::from(SIZE.width), usize::from(SIZE.height));
    let mut data: Vec<u8> = (0..width * height)
      .map(|index| low + (index % width * usize::from(range) / width) as u8)
      .collect();
    data.extend(std::iter::repeat_n(128, width * height / 2));
    VideoFrame::new(SIZE, 0, data).unwrap()
  }

  fn flat(luma: u8, cb: u8, cr: u8) -> VideoFrame {
    VideoFrame::new(SIZE, 0, flat_data(luma, cb, cr)).unwrap()
  }

  fn flat_data(luma: u8, cb: u8, cr: u8) -> Vec<u8> {
    let pixels = usize::from(SIZE.width) * usize::from(SIZE.height);
    let mut data = vec![luma; pixels];
    data.extend(std::iter::repeat_n(cb, pixels / 4));
    data.extend(std::iter::repeat_n(cr, pixels / 4));
    data
  }
}
