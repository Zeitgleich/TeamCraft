//! Thumbnails of image attachments. Made by the sender's client and sent encrypted inside the
//! message, so receivers show a preview before (or without) downloading the file.
//!
//! A thumbnail is a JPEG, or a PNG when the picture has transparency. Receivers read the format
//! from the bytes (older clients made PNGs only), so either works everywhere.
//!
//! The pictures are decoded and encoded by the platform's [`PictureCodec`]: natively the `image`
//! crate (`native.rs`); in the browser the browser's own decoders and canvas (`browser.rs`), so the
//! web app carries no image codecs of its own (#135; docs/performance.md, "Web bundle size"). The
//! engine asks through [`make_thumbnail`] and [`make_without_metadata`]. [`image_dimensions`] reads headers only: natively with `image`, in
//! the browser with the small reader in `image_header.rs`.

use std::future::Future;
use std::pin::Pin;

#[cfg(target_arch = "wasm32")]
pub mod browser;
#[cfg(not(target_arch = "wasm32"))]
mod native;

#[cfg(target_arch = "wasm32")]
pub use browser::BrowserCodec;

#[cfg(not(target_arch = "wasm32"))]
pub use native::{NativeCodec, thumbnail, thumbnail_within, without_metadata};

/// Longest side of a thumbnail in pixels: sharp at the ~300 dp a phone shows it at on a 2–3×
/// screen, and at 360 CSS pixels on a HiDPI desktop.
pub const THUMBNAIL_SIZE: u32 = 640;
/// Side of a space picture in pixels (the rail shows it at up to 48 CSS pixels, ×4 for dense screens
/// and the settings preview).
pub const SPACE_IMAGE_SIZE: u32 = 256;
/// Side of an avatar in pixels (shown at up to 64 CSS pixels in profiles, ×4 for dense screens and
/// the profile card).
pub const AVATAR_IMAGE_SIZE: u32 = 512;
/// Longest side of the crop dialog's copy of a picture: plenty for a 512 px result even when zoomed
/// in, and a 40 MP photo still fits in a few MB of memory.
pub const EDITOR_IMAGE_MAX_SIDE: u32 = 2048;
/// A thumbnail travels inside the message, which the server caps (1 MiB of ciphertext by default)
/// for every file of the message together; larger ones are made smaller or lower in quality.
pub const MAX_THUMBNAIL_BYTES: usize = 96 * 1024;
/// Sizes and JPEG qualities (in percent) tried in turn until a thumbnail fits
/// [`MAX_THUMBNAIL_BYTES`]. Every [`PictureCodec`] goes through them.
pub const JPEG_STEPS: [(u32, u8); 4] = [(THUMBNAIL_SIZE, 80), (THUMBNAIL_SIZE, 65), (480, 65), (320, 60)];
/// Sizes tried in turn for pictures with transparency (PNG has no quality to lower).
pub const PNG_STEPS: [u32; 3] = [THUMBNAIL_SIZE, 480, 320];
/// Images larger than this many pixels aren't decoded (decompression bombs).
pub const MAX_PIXELS: u64 = 40_000_000;
/// JPEG quality (in percent) of a screenshot encoded again without its metadata.
pub const SCREENSHOT_JPEG_QUALITY: u8 = 90;

/// A future of a [`PictureCodec`]. Not `Send`: the browser's runs on the page's thread.
pub type CodecFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The platform's image codecs, as the engine uses them. Natively `NativeCodec` (the `image`
/// crate); in the browser `BrowserCodec`, with the browser's decoders and canvas. Both keep the same
/// contract, the one written on each method; anything a codec can't do is `None` (a picture without
/// a thumbnail, never a failed send).
pub trait PictureCodec {
  /// A thumbnail of an image file: upright (EXIF orientation applied), at most
  /// [`THUMBNAIL_SIZE`] pixels on its longer side and never enlarged, at most
  /// [`MAX_THUMBNAIL_BYTES`]; a PNG ([`PNG_STEPS`]) when the picture has transparency, else a JPEG
  /// ([`JPEG_STEPS`]). `None` for anything that isn't a decodable image, one over [`MAX_PIXELS`]
  /// (read from its header, before decoding), or one that can't be made small enough.
  fn thumbnail<'a>(&'a self, bytes: &'a [u8], media_type: &'a str) -> CodecFuture<'a, Option<Vec<u8>>>;

  /// A picture encoded again from its pixels, upright, without its metadata (EXIF with the camera,
  /// time and GPS position, text chunks): what a problem report sends of a screenshot
  /// (docs/bug-reports.md). A JPEG stays a JPEG ([`SCREENSHOT_JPEG_QUALITY`]), anything else
  /// becomes a PNG. `None` for anything that isn't a decodable picture within [`MAX_PIXELS`].
  fn without_metadata<'a>(&'a self, bytes: &'a [u8]) -> CodecFuture<'a, Option<(Vec<u8>, &'static str)>>;
}

/// [`PictureCodec::thumbnail`] as the platform makes it: natively right here, in the browser by
/// the browser.
pub async fn make_thumbnail(bytes: &[u8], media_type: &str) -> Option<Vec<u8>> {
  if !essence(media_type).starts_with("image/") {
    return None;
  }
  #[cfg(target_arch = "wasm32")]
  {
    BrowserCodec.thumbnail(bytes, media_type).await
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    thumbnail(bytes, media_type)
  }
}

/// [`PictureCodec::without_metadata`] as the platform does it (see [`make_thumbnail`]).
pub async fn make_without_metadata(bytes: &[u8]) -> Option<(Vec<u8>, &'static str)> {
  #[cfg(target_arch = "wasm32")]
  {
    BrowserCodec.without_metadata(bytes).await
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    without_metadata(bytes)
  }
}

/// A media type without parameters, trimmed and lowercase: senders write whatever they like.
pub fn essence(media_type: &str) -> String {
  media_type
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase()
}

/// The size of a `width` × `height` picture scaled to at most `side` pixels on its longer side,
/// keeping its aspect ratio (rounded, at least a pixel); a smaller picture keeps its size. What
/// every codec scales a thumbnail to.
pub fn fitted_size(width: u32, height: u32, side: u32) -> (u32, u32) {
  if width.max(height) <= side {
    return (width, height);
  }
  let ratio = f64::from(side) / f64::from(width.max(height));
  let scaled = |length: u32| ((f64::from(length) * ratio).round() as u32).clamp(1, side);
  (scaled(width), scaled(height))
}

/// Width and height of an image from its header, without decoding it; `None` for anything else.
/// Turned upright like the thumbnail: a portrait photo stored sideways reports portrait dimensions.
pub fn image_dimensions(bytes: &[u8], media_type: &str) -> Option<(u32, u32)> {
  if !essence(media_type).starts_with("image/") {
    return None;
  }
  #[cfg(target_arch = "wasm32")]
  {
    crate::image_header::upright_size(bytes)
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    native::image_dimensions(bytes)
  }
}
