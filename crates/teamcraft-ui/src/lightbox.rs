//! State of the media lightbox (images, animated images and videos), kept apart from the view so it
//! can be tested without a browser: which files it pages through (one message's album or the whole
//! conversation), where the current one is, how far it is zoomed, panned and rotated, which
//! thumbnails the strip shows, and which key does what.
//!
//! Positions are in CSS pixels relative to the centre of the stage (the area the image is shown in).
//! The image is first fitted into the stage ("fit", scale 1) and then rotated, scaled around its
//! centre and moved by an offset, which is what the view writes into the image's `transform`.

use std::ops::Range;

use teamcraft_client_api::actions::FileInfo;
use teamcraft_proto::AttachmentId;

use crate::model::MessageId;

/// Largest zoom, relative to the fitted size.
pub const DEFAULT_MAX_SCALE: f64 = 8.0;
/// Zoom of a double-click or double-tap on a fitted image.
pub const DOUBLE_CLICK_SCALE: f64 = 2.5;
/// Zoom step of the `+` and `-` keys and buttons.
pub const KEY_ZOOM_STEP: f64 = 1.25;
/// Horizontal travel of a finger on a fitted image that pages to the next or previous image.
pub const SWIPE_MIN_PX: f64 = 60.0;
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
  /// user asked the system for reduced motion).
  Animated,
  /// Played by the browser's own `<video>` with its controls.
  Video,
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

/// Which images previous/next page through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LightboxScope {
  /// Only the images of the message the lightbox was opened from.
  Album,
  /// Every image of the conversation the client knows.
  #[default]
  Conversation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
  Previous,
  Next,
}

/// Where the current image is in the list, for "3 / 12" and the arrows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
  pub index: usize,
  pub len: usize,
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
}

/// Follows the pointers on the stage and turns them into pans, pinches, swipes and taps.
#[derive(Clone, Debug, Default)]
pub struct Pointers {
  /// Pointers currently pressed, by pointer id.
  active: Vec<(i32, Point)>,
  /// Where the single pointer of the current press went down; `None` once a second one joined.
  maybe_press_start: Option<Point>,
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

/// What releasing a pointer means.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Release {
  /// Still pressed by another pointer, or a drag that panned.
  None,
  Tap(Point),
  DoubleTap(Point),
  Swipe(Direction),
}

impl MediaKey {
  /// The key of a file that the lightbox can show: a received image or video with an attachment id
  /// (files still uploading have none yet).
  pub fn of(file: &FileInfo) -> Option<Self> {
    MediaKind::of(&file.media_type)?;
    let attachment = file.file_id.parse().ok()?;
    Some(Self {
      message: file.message,
      attachment,
    })
  }

  pub fn matches(&self, file: &FileInfo) -> bool {
    file.message == self.message && file.file_id == self.attachment.to_string()
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

  /// An animated image starts paused when the user prefers reduced motion; a video never plays by
  /// itself.
  pub fn starts_paused(self, prefers_reduced_motion: bool) -> bool {
    match self {
      Self::Animated => prefers_reduced_motion,
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

impl LightboxScope {
  /// A message with several images opens as an album; a single image pages through the conversation.
  pub fn for_album_of(images_in_message: usize) -> Self {
    match images_in_message {
      0 | 1 => Self::Conversation,
      _ => Self::Album,
    }
  }
}

impl Position {
  pub fn has_previous(self) -> bool {
    self.index > 0
  }

  pub fn has_next(self) -> bool {
    self.index + 1 < self.len
  }
}

impl Point {
  pub const ORIGIN: Self = Self { x: 0.0, y: 0.0 };

  pub fn new(x: f64, y: f64) -> Self {
    Self { x, y }
  }

  fn minus(self, other: Self) -> Self {
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

  /// The zoom slider's position for the current zoom, from 0 (fit) to 1 (the maximum). The slider
  /// is logarithmic, so every step zooms by the same factor.
  pub fn slider_position(&self) -> f64 {
    match self.max_scale > MIN_SCALE {
      true => (self.scale.ln() / self.max_scale.ln()).clamp(0.0, 1.0),
      false => 0.0,
    }
  }

  /// Zooms to the slider's `position` (0 to 1) around the stage centre.
  pub fn zoom_to_slider(&mut self, position: f64, image: Size, stage: Size) {
    if !position.is_finite() {
      return;
    }
    let target = self.max_scale.powf(position.clamp(0.0, 1.0));
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
    self.scale < self.max_scale
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
    let scale = (self.scale * factor).clamp(MIN_SCALE, self.max_scale);
    let applied = scale / self.scale;
    self.offset = Point::new(
      anchor.x - (anchor.x - self.offset.x) * applied,
      anchor.y - (anchor.y - self.offset.y) * applied,
    );
    self.scale = scale;
    self.clamp_offset(image, stage);
  }

  /// Double-click or double-tap: a fitted image zooms in at `anchor`, a zoomed one goes back to fit.
  pub fn toggle_zoom(&mut self, anchor: Point, image: Size, stage: Size) {
    if self.is_zoomed() {
      self.reset();
    } else {
      self.zoom_by(DOUBLE_CLICK_SCALE, anchor, image, stage);
    }
  }

  /// Moves a zoomed image; it can't be dragged out of the stage.
  pub fn pan_by(&mut self, delta: Point, image: Size, stage: Size) {
    self.offset = Point::new(self.offset.x + delta.x, self.offset.y + delta.y);
    self.clamp_offset(image, stage);
  }

  /// The image's edges never come inside the stage's edges; a side smaller than the stage stays
  /// centred.
  fn clamp_offset(&mut self, image: Size, stage: Size) {
    let shown = self.rotation.orient(image);
    let scale = self.display_scale(image, stage);
    let limit = |image_side: f64, stage_side: f64| ((image_side * scale - stage_side) / 2.0).max(0.0);
    let (limit_x, limit_y) = (limit(shown.width, stage.width), limit(shown.height, stage.height));
    self.offset = Point::new(
      self.offset.x.clamp(-limit_x, limit_x),
      self.offset.y.clamp(-limit_y, limit_y),
    );
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
  }

  /// A pressed pointer moved: one pointer pans, two pinch.
  pub fn moved(&mut self, id: i32, at: Point) -> Gesture {
    let Some(slot) = self.active.iter().position(|(known, _)| *known == id) else {
      return Gesture::None;
    };
    let before = self.active[slot].1;
    self.active[slot].1 = at;
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

  /// A pointer was released at `time_ms` (the event's time stamp). `is_zoomed` turns horizontal
  /// drags into pans instead of swipes.
  pub fn up(&mut self, id: i32, at: Point, time_ms: f64, is_zoomed: bool) -> Release {
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
    let travel = at.minus(start);
    if travel.x.hypot(travel.y) <= TAP_MAX_PX {
      return self.tap(at, time_ms);
    }
    self.maybe_last_tap = None;
    let is_horizontal = travel.x.abs() > 2.0 * travel.y.abs();
    match (is_zoomed, is_horizontal, travel.x) {
      (false, true, dx) if dx <= -SWIPE_MIN_PX => Release::Swipe(Direction::Next),
      (false, true, dx) if dx >= SWIPE_MIN_PX => Release::Swipe(Direction::Previous),
      _ => Release::None,
    }
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
  let mut images: Vec<FileInfo> = files
    .iter()
    .filter(|file| MediaKey::of(file).is_some())
    .filter(|file| scope == LightboxScope::Conversation || file.message == start.message)
    .cloned()
    .collect();
  oldest_first(&mut images);
  images
}

/// How many images the message of `start` has, to choose the scope it opens with.
pub fn album_len(files: &[FileInfo], start: MediaKey) -> usize {
  files
    .iter()
    .filter(|file| file.message == start.message && MediaKey::of(file).is_some())
    .count()
}

/// Where `current` is in `images`.
pub fn position(images: &[FileInfo], current: MediaKey) -> Option<Position> {
  images
    .iter()
    .position(|file| current.matches(file))
    .map(|index| Position {
      index,
      len: images.len(),
    })
}

/// The image before or after `current`; `None` at either end (the list doesn't wrap around).
pub fn neighbor(images: &[FileInfo], current: MediaKey, direction: Direction) -> Option<MediaKey> {
  let at = position(images, current)?;
  let index = match direction {
    Direction::Previous => at.index.checked_sub(1)?,
    Direction::Next => at.index + 1,
  };
  images.get(index).and_then(MediaKey::of)
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

/// Turns the newest-first list into oldest-first while keeping the files of one message in the order
/// they were attached.
fn oldest_first(files: &mut [FileInfo]) {
  files.reverse();
  let mut start = 0;
  while start < files.len() {
    let message = files[start].message;
    let len = files[start..]
      .iter()
      .take_while(|file| file.message == message)
      .count();
    files[start..start + len].reverse();
    start += len;
  }
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
    fn double_click_toggles_between_fit_and_zoomed() {
      let mut viewport = Viewport::default();
      viewport.toggle_zoom(Point::ORIGIN, IMAGE, STAGE);
      assert_eq!(viewport.scale(), DOUBLE_CLICK_SCALE);
      viewport.toggle_zoom(Point::new(100.0, 100.0), IMAGE, STAGE);
      assert_eq!(viewport, Viewport::default());
    }

    #[test]
    fn wheel_down_zooms_out_and_up_zooms_in() {
      assert!(wheel_zoom_factor(100.0, false) < 1.0);
      assert!(wheel_zoom_factor(-100.0, false) > 1.0);
      assert!(wheel_zoom_factor(-10.0, true) > wheel_zoom_factor(-10.0, false));
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
    fn only_animated_images_pause_for_reduced_motion() {
      assert!(MediaKind::Animated.starts_paused(true));
      assert!(!MediaKind::Animated.starts_paused(false));
      assert!(!MediaKind::Image.starts_paused(true));
      assert!(
        !MediaKind::Video.starts_paused(true),
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
    fn a_fitted_horizontal_drag_swipes_and_a_zoomed_one_does_not() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(300.0, 0.0));
      assert_eq!(
        pointers.up(1, Point::new(100.0, 10.0), 0.0, false),
        Release::Swipe(Direction::Next)
      );
      pointers.down(1, Point::new(100.0, 0.0));
      assert_eq!(
        pointers.up(1, Point::new(300.0, 0.0), 1000.0, false),
        Release::Swipe(Direction::Previous)
      );
      pointers.down(1, Point::new(300.0, 0.0));
      assert_eq!(
        pointers.up(1, Point::new(100.0, 0.0), 2000.0, true),
        Release::None
      );
    }

    #[test]
    fn short_or_vertical_drags_do_not_swipe() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(0.0, 0.0));
      assert_eq!(pointers.up(1, Point::new(-40.0, 0.0), 0.0, false), Release::None);
      pointers.down(1, Point::new(0.0, 0.0));
      assert_eq!(
        pointers.up(1, Point::new(-80.0, 200.0), 500.0, false),
        Release::None
      );
    }

    #[test]
    fn two_quick_taps_are_a_double_tap() {
      let mut pointers = Pointers::default();
      let at = Point::new(10.0, 10.0);
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 0.0, false), Release::Tap(at));
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 200.0, false), Release::DoubleTap(at));
      pointers.down(1, at);
      assert_eq!(
        pointers.up(1, at, 2000.0, false),
        Release::Tap(at),
        "a third tap starts over"
      );
      pointers.down(1, at);
      assert_eq!(pointers.up(1, at, 2500.0, false), Release::Tap(at), "too slow");
    }

    #[test]
    fn lifting_the_fingers_of_a_pinch_neither_taps_nor_swipes() {
      let mut pointers = Pointers::default();
      pointers.down(1, Point::new(0.0, 0.0));
      pointers.down(2, Point::new(100.0, 0.0));
      assert_eq!(pointers.up(2, Point::new(300.0, 0.0), 0.0, false), Release::None);
      assert_eq!(pointers.up(1, Point::new(0.0, 0.0), 10.0, false), Release::None);
    }

    #[test]
    fn unknown_or_cancelled_pointers_are_ignored() {
      let mut pointers = Pointers::default();
      assert_eq!(pointers.moved(7, Point::ORIGIN), Gesture::None);
      assert_eq!(pointers.up(7, Point::ORIGIN, 0.0, false), Release::None);
      pointers.down(1, Point::ORIGIN);
      pointers.cancel(1);
      assert_eq!(pointers.up(1, Point::ORIGIN, 0.0, false), Release::None);
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
      maybe_dimensions: None,
    }
  }

  fn id(message: i64) -> MessageId {
    message.to_string().parse().expect("valid id")
  }
}
