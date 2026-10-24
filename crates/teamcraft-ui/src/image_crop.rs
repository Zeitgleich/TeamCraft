//! Picture cropping (issue #323): what a picked file must be, the crop dialog's geometry, and the
//! client-side crop. One dialog serves avatars and space pictures; the result is a small square PNG
//! the existing upload sends (the server checks and re-encodes it again).
//!
//! Geometry: the editor shows a picture `width` × `height` pixels (after turning it by
//! `quarter_turns`) behind a square frame of `frame` CSS pixels. At zoom 1 the picture's shorter
//! side fills the frame; the frame always stays inside the picture.

/// Largest file the picker reads; it is scaled down for the editor and again for the upload (the
/// server has its own limit for what arrives).
pub const MAX_IMAGE_PICK_BYTES: f64 = 20.0 * 1024.0 * 1024.0;
/// The crop square never gets smaller than this many pixels of the editor's picture (or the
/// picture's shorter side, if that is smaller): further zooming would only show blur.
const MIN_CROP_SIDE_PX: f64 = 96.0;
/// The most a picture can be zoomed in, however large it is.
const MAX_ZOOM: f64 = 8.0;

/// The square of the picture the user is choosing, and how it sits in the frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropView {
  /// The picture as decoded (before turning).
  natural_width: f64,
  natural_height: f64,
  quarter_turns: u8,
  zoom: f64,
  /// The frame's center in picture pixels (after turning).
  center_x: f64,
  center_y: f64,
}

/// What the crop dialog produces: a square in pixels of the turned picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
  pub quarter_turns: u8,
  pub x: u32,
  pub y: u32,
  pub side: u32,
}

/// Why a picked file is not used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImagePickError {
  /// Not PNG, JPEG or WebP (SVG included: it may carry script).
  Type,
  Size,
  Unreadable,
}

impl CropView {
  /// The whole picture's centered square at zoom 1.
  pub fn new(width: u32, height: u32) -> Self {
    let (width, height) = (f64::from(width.max(1)), f64::from(height.max(1)));
    Self {
      natural_width: width,
      natural_height: height,
      quarter_turns: 0,
      zoom: 1.0,
      center_x: width / 2.0,
      center_y: height / 2.0,
    }
  }

  pub fn zoom(&self) -> f64 {
    self.zoom
  }

  /// How far the picture can be zoomed in (at least 1).
  pub fn max_zoom(&self) -> f64 {
    let short = self.short_side();
    (short / MIN_CROP_SIDE_PX.min(short)).clamp(1.0, MAX_ZOOM)
  }

  /// Moves the picture by `dx`, `dy` CSS pixels on screen (a drag or an arrow key).
  pub fn pan_by(&mut self, dx: f64, dy: f64, frame: f64) {
    let scale = self.scale(frame);
    self.center_x -= dx / scale;
    self.center_y -= dy / scale;
    self.clamp();
  }

  /// Zooms to `zoom`, keeping the picture point under `anchor` (CSS pixels from the frame's
  /// top-left corner; `None` for the frame's center) where it is.
  pub fn zoom_to(&mut self, zoom: f64, maybe_anchor: Option<(f64, f64)>, frame: f64) {
    let (anchor_x, anchor_y) = maybe_anchor.unwrap_or((frame / 2.0, frame / 2.0));
    let before = self.scale(frame);
    let point_x = self.center_x + (anchor_x - frame / 2.0) / before;
    let point_y = self.center_y + (anchor_y - frame / 2.0) / before;
    self.zoom = if zoom.is_finite() {
      zoom.clamp(1.0, self.max_zoom())
    } else {
      1.0
    };
    let after = self.scale(frame);
    self.center_x = point_x - (anchor_x - frame / 2.0) / after;
    self.center_y = point_y - (anchor_y - frame / 2.0) / after;
    self.clamp();
  }

  /// Turns the picture clockwise by 90°; the same part stays in the frame where it can.
  pub fn rotate_clockwise(&mut self) {
    // The turned picture's height is the width before; a point (x, y) moves to (height - y, x).
    let height = self.height();
    let (x, y) = (self.center_x, self.center_y);
    self.quarter_turns = (self.quarter_turns + 1) % 4;
    self.center_x = height - y;
    self.center_y = x;
    self.clamp();
  }

  /// Back to the whole picture's centered square, keeping the rotation.
  pub fn reset(&mut self) {
    self.zoom = 1.0;
    self.center_x = self.width() / 2.0;
    self.center_y = self.height() / 2.0;
  }

  /// The CSS `transform` of the picture's `<img>` (natural size, `transform-origin: 0 0`) that puts
  /// it behind a frame of `frame` CSS pixels whose top-left corner is `inset` pixels into the
  /// positioned parent.
  pub fn css_transform(&self, frame: f64, inset: f64) -> String {
    let scale = self.scale(frame);
    // Turning maps an image pixel q to p = R·q + t (picture coordinates after turning).
    let (a, b, c, d, e, f) = match self.quarter_turns {
      1 => (0.0, 1.0, -1.0, 0.0, self.natural_height, 0.0),
      2 => (-1.0, 0.0, 0.0, -1.0, self.natural_width, self.natural_height),
      3 => (0.0, -1.0, 1.0, 0.0, 0.0, self.natural_width),
      _ => (1.0, 0.0, 0.0, 1.0, 0.0, 0.0),
    };
    // Screen = inset + frame/2 + scale·(p - center).
    let shift_x = inset + frame / 2.0 + scale * (e - self.center_x);
    let shift_y = inset + frame / 2.0 + scale * (f - self.center_y);
    format!(
      "matrix({:.6}, {:.6}, {:.6}, {:.6}, {shift_x:.3}, {shift_y:.3})",
      a * scale,
      b * scale,
      c * scale,
      d * scale
    )
  }

  /// The chosen square in whole pixels of the turned picture.
  pub fn crop(&self) -> CropRect {
    let side = self.side();
    let left = (self.center_x - side / 2.0).max(0.0);
    let top = (self.center_y - side / 2.0).max(0.0);
    CropRect {
      quarter_turns: self.quarter_turns,
      x: to_pixels(left),
      y: to_pixels(top),
      side: to_pixels(side).max(1),
    }
  }

  /// Width of the turned picture.
  fn width(&self) -> f64 {
    if self.quarter_turns.is_multiple_of(2) {
      self.natural_width
    } else {
      self.natural_height
    }
  }

  fn height(&self) -> f64 {
    if self.quarter_turns.is_multiple_of(2) {
      self.natural_height
    } else {
      self.natural_width
    }
  }

  fn short_side(&self) -> f64 {
    self.natural_width.min(self.natural_height)
  }

  /// The crop square's side in picture pixels.
  fn side(&self) -> f64 {
    self.short_side() / self.zoom
  }

  /// CSS pixels per picture pixel for a frame of `frame` CSS pixels.
  fn scale(&self, frame: f64) -> f64 {
    frame / self.side()
  }

  /// Keeps the frame inside the picture.
  fn clamp(&mut self) {
    let half = self.side() / 2.0;
    self.center_x = self.center_x.clamp(half, (self.width() - half).max(half));
    self.center_y = self.center_y.clamp(half, (self.height() - half).max(half));
  }
}

/// Checks a picked file before it is read: PNG, JPEG or WebP, at most [`MAX_IMAGE_PICK_BYTES`].
pub fn check_pick(media_type: &str, size_bytes: f64) -> Result<(), ImagePickError> {
  if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp") {
    return Err(ImagePickError::Type);
  }
  if size_bytes > MAX_IMAGE_PICK_BYTES {
    return Err(ImagePickError::Size);
  }
  Ok(())
}

impl ImagePickError {
  pub fn i18n_key(self) -> &'static str {
    match self {
      Self::Type => "image-pick-type",
      Self::Size => "image-pick-size",
      Self::Unreadable => "image-pick-unreadable",
    }
  }
}

/// The editor's copy of a picked file's bytes: upright, bounded in size, as PNG with its size
/// (teamcraft-core, tested there). The UI only runs as wasm (browser and desktop webview); the
/// native build of this crate is for tests and has no core.
pub fn prepare_editor_image(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), ImagePickError> {
  #[cfg(target_arch = "wasm32")]
  let maybe_image =
    teamcraft_core::thumbnail::editor_image(bytes).map(|image| (image.png, image.width, image.height));
  #[cfg(not(target_arch = "wasm32"))]
  let maybe_image: Option<(Vec<u8>, u32, u32)> = {
    let _ = bytes;
    None
  };
  maybe_image.ok_or(ImagePickError::Unreadable)
}

/// Cuts `rect` out of the editor's PNG and scales it to at most `size_px`, as PNG.
pub fn crop_png(editor_png: &[u8], rect: CropRect, size_px: u32) -> Result<Vec<u8>, ImagePickError> {
  #[cfg(target_arch = "wasm32")]
  let maybe_png = {
    let square = teamcraft_core::thumbnail::SquareCrop {
      quarter_turns: rect.quarter_turns,
      x: rect.x,
      y: rect.y,
      side: rect.side,
    };
    teamcraft_core::thumbnail::cropped_square_png(editor_png, square, size_px)
  };
  #[cfg(not(target_arch = "wasm32"))]
  let maybe_png: Option<Vec<u8>> = {
    let _ = (editor_png, rect, size_px);
    None
  };
  maybe_png.ok_or(ImagePickError::Unreadable)
}

/// Rounds a non-negative pixel position; the picture is at most a few thousand pixels wide.
fn to_pixels(value: f64) -> u32 {
  // `as` saturates (a negative value becomes 0); callers clamp to the picture first.
  value.round() as u32
}

#[cfg(test)]
mod tests {
  use super::*;

  const FRAME: f64 = 200.0;

  #[test]
  fn a_new_view_shows_the_centered_square() {
    let view = CropView::new(1000, 500);
    assert_eq!(
      view.crop(),
      CropRect {
        quarter_turns: 0,
        x: 250,
        y: 0,
        side: 500
      }
    );
    // 500 picture pixels in 200 CSS pixels; the picture starts 250 px left of the frame.
    assert_eq!(
      view.css_transform(FRAME, 0.0),
      "matrix(0.400000, 0.000000, 0.000000, 0.400000, -100.000, 0.000)"
    );
    assert_eq!(
      view.css_transform(FRAME, 10.0),
      "matrix(0.400000, 0.000000, 0.000000, 0.400000, -90.000, 10.000)"
    );
  }

  mod panning {
    use super::*;

    #[test]
    fn dragging_moves_the_picture_with_the_pointer() {
      let mut view = CropView::new(1000, 500);
      // Dragging right by 40 CSS px (100 picture px) shows more of the left.
      view.pan_by(40.0, 0.0, FRAME);
      assert_eq!(view.crop().x, 150);
      view.pan_by(-80.0, 0.0, FRAME);
      assert_eq!(view.crop().x, 350);
    }

    #[test]
    fn the_frame_never_leaves_the_picture() {
      let mut view = CropView::new(1000, 500);
      view.pan_by(10_000.0, 10_000.0, FRAME);
      assert_eq!((view.crop().x, view.crop().y), (0, 0));
      view.pan_by(-10_000.0, -10_000.0, FRAME);
      assert_eq!((view.crop().x, view.crop().y), (500, 0));
    }
  }

  mod zooming {
    use super::*;

    #[test]
    fn zooming_around_the_center_keeps_the_center() {
      let mut view = CropView::new(1000, 500);
      view.zoom_to(2.0, None, FRAME);
      assert_eq!(
        view.crop(),
        CropRect {
          quarter_turns: 0,
          x: 375,
          y: 125,
          side: 250
        }
      );
    }

    #[test]
    fn zooming_at_a_point_keeps_that_point_under_the_pointer() {
      let mut view = CropView::new(1000, 1000);
      // The frame's top-left corner shows picture pixel (0, 0); it stays there.
      view.zoom_to(4.0, Some((0.0, 0.0)), FRAME);
      assert_eq!(
        view.crop(),
        CropRect {
          quarter_turns: 0,
          x: 0,
          y: 0,
          side: 250
        }
      );
    }

    #[test]
    fn zoom_is_bounded_by_the_picture_and_the_maximum() {
      let mut view = CropView::new(4000, 3000);
      view.zoom_to(100.0, None, FRAME);
      assert_eq!(view.zoom(), MAX_ZOOM);
      view.zoom_to(0.1, None, FRAME);
      assert_eq!(view.zoom(), 1.0);
      view.zoom_to(f64::NAN, None, FRAME);
      assert_eq!(view.zoom(), 1.0);
      // 192 px short side: at most twice (96 px squares).
      assert_eq!(CropView::new(400, 192).max_zoom(), 2.0);
      // Tiny pictures can't be zoomed.
      assert_eq!(CropView::new(50, 40).max_zoom(), 1.0);
    }

    #[test]
    fn zooming_out_pulls_the_frame_back_inside() {
      let mut view = CropView::new(1000, 500);
      view.zoom_to(4.0, None, FRAME);
      view.pan_by(-10_000.0, 0.0, FRAME);
      view.zoom_to(1.0, None, FRAME);
      assert_eq!(view.crop().x, 500);
    }
  }

  mod rotation {
    use super::*;

    #[test]
    fn turning_swaps_the_sides_and_keeps_the_chosen_part() {
      let mut view = CropView::new(1000, 500);
      view.zoom_to(4.0, None, FRAME);
      // Show the top-left corner, then turn: it ends up at the top-right.
      view.pan_by(10_000.0, 10_000.0, FRAME);
      view.rotate_clockwise();
      let crop = view.crop();
      assert_eq!((crop.quarter_turns, crop.x, crop.y, crop.side), (1, 375, 0, 125));
    }

    #[test]
    fn the_transform_turns_the_picture_about_its_corner() {
      let mut view = CropView::new(1000, 500);
      view.rotate_clockwise();
      // Turned: 500 wide, 1000 high; image pixel (0, 0) lands at the top-right of the turned picture.
      assert_eq!(
        view.css_transform(FRAME, 0.0),
        "matrix(0.000000, 0.400000, -0.400000, 0.000000, 200.000, -100.000)"
      );
      for _ in 0..3 {
        view.rotate_clockwise();
      }
      assert_eq!(view.crop().quarter_turns, 0);
      assert_eq!(view, CropView::new(1000, 500));
    }

    #[test]
    fn reset_keeps_the_rotation() {
      let mut view = CropView::new(1000, 500);
      view.rotate_clockwise();
      view.zoom_to(3.0, Some((0.0, 0.0)), FRAME);
      view.reset();
      assert_eq!(
        view.crop(),
        CropRect {
          quarter_turns: 1,
          x: 0,
          y: 250,
          side: 500
        }
      );
    }
  }

  #[test]
  fn only_raster_images_of_bounded_size_are_picked() {
    assert_eq!(check_pick("image/png", 1000.0), Ok(()));
    assert_eq!(check_pick("image/jpeg", 1000.0), Ok(()));
    assert_eq!(check_pick("image/webp", 1000.0), Ok(()));
    assert_eq!(check_pick("image/svg+xml", 10.0), Err(ImagePickError::Type));
    assert_eq!(check_pick("image/gif", 10.0), Err(ImagePickError::Type));
    assert_eq!(check_pick("", 10.0), Err(ImagePickError::Type));
    assert_eq!(
      check_pick("image/png", MAX_IMAGE_PICK_BYTES + 1.0),
      Err(ImagePickError::Size)
    );
    assert_eq!(
      prepare_editor_image(b"not an image"),
      Err(ImagePickError::Unreadable)
    );
  }
}
