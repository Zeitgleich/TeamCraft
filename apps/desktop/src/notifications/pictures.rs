//! The picture beside a notification's text (#543): the sender's avatar, or the channel's or space's
//! picture. The webview names it by its key in the persistent media cache (`core::media`), where
//! the app keeps every picture it showed; the shell reads it from there, never from the network,
//! cuts it to its shape at [`NOTIFICATION_PICTURE_PX`] and keeps the result for the next
//! notification of the same person or place. Without a cached picture it shows the tile the
//! webview drew (initials in the app's colours).
//!
//! | Platform | Handed over as                                    | Files                             |
//! |----------|---------------------------------------------------|-----------------------------------|
//! | Linux    | `image-data` hint: the pixels over D-Bus          | none                              |
//! | Windows  | toast `appLogoOverride`, circle crop for people   | one PNG per picture, while kept   |
//! | macOS    | `UNNotificationAttachment`                        | a copy per notification, moved    |
//!
//! The files are plain PNGs in the app's cache directory, since the toast service and the
//! notification center read them from other processes: small copies of avatars and space pictures,
//! which are directory data, never message content. They are deleted at sign-out and at start.

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits, RgbaImage};
use teamcraft_client_api::media::{MAX_MEDIA_MAX_AGE, MediaKey};
use teamcraft_client_api::os_notify::{NOTIFICATION_PICTURE_PX, NotificationPicture, PictureShape};
use teamcraft_core::media_cache::FileMediaCache;

/// Pictures kept prepared: the people and places of the last notifications (64 KiB each).
const MAX_PREPARED: usize = 32;
/// Largest side of a picture that is decoded; avatars are 512 px, space pictures 256 px.
const MAX_SOURCE_SIDE: u32 = 4096;
/// Memory the decoder may allocate.
const MAX_DECODER_BYTES: u64 = 64 * 1024 * 1024;
/// Folder of the picture files in the app's cache directory.
pub const PICTURES_DIR: &str = "notification-pictures";

/// See the module docs. Managed state of the shell.
#[derive(Default)]
pub struct NotificationPictures {
  /// Newest last.
  prepared: Mutex<VecDeque<(Source, Arc<PreparedPicture>)>>,
  /// Where picture files go; unset (no files) until the app's cache directory is known.
  dir: OnceLock<PathBuf>,
  /// Windows and macOS: picture files; Linux sends the pixels in the notification itself.
  #[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
  next_file: AtomicU64,
}

/// A picture ready for the notification services: square, cut to its shape, with transparency.
pub struct PreparedPicture {
  image: RgbaImage,
  /// Windows crops a person's logo itself.
  #[cfg_attr(not(any(windows, test)), allow(dead_code))]
  shape: PictureShape,
  /// Windows: the file written for it, deleted when the picture is no longer kept.
  maybe_file: Mutex<Option<PathBuf>>,
}

/// Where a prepared picture came from; a newer version of a cached picture is another source.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
  Cached {
    key: MediaKey,
    version: String,
    shape: PictureShape,
  },
  /// The tile the webview drew, by its PNG.
  Generated { png: Vec<u8>, shape: PictureShape },
}

/// Why a picture is not shown.
#[derive(Debug, thiserror::Error)]
pub enum PictureError {
  #[error("unreadable picture: {0}")]
  Unreadable(#[from] image::ImageError),
  #[error("the picture has no pixels")]
  Empty,
  #[error("could not write the picture: {0}")]
  Io(#[from] std::io::Error),
}

impl NotificationPictures {
  /// Picture files go to `dir` from now on; what an earlier run left there is deleted.
  pub fn use_dir(&self, dir: PathBuf) {
    remove_dir(&dir);
    if self.dir.set(dir).is_err() {
      tracing::debug!("the folder of notification pictures was already set");
    }
  }

  /// The picture to show for `picture`: the cached one where `maybe_cache` has it, else the
  /// generated tile, else none. Failures only cost the picture.
  pub async fn resolve(
    &self,
    maybe_cache: Option<Arc<FileMediaCache>>,
    picture: &NotificationPicture,
  ) -> Option<Arc<PreparedPicture>> {
    if let (Some(cache), Some(key)) = (maybe_cache, picture.maybe_key)
      && let Some(prepared) = self.cached(cache, key, picture.shape).await
    {
      return Some(prepared);
    }
    let png = picture.maybe_fallback_png.as_ref()?.as_slice().to_vec();
    let source = Source::Generated {
      png: png.clone(),
      shape: picture.shape,
    };
    if let Some(known) = self.find(&source) {
      return Some(known);
    }
    // The webview drew it in its shape already.
    let shape = picture.shape;
    let prepared = blocking(move || prepare(&png, shape, Cut::AsDrawn))
      .await
      .or_logged()?;
    Some(self.keep(source, prepared))
  }

  /// Sign-out: forgets every picture and deletes the files.
  pub fn forget_all(&self) {
    self
      .prepared
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();
    if let Some(dir) = self.dir.get() {
      remove_dir(dir);
    }
  }

  /// Windows: the file of `picture` for a toast, written once and kept while the picture is kept
  /// (a toast in the Action Center shows it again later).
  #[cfg(any(windows, test))]
  pub fn toast_file(&self, picture: &PreparedPicture) -> Option<PathBuf> {
    let mut maybe_file = picture.maybe_file.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(file) = maybe_file.as_ref() {
      return Some(file.clone());
    }
    let file = self.write_file(picture)?;
    *maybe_file = Some(file.clone());
    Some(file)
  }

  /// macOS: a new file of `picture` for one notification; the system moves it into its own store.
  #[cfg(any(target_os = "macos", test))]
  pub fn attachment_file(&self, picture: &PreparedPicture) -> Option<PathBuf> {
    self.write_file(picture)
  }

  async fn cached(
    &self,
    cache: Arc<FileMediaCache>,
    key: MediaKey,
    shape: PictureShape,
  ) -> Option<Arc<PreparedPicture>> {
    // The longest age the settings offer: the webview's own reads drop what the user's setting
    // doesn't keep, and a read here never deletes what it keeps.
    let media = blocking(move || cache.get(&key, MAX_MEDIA_MAX_AGE)).await??;
    let source = Source::Cached {
      key,
      version: media.version.clone(),
      shape,
    };
    if let Some(known) = self.find(&source) {
      return Some(known);
    }
    let prepared = blocking(move || prepare(media.bytes.expose_secret(), shape, Cut::ToShape))
      .await
      .or_logged()?;
    Some(self.keep(source, prepared))
  }

  fn find(&self, source: &Source) -> Option<Arc<PreparedPicture>> {
    let mut prepared = self.prepared.lock().unwrap_or_else(PoisonError::into_inner);
    let index = prepared.iter().position(|(known, _)| known == source)?;
    // Used again: the newest now.
    let entry = prepared.remove(index)?;
    let picture = entry.1.clone();
    prepared.push_back(entry);
    Some(picture)
  }

  /// Keeps `picture` as the newest; the oldest beyond the limit go, with their files.
  fn keep(&self, source: Source, picture: PreparedPicture) -> Arc<PreparedPicture> {
    let picture = Arc::new(picture);
    let gone: Vec<Arc<PreparedPicture>> = {
      let mut prepared = self.prepared.lock().unwrap_or_else(PoisonError::into_inner);
      prepared.retain(|(known, _)| *known != source);
      prepared.push_back((source, picture.clone()));
      let excess = prepared.len().saturating_sub(MAX_PREPARED);
      prepared.drain(..excess).map(|(_, gone)| gone).collect()
    };
    for gone in gone {
      gone.delete_file();
    }
    picture
  }

  #[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
  fn write_file(&self, picture: &PreparedPicture) -> Option<PathBuf> {
    let dir = self.dir.get()?;
    let number = self.next_file.fetch_add(1, Ordering::Relaxed);
    let file = dir.join(format!("{}-{number}.png", std::process::id()));
    match write_private(dir, &file, &picture.png()?) {
      Ok(()) => Some(file),
      Err(error) => {
        tracing::debug!(%error, "could not write a notification picture");
        None
      },
    }
  }
}

impl PreparedPicture {
  #[cfg(any(target_os = "linux", test))]
  pub fn side(&self) -> u32 {
    self.image.width()
  }

  /// Straight (not premultiplied) RGBA, row by row: the `image-data` hint.
  #[cfg(any(target_os = "linux", test))]
  pub fn rgba(&self) -> &[u8] {
    self.image.as_raw()
  }

  #[cfg(any(windows, test))]
  pub fn shape(&self) -> PictureShape {
    self.shape
  }

  /// The picture as PNG, for a file.
  #[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
  pub fn png(&self) -> Option<Vec<u8>> {
    let mut png = Cursor::new(Vec::new());
    match self.image.write_to(&mut png, ImageFormat::Png) {
      Ok(()) => Some(png.into_inner()),
      Err(error) => {
        tracing::debug!(%error, "could not encode a notification picture");
        None
      },
    }
  }

  fn delete_file(&self) {
    if let Some(file) = self
      .maybe_file
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
    {
      // Already gone (sign-out, a cleaned cache) is fine.
      drop(std::fs::remove_file(file));
    }
  }
}

/// Whether [`prepare`] cuts the picture to its shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cut {
  /// A cached picture: square, as uploaded.
  ToShape,
  /// The webview's tile, drawn in its shape.
  AsDrawn,
}

/// Decodes `bytes`, takes the middle square, scales it to [`NOTIFICATION_PICTURE_PX`] and, for
/// [`Cut::ToShape`], makes everything outside `shape` transparent with a smooth edge.
fn prepare(bytes: &[u8], shape: PictureShape, cut: Cut) -> Result<PreparedPicture, PictureError> {
  let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
  let mut limits = Limits::default();
  limits.max_image_width = Some(MAX_SOURCE_SIDE);
  limits.max_image_height = Some(MAX_SOURCE_SIDE);
  limits.max_alloc = Some(MAX_DECODER_BYTES);
  reader.limits(limits);
  let decoded = reader.decode()?;
  let side = decoded.width().min(decoded.height());
  if side == 0 {
    return Err(PictureError::Empty);
  }
  let square = decoded.crop_imm(
    (decoded.width() - side) / 2,
    (decoded.height() - side) / 2,
    side,
    side,
  );
  let mut image = if side == NOTIFICATION_PICTURE_PX {
    square.to_rgba8()
  } else {
    square
      .resize_exact(
        NOTIFICATION_PICTURE_PX,
        NOTIFICATION_PICTURE_PX,
        FilterType::Lanczos3,
      )
      .to_rgba8()
  };
  if cut == Cut::ToShape {
    cut_to(&mut image, shape);
  }
  Ok(PreparedPicture {
    image,
    shape,
    maybe_file: Mutex::new(None),
  })
}

/// Fades out what lies outside the shape: a square with corners rounded by the shape's share of
/// the side (a circle at one half), anti-aliased over one pixel.
fn cut_to(image: &mut RgbaImage, shape: PictureShape) {
  let size = image.width() as f32;
  let half = size / 2.0;
  let radius = size * shape.corner_share();
  if radius <= 0.0 {
    return;
  }
  for (x, y, pixel) in image.enumerate_pixels_mut() {
    // Signed distance of the pixel's center from the rounded square's edge (negative: inside).
    let dx = ((x as f32 + 0.5) - half).abs() - (half - radius);
    let dy = ((y as f32 + 0.5) - half).abs() - (half - radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    let distance = outside + inside - radius;
    let coverage = (0.5 - distance).clamp(0.0, 1.0);
    pixel[3] = (f32::from(pixel[3]) * coverage).round() as u8;
  }
}

/// Runs `work` on a blocking thread; `None` if the thread failed.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
  match tauri::async_runtime::spawn_blocking(work).await {
    Ok(result) => Some(result),
    Err(error) => {
      tracing::debug!(%error, "notification picture work failed");
      None
    },
  }
}

trait OrLogged<T> {
  fn or_logged(self) -> Option<T>;
}

impl<T> OrLogged<T> for Option<Result<T, PictureError>> {
  /// The prepared picture, or `None` with the reason logged.
  fn or_logged(self) -> Option<T> {
    match self? {
      Ok(value) => Some(value),
      Err(error) => {
        tracing::debug!(%error, "notification picture not shown");
        None
      },
    }
  }
}

/// Writes `bytes` to `file` in `dir`, readable by this user only where the system has modes.
#[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
fn write_private(dir: &Path, file: &Path, bytes: &[u8]) -> Result<(), PictureError> {
  use std::io::Write as _;
  std::fs::create_dir_all(dir)?;
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
  options.open(file)?.write_all(bytes)?;
  Ok(())
}

fn remove_dir(dir: &Path) {
  match std::fs::remove_dir_all(dir) {
    Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
      tracing::debug!(%error, "could not delete the notification pictures");
    },
    _ => {},
  }
}

#[cfg(test)]
mod tests {
  // Test-only module: a failing step should abort the test with a clear message.
  #![allow(clippy::expect_used, clippy::unwrap_used)]

  use image::Rgba;
  use teamcraft_core::media_cache::{CachePolicy, CachedMedia};
  use teamcraft_crypto::secret::{SecretBytes, SymmetricKey};
  use teamcraft_proto::Base64Bytes;

  use super::*;

  const RED: Rgba<u8> = Rgba([200, 0, 0, 255]);

  mod shapes {
    use super::*;

    #[test]
    fn a_picture_becomes_the_middle_square_at_the_notification_size() {
      let prepared = prepare(&png(300, 200, RED), PictureShape::Unknown, Cut::ToShape).expect("prepared");
      assert_eq!(prepared.side(), NOTIFICATION_PICTURE_PX);
      assert_eq!(
        prepared.rgba().len(),
        (NOTIFICATION_PICTURE_PX * NOTIFICATION_PICTURE_PX * 4) as usize
      );
      assert_eq!(alpha(&prepared, 0, 0), 255, "a plain square keeps its corners");
    }

    #[test]
    fn people_are_round_and_places_have_rounded_corners() {
      let last = NOTIFICATION_PICTURE_PX - 1;
      let middle = NOTIFICATION_PICTURE_PX / 2;
      let round = prepare(&png(512, 512, RED), PictureShape::Circle, Cut::ToShape).expect("prepared");
      for (x, y) in [(0, 0), (last, 0), (0, last), (last, last)] {
        assert_eq!(alpha(&round, x, y), 0, "corner {x},{y}");
      }
      for (x, y) in [(middle, middle), (middle, 1), (1, middle)] {
        assert_eq!(alpha(&round, x, y), 255, "inside {x},{y}");
      }
      // Along the diagonal the edge is neither fully in nor out.
      let edge = (0..NOTIFICATION_PICTURE_PX)
        .map(|step| alpha(&round, step, step))
        .find(|alpha| *alpha > 0)
        .expect("the circle reaches the diagonal");
      assert!(edge < 255, "the edge is smooth, not jagged");

      let tile = prepare(&png(256, 256, RED), PictureShape::RoundedSquare, Cut::ToShape).expect("prepared");
      assert_eq!(alpha(&tile, 0, 0), 0, "a rounded corner");
      assert_eq!(alpha(&tile, middle, 0), 255, "the straight top edge");
      assert_eq!(alpha(&tile, 20, 20), 255, "inside the corner's curve");
    }

    #[test]
    fn a_drawn_tile_is_taken_as_it_is() {
      let tile = prepare(&png(128, 128, RED), PictureShape::Circle, Cut::AsDrawn).expect("prepared");
      assert_eq!(alpha(&tile, 0, 0), 255, "the webview cut it already");
      assert_eq!(tile.shape(), PictureShape::Circle);
    }

    #[test]
    fn garbage_and_oversized_pictures_are_refused() {
      assert!(prepare(b"not a picture", PictureShape::Circle, Cut::ToShape).is_err());
      assert!(prepare(&[], PictureShape::Circle, Cut::ToShape).is_err());
      let huge = png(MAX_SOURCE_SIDE + 1, 1, RED);
      assert!(prepare(&huge, PictureShape::Circle, Cut::ToShape).is_err());
    }

    #[test]
    fn the_png_reads_back_as_the_same_picture() {
      let prepared = prepare(&png(64, 64, RED), PictureShape::Circle, Cut::ToShape).expect("prepared");
      let decoded = image::load_from_memory(&prepared.png().expect("png"))
        .expect("decodes")
        .to_rgba8();
      assert_eq!(decoded.as_raw(), prepared.rgba());
    }
  }

  mod resolving {
    use super::*;

    #[test]
    fn the_cached_picture_wins_and_is_prepared_once_per_version() {
      let scratch = Scratch::new("cached");
      let cache = scratch.cache();
      let key = MediaKey::Avatar(id(7));
      put(&cache, key, "v1", RED);
      let pictures = NotificationPictures::default();
      let first = resolve(&pictures, &cache, picture(Some(key), true)).expect("shown");
      assert_eq!(alpha(&first, 0, 0), 0, "cut to a circle: not the drawn tile");
      let again = resolve(&pictures, &cache, picture(Some(key), true)).expect("shown");
      assert!(Arc::ptr_eq(&first, &again), "kept, not prepared again");
      put(&cache, key, "v2", Rgba([0, 0, 200, 255]));
      let newer = resolve(&pictures, &cache, picture(Some(key), true)).expect("shown");
      assert!(!Arc::ptr_eq(&first, &newer), "a new version is prepared anew");
    }

    #[test]
    fn without_a_cached_picture_the_drawn_tile_shows_and_without_either_nothing() {
      let scratch = Scratch::new("fallback");
      let cache = scratch.cache();
      let pictures = NotificationPictures::default();
      let missing = MediaKey::SpaceImage(id(3));
      let tile = resolve(&pictures, &cache, picture(Some(missing), true)).expect("the tile");
      assert_eq!(alpha(&tile, 0, 0), 255, "the drawn tile, as drawn");
      assert!(resolve(&pictures, &cache, picture(Some(missing), false)).is_none());
      let no_cache = tauri::async_runtime::block_on(pictures.resolve(None, &picture(Some(missing), true)));
      assert!(no_cache.is_some(), "no running core: still the tile");
    }

    #[test]
    fn only_the_newest_pictures_are_kept() {
      let pictures = NotificationPictures::default();
      for index in 0..=MAX_PREPARED {
        keep_generated(&pictures, index);
      }
      assert!(pictures.find(&generated_source(0)).is_none(), "the oldest went");
      assert!(pictures.find(&generated_source(MAX_PREPARED)).is_some());
    }
  }

  mod files {
    use super::*;

    #[test]
    fn a_toast_reuses_its_file_and_forgetting_deletes_them_all() {
      let scratch = Scratch::new("files");
      let pictures = NotificationPictures::default();
      std::fs::create_dir_all(scratch.pictures()).expect("dir");
      std::fs::write(scratch.pictures().join("left-over.png"), b"old").expect("left over");
      pictures.use_dir(scratch.pictures());
      assert!(
        !scratch.pictures().exists(),
        "an earlier run's files went at start"
      );

      let picture = keep_generated(&pictures, 1);
      let file = pictures.toast_file(&picture).expect("written");
      assert_eq!(
        pictures.toast_file(&picture),
        Some(file.clone()),
        "one file per picture"
      );
      let written = image::open(&file).expect("a PNG").to_rgba8();
      assert_eq!(written.as_raw(), picture.rgba());
      #[cfg(unix)]
      {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o077, 0, "only the user reads it");
      }

      let first = pictures.attachment_file(&picture).expect("written");
      let second = pictures.attachment_file(&picture).expect("written");
      assert_ne!(first, second, "every notification gets its own copy");

      pictures.forget_all();
      assert!(!scratch.pictures().exists(), "sign-out deletes them");
      assert!(pictures.find(&generated_source(1)).is_none());
    }

    #[test]
    fn a_picture_that_is_no_longer_kept_takes_its_file_along() {
      let scratch = Scratch::new("evict");
      let pictures = NotificationPictures::default();
      pictures.use_dir(scratch.pictures());
      let first = keep_generated(&pictures, 0);
      let file = pictures.toast_file(&first).expect("written");
      for index in 1..=MAX_PREPARED {
        keep_generated(&pictures, index);
      }
      assert!(!file.exists());
    }

    #[test]
    fn without_a_folder_there_are_no_files() {
      let pictures = NotificationPictures::default();
      let picture = keep_generated(&pictures, 1);
      assert_eq!(pictures.toast_file(&picture), None);
      assert_eq!(pictures.attachment_file(&picture), None);
    }
  }

  /// A temporary folder of one test, deleted at its end.
  struct Scratch(PathBuf);

  impl Scratch {
    fn new(name: &str) -> Self {
      let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
      Self(std::env::temp_dir().join(format!(
        "teamcraft-notification-pictures-{name}-{}-{unique}",
        std::process::id()
      )))
    }

    fn cache(&self) -> Arc<FileMediaCache> {
      let key = SymmetricKey::from_bytes([5; 32]);
      Arc::new(FileMediaCache::open(self.0.join("cache"), &key).expect("cache"))
    }

    fn pictures(&self) -> PathBuf {
      self.0.join(PICTURES_DIR)
    }
  }

  impl Drop for Scratch {
    fn drop(&mut self) {
      drop(std::fs::remove_dir_all(&self.0));
    }
  }

  fn resolve(
    pictures: &NotificationPictures,
    cache: &Arc<FileMediaCache>,
    picture: NotificationPicture,
  ) -> Option<Arc<PreparedPicture>> {
    tauri::async_runtime::block_on(pictures.resolve(Some(cache.clone()), &picture))
  }

  fn picture(maybe_key: Option<MediaKey>, has_fallback: bool) -> NotificationPicture {
    NotificationPicture {
      shape: PictureShape::Circle,
      maybe_key,
      maybe_fallback_png: has_fallback.then(|| Base64Bytes::new(png(128, 128, RED))),
    }
  }

  fn put(cache: &FileMediaCache, key: MediaKey, version: &str, colour: Rgba<u8>) {
    let media = CachedMedia {
      version: version.to_owned(),
      content_type: "image/png".to_owned(),
      bytes: SecretBytes::new(png(512, 512, colour)),
    };
    cache.put(&key, &media, CachePolicy::default()).expect("cached");
  }

  fn keep_generated(pictures: &NotificationPictures, index: usize) -> Arc<PreparedPicture> {
    let prepared = prepare(&png(8, 8, RED), PictureShape::Circle, Cut::AsDrawn).expect("prepared");
    pictures.keep(generated_source(index), prepared)
  }

  fn generated_source(index: usize) -> Source {
    Source::Generated {
      png: index.to_be_bytes().to_vec(),
      shape: PictureShape::Circle,
    }
  }

  fn png(width: u32, height: u32, colour: Rgba<u8>) -> Vec<u8> {
    let image = RgbaImage::from_pixel(width, height, colour);
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, ImageFormat::Png).expect("encodes");
    png.into_inner()
  }

  fn alpha(picture: &PreparedPicture, x: u32, y: u32) -> u8 {
    picture.image.get_pixel(x, y)[3]
  }

  fn id<T: teamcraft_proto::Id>(raw: i64) -> T {
    T::from_snowflake(teamcraft_proto::Snowflake::new(raw).expect("a valid test ID"))
  }
}
