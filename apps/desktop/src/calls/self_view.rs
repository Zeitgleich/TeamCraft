//! The self view on the desktop: the camera as the call sends it (after background effects), as
//! small JPEG images for the webview, which has no access to the camera the shell opened.
//!
//! The encoder thread offers a frame every [`MIN_INTERVAL`] while the webview watches; a thread of
//! the self view's own scales and encodes it, so the encoder never waits. Frames that arrive while
//! the previous one is still being encoded are dropped: each image stands alone. Nothing is copied
//! or encoded while nobody watches (robustness.md, "Performance and resources").

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
/// The self view is a small tile; larger images would only cost CPU and IPC.
const MAX_SIZE: VideoResolution = VideoResolution {
  width: 640,
  height: 360,
};
const JPEG_QUALITY: u8 = 70;
/// The stream id in the self view's messages: the own camera has no track id of its own here.
const SELF_VIEW_STREAM: MediaStreamId = MediaStreamId(0);

/// Who watches the own camera, shared by the camera's frame filter and the commands.
#[derive(Clone, Default)]
pub(crate) struct SelfView {
  maybe_viewer: Arc<Mutex<Option<Viewer>>>,
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
    let spawned = std::thread::Builder::new()
      .name("teamcraft-self-view".into())
      .spawn(move || {
        encode_images(&queue, &send);
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

fn encode_images(queue: &std_mpsc::Receiver<VideoFrame>, send: &dyn Fn(Vec<u8>) -> bool) {
  while let Ok(frame) = queue.recv() {
    let small = match frame.scaled(fit_within(frame.resolution(), MAX_SIZE)) {
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

  #[test]
  fn large_frames_shrink_to_the_self_view_size_with_their_aspect_ratio() {
    let hd = VideoResolution {
      width: 1280,
      height: 720,
    };
    assert_eq!(fit_within(hd, MAX_SIZE), MAX_SIZE);
    let portrait = VideoResolution {
      width: 720,
      height: 1280,
    };
    assert_eq!(
      fit_within(portrait, MAX_SIZE),
      VideoResolution {
        width: 202,
        height: 360
      }
    );
    let small = VideoResolution {
      width: 320,
      height: 240,
    };
    assert_eq!(fit_within(small, MAX_SIZE), small);
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
