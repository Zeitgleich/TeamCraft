//! The self view on the desktop: the camera's capture (after background effects, before the
//! encoder), as JPEG images for the webview, which has no access to the camera the shell opened.
//!
//! The images are as sharp as the tile shows them (#937): the webview tells the size it draws in,
//! in device pixels ([`SelfView::set_drawn_size`]), and each image is the capture scaled down just
//! enough to cover it, never up, at a quality close to the capture. Until the size is known the
//! whole capture goes. JPEG rather than raw pixels: a 1280x720 image at [`JPEG_QUALITY`] is about
//! 150 KB (about 44 dB PSNR against the capture), I420 would be 1.4 MB and RGBA 3.7 MB per image.
//!
//! The encoder thread offers a frame every [`MIN_INTERVAL`] while the webview watches; a thread of
//! the self view's own scales and encodes it, so the encoder never waits. Frames that arrive while
//! the previous one is still being encoded are dropped: each image stands alone. Nothing is copied
//! or encoded while nobody watches (robustness.md, "Performance and resources").

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teamcraft_media::video::{VideoFrame, VideoResolution};
use teamcraft_media::video_jpeg::encode_jpeg;
use teamcraft_media::video_pipeline::FrameFilter;
use teamcraft_proto::MediaStreamId;
use tracing::{debug, warn};

use super::ChannelId;
use super::video::jpeg_message;

/// About 15 images per second: enough for a preview, a fraction of the camera's 30.
const MIN_INTERVAL: Duration = Duration::from_millis(66);
/// The largest image, whatever the tile: a camera that only offers 4K would otherwise cost tens of
/// milliseconds per image. Calls capture at most 1920x1080 (`video::camera_format`, #967).
const MAX_SIZE: VideoResolution = VideoResolution {
  width: 1920,
  height: 1080,
};
/// Close to the capture: 43.5 to 44.5 dB PSNR at 1280x720, where 70 gave 40 to 41.5 dB (measured on
/// photos of rooms for #937). Each image stands alone: no motion hides its artefacts.
const JPEG_QUALITY: u8 = 85;
/// The stream id in the self view's messages: the own camera has no track id of its own here.
const SELF_VIEW_STREAM: MediaStreamId = MediaStreamId(0);

/// Who watches the own camera, shared by the camera's frame filter and the commands.
#[derive(Clone, Default)]
pub(crate) struct SelfView {
  maybe_viewer: Arc<Mutex<Option<Viewer>>>,
  /// The size the webview draws the self view in, in device pixels ([`pack_size`]); 0 while it
  /// hasn't said.
  drawn_size: Arc<AtomicU32>,
}

/// The encoding thread of one watcher; it ends when this is dropped or an image can't be
/// delivered.
struct Viewer {
  channel: ChannelId,
  frames: std_mpsc::SyncSender<VideoFrame>,
}

impl SelfView {
  /// Sends the own camera to `send` (one message per image, format of `video::video_message`)
  /// until [`Self::unwatch`], another watcher, or `send` answering that it could not deliver.
  pub(crate) fn watch(&self, channel: ChannelId, send: impl Fn(Vec<u8>) -> bool + Send + 'static) {
    // One frame in the queue: the encoder thread never blocks on a busy self view.
    let (frames, queue) = std_mpsc::sync_channel::<VideoFrame>(1);
    // A new view says its size with its first images; until then the whole capture.
    self.drawn_size.store(0, Ordering::Relaxed);
    let drawn_size = Arc::clone(&self.drawn_size);
    let spawned = std::thread::Builder::new()
      .name("teamcraft-self-view".into())
      .spawn(move || {
        encode_images(&queue, &drawn_size, &send);
        // The queue first: once `send` is gone (what a test waits for), offers already fail.
        drop(queue);
        drop(send);
      });
    if let Err(error) = spawned {
      warn!(%error, "no thread for the self view; it stays blank");
      return;
    }
    *self.lock() = Some(Viewer { channel, frames });
  }

  /// Stops the watcher of `channel`; a newer watcher (another channel) stays.
  pub(crate) fn unwatch(&self, channel: ChannelId) {
    let mut maybe_viewer = self.lock();
    if maybe_viewer
      .as_ref()
      .is_some_and(|viewer| viewer.channel == channel)
    {
      *maybe_viewer = None;
    }
  }

  /// The webview draws the self view in `width` x `height` device pixels (the tile's size times
  /// the display's scale); the next images cover that. Zero means unknown: the whole capture.
  pub(crate) fn set_drawn_size(&self, width: u32, height: u32) {
    self.drawn_size.store(pack_size(width, height), Ordering::Relaxed);
  }

  /// The camera's frame filter: `maybe_inner` (background effects) first, then a copy for the
  /// self view when one is due.
  pub(crate) fn filter(&self, maybe_inner: Option<FrameFilter>) -> FrameFilter {
    let self_view = self.clone();
    let mut maybe_inner = maybe_inner;
    let mut maybe_last_us = None::<u64>;
    let min_interval_us = u64::try_from(MIN_INTERVAL.as_micros()).unwrap_or(u64::MAX);
    Box::new(move |frame| {
      let frame = match &mut maybe_inner {
        Some(inner) => inner(frame),
        None => frame,
      };
      let timestamp_us = frame.timestamp_us();
      let is_due =
        maybe_last_us.is_none_or(|last| timestamp_us < last || timestamp_us - last >= min_interval_us);
      if is_due && self_view.offer(&frame) {
        maybe_last_us = Some(timestamp_us);
      }
      frame
    })
  }

  /// Hands a copy of `frame` to the watcher, if there is one and it isn't busy.
  fn offer(&self, frame: &VideoFrame) -> bool {
    let mut maybe_viewer = self.lock();
    let Some(viewer) = maybe_viewer.as_ref() else {
      return false;
    };
    match viewer.frames.try_send(frame.clone()) {
      Ok(()) => true,
      Err(std_mpsc::TrySendError::Full(_)) => false,
      // The thread ended: the webview took no more images (it was reloaded or closed).
      Err(std_mpsc::TrySendError::Disconnected(_)) => {
        *maybe_viewer = None;
        false
      },
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Option<Viewer>> {
    self.maybe_viewer.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

fn encode_images(
  queue: &std_mpsc::Receiver<VideoFrame>,
  drawn_size: &AtomicU32,
  send: &dyn Fn(Vec<u8>) -> bool,
) {
  while let Ok(frame) = queue.recv() {
    let maybe_drawn = unpack_size(drawn_size.load(Ordering::Relaxed));
    let small = match frame.scaled(image_size(frame.resolution(), maybe_drawn)) {
      Ok(small) => small,
      Err(error) => {
        debug!(%error, "self view frame not scaled");
        continue;
      },
    };
    match encode_jpeg(&small, JPEG_QUALITY) {
      Ok(jpeg) => {
        if !send(jpeg_message(SELF_VIEW_STREAM, frame.timestamp_us(), &jpeg)) {
          debug!("the webview takes no more self view images");
          return;
        }
      },
      Err(error) => debug!(%error, "self view image not encoded"),
    }
  }
}

/// The size of the image for a capture of `source` drawn in `maybe_drawn` device pixels: scaled
/// down just enough to still cover the drawn area (the tile crops like `object-fit: cover`), never
/// up, and within [`MAX_SIZE`]. Even sizes, as I420 needs.
fn image_size(source: VideoResolution, maybe_drawn: Option<VideoResolution>) -> VideoResolution {
  let limited = fit_within(source, MAX_SIZE);
  let Some(drawn) = maybe_drawn else {
    return limited;
  };
  let (width, height) = (u32::from(limited.width), u32::from(limited.height));
  let (drawn_width, drawn_height) = (u32::from(drawn.width), u32::from(drawn.height));
  if drawn_width >= width || drawn_height >= height {
    return limited;
  }
  // The looser side decides (cover): drawn_width / width vs drawn_height / height, without floats.
  let (target_width, target_height) = if drawn_width * height >= drawn_height * width {
    (drawn_width, (height * drawn_width).div_ceil(width))
  } else {
    ((width * drawn_height).div_ceil(height), drawn_height)
  };
  let even_up = |value: u32, max: u16| u16::try_from((value + 1) & !1).unwrap_or(max).clamp(2, max);
  VideoResolution {
    width: even_up(target_width, limited.width),
    height: even_up(target_height, limited.height),
  }
}

/// A size in one atomic: the width in the high, the height in the low 16 bits; larger values are
/// clamped (no tile is that large).
fn pack_size(width: u32, height: u32) -> u32 {
  let clamp = |value: u32| value.min(u32::from(u16::MAX));
  (clamp(width) << 16) | clamp(height)
}

fn unpack_size(packed: u32) -> Option<VideoResolution> {
  let (width, height) = ((packed >> 16) as u16, (packed & 0xffff) as u16);
  (width > 0 && height > 0).then_some(VideoResolution { width, height })
}

/// `source` scaled down to fit `max` with its aspect ratio kept (even sizes, as I420 needs).
fn fit_within(source: VideoResolution, max: VideoResolution) -> VideoResolution {
  if source.width <= max.width && source.height <= max.height {
    return source;
  }
  let (width, height) = (u32::from(source.width), u32::from(source.height));
  let (max_width, max_height) = (u32::from(max.width), u32::from(max.height));
  // The tighter side decides: width / max_width vs height / max_height, without floats.
  let (target_width, target_height) = if width * max_height >= height * max_width {
    (max_width, height * max_width / width)
  } else {
    (width * max_height / height, max_height)
  };
  let even = |value: u32| u16::try_from((value & !1).max(2)).unwrap_or(u16::MAX - 1);
  VideoResolution {
    width: even(target_width),
    height: even(target_height),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const HD: VideoResolution = size(1280, 720);

  const fn size(width: u16, height: u16) -> VideoResolution {
    VideoResolution { width, height }
  }

  #[test]
  fn without_a_drawn_size_the_whole_capture_goes() {
    // #937: every tile got 640x360.
    assert_eq!(image_size(HD, None), HD);
    assert_eq!(image_size(size(640, 480), None), size(640, 480));
    // A camera that only offers 4K stays within the cap, with its aspect ratio.
    assert_eq!(image_size(size(3840, 2160), None), MAX_SIZE);
    assert_eq!(image_size(size(2160, 3840), None), size(606, 1080));
  }

  #[test]
  fn a_tile_larger_than_the_capture_gets_the_capture_unscaled() {
    // The pre-join tile on a Retina display: 760x475 CSS pixels at 2x.
    assert_eq!(image_size(HD, Some(size(1520, 950))), HD);
    // As tall or as wide as the capture: cropped on the other side, so nothing to spare.
    assert_eq!(image_size(HD, Some(size(400, 720))), HD);
    assert_eq!(image_size(HD, Some(size(1280, 300))), HD);
  }

  #[test]
  fn a_smaller_tile_gets_an_image_that_just_covers_it() {
    // A 16:10 filmstrip tile at 2x: the height decides, the tile crops the sides.
    assert_eq!(image_size(HD, Some(size(440, 276))), size(492, 276));
    // The same tile at 1x.
    assert_eq!(image_size(HD, Some(size(220, 138))), size(246, 138));
    // A tile of the capture's own shape gets exactly its size.
    assert_eq!(image_size(HD, Some(size(960, 540))), size(960, 540));
    // A wide strip: the width decides.
    assert_eq!(image_size(HD, Some(size(1000, 100))), size(1000, 564));
    // Tiny and odd sizes stay even and at least 2 pixels.
    assert_eq!(image_size(HD, Some(size(1, 1))), size(2, 2));
    assert_eq!(image_size(HD, Some(size(333, 187))), size(334, 188));
  }

  #[test]
  fn the_image_covers_the_drawn_area_without_upscaling() {
    for source in [HD, size(640, 480), size(1920, 1080), size(720, 1280)] {
      let limited = fit_within(source, MAX_SIZE);
      for width in (1..=2000).step_by(37) {
        for height in (1..=1600).step_by(41) {
          let drawn = size(width, height);
          let image = image_size(source, Some(drawn));
          assert!(image.width <= limited.width && image.height <= limited.height);
          assert!(image.width.is_multiple_of(2) && image.height.is_multiple_of(2));
          let is_covered = image == limited || (image.width >= drawn.width && image.height >= drawn.height);
          assert!(is_covered, "{source:?} drawn at {drawn:?} gave {image:?}");
        }
      }
    }
  }

  #[test]
  fn drawn_sizes_survive_the_atomic() {
    assert_eq!(unpack_size(pack_size(1520, 950)), Some(size(1520, 950)));
    assert_eq!(unpack_size(pack_size(0, 950)), None);
    assert_eq!(unpack_size(0), None);
    assert_eq!(
      unpack_size(pack_size(100_000, 70_000)),
      Some(size(u16::MAX, u16::MAX))
    );
  }

  #[test]
  fn images_follow_the_drawn_size_and_a_new_watcher_starts_over() {
    let self_view = SelfView::default();
    let mut filter = self_view.filter(None);
    let (sent, received) = std_mpsc::channel();
    self_view.watch(ChannelId(1), move |message| sent.send(message).is_ok());
    filter(gray_frame(1_000_000));
    let first = received.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(jpeg_size(&first), (1280, 720));

    self_view.set_drawn_size(440, 276);
    filter(gray_frame(2_000_000));
    let sized = received.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(jpeg_size(&sized), (492, 276));

    // Another view (a new canvas) hasn't said its size yet.
    let (sent, received) = std_mpsc::channel();
    self_view.watch(ChannelId(2), move |message| sent.send(message).is_ok());
    filter(gray_frame(3_000_000));
    let restarted = received.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(jpeg_size(&restarted), (1280, 720));
  }

  /// Width and height of a message's baseline JPEG, from its SOF0 segment.
  fn jpeg_size(message: &[u8]) -> (u16, u16) {
    let jpeg = &message[16..];
    let at = jpeg
      .windows(2)
      .position(|marker| marker == [0xff, 0xc0])
      .expect("a baseline JPEG");
    let field = |offset: usize| u16::from_be_bytes([jpeg[at + offset], jpeg[at + offset + 1]]);
    (field(7), field(5))
  }

  #[test]
  fn frames_reach_the_watcher_as_jpeg_messages_at_a_reduced_rate() {
    let self_view = SelfView::default();
    let mut filter = self_view.filter(None);
    // Not watched: frames pass through, nothing is sent.
    let unwatched = filter(gray_frame(0));
    assert_eq!(unwatched.timestamp_us(), 0);

    let (sent, received) = std_mpsc::channel();
    self_view.watch(ChannelId(1), move |message| sent.send(message).is_ok());
    filter(gray_frame(1_000_000));
    let message = received.recv_timeout(Duration::from_secs(10)).unwrap();
    // Header: version 1, keyframe + JPEG, codec JPEG, stream 0, then the JPEG's start marker.
    assert_eq!(&message[..8], &[1, 3, 3, 0, 0, 0, 0, 0]);
    assert_eq!(&message[16..18], &[0xff, 0xd8]);

    // After unwatching nothing more arrives: the thread ends, and with it the sender.
    self_view.unwatch(ChannelId(1));
    filter(gray_frame(2_000_000));
    assert!(received.recv().is_err());
  }

  #[test]
  fn a_late_unwatch_of_a_replaced_channel_keeps_the_newer_watcher() {
    let self_view = SelfView::default();
    let mut filter = self_view.filter(None);
    let (old, _old_received) = std_mpsc::channel::<Vec<u8>>();
    self_view.watch(ChannelId(1), move |message| old.send(message).is_ok());
    let (sent, received) = std_mpsc::channel();
    self_view.watch(ChannelId(2), move |message| sent.send(message).is_ok());
    self_view.unwatch(ChannelId(1));
    filter(gray_frame(1_000_000));
    assert!(received.recv_timeout(Duration::from_secs(10)).is_ok());
  }

  #[test]
  fn the_thread_ends_when_the_webview_takes_no_more_images() {
    let self_view = SelfView::default();
    let mut filter = self_view.filter(None);
    let (attempts, attempted) = std_mpsc::channel();
    // The webview is gone: every send fails.
    self_view.watch(ChannelId(1), move |_message| {
      let _ = attempts.send(());
      false
    });
    filter(gray_frame(1_000_000));
    attempted.recv_timeout(Duration::from_secs(10)).unwrap();
    // The thread ended after the failed send and dropped its sender.
    assert!(attempted.recv().is_err());
    // The next offer notices and forgets the watcher.
    filter(gray_frame(2_000_000));
    assert!(self_view.lock().is_none());
  }

  fn gray_frame(timestamp_us: u64) -> VideoFrame {
    let resolution = VideoResolution {
      width: 1280,
      height: 720,
    };
    VideoFrame::new(resolution, timestamp_us, vec![128; 1280 * 720 * 3 / 2]).unwrap()
  }
}
