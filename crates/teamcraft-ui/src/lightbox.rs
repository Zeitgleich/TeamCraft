//! State of the media lightbox (images, animated images and videos), kept apart from the view so it
//! can be tested without a browser: which files it pages through (one message's album or the whole
//! conversation), where the current one is, how far it is zoomed, panned and rotated, which
//! thumbnails the strip shows, and which key does what.
//!
//! Positions are in CSS pixels relative to the centre of the stage (the area the image is shown in).
//! The image is first fitted into the stage ("fit", scale 1) and then rotated, scaled around its
//! centre and moved by an offset, which is what the view writes into the image's `transform`.
//!
//! The motion of gestures (following the finger, springs, momentum, rubber-banding) is in
//! [`physics`].

pub mod physics;

use std::ops::Range;

use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::media_viewer::{self, DOUBLE_TAP_ZOOM, FileRef, MAX_ZOOM, ViewerMedia};
// Which files the lightbox pages through and how it reads them is shared with the mobile viewers.
pub use teamcraft_client_api::media_viewer::{Direction, MediaPages, MediaScope as LightboxScope, Position};
use teamcraft_proto::AttachmentId;

use crate::model::MessageId;
use crate::prefs::AnimatedImages;

/// Largest zoom, relative to the fitted size (the same on every platform).
pub const DEFAULT_MAX_SCALE: f64 = MAX_ZOOM;
/// Zoom of a double-click or double-tap on a fitted image.
pub const DOUBLE_CLICK_SCALE: f64 = DOUBLE_TAP_ZOOM;
/// A picture with more pixels than the fit-relative maximum reaches can go to this multiple of its
/// own pixels.
const MAX_OWN_PIXEL_RATIO: f64 = 4.0;
/// ...but it is never laid out larger than this on its longer side: the engine paints only the
/// visible part, this keeps its layout and tile bookkeeping in sane numbers.
const MAX_LAID_OUT_SIDE_PX: f64 = 32_000.0;
/// A double-click goes to the picture's own pixels only when that is a real zoom-in.
const MIN_ACTUAL_SIZE_ZOOM: f64 = KEY_ZOOM_STEP;
/// Zoom step of the `+` and `-` keys and buttons.
pub const KEY_ZOOM_STEP: f64 = 1.25;
/// Two taps closer than this in time and space are a double-tap.
pub const DOUBLE_TAP_MAX_MS: f64 = 300.0;
pub const DOUBLE_TAP_MAX_PX: f64 = 30.0;
/// A pointer that moved less than this between press and release tapped (no drag).
pub const TAP_MAX_PX: f64 = 8.0;
/// Most thumbnails the strip under the image renders at once, around the current one.
pub const MAX_STRIP_THUMBNAILS: usize = 15;
/// Fitted scale.
const MIN_SCALE: f64 = 1.0;

/// Identifies one image or video across list refreshes: new messages shift indexes, not keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MediaKey {
  pub message: MessageId,
  pub attachment: AttachmentId,
}

/// What the lightbox shows a file as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
  /// A still image: zoom, pan and rotate.
  Image,
  /// A GIF or APNG: like an image, but it moves, so it can be paused (and starts paused when the
  /// "Animated images" setting or the system's reduced motion says so, #201).
  Animated,
  /// Played by the browser's own `<video>` with its controls.
  Video,
}

/// Which file the current slot is bringing in, and whether its full picture is on screen yet. Until it
/// is, that file's own placeholder (its thumbnail or inline rendition) stays in front; then the two
/// cross-fade. Keyed by the file, so a late `load` of a file paged away from changes nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Arrival {
  maybe_key: Option<MediaKey>,
  is_full_shown: bool,
}

/// A quarter-turn rotation of the view (the file is not changed).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Rotation {
  #[default]
  Upright,
  Clockwise,
  UpsideDown,
  CounterClockwise,
}

/// Everything the lightbox does from the keyboard; the tooltips name the same keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
  Previous,
  Next,
  ZoomIn,
  ZoomOut,
  ZoomReset,
  Rotate,
  ToggleThread,
  TogglePlay,
  JumpToMessage,
  Download,
  CopyImage,
}

/// Whether shortcuts are shown with ⌘ (Apple platforms) or Ctrl.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierStyle {
  Ctrl,
  Command,
}

/// A point or a distance in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
  pub x: f64,
  pub y: f64,
}

/// A width and height in CSS pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
  pub width: f64,
  pub height: f64,
}

/// Zoom, pan and rotation of the current image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
  scale: f64,
  offset: Point,
  max_scale: f64,
  rotation: Rotation,
  maybe_picture: Option<Picture>,
}

/// What the viewport shows: what a zoom means in the picture's own pixels and on the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
  /// The picture's own pixels.
  pub natural: Size,
  /// Its size at fit before a turn, see [`fitted_layout`].
  pub laid_out: Size,
  pub stage: Size,
  pub device_pixel_ratio: f64,
}

/// How the picture's element is drawn at one moment (#1089).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PictureLayout {
  /// The element's real size; `None` leaves it to the stylesheet, which fits it.
  pub maybe_size: Option<Size>,
  /// What a transform still has to scale it by.
  pub scale: f64,
}

/// Follows the pointers on the stage and turns them into pans, pinches, swipes and taps.
#[derive(Clone, Debug, Default)]
pub struct Pointers {
  /// Pointers currently pressed, by pointer id.
  active: Vec<(i32, Point)>,
  /// Where the single pointer of the current press went down; `None` once a second one joined.
  maybe_press_start: Option<Point>,
  /// The current press moved further than a tap may: its release is no tap.
  is_drag: bool,
  /// Time and place of the last tap, to recognise a double-tap.
  maybe_last_tap: Option<(f64, Point)>,
}

/// What a pointer move does to the image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Gesture {
  None,
  Pan(Point),
  /// Zoom by `factor` around `center`.
  Pinch {
    factor: f64,
    center: Point,
  },
}

/// What releasing a pointer means. Drags (paging, closing, panning) follow the finger while it moves
/// and settle on release; see [`physics`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Release {
  /// Still pressed by another pointer, or the end of a drag.
  None,
  Tap(Point),
  DoubleTap(Point),
}

impl MediaKey {
  /// The key of a file that the lightbox can show: a received image or video with an attachment id
  /// (files still uploading have none yet).
  pub fn of(file: &FileInfo) -> Option<Self> {
    MediaKind::of(&file.media_type)?;
    Self::for_file(file)
  }

  /// The key of any received file (the preview of documents, #557), with or without a picture.
  pub fn for_file(file: &FileInfo) -> Option<Self> {
    let attachment = file.file_id.parse().ok()?;
    Some(Self {
      message: file.message,
      attachment,
    })
  }

  /// The `data-media-key` of the tiles that open this file, for the lightbox to grow out of.
  pub fn dom_key(&self) -> String {
    format!("{}:{}", self.message, self.attachment)
  }

  pub fn matches(&self, file: &FileInfo) -> bool {
    file.message == self.message && file.file_id == self.attachment.to_string()
  }

  fn file_ref(&self) -> FileRef {
    FileRef {
      message: self.message,
      file_id: self.attachment.to_string(),
    }
  }
}

impl Arrival {
  /// `key` is now the current file; its full picture is not on screen yet.
  pub fn start(key: MediaKey) -> Self {
    Self {
      maybe_key: Some(key),
      is_full_shown: false,
    }
  }

  /// The full picture of `key` is decoded and on screen (its `load`, or it was already complete).
  pub fn full_shown(&mut self, key: MediaKey) {
    if self.maybe_key == Some(key) {
      self.is_full_shown = true;
    }
  }

  pub fn key(&self) -> Option<MediaKey> {
    self.maybe_key
  }

  /// Whether `key`'s full picture is on screen, so its placeholder fades out.
  pub fn is_full_shown(&self, key: MediaKey) -> bool {
    self.maybe_key == Some(key) && self.is_full_shown
  }
}

impl Picture {
  /// Device pixels per picture pixel at fit, before a turn's shrinking.
  fn fitted_own_pixel_ratio(&self) -> f64 {
    self.laid_out.width * self.device_pixel_ratio / self.natural.width
  }

  fn is_usable(&self) -> bool {
    self.natural.width > 0.0
      && self.natural.height > 0.0
      && self.laid_out.width > 0.0
      && self.laid_out.height > 0.0
      && self.device_pixel_ratio > 0.0
  }
}

impl MediaKind {
  /// `None` for files the lightbox doesn't show (documents, audio, archives).
  pub fn of(media_type: &str) -> Option<Self> {
    let media_type = media_type.trim().to_ascii_lowercase();
    match media_type.as_str() {
      "image/gif" | "image/apng" => Some(Self::Animated),
      // SVG is shown as an image only: inside `<img>` it can't run scripts.
      other if other.starts_with("image/") => Some(Self::Image),
      other if other.starts_with("video/") => Some(Self::Video),
      _ => None,
    }
  }

  /// Zoom, pan and rotation apply to pictures; a video keeps its controls.
  pub fn can_transform(self) -> bool {
    matches!(self, Self::Image | Self::Animated)
  }

  /// An animated image starts paused when it only moves on request ([`AnimatedImages::effective`]);
  /// the viewer has no hover to wait for, and opening it asked to see the picture. A video never
  /// plays by itself.
  pub fn starts_paused(self, mode: AnimatedImages) -> bool {
    match self {
      Self::Animated => mode == AnimatedImages::OnTap,
      Self::Image | Self::Video => false,
    }
  }
}

impl Rotation {
  pub fn clockwise(self) -> Self {
    match self {
      Self::Upright => Self::Clockwise,
      Self::Clockwise => Self::UpsideDown,
      Self::UpsideDown => Self::CounterClockwise,
      Self::CounterClockwise => Self::Upright,
    }
  }

  pub fn degrees(self) -> u16 {
    match self {
      Self::Upright => 0,
      Self::Clockwise => 90,
      Self::UpsideDown => 180,
      Self::CounterClockwise => 270,
    }
  }

  /// A quarter turn swaps the image's width and height on screen.
  pub fn is_sideways(self) -> bool {
    matches!(self, Self::Clockwise | Self::CounterClockwise)
  }

  fn orient(self, size: Size) -> Size {
    match self.is_sideways() {
      true => Size::new(size.height, size.width),
      false => size,
    }
  }
}

impl Command {
  /// The command of a key press, `None` for keys the lightbox leaves alone. `has_shortcut_modifier`
  /// is Ctrl, or ⌘ on Apple platforms.
  pub fn for_key(key: &str, has_shortcut_modifier: bool, has_alt: bool) -> Option<Self> {
    if has_alt {
      return None;
    }
    if has_shortcut_modifier {
      return match key {
        "s" | "S" => Some(Self::Download),
        "c" | "C" => Some(Self::CopyImage),
        _ => None,
      };
    }
    match key {
      "ArrowLeft" => Some(Self::Previous),
      "ArrowRight" => Some(Self::Next),
      "+" | "=" => Some(Self::ZoomIn),
      "-" | "_" => Some(Self::ZoomOut),
      "0" => Some(Self::ZoomReset),
      "r" | "R" => Some(Self::Rotate),
      "t" | "T" => Some(Self::ToggleThread),
      "k" | "K" => Some(Self::TogglePlay),
      "j" | "J" => Some(Self::JumpToMessage),
      _ => None,
    }
  }

  /// The key as the tooltips and `aria-keyshortcuts` name it.
  pub fn shortcut_label(self, style: ModifierStyle) -> &'static str {
    let is_apple = style == ModifierStyle::Command;
    match self {
      Self::Previous => "←",
      Self::Next => "→",
      Self::ZoomIn => "+",
      Self::ZoomOut => "−",
      Self::ZoomReset => "0",
      Self::Rotate => "R",
      Self::ToggleThread => "T",
      Self::TogglePlay => "K",
      Self::JumpToMessage => "J",
      Self::Download if is_apple => "⌘S",
      Self::Download => "Ctrl+S",
      Self::CopyImage if is_apple => "⌘C",
      Self::CopyImage => "Ctrl+C",
    }
  }

  /// The `aria-keyshortcuts` value (WAI-ARIA key names).
  pub fn aria_shortcut(self, style: ModifierStyle) -> &'static str {
    let is_apple = style == ModifierStyle::Command;
    match self {
      Self::Previous => "ArrowLeft",
      Self::Next => "ArrowRight",
      Self::ZoomIn => "+",
      Self::ZoomOut => "-",
      Self::ZoomReset => "0",
      Self::Rotate => "R",
      Self::ToggleThread => "T",
      Self::TogglePlay => "K",
      Self::JumpToMessage => "J",
      Self::Download if is_apple => "Meta+S",
      Self::Download => "Control+S",
      Self::CopyImage if is_apple => "Meta+C",
      Self::CopyImage => "Control+C",
    }
  }
}

impl ModifierStyle {
  /// ⌘ on macOS and iOS, Ctrl elsewhere, from the browser's platform string.
  pub fn for_platform(platform: &str) -> Self {
    let platform = platform.to_ascii_lowercase();
    let is_apple = ["mac", "iphone", "ipad", "ipod"]
      .iter()
      .any(|name| platform.contains(name));
    match is_apple {
      true => Self::Command,
      false => Self::Ctrl,
    }
  }
}

impl Point {
  pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

  pub fn new(x: f64, y: f64) -> Self {
    Self { x, y }
  }

  pub fn minus(self, other: Self) -> Self {
    Self::new(self.x - other.x, self.y - other.y)
  }

  fn distance(self, other: Self) -> f64 {
    let delta = self.minus(other);
    delta.x.hypot(delta.y)
  }

  fn midpoint(self, other: Self) -> Self {
    Self::new((self.x + other.x) / 2.0, (self.y + other.y) / 2.0)
  }
}

impl Size {
  pub fn new(width: f64, height: f64) -> Self {
    Self { width, height }
  }
}

impl Default for Viewport {
  fn default() -> Self {
    Self::new(DEFAULT_MAX_SCALE)
  }
}

impl Viewport {
  pub fn new(max_scale: f64) -> Self {
    Self {
      scale: MIN_SCALE,
      offset: Point::ORIGIN,
      max_scale: max_scale.max(MIN_SCALE),
      rotation: Rotation::Upright,
      maybe_picture: None,
    }
  }

  /// The picture is known, or the window or stage changed. Without usable sizes (not decoded yet)
  /// the zoom says nothing about its own pixels.
  pub fn set_picture(&mut self, maybe_picture: Option<Picture>) {
    self.maybe_picture = maybe_picture.filter(Picture::is_usable);
    self.scale = self.scale.min(self.max_scale());
    if let Some(picture) = self.maybe_picture {
      self.clamp_offset(picture.laid_out, picture.stage);
    }
  }

  /// Zoom relative to the fitted image (1 = fit).
  pub fn scale(&self) -> f64 {
    self.scale
  }

  pub fn rotation(&self) -> Rotation {
    self.rotation
  }

  /// The CSS scale of the laid-out image: the zoom times what a quarter turn needs to fit again.
  /// `image` is the laid-out (unrotated) size, which the stylesheet already fits into the stage.
  pub fn display_scale(&self, image: Size, stage: Size) -> f64 {
    self.scale * self.fit_factor(image, stage)
  }

  /// [`Self::display_scale`] of the known picture.
  pub fn shown_scale(&self) -> f64 {
    match self.maybe_picture {
      Some(picture) => self.display_scale(picture.laid_out, picture.stage),
      None => self.scale,
    }
  }

  /// Device pixels per picture pixel on screen, as other image viewers mean their percentage: 1 is
  /// one picture pixel on one device pixel. `None` until the picture is known.
  pub fn own_pixel_ratio(&self) -> Option<f64> {
    self
      .maybe_picture
      .map(|picture| self.shown_scale() * picture.fitted_own_pixel_ratio())
  }

  /// The zoom ([`Self::scale`]) that shows one picture pixel on one device pixel.
  pub fn actual_size_scale(&self) -> Option<f64> {
    let picture = self.maybe_picture?;
    let at_fit = self.display_scale(picture.laid_out, picture.stage) / self.scale;
    Some(1.0 / (at_fit * picture.fitted_own_pixel_ratio()))
  }

  /// How the element is drawn when it was last laid out at the display scale `maybe_laid_at`:
  /// painted at its real size, nothing depends on how an engine rasterises a transformed image, and
  /// only what a running gesture added since is a transform. `None` is the stylesheet's fit.
  pub fn layout_at(&self, maybe_laid_at: Option<f64>) -> PictureLayout {
    let shown = self.shown_scale();
    match (maybe_laid_at, self.maybe_picture) {
      (Some(laid_at), Some(picture)) if laid_at > 0.0 => PictureLayout {
        maybe_size: Some(Size::new(
          snap_to_device(picture.laid_out.width * laid_at, picture.device_pixel_ratio),
          snap_to_device(picture.laid_out.height * laid_at, picture.device_pixel_ratio),
        )),
        scale: shown / laid_at,
      },
      _ => PictureLayout {
        maybe_size: None,
        scale: shown,
      },
    }
  }

  /// The offset on whole device pixels: at rest a half pixel would resample the whole picture.
  pub fn snapped_offset(&self) -> Point {
    let ratio = self
      .maybe_picture
      .map_or(1.0, |picture| picture.device_pixel_ratio);
    Point::new(
      snap_to_device(self.offset.x, ratio),
      snap_to_device(self.offset.y, ratio),
    )
  }

  /// The zoom slider's position for the current zoom, from 0 (fit) to 1 (the maximum). The slider
  /// is logarithmic, so every step zooms by the same factor.
  pub fn slider_position(&self) -> f64 {
    let max_scale = self.max_scale();
    match max_scale > MIN_SCALE {
      true => (self.scale.ln() / max_scale.ln()).clamp(0.0, 1.0),
      false => 0.0,
    }
  }

  /// Zooms to the slider's `position` (0 to 1) around the stage centre.
  pub fn zoom_to_slider(&mut self, position: f64, image: Size, stage: Size) {
    if !position.is_finite() {
      return;
    }
    let target = self.max_scale().powf(position.clamp(0.0, 1.0));
    self.zoom_by(target / self.scale, Point::ORIGIN, image, stage);
  }

  /// Turns the view a quarter clockwise and fits the image again.
  pub fn rotate_clockwise(&mut self) {
    self.rotation = self.rotation.clockwise();
    self.reset();
  }

  pub fn offset(&self) -> Point {
    self.offset
  }

  pub fn is_zoomed(&self) -> bool {
    self.scale > MIN_SCALE
  }

  pub fn can_zoom_in(&self) -> bool {
    self.scale < self.max_scale()
  }

  /// Back to the fitted image.
  pub fn reset(&mut self) {
    self.scale = MIN_SCALE;
    self.offset = Point::ORIGIN;
  }

  /// Zooms by `factor` keeping the image point under `anchor` where it is (the cursor, the pinch
  /// centre, or the stage centre for keys), then keeps the image inside the stage.
  pub fn zoom_by(&mut self, factor: f64, anchor: Point, image: Size, stage: Size) {
    if !factor.is_finite() || factor <= 0.0 {
      return;
    }
    let scale = (self.scale * factor).clamp(MIN_SCALE, self.max_scale());
    let applied = scale / self.scale;
    self.offset = Point::new(
      anchor.x - (anchor.x - self.offset.x) * applied,
      anchor.y - (anchor.y - self.offset.y) * applied,
    );
    self.scale = scale;
    self.clamp_offset(image, stage);
  }

  /// Double-click or double-tap: where it leads. A fitted image goes to its own pixels at `anchor`
  /// (when that is a zoom-in, else by [`DOUBLE_CLICK_SCALE`]), a zoomed one back to fit.
  pub fn toggled(&self, anchor: Point, image: Size, stage: Size) -> (f64, Point) {
    if self.is_zoomed() {
      return (MIN_SCALE, Point::ORIGIN);
    }
    let factor = self
      .actual_size_scale()
      .filter(|scale| *scale >= MIN_ACTUAL_SIZE_ZOOM)
      .unwrap_or(DOUBLE_CLICK_SCALE);
    let mut zoomed = *self;
    zoomed.zoom_by(factor, anchor, image, stage);
    (zoomed.scale, zoomed.offset)
  }

  /// The fit-relative maximum, or further for a picture whose own pixels lie beyond it. Bounded in
  /// the size it lays the picture out at.
  pub fn max_scale(&self) -> f64 {
    let Some(picture) = self.maybe_picture else {
      return self.max_scale;
    };
    let longer_side = picture.laid_out.width.max(picture.laid_out.height);
    let at_fit = self.display_scale(picture.laid_out, picture.stage) / self.scale;
    let side_limit = MAX_LAID_OUT_SIDE_PX / (longer_side * at_fit);
    let own_pixels = self.actual_size_scale().unwrap_or(MIN_SCALE) * MAX_OWN_PIXEL_RATIO;
    self.max_scale.max(own_pixels.min(side_limit))
  }

  /// Shows `scale` and `offset` as they are, also past their limits: the frames of a pinch or pan
  /// that rubber-bands, and of the spring that settles it.
  pub fn present(&mut self, scale: f64, offset: Point) {
    if scale.is_finite() && scale > 0.0 && offset.x.is_finite() && offset.y.is_finite() {
      self.scale = scale;
      self.offset = offset;
    }
  }

  /// How far the offset may go each way at `scale`: half of what the shown image is larger than the
  /// stage.
  pub fn pan_limits(&self, scale: f64, image: Size, stage: Size) -> Point {
    let shown = self.rotation.orient(image);
    let scale = scale * self.fit_factor(image, stage);
    let limit = |image_side: f64, stage_side: f64| ((image_side * scale - stage_side) / 2.0).max(0.0);
    Point::new(limit(shown.width, stage.width), limit(shown.height, stage.height))
  }

  /// Where a pinch or pan released here comes to rest: the zoom back inside its limits around
  /// `anchor` (the last pinch centre), and the offset back inside the stage.
  pub fn settled(&self, anchor: Point, image: Size, stage: Size) -> (f64, Point) {
    let scale = self.scale.clamp(MIN_SCALE, self.max_scale());
    let applied = scale / self.scale;
    let limits = self.pan_limits(scale, image, stage);
    let offset = Point::new(
      (anchor.x - (anchor.x - self.offset.x) * applied).clamp(-limits.x, limits.x),
      (anchor.y - (anchor.y - self.offset.y) * applied).clamp(-limits.y, limits.y),
    );
    (scale, offset)
  }

  /// Moves a zoomed image; it can't be dragged out of the stage.
  pub fn pan_by(&mut self, delta: Point, image: Size, stage: Size) {
    self.offset = Point::new(self.offset.x + delta.x, self.offset.y + delta.y);
    self.clamp_offset(image, stage);
  }

  /// The image's edges never come inside the stage's edges; a side smaller than the stage stays
  /// centred.
  fn clamp_offset(&mut self, image: Size, stage: Size) {
    let limits = self.pan_limits(self.scale, image, stage);
    self.offset = Point::new(
      self.offset.x.clamp(-limits.x, limits.x),
      self.offset.y.clamp(-limits.y, limits.y),
    );
  }

  /// The size the image takes on screen at fit, turned: what the lightbox opens from and closes to.
  pub fn fitted_size(&self, image: Size, stage: Size) -> Size {
    let shown = self.rotation.orient(image);
    let fit = self.fit_factor(image, stage);
    Size::new(shown.width * fit, shown.height * fit)
  }

  /// A sideways image may be too tall or wide for the stage once turned: shrink it until it fits.
  fn fit_factor(&self, image: Size, stage: Size) -> f64 {
    let shown = self.rotation.orient(image);
    if shown.width <= 0.0 || shown.height <= 0.0 {
      return 1.0;
    }
    (stage.width / shown.width)
      .min(stage.height / shown.height)
      .clamp(f64::MIN_POSITIVE, 1.0)
  }
}

impl Pointers {
  pub fn down(&mut self, id: i32, at: Point) {
    self.active.retain(|(known, _)| *known != id);
    self.active.push((id, at));
    self.maybe_press_start = match self.active.len() {
      1 => Some(at),
      _ => None,
    };
    if self.active.len() == 1 {
      self.is_drag = false;
    }
  }

  /// How many pointers are pressed.
  pub fn count(&self) -> usize {
    self.active.len()
  }

  /// Where the single pointer of the current press went down.
  pub fn press_start(&self) -> Option<Point> {
    self.maybe_press_start
  }

  /// A pressed pointer moved: one pointer pans, two pinch.
  pub fn moved(&mut self, id: i32, at: Point) -> Gesture {
    let Some(slot) = self.active.iter().position(|(known, _)| *known == id) else {
      return Gesture::None;
    };
    let before = self.active[slot].1;
    self.active[slot].1 = at;
    if self
      .maybe_press_start
      .is_some_and(|start| at.distance(start) > TAP_MAX_PX)
    {
      self.is_drag = true;
    }
    match self.active.as_slice() {
      [_] => Gesture::Pan(at.minus(before)),
      [(_, first), (_, second)] => {
        let other = if slot == 0 { *second } else { *first };
        let (old_distance, new_distance) = (before.distance(other), at.distance(other));
        if old_distance < 1.0 {
          return Gesture::None;
        }
        Gesture::Pinch {
          factor: new_distance / old_distance,
          center: at.midpoint(other),
        }
      },
      _ => Gesture::None,
    }
  }

  /// A pointer was released at `time_ms` (the event's time stamp).
  pub fn up(&mut self, id: i32, at: Point, time_ms: f64) -> Release {
    let was_pressed = self.active.iter().any(|(known, _)| *known == id);
    self.active.retain(|(known, _)| *known != id);
    if !was_pressed || !self.active.is_empty() {
      // The other finger of a pinch stays down; it must not page or tap when it lifts.
      self.maybe_press_start = None;
      return Release::None;
    }
    let Some(start) = self.maybe_press_start.take() else {
      return Release::None;
    };
    if !self.is_drag && at.distance(start) <= TAP_MAX_PX {
      return self.tap(at, time_ms);
    }
    self.maybe_last_tap = None;
    Release::None
  }

  /// The pointer left without a release (the browser took it over, the window lost focus).
  pub fn cancel(&mut self, id: i32) {
    self.active.retain(|(known, _)| *known != id);
    self.maybe_press_start = None;
  }

  fn tap(&mut self, at: Point, time_ms: f64) -> Release {
    match self.maybe_last_tap.take() {
      Some((last_ms, last_at))
        if time_ms - last_ms <= DOUBLE_TAP_MAX_MS && at.distance(last_at) <= DOUBLE_TAP_MAX_PX =>
      {
        Release::DoubleTap(at)
      },
      _ => {
        self.maybe_last_tap = Some((time_ms, at));
        Release::Tap(at)
      },
    }
  }
}

/// The images the lightbox pages through, oldest first (left to right). `files` comes newest first,
/// as [`crate::backend::ChatBackend::conversation_files`] returns it.
pub fn images_in_scope(files: &[FileInfo], scope: LightboxScope, start: MediaKey) -> Vec<FileInfo> {
  media_viewer::images_in_scope(files, scope, start.message, ViewerMedia::PicturesAndVideos)
}

/// How many images the message of `start` has, to choose the scope it opens with.
pub fn album_len(files: &[FileInfo], start: MediaKey) -> usize {
  media_viewer::album_len(files, start.message, ViewerMedia::PicturesAndVideos)
}

/// Where `current` is in `images`.
pub fn position(images: &[FileInfo], current: MediaKey) -> Option<Position> {
  media_viewer::position(images, &current.file_ref())
}

/// The image before or after `current`; `None` at either end (the list doesn't wrap around).
pub fn neighbor(images: &[FileInfo], current: MediaKey, direction: Direction) -> Option<MediaKey> {
  media_viewer::neighbor(images, &current.file_ref(), direction).and_then(MediaKey::of)
}

/// The thumbnails the strip renders: at most `max` around `index`, so a conversation with thousands
/// of images renders a handful of tiles.
pub fn strip_range(len: usize, index: usize, max: usize) -> Range<usize> {
  if len <= max {
    return 0..len;
  }
  let start = index.saturating_sub(max / 2).min(len - max);
  start..start + max
}

/// A pinch or ctrl+wheel reports small deltas, a mouse wheel large ones; both map to a smooth zoom.
pub fn wheel_zoom_factor(delta_y_px: f64, is_pinch: bool) -> f64 {
  let sensitivity = if is_pinch { 0.01 } else { 0.002 };
  (-delta_y_px * sensitivity).exp()
}

/// The size the stylesheet gives a picture at fit: `max-width` and `max-height` of `room`, keeping
/// its proportions, never larger than its own pixels. Computed, as the element itself is laid out at
/// its zoom while zoomed.
pub fn fitted_layout(room: Size, natural: Size) -> Size {
  let scale = (room.width / natural.width)
    .min(room.height / natural.height)
    .min(1.0);
  Size::new(natural.width * scale, natural.height * scale)
}

fn snap_to_device(css_px: f64, device_pixel_ratio: f64) -> f64 {
  (css_px * device_pixel_ratio).round() / device_pixel_ratio
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_client_api::actions::FileTransfer;
  use teamcraft_proto::ConversationId;

  const STAGE: Size = Size {
    width: 1000.0,
    height: 800.0,
  };
  const IMAGE: Size = Size {
    width: 800.0,
    height: 600.0,
  };

  mod navigation {
    use super::*;

    #[test]
    fn the_conversation_pages_oldest_to_newest_keeping_album_order() {
      let files = conversation();
      let images = images_in_scope(&files, LightboxScope::Conversation, key(1, 10));
      let names: Vec<&str> = images.iter().map(|file| file.file_name.as_str()).collect();
      assert_eq!(names, ["a.png", "b1.png", "b2.png", "b3.png", "c.png"]);
    }

    #[test]
    fn an_album_holds_only_the_images_of_its_message() {
      let files = conversation();
      let images = images_in_scope(&files, LightboxScope::Album, key(2, 21));
      let names: Vec<&str> = images.iter().map(|file| file.file_name.as_str()).collect();
      assert_eq!(
        names,
        ["b1.png", "b2.png", "b3.png"],
        "the text file is not an image"
      );
    }

    #[test]
    fn previous_and_next_stop_at_the_album_boundaries() {
      let files = conversation();
      let album = images_in_scope(&files, LightboxScope::Album, key(2, 21));
      assert_eq!(neighbor(&album, key(2, 21), Direction::Previous), None);
      assert_eq!(neighbor(&album, key(2, 21), Direction::Next), Some(key(2, 22)));
      assert_eq!(neighbor(&album, key(2, 23), Direction::Next), None);
      let first = position(&album, key(2, 21)).expect("in the album");
      assert_eq!(first, Position { index: 0, len: 3 });
      assert!(!first.has_previous() && first.has_next());
      let last = position(&album, key(2, 23)).expect("in the album");
      assert!(last.has_previous() && !last.has_next());
    }

    #[test]
    fn the_conversation_continues_past_the_album() {
      let files = conversation();
      let all = images_in_scope(&files, LightboxScope::Conversation, key(2, 23));
      assert_eq!(neighbor(&all, key(2, 23), Direction::Next), Some(key(3, 30)));
      assert_eq!(neighbor(&all, key(2, 21), Direction::Previous), Some(key(1, 10)));
      assert_eq!(
        neighbor(&all, key(3, 30), Direction::Next),
        None,
        "no wrap-around"
      );
    }

    #[test]
    fn an_image_that_is_gone_has_no_position_or_neighbor() {
      let files = conversation();
      let all = images_in_scope(&files, LightboxScope::Conversation, key(1, 10));
      assert_eq!(position(&all, key(9, 90)), None);
      assert_eq!(neighbor(&all, key(9, 90), Direction::Next), None);
    }

    #[test]
    fn the_scope_follows_the_number_of_images_in_the_message() {
      let files = conversation();
      assert_eq!(album_len(&files, key(2, 21)), 3);
      assert_eq!(
        LightboxScope::for_album_of(album_len(&files, key(2, 21))),
        LightboxScope::Album
      );
      assert_eq!(
        LightboxScope::for_album_of(album_len(&files, key(1, 10))),
        LightboxScope::Conversation
      );
    }

    #[test]
    fn uploads_and_other_files_have_no_key() {
      let mut uploading = image(4, 40, "d.png");
      uploading.file_id = "pending-0".to_owned();
      assert_eq!(MediaKey::of(&uploading), None);
      assert_eq!(MediaKey::of(&file(4, 41, "notes.txt", "text/plain")), None);
      assert_eq!(MediaKey::of(&image(4, 42, "e.png")), Some(key(4, 42)));
    }
  }

  mod zoom {
    use super::*;

    #[test]
    fn zoom_is_clamped_between_fit_and_the_maximum() {
      let mut viewport = Viewport::new(4.0);
      viewport.zoom_by(0.5, Point::ORIGIN, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 1.0);
      assert!(!viewport.is_zoomed());
      viewport.zoom_by(100.0, Point::ORIGIN, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 4.0);
      assert!(!viewport.can_zoom_in());
    }

    #[test]
    fn a_maximum_below_fit_is_raised_to_fit() {
      let mut viewport = Viewport::new(0.5);
      viewport.zoom_by(2.0, Point::ORIGIN, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 1.0);
    }

    #[test]
    fn invalid_factors_change_nothing() {
      let mut viewport = Viewport::default();
      for factor in [0.0, -2.0, f64::NAN, f64::INFINITY] {
        viewport.zoom_by(factor, Point::new(10.0, 10.0), IMAGE, STAGE);
      }
      assert_eq!(viewport, Viewport::default());
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
      let mut viewport = Viewport::default();
      let cursor = Point::new(200.0, 100.0);
      viewport.zoom_by(2.0, cursor, IMAGE, STAGE);
      // The image point under the cursor was (200, 100) at fit; it still is after zooming.
      let under_cursor = Point::new(
        (cursor.x - viewport.offset().x) / viewport.scale(),
        (cursor.y - viewport.offset().y) / viewport.scale(),
      );
      assert_eq!(under_cursor, cursor);
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
      let mut viewport = Viewport::default();
      viewport.pan_by(Point::new(500.0, 500.0), IMAGE, STAGE);
      assert_eq!(viewport.offset(), Point::ORIGIN, "a fitted image doesn't move");
      viewport.zoom_by(2.0, Point::ORIGIN, IMAGE, STAGE);
      viewport.pan_by(Point::new(5000.0, -5000.0), IMAGE, STAGE);
      // 1600×1200 shown in 1000×800: 300 px to spare on each side horizontally, 200 vertically.
      assert_eq!(viewport.offset(), Point::new(300.0, -200.0));
    }

    #[test]
    fn zooming_out_pulls_the_image_back_into_the_stage() {
      let mut viewport = Viewport::default();
      viewport.zoom_by(4.0, Point::new(400.0, 300.0), IMAGE, STAGE);
      viewport.zoom_by(0.25, Point::new(-400.0, -300.0), IMAGE, STAGE);
      assert_eq!(viewport.offset(), Point::ORIGIN);
    }

    #[test]
    fn a_pinch_past_the_limits_settles_back_inside() {
      let mut viewport = Viewport::new(4.0);
      // Pinched to 5× (past the maximum) around (100, 0), and panned past the edge.
      viewport.present(5.0, Point::new(2000.0, -50.0));
      let (scale, offset) = viewport.settled(Point::new(100.0, 0.0), IMAGE, STAGE);
      assert_eq!(scale, 4.0);
      // 3200×2400 in 1000×800: 1100 px to spare horizontally, 800 vertically.
      assert_eq!(viewport.pan_limits(4.0, IMAGE, STAGE), Point::new(1100.0, 800.0));
      assert_eq!(offset.x, 1100.0, "back at the edge");
      assert!(
        (offset.y + 40.0).abs() < 1e-9,
        "zoomed back around the anchor: {}",
        offset.y
      );
      viewport.present(0.7, Point::new(30.0, 0.0));
      assert_eq!(
        viewport.settled(Point::ORIGIN, IMAGE, STAGE),
        (1.0, Point::ORIGIN),
        "pinched below fit: back to fit"
      );
    }

    #[test]
    fn presenting_rejects_invalid_values() {
      let mut viewport = Viewport::default();
      viewport.present(f64::NAN, Point::ORIGIN);
      viewport.present(0.0, Point::ORIGIN);
      viewport.present(2.0, Point::new(f64::INFINITY, 0.0));
      assert_eq!(viewport, Viewport::default());
    }

    #[test]
    fn the_fitted_size_is_turned_with_the_view() {
      let mut viewport = Viewport::default();
      assert_eq!(viewport.fitted_size(IMAGE, STAGE), IMAGE);
      viewport.rotate_clockwise();
      assert_eq!(viewport.fitted_size(IMAGE, STAGE), Size::new(600.0, 800.0));
    }

    #[test]
    fn double_click_toggles_between_fit_and_zoomed() {
      let mut viewport = Viewport::default();
      toggle(&mut viewport, Point::ORIGIN);
      assert_eq!(viewport.scale(), DOUBLE_CLICK_SCALE);
      toggle(&mut viewport, Point::new(100.0, 100.0));
      assert_eq!(viewport.scale(), 1.0);
      assert_eq!(viewport.offset(), Point::ORIGIN);
    }

    fn toggle(viewport: &mut Viewport, anchor: Point) {
      let (scale, offset) = viewport.toggled(anchor, IMAGE, STAGE);
      viewport.present(scale, offset);
    }

    #[test]
    fn wheel_down_zooms_out_and_up_zooms_in() {
      assert!(wheel_zoom_factor(100.0, false) < 1.0);
      assert!(wheel_zoom_factor(-100.0, false) > 1.0);
      assert!(wheel_zoom_factor(-10.0, true) > wheel_zoom_factor(-10.0, false));
    }
  }

  mod own_pixels {
    use super::*;

    /// 2000×1400 own pixels, fitted to 1000×700 in a 1000×800 stage.
    const LARGE: Picture = Picture {
      natural: Size {
        width: 2000.0,
        height: 1400.0,
      },
      laid_out: Size {
        width: 1000.0,
        height: 700.0,
      },
      stage: STAGE,
      device_pixel_ratio: 1.0,
    };

    fn viewport_of(picture: Picture) -> Viewport {
      let mut viewport = Viewport::default();
      viewport.set_picture(Some(picture));
      viewport
    }

    #[test]
    fn the_fit_says_its_real_percentage() {
      assert_eq!(viewport_of(LARGE).own_pixel_ratio(), Some(0.5));
    }

    #[test]
    fn a_device_pixel_ratio_counts() {
      let picture = Picture {
        device_pixel_ratio: 2.0,
        ..LARGE
      };
      assert_eq!(viewport_of(picture).own_pixel_ratio(), Some(1.0));
      assert_eq!(viewport_of(picture).actual_size_scale(), Some(1.0));
    }

    #[test]
    fn a_small_picture_at_fit_is_at_its_own_size() {
      let picture = Picture {
        natural: IMAGE,
        laid_out: IMAGE,
        ..LARGE
      };
      assert_eq!(viewport_of(picture).own_pixel_ratio(), Some(1.0));
    }

    #[test]
    fn a_turn_that_shrinks_the_picture_counts() {
      // Turned, 1000×700 is 700 wide and 1000 tall in an 800 tall stage: shrunk to 0.8.
      let mut viewport = viewport_of(LARGE);
      viewport.rotate_clockwise();
      assert_eq!(viewport.own_pixel_ratio(), Some(0.4));
      assert_eq!(viewport.actual_size_scale(), Some(2.5));
    }

    #[test]
    fn nothing_is_said_before_the_picture_is_known() {
      assert_eq!(Viewport::default().own_pixel_ratio(), None);
      let unknown = Picture {
        natural: Size::default(),
        ..LARGE
      };
      assert_eq!(viewport_of(unknown).own_pixel_ratio(), None);
      assert_eq!(viewport_of(unknown).max_scale(), DEFAULT_MAX_SCALE);
    }

    #[test]
    fn double_click_on_a_large_picture_goes_to_its_own_pixels() {
      let viewport = viewport_of(LARGE);
      let (scale, offset) = viewport.toggled(Point::ORIGIN, LARGE.laid_out, STAGE);
      assert_eq!((scale, offset), (2.0, Point::ORIGIN));
    }

    #[test]
    fn double_click_where_own_pixels_are_no_zoom_in_zooms_as_before() {
      for natural in [IMAGE, Size::new(1000.0, 700.0), Size::new(1100.0, 770.0)] {
        let picture = Picture {
          natural,
          laid_out: fitted_layout(STAGE, natural),
          ..LARGE
        };
        let viewport = viewport_of(picture);
        let (scale, _) = viewport.toggled(Point::ORIGIN, picture.laid_out, STAGE);
        assert_eq!(scale, DOUBLE_CLICK_SCALE, "{natural:?}");
      }
    }

    #[test]
    fn a_picture_beyond_the_fit_relative_maximum_can_reach_its_own_pixels() {
      // 12000 own pixels on a 1000 wide fit: the maximum of 8 would stop at 67 %.
      let natural = Size::new(12_000.0, 8_400.0);
      let viewport = viewport_of(Picture { natural, ..LARGE });
      assert!(viewport.actual_size_scale().unwrap() < viewport.max_scale());
      assert_eq!(viewport.max_scale(), 32.0, "bounded by the laid-out side");
    }

    #[test]
    fn the_maximum_never_falls_below_the_shared_one() {
      assert_eq!(viewport_of(LARGE).max_scale(), DEFAULT_MAX_SCALE);
      let small = Picture {
        natural: Size::new(100.0, 70.0),
        laid_out: Size::new(100.0, 70.0),
        ..LARGE
      };
      assert_eq!(viewport_of(small).max_scale(), DEFAULT_MAX_SCALE);
    }

    #[test]
    fn a_new_picture_takes_back_a_zoom_past_its_maximum() {
      let mut viewport = viewport_of(Picture {
        natural: Size::new(12_000.0, 8_400.0),
        ..LARGE
      });
      viewport.present(30.0, Point::ORIGIN);
      viewport.set_picture(Some(LARGE));
      assert_eq!(viewport.scale(), DEFAULT_MAX_SCALE);
    }

    #[test]
    fn at_rest_the_element_has_its_real_size_and_no_scale() {
      let mut viewport = viewport_of(LARGE);
      viewport.present(2.0, Point::ORIGIN);
      assert_eq!(
        viewport.layout_at(Some(2.0)),
        PictureLayout {
          maybe_size: Some(Size::new(2000.0, 1400.0)),
          scale: 1.0,
        }
      );
    }

    #[test]
    fn a_gesture_scales_what_was_last_laid_out() {
      let mut viewport = viewport_of(LARGE);
      viewport.present(3.0, Point::ORIGIN);
      assert_eq!(
        viewport.layout_at(Some(2.0)),
        PictureLayout {
          maybe_size: Some(Size::new(2000.0, 1400.0)),
          scale: 1.5,
        }
      );
    }

    #[test]
    fn at_fit_the_stylesheet_lays_it_out() {
      assert_eq!(
        viewport_of(LARGE).layout_at(None),
        PictureLayout {
          maybe_size: None,
          scale: 1.0,
        }
      );
    }

    #[test]
    fn sizes_and_offsets_land_on_device_pixels() {
      let mut viewport = viewport_of(Picture {
        device_pixel_ratio: 2.0,
        ..LARGE
      });
      viewport.present(1.0, Point::new(10.3, -10.3));
      assert_eq!(viewport.snapped_offset(), Point::new(10.5, -10.5));
      viewport.present(1.0, Point::new(10.2, 0.0));
      assert_eq!(viewport.snapped_offset(), Point::new(10.0, 0.0));
      assert_eq!(
        viewport.layout_at(Some(1.0001)).maybe_size,
        Some(Size::new(1000.0, 700.0))
      );
    }

    #[test]
    fn the_fit_is_what_the_stylesheet_does() {
      let room = Size::new(1272.0, 900.0);
      assert_eq!(
        fitted_layout(room, Size::new(636.0, 450.0)),
        Size::new(636.0, 450.0)
      );
      assert_eq!(
        fitted_layout(room, Size::new(2544.0, 900.0)),
        Size::new(1272.0, 450.0)
      );
      assert_eq!(
        fitted_layout(room, Size::new(900.0, 1800.0)),
        Size::new(450.0, 900.0)
      );
    }
  }

  mod arrival {
    use super::*;

    #[test]
    fn the_placeholder_stays_until_the_full_picture_of_the_same_file_is_shown() {
      let mut arrival = Arrival::start(key(1, 10));
      assert!(!arrival.is_full_shown(key(1, 10)));
      arrival.full_shown(key(1, 10));
      assert!(arrival.is_full_shown(key(1, 10)));
      assert!(
        !arrival.is_full_shown(key(1, 11)),
        "another file is not shown by it"
      );
    }

    #[test]
    fn a_late_load_of_the_previous_file_does_not_hide_the_new_placeholder() {
      // Paged from 10 to 11; 10's image fires its `load` afterwards.
      let mut arrival = Arrival::start(key(1, 11));
      arrival.full_shown(key(1, 10));
      assert!(!arrival.is_full_shown(key(1, 11)));
      assert_eq!(arrival.key(), Some(key(1, 11)));
    }

    #[test]
    fn paging_back_to_a_file_brings_its_placeholder_back_until_it_loads_again() {
      let mut arrival = Arrival::start(key(1, 10));
      arrival.full_shown(key(1, 10));
      assert!(arrival.is_full_shown(key(1, 10)));
      arrival = Arrival::start(key(1, 10));
      assert!(!arrival.is_full_shown(key(1, 10)), "its new image loads again");
    }

    #[test]
    fn nothing_is_shown_before_a_file_starts() {
      let mut arrival = Arrival::default();
      arrival.full_shown(key(1, 10));
      assert!(!arrival.is_full_shown(key(1, 10)));
      assert_eq!(arrival.key(), None);
    }
  }

  mod media {
    use super::*;

    #[test]
    fn images_gifs_and_videos_open_and_other_files_do_not() {
      assert_eq!(MediaKind::of("image/png"), Some(MediaKind::Image));
      assert_eq!(MediaKind::of("image/svg+xml"), Some(MediaKind::Image));
      assert_eq!(MediaKind::of("image/gif"), Some(MediaKind::Animated));
      assert_eq!(MediaKind::of(" Image/APNG "), Some(MediaKind::Animated));
      assert_eq!(MediaKind::of("video/mp4"), Some(MediaKind::Video));
      assert_eq!(MediaKind::of("video/webm"), Some(MediaKind::Video));
      for other in ["", "audio/ogg", "text/html", "application/pdf", "imagex/png"] {
        assert_eq!(MediaKind::of(other), None, "{other:?}");
      }
      assert_eq!(
        MediaKey::of(&file(5, 50, "clip.mp4", "video/mp4")),
        Some(key(5, 50))
      );
    }

    #[test]
    fn only_animated_images_start_paused_and_only_on_request() {
      assert!(MediaKind::Animated.starts_paused(AnimatedImages::OnTap));
      assert!(!MediaKind::Animated.starts_paused(AnimatedImages::OnHover));
      assert!(!MediaKind::Animated.starts_paused(AnimatedImages::Autoplay));
      assert!(!MediaKind::Image.starts_paused(AnimatedImages::OnTap));
      assert!(
        !MediaKind::Video.starts_paused(AnimatedImages::OnTap),
        "videos never play by themselves anyway"
      );
    }

    #[test]
    fn videos_keep_their_controls_instead_of_zoom_and_rotation() {
      assert!(MediaKind::Image.can_transform());
      assert!(MediaKind::Animated.can_transform());
      assert!(!MediaKind::Video.can_transform());
    }

    #[test]
    fn a_video_in_an_album_pages_like_an_image() {
      let mut files = conversation();
      files.insert(2, file(2, 25, "clip.webm", "video/webm"));
      let album = images_in_scope(&files, LightboxScope::Album, key(2, 21));
      let names: Vec<&str> = album.iter().map(|file| file.file_name.as_str()).collect();
      assert_eq!(names, ["b1.png", "clip.webm", "b2.png", "b3.png"]);
    }
  }

  mod rotation {
    use super::*;

    #[test]
    fn four_quarter_turns_come_back_upright() {
      let mut rotation = Rotation::default();
      let mut seen = Vec::new();
      for _ in 0..4 {
        rotation = rotation.clockwise();
        seen.push(rotation.degrees());
      }
      assert_eq!(seen, [90, 180, 270, 0]);
      assert_eq!(rotation, Rotation::Upright);
      assert!(Rotation::Clockwise.is_sideways() && !Rotation::UpsideDown.is_sideways());
    }

    #[test]
    fn rotating_fits_the_image_again() {
      let mut viewport = Viewport::default();
      viewport.zoom_by(3.0, Point::new(100.0, 50.0), IMAGE, STAGE);
      viewport.rotate_clockwise();
      assert_eq!(viewport.rotation(), Rotation::Clockwise);
      assert_eq!((viewport.scale(), viewport.offset()), (1.0, Point::ORIGIN));
      viewport.reset();
      assert_eq!(
        viewport.rotation(),
        Rotation::Clockwise,
        "fit to window keeps the turn"
      );
    }

    #[test]
    fn a_sideways_image_shrinks_to_fit_the_stage() {
      // 1000×500 fits a 1000×800 stage upright; turned it is 500 wide and 1000 tall, so it shrinks
      // to 800 / 1000.
      let wide = Size::new(1000.0, 500.0);
      let mut viewport = Viewport::default();
      assert_eq!(viewport.display_scale(wide, STAGE), 1.0);
      viewport.rotate_clockwise();
      assert_eq!(viewport.display_scale(wide, STAGE), 0.8);
      viewport.rotate_clockwise();
      assert_eq!(
        viewport.display_scale(wide, STAGE),
        1.0,
        "upside down fits as it is"
      );
      assert_eq!(
        Viewport::default().display_scale(Size::default(), STAGE),
        1.0,
        "not laid out yet"
      );
    }

    #[test]
    fn panning_a_sideways_image_uses_its_turned_size() {
      let mut viewport = Viewport::default();
      viewport.rotate_clockwise();
      viewport.zoom_by(2.0, Point::ORIGIN, IMAGE, STAGE);
      viewport.pan_by(Point::new(5000.0, 5000.0), IMAGE, STAGE);
      // 800×600 turned is 600×800, fits as it is; at 2× it is 1200×1600 in 1000×800.
      assert_eq!(viewport.offset(), Point::new(100.0, 400.0));
    }
  }

  mod slider {
    use super::*;

    #[test]
    fn the_slider_runs_from_fit_to_the_maximum() {
      let mut viewport = Viewport::new(4.0);
      assert_eq!(viewport.slider_position(), 0.0);
      viewport.zoom_to_slider(1.0, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 4.0);
      assert_eq!(viewport.slider_position(), 1.0);
      viewport.zoom_to_slider(0.5, IMAGE, STAGE);
      assert!(
        (viewport.scale() - 2.0).abs() < 1e-9,
        "logarithmic: halfway is 2× of 4×"
      );
      viewport.zoom_to_slider(0.0, IMAGE, STAGE);
      assert!(!viewport.is_zoomed());
    }

    #[test]
    fn out_of_range_slider_values_are_clamped_or_ignored() {
      let mut viewport = Viewport::new(4.0);
      viewport.zoom_to_slider(7.0, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 4.0);
      viewport.zoom_to_slider(f64::NAN, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 4.0);
      viewport.zoom_to_slider(-1.0, IMAGE, STAGE);
      assert_eq!(viewport.scale(), 1.0);
      assert_eq!(Viewport::new(1.0).slider_position(), 0.0, "no zoom range");
    }
  }

  mod keys {
    use super::*;

    #[test]
    fn plain_keys_page_zoom_rotate_and_toggle() {
      let cases = [
        ("ArrowLeft", Command::Previous),
        ("ArrowRight", Command::Next),
        ("+", Command::ZoomIn),
        ("=", Command::ZoomIn),
        ("-", Command::ZoomOut),
        ("0", Command::ZoomReset),
        ("r", Command::Rotate),
        ("R", Command::Rotate),
        ("t", Command::ToggleThread),
        ("k", Command::TogglePlay),
        ("j", Command::JumpToMessage),
      ];
      for (key, command) in cases {
        assert_eq!(Command::for_key(key, false, false), Some(command), "{key}");
      }
      assert_eq!(Command::for_key("x", false, false), None);
      assert_eq!(
        Command::for_key("Escape", false, false),
        None,
        "the dialog closes itself"
      );
    }

    #[test]
    fn modifiers_save_and_copy_and_alt_is_left_alone() {
      assert_eq!(Command::for_key("s", true, false), Some(Command::Download));
      assert_eq!(Command::for_key("c", true, false), Some(Command::CopyImage));
      assert_eq!(
        Command::for_key("r", true, false),
        None,
        "Ctrl+R reloads, as always"
      );
      assert_eq!(Command::for_key("ArrowLeft", true, false), None);
      assert_eq!(Command::for_key("r", false, true), None);
    }

    #[test]
    fn tooltips_name_the_platform_modifier() {
      let mac = ModifierStyle::for_platform("MacIntel");
      let other = ModifierStyle::for_platform("Linux x86_64");
      assert_eq!(mac, ModifierStyle::Command);
      assert_eq!(ModifierStyle::for_platform("iPhone"), ModifierStyle::Command);
      assert_eq!(other, ModifierStyle::Ctrl);
      assert_eq!(Command::Download.shortcut_label(mac), "⌘S");
      assert_eq!(Command::Download.shortcut_label(other), "Ctrl+S");
      assert_eq!(Command::CopyImage.aria_shortcut(other), "Control+C");
      assert_eq!(Command::Rotate.shortcut_label(other), "R");
    }

    #[test]
    fn every_shortcut_label_is_a_key_that_triggers_it() {
      let all = [
        Command::Previous,
        Command::Next,
        Command::ZoomIn,
        Command::ZoomReset,
        Command::Rotate,
        Command::ToggleThread,
        Command::TogglePlay,
        Command::JumpToMessage,
      ];
      for command in all {
        let key = match command.aria_shortcut(ModifierStyle::Ctrl) {
          "R" | "T" | "K" | "J" => command.aria_shortcut(ModifierStyle::Ctrl).to_lowercase(),
          other => other.to_owned(),
        };
        assert_eq!(Command::for_key(&key, false, false), Some(command), "{command:?}");
      }
      assert_eq!(Command::for_key("-", false, false), Some(Command::ZoomOut));
    }
  }

  mod strip {
    use super::*;

    #[test]
    fn a_short_list_shows_every_thumbnail() {
      assert_eq!(strip_range(5, 3, 15), 0..5);
      assert_eq!(strip_range(0, 0, 15), 0..0);
    }

    #[test]
    fn a_long_list_shows_a_window_around_the_current_image() {
      assert_eq!(strip_range(1000, 500, 15), 493..508);
      assert_eq!(strip_range(1000, 2, 15), 0..15, "at the start");
      assert_eq!(strip_range(1000, 999, 15), 985..1000, "at the end");
      assert_eq!(strip_range(1000, 5000, 15), 985..1000, "an index past the end");
    }
  }

  mod gestures {
    use super::*;

    #[test]
    fn one_pointer_pans_and_two_pinch() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(0.0, 0.0));
      assert_eq!(
        pointers.moved(1, Point::new(5.0, -3.0)),
        Gesture::Pan(Point::new(5.0, -3.0))
      );
      pointers.down(2, Point::new(105.0, -3.0));
      let Gesture::Pinch { factor, center } = pointers.moved(2, Point::new(205.0, -3.0)) else {
        panic!("two pointers pinch");
      };
      assert_eq!(factor, 2.0);
      assert_eq!(center, Point::new(105.0, -3.0));
    }

    #[test]
    fn a_drag_is_no_tap_even_when_it_comes_back() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(300.0, 0.0));
      assert_eq!(pointers.press_start(), Some(Point::new(300.0, 0.0)));
      pointers.moved(1, Point::new(100.0, 10.0));
      assert_eq!(pointers.up(1, Point::new(300.0, 0.0), 0.0), Release::None);
      pointers.down(1, Point::new(0.0, 0.0));
      pointers.moved(1, Point::new(4.0, 3.0));
      assert_eq!(
        pointers.up(1, Point::new(4.0, 3.0), 1000.0),
        Release::Tap(Point::new(4.0, 3.0)),
        "a wobble is still a tap"
      );
    }

    #[test]
    fn two_quick_taps_are_a_double_tap() {
      let mut pointers = Pointers::default();
      let at = Point::new(10.0, 10.0);
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 0.0), Release::Tap(at));
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 200.0), Release::DoubleTap(at));
      pointers.down(1, at);
      assert_eq!(
        pointers.up(1, at, 2000.0),
        Release::Tap(at),
        "a third tap starts over"
      );
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 2500.0), Release::Tap(at), "too slow");
    }

    #[test]
    fn lifting_the_fingers_of_a_pinch_neither_taps_nor_swipes() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(0.0, 0.0));
      pointers.down(2, Point::new(100.0, 0.0));
      assert_eq!(pointers.up(2, Point::new(300.0, 0.0), 0.0), Release::None);
      assert_eq!(pointers.up(1, Point::new(0.0, 0.0), 10.0), Release::None);
    }

    #[test]
    fn unknown_or_cancelled_pointers_are_ignored() {
      let mut pointers = Pointers::default();
      assert_eq!(pointers.moved(7, Point::ORIGIN), Gesture::None);
      assert_eq!(pointers.up(7, Point::ORIGIN, 0.0), Release::None);
      pointers.down(1, Point::ORIGIN);
      pointers.cancel(1);
      assert_eq!(pointers.up(1, Point::ORIGIN, 0.0), Release::None);
    }
  }

  /// Newest first, like the backend: message 3 (c), message 2 (b1, notes, b2, b3), message 1 (a).
  fn conversation() -> Vec<FileInfo> {
    vec![
      image(3, 30, "c.png"),
      image(2, 21, "b1.png"),
      file(2, 24, "notes.txt", "text/plain"),
      image(2, 22, "b2.png"),
      image(2, 23, "b3.png"),
      image(1, 10, "a.png"),
    ]
  }

  fn key(message: i64, attachment: i64) -> MediaKey {
    MediaKey {
      message: id(message),
      attachment: attachment.to_string().parse().expect("valid id"),
    }
  }

  fn image(message: i64, attachment: i64, name: &str) -> FileInfo {
    file(message, attachment, name, "image/png")
  }

  fn file(message: i64, attachment: i64, name: &str, media_type: &str) -> FileInfo {
    FileInfo {
      conversation: "1".parse::<ConversationId>().expect("valid id"),
      message: id(message),
      file_id: attachment.to_string(),
      file_name: name.to_owned(),
      media_type: media_type.to_owned(),
      size_bytes: 1,
      maybe_thumbnail_png: None,
      transfer: FileTransfer::Remote,
      maybe_sender: None,
      maybe_sent_at: None,
      maybe_imported_author: None,
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_content_sha256: None,
    }
  }

  fn id(message: i64) -> MessageId {
    message.to_string().parse().expect("valid id")
  }
}
