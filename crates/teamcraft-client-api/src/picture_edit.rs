//! Editing a picture before it is sent (issue #1354): a crop frame in shares of the picture as it
//! is shown, turned with the picture a quarter at a time. The same rules as the Android and iOS
//! editors (`PictureCrop.kt`, `PictureCrop.swift`); the browser does the pixel work.

/// The smallest crop, as a share of the picture's side: a handle can't fold the frame up.
pub const MIN_CROP_SHARE: f64 = 0.08;

/// A crop frame in shares (0 to 1) of the picture's width and height as it is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropFrame {
  pub left: f64,
  pub top: f64,
  pub right: f64,
  pub bottom: f64,
}

/// A corner of the frame the user drags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropCorner {
  TopLeft,
  TopRight,
  BottomLeft,
  BottomRight,
}

impl Default for CropFrame {
  fn default() -> Self {
    Self {
      left: 0.0,
      top: 0.0,
      right: 1.0,
      bottom: 1.0,
    }
  }
}

impl CropFrame {
  pub fn width(&self) -> f64 {
    self.right - self.left
  }

  pub fn height(&self) -> f64 {
    self.bottom - self.top
  }

  /// The whole picture: nothing to crop.
  pub fn is_whole(&self) -> bool {
    self.left <= 0.0 && self.top <= 0.0 && self.right >= 1.0 && self.bottom >= 1.0
  }

  /// The same part of the picture after it turned a quarter to the left (counter-clockwise).
  pub fn turned_left(&self) -> Self {
    Self {
      left: self.top,
      top: 1.0 - self.right,
      right: self.bottom,
      bottom: 1.0 - self.left,
    }
  }

  /// The same part after a quarter to the right: undoes [`Self::turned_left`].
  pub fn turned_right(&self) -> Self {
    Self {
      left: 1.0 - self.bottom,
      top: self.left,
      right: 1.0 - self.top,
      bottom: self.right,
    }
  }

  /// Moved by a share of the picture, kept inside it with its size.
  pub fn moved(&self, dx: f64, dy: f64) -> Self {
    let (width, height) = (self.width(), self.height());
    let left = (self.left + dx).clamp(0.0, 1.0 - width);
    let top = (self.top + dy).clamp(0.0, 1.0 - height);
    Self {
      left,
      top,
      right: left + width,
      bottom: top + height,
    }
  }

  /// One corner dragged by a share of the picture; the frame stays inside, at least
  /// [`MIN_CROP_SHARE`] on each side.
  pub fn dragged(&self, corner: CropCorner, dx: f64, dy: f64) -> Self {
    let mut frame = *self;
    let to_left = |value: f64, right: f64| value.clamp(0.0, right - MIN_CROP_SHARE);
    let to_right = |value: f64, left: f64| value.clamp(left + MIN_CROP_SHARE, 1.0);
    match corner {
      CropCorner::TopLeft => {
        frame.left = to_left(self.left + dx, self.right);
        frame.top = to_left(self.top + dy, self.bottom);
      },
      CropCorner::TopRight => {
        frame.right = to_right(self.right + dx, self.left);
        frame.top = to_left(self.top + dy, self.bottom);
      },
      CropCorner::BottomLeft => {
        frame.left = to_left(self.left + dx, self.right);
        frame.bottom = to_right(self.bottom + dy, self.top);
      },
      CropCorner::BottomRight => {
        frame.right = to_right(self.right + dx, self.left);
        frame.bottom = to_right(self.bottom + dy, self.top);
      },
    }
    frame
  }

  /// The frame in the picture before `quarter_turns_left` turns to the left: where to cut the
  /// original before turning it.
  pub fn in_original(&self, quarter_turns_left: u8) -> Self {
    (0..quarter_turns_left % 4).fold(*self, |frame, _| frame.turned_right())
  }

  /// The source rectangle in pixels of a picture of `width` × `height` (before the turns): x, y,
  /// width and height, at least one pixel each.
  pub fn source_rect(&self, quarter_turns_left: u8, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let original = self.in_original(quarter_turns_left);
    let pixel = |share: f64, side: u32| {
      let value = (share * f64::from(side)).round();
      // A share is within 0..=1, so the value is within 0..=side.
      if value <= 0.0 {
        0
      } else {
        (value as u64).min(u64::from(side)) as u32
      }
    };
    let x = pixel(original.left, width).min(width.saturating_sub(1));
    let y = pixel(original.top, height).min(height.saturating_sub(1));
    let w = pixel(original.right, width).saturating_sub(x).max(1);
    let h = pixel(original.bottom, height).saturating_sub(y).max(1);
    (x, y, w, h)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn turning_left_four_times_gives_the_same_frame() {
    let frame = sample();
    assert_close(
      frame,
      frame.turned_left().turned_left().turned_left().turned_left(),
    );
  }

  #[test]
  fn the_top_left_quarter_is_the_bottom_left_one_after_a_turn_left() {
    let quarter = CropFrame {
      left: 0.0,
      top: 0.0,
      right: 0.5,
      bottom: 0.5,
    };
    assert_close(
      CropFrame {
        left: 0.0,
        top: 0.5,
        right: 0.5,
        bottom: 1.0,
      },
      quarter.turned_left(),
    );
  }

  #[test]
  fn the_original_frame_undoes_the_turns() {
    for turns in 0..6_u8 {
      let shown = (0..turns).fold(sample(), |frame, _| frame.turned_left());
      assert_close(sample(), shown.in_original(turns));
    }
  }

  #[test]
  fn a_corner_never_folds_the_frame_or_leaves_the_picture() {
    let frame = CropFrame::default().dragged(CropCorner::TopLeft, 2.0, -1.0);
    assert!((frame.top - 0.0).abs() < 1e-9);
    assert!((frame.left - (1.0 - MIN_CROP_SHARE)).abs() < 1e-9);
  }

  #[test]
  fn moving_keeps_the_size_inside_the_picture() {
    let frame = CropFrame {
      left: 0.2,
      top: 0.2,
      right: 0.6,
      bottom: 0.5,
    }
    .moved(0.9, -0.9);
    assert!((frame.width() - 0.4).abs() < 1e-9);
    assert!((frame.right - 1.0).abs() < 1e-9 && frame.top.abs() < 1e-9);
  }

  #[test]
  fn the_source_rect_is_in_pixels_of_the_original() {
    let frame = CropFrame {
      left: 0.5,
      top: 0.0,
      right: 1.0,
      bottom: 0.5,
    };
    assert_eq!(frame.source_rect(0, 1000, 600), (500, 0, 500, 300));
    // Shown turned left, the top-right quarter is the original's bottom-right one.
    assert_eq!(frame.source_rect(1, 1000, 600), (500, 300, 500, 300));
  }

  fn sample() -> CropFrame {
    CropFrame {
      left: 0.1,
      top: 0.2,
      right: 0.5,
      bottom: 0.9,
    }
  }

  fn assert_close(expected: CropFrame, actual: CropFrame) {
    for (a, b) in [
      (expected.left, actual.left),
      (expected.top, actual.top),
      (expected.right, actual.right),
      (expected.bottom, actual.bottom),
    ] {
      assert!((a - b).abs() < 1e-9, "{expected:?} != {actual:?}");
    }
  }
}
