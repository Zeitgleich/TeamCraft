//! Video glue between the phone's camera, screen and display and `teamcraft-media`: the app's
//! camera frames (Android `YUV_420_888`, any row and pixel stride, rotated as the sensor sits) and
//! screen frames (RGBA, as a screen capture hands them out) become upright I420 for the encoder,
//! and decoded frames become RGBA for a bitmap.

use std::sync::{Arc, Mutex, MutexGuard};

use teamcraft_media::video::{VideoError, VideoFrame, VideoResolution};
use teamcraft_media::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// One camera image as the platform hands it out: three planes with their strides.
#[derive(Clone, uniffi::Record)]
pub struct CameraFrame {
  pub width: u32,
  pub height: u32,
  pub y: Vec<u8>,
  pub u: Vec<u8>,
  pub v: Vec<u8>,
  pub y_row_stride: u32,
  pub uv_row_stride: u32,
  /// 1 for planar chroma, 2 for interleaved (NV12/NV21 behind `YUV_420_888`).
  pub uv_pixel_stride: u32,
  /// Clockwise rotation that makes the image upright (0, 90, 180 or 270).
  pub rotation_degrees: u32,
  pub timestamp_us: u64,
}

/// One picture of the shared screen as the platform hands it out (Android's `ImageReader` in
/// `RGBA_8888`): `width × height` pixels of 4 bytes, rows `row_stride` bytes apart.
#[derive(Clone, uniffi::Record)]
pub struct ScreenFrame {
  pub width: u32,
  pub height: u32,
  pub rgba: Vec<u8>,
  pub row_stride: u32,
  pub timestamp_us: u64,
}

/// A decoded frame of another device's video, ready for a bitmap.
#[derive(Clone, uniffi::Record)]
pub struct VideoFrameRgba {
  pub stream_id: u32,
  pub width: u32,
  pub height: u32,
  /// `width × height` pixels, 4 bytes each (R, G, B, A).
  pub rgba: Vec<u8>,
}

/// The camera or the screen as a capture source for the encoder: the app pushes frames,
/// [`VideoCapture::start`] hands over the queue they go to.
pub(crate) struct AppCapture {
  format: CaptureFormat,
  slot: FrameSlot,
  /// This capture's start, while its queue is the slot's.
  maybe_start: Option<u64>,
}

/// Where pushed frames go while their track (camera or screen) runs.
#[derive(Clone, Default)]
pub(crate) struct FrameSlot {
  inner: Arc<Mutex<SlotState>>,
}

/// The queue of the capture that started last, numbered by its start.
#[derive(Default)]
struct SlotState {
  starts: u64,
  maybe_current: Option<(u64, FrameSender)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rotation {
  None,
  Quarter,
  Half,
  ThreeQuarters,
}

impl AppCapture {
  pub(crate) fn new(format: CaptureFormat, slot: FrameSlot) -> Self {
    Self {
      format,
      slot,
      maybe_start: None,
    }
  }
}

impl VideoCapture for AppCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let mut state = self.slot.lock();
    state.starts += 1;
    state.maybe_current = Some((state.starts, frames));
    self.maybe_start = Some(state.starts);
    Ok(())
  }

  /// Leaves the slot to a capture that started since: the call starts a replacing track before
  /// it stops the running one.
  fn stop(&mut self) {
    let Some(start) = self.maybe_start.take() else {
      return;
    };
    let mut state = self.slot.lock();
    if state
      .maybe_current
      .as_ref()
      .is_some_and(|(current, _)| *current == start)
    {
      state.maybe_current = None;
    }
  }
}

impl FrameSlot {
  /// Converts and queues a camera image; `false` while no camera track runs or the image is
  /// malformed.
  pub(crate) fn push_camera(&self, frame: &CameraFrame) -> bool {
    self.push_with(|| camera_to_i420(frame))
  }

  /// Converts and queues a screen picture; `false` while no screen track runs or the picture is
  /// malformed.
  pub(crate) fn push_screen(&self, frame: &ScreenFrame) -> bool {
    self.push_with(|| screen_to_i420(frame))
  }

  /// Converts only while a track takes the frame: a phone pushes frames before and after.
  fn push_with(&self, convert: impl FnOnce() -> Result<VideoFrame, VideoError>) -> bool {
    let Some(sender) = self
      .lock()
      .maybe_current
      .as_ref()
      .map(|(_, sender)| sender.clone())
    else {
      return false;
    };
    match convert() {
      Ok(converted) => sender.send(converted),
      Err(error) => {
        tracing::debug!(%error, "pushed video frame dropped");
        false
      },
    }
  }

  pub(crate) fn clear(&self) {
    self.lock().maybe_current = None;
  }

  fn lock(&self) -> MutexGuard<'_, SlotState> {
    // Each update is a single assignment; a poisoned lock holds a consistent value.
    self
      .inner
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

/// Upright I420 from a camera image; odd dimensions lose their last column or row.
pub(crate) fn camera_to_i420(frame: &CameraFrame) -> Result<VideoFrame, VideoError> {
  let width = even(frame.width);
  let height = even(frame.height);
  let rotation = Rotation::from_degrees(frame.rotation_degrees);
  let (out_width, out_height) = if rotation.swaps_sides() {
    (height, width)
  } else {
    (width, height)
  };
  let mut data = Vec::with_capacity(out_width * out_height * 3 / 2);
  let y = Plane {
    data: &frame.y,
    row_stride: as_usize(frame.y_row_stride),
    pixel_stride: 1,
  };
  copy_rotated(&y, width, height, rotation, &mut data);
  for chroma in [&frame.u, &frame.v] {
    let plane = Plane {
      data: chroma,
      row_stride: as_usize(frame.uv_row_stride),
      pixel_stride: as_usize(frame.uv_pixel_stride).max(1),
    };
    copy_rotated(&plane, width / 2, height / 2, rotation, &mut data);
  }
  VideoFrame::new(
    VideoResolution {
      width: dimension(out_width),
      height: dimension(out_height),
    },
    frame.timestamp_us,
    data,
  )
}

/// I420 from a screen picture in RGBA (BT.601 limited range, as the encoders expect); odd
/// dimensions lose their last column or row. Each chroma sample averages its 2×2 pixels, so thin
/// coloured text keeps its hue.
pub(crate) fn screen_to_i420(frame: &ScreenFrame) -> Result<VideoFrame, VideoError> {
  let width = even(frame.width);
  let height = even(frame.height);
  let stride = as_usize(frame.row_stride);
  let pixel = |column: usize, row: usize| -> [i32; 3] {
    let at = row * stride + column * 4;
    match frame.rgba.get(at..at + 3) {
      Some(rgb) => [i32::from(rgb[0]), i32::from(rgb[1]), i32::from(rgb[2])],
      // A short buffer reads as black, as a short camera plane does.
      None => [0, 0, 0],
    }
  };
  let mut data = Vec::with_capacity(width * height * 3 / 2);
  for row in 0..height {
    for column in 0..width {
      let [r, g, b] = pixel(column, row);
      data.push(clamp(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16));
    }
  }
  let mut v_plane = Vec::with_capacity(width * height / 4);
  for row in (0..height).step_by(2) {
    for column in (0..width).step_by(2) {
      let mut sum = [0; 3];
      for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        let rgb = pixel(column + dx, row + dy);
        for channel in 0..3 {
          sum[channel] += rgb[channel];
        }
      }
      let [r, g, b] = sum.map(|channel| (channel + 2) / 4);
      data.push(clamp(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128));
      v_plane.push(clamp(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128));
    }
  }
  data.extend_from_slice(&v_plane);
  VideoFrame::new(
    VideoResolution {
      width: dimension(width),
      height: dimension(height),
    },
    frame.timestamp_us,
    data,
  )
}

/// RGBA pixels of an I420 frame (BT.601 limited range, as the encoders produce).
pub(crate) fn i420_to_rgba(frame: &VideoFrame) -> Vec<u8> {
  let resolution = frame.resolution();
  let width = usize::from(resolution.width);
  let height = usize::from(resolution.height);
  let (y_plane, u_plane, v_plane) = frame.planes();
  let mut rgba = Vec::with_capacity(width * height * 4);
  for row in 0..height {
    for column in 0..width {
      let chroma = (row / 2) * (width / 2) + column / 2;
      let luma = (i32::from(y_plane[row * width + column]) - 16) * 298;
      let u = i32::from(u_plane[chroma]) - 128;
      let v = i32::from(v_plane[chroma]) - 128;
      rgba.push(clamp((luma + 409 * v + 128) >> 8));
      rgba.push(clamp((luma - 100 * u - 208 * v + 128) >> 8));
      rgba.push(clamp((luma + 516 * u + 128) >> 8));
      rgba.push(u8::MAX);
    }
  }
  rgba
}

struct Plane<'a> {
  data: &'a [u8],
  row_stride: usize,
  pixel_stride: usize,
}

impl Plane<'_> {
  /// Missing bytes (a short last row, as some devices hand out) read as black.
  fn at(&self, column: usize, row: usize) -> u8 {
    self
      .data
      .get(row * self.row_stride + column * self.pixel_stride)
      .copied()
      .unwrap_or(0)
  }
}

impl Rotation {
  fn from_degrees(degrees: u32) -> Self {
    match degrees % 360 {
      90 => Self::Quarter,
      180 => Self::Half,
      270 => Self::ThreeQuarters,
      _ => Self::None,
    }
  }

  fn swaps_sides(self) -> bool {
    matches!(self, Self::Quarter | Self::ThreeQuarters)
  }
}

/// Appends the `width × height` plane, turned clockwise by `rotation`.
fn copy_rotated(plane: &Plane, width: usize, height: usize, rotation: Rotation, out: &mut Vec<u8>) {
  let (out_width, out_height) = if rotation.swaps_sides() {
    (height, width)
  } else {
    (width, height)
  };
  for y in 0..out_height {
    for x in 0..out_width {
      let (column, row) = match rotation {
        Rotation::None => (x, y),
        Rotation::Quarter => (y, height - 1 - x),
        Rotation::Half => (width - 1 - x, height - 1 - y),
        Rotation::ThreeQuarters => (width - 1 - y, x),
      };
      out.push(plane.at(column, row));
    }
  }
}

fn even(value: u32) -> usize {
  as_usize(value & !1)
}

fn as_usize(value: u32) -> usize {
  usize::try_from(value).unwrap_or(usize::MAX)
}

/// Frame sides fit `u16` (cameras deliver at most a few thousand pixels); larger ones fail in
/// [`VideoFrame::new`] as zero-sized.
fn dimension(value: usize) -> u16 {
  u16::try_from(value).unwrap_or(0)
}

fn clamp(value: i32) -> u8 {
  u8::try_from(value.clamp(0, 255)).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn interleaved_chroma_becomes_planar() {
    // 2x2 luma, one chroma sample in an NV12-style interleaved buffer (U then V).
    let frame = camera_frame(2, 2, vec![1, 2, 3, 4], vec![10, 20], vec![20], 2, 2, 0);
    let converted = camera_to_i420(&frame).unwrap();
    assert_eq!(converted.data(), &[1, 2, 3, 4, 10, 20]);
  }

  #[test]
  fn rotation_turns_clockwise_and_swaps_sides() {
    // 4x2 luma: rows [1 2 3 4] and [5 6 7 8].
    let y = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let quarter = camera_to_i420(&camera_frame(4, 2, y.clone(), vec![9, 9], vec![9, 9], 4, 2, 90)).unwrap();
    assert_eq!(quarter.resolution(), VideoResolution { width: 2, height: 4 });
    assert_eq!(&quarter.data()[..8], &[5, 1, 6, 2, 7, 3, 8, 4]);
    let three_quarters =
      camera_to_i420(&camera_frame(4, 2, y.clone(), vec![9, 9], vec![9, 9], 4, 2, 270)).unwrap();
    assert_eq!(&three_quarters.data()[..8], &[4, 8, 3, 7, 2, 6, 1, 5]);
    let half = camera_to_i420(&camera_frame(4, 2, y, vec![9, 9], vec![9, 9], 4, 2, 180)).unwrap();
    assert_eq!(&half.data()[..8], &[8, 7, 6, 5, 4, 3, 2, 1]);
  }

  #[test]
  fn odd_sides_and_padding_are_dropped() {
    // 3x3 with a row stride of 4: the last column and row go.
    let y = vec![1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0];
    let frame = camera_frame(3, 3, y, vec![10], vec![20], 4, 1, 0);
    let converted = camera_to_i420(&frame).unwrap();
    assert_eq!(converted.data(), &[1, 2, 4, 5, 10, 20]);
  }

  #[test]
  fn short_buffers_do_not_panic() {
    let frame = camera_frame(4, 4, vec![1], Vec::new(), Vec::new(), 4, 1, 90);
    assert_eq!(camera_to_i420(&frame).unwrap().data().len(), 24);
  }

  #[test]
  fn pushes_only_while_started() {
    let slot = FrameSlot::default();
    let frame = camera_frame(2, 2, vec![0; 4], vec![128], vec![128], 2, 1, 0);
    assert!(!slot.push_camera(&frame));
    let format = CaptureFormat {
      resolution: VideoResolution { width: 2, height: 2 },
      framerate: 15,
    };
    let mut camera = AppCapture::new(format, slot.clone());
    let (sender, receiver) = teamcraft_media::video_capture::frame_queue();
    camera.start(sender).unwrap();
    assert!(slot.push_camera(&frame));
    assert_eq!(receiver.recv().unwrap().resolution(), format.resolution);
    camera.stop();
    assert!(!slot.push_camera(&frame));
  }

  /// A camera track that replaces the running one starts before the old one stops.
  #[test]
  fn a_replaced_camera_leaves_the_slot_to_its_successor() {
    let slot = FrameSlot::default();
    let frame = camera_frame(2, 2, vec![0; 4], vec![128], vec![128], 2, 1, 0);
    let format = CaptureFormat {
      resolution: VideoResolution { width: 2, height: 2 },
      framerate: 15,
    };
    let mut old = AppCapture::new(format, slot.clone());
    let (old_sender, _old_receiver) = teamcraft_media::video_capture::frame_queue();
    old.start(old_sender).unwrap();
    let mut new = AppCapture::new(format, slot.clone());
    let (new_sender, new_receiver) = teamcraft_media::video_capture::frame_queue();
    new.start(new_sender).unwrap();
    old.stop();
    assert!(slot.push_camera(&frame));
    assert_eq!(new_receiver.recv().unwrap().resolution(), format.resolution);
    new.stop();
    assert!(!slot.push_camera(&frame));
  }

  #[test]
  fn screen_pictures_become_limited_range_i420() {
    // 2x2 white, then 2x2 pure red, side by side in one row-padded 4x2 picture.
    let white = [255, 255, 255, 255];
    let red = [255, 0, 0, 255];
    let mut rgba = Vec::new();
    for _ in 0..2 {
      rgba.extend_from_slice(&white);
      rgba.extend_from_slice(&white);
      rgba.extend_from_slice(&red);
      rgba.extend_from_slice(&red);
      rgba.extend_from_slice(&[9; 4]);
    }
    let frame = screen_frame(4, 2, rgba, 20);
    let converted = screen_to_i420(&frame).unwrap();
    assert_eq!(converted.resolution(), VideoResolution { width: 4, height: 2 });
    assert_eq!(&converted.data()[..4], &[235, 235, 82, 82]);
    // U then V, one sample per 2x2 block: white is neutral, red is low U, high V.
    assert_eq!(&converted.data()[8..], &[128, 90, 128, 240]);
  }

  #[test]
  fn short_or_odd_screen_pictures_do_not_panic() {
    let frame = screen_frame(5, 3, vec![255; 7], 20);
    let converted = screen_to_i420(&frame).unwrap();
    assert_eq!(converted.resolution(), VideoResolution { width: 4, height: 2 });
    assert_eq!(converted.data().len(), 12);
  }

  #[test]
  fn screen_frames_go_to_the_screen_track_only_while_it_runs() {
    let slot = FrameSlot::default();
    let frame = screen_frame(2, 2, vec![0; 16], 8);
    assert!(!slot.push_screen(&frame));
    let format = CaptureFormat {
      resolution: VideoResolution { width: 2, height: 2 },
      framerate: 15,
    };
    let mut screen = AppCapture::new(format, slot.clone());
    let (sender, receiver) = teamcraft_media::video_capture::frame_queue();
    screen.start(sender).unwrap();
    assert!(slot.push_screen(&frame));
    assert_eq!(receiver.recv().unwrap().resolution(), format.resolution);
    screen.stop();
    assert!(!slot.push_screen(&frame));
  }

  #[test]
  fn grey_and_white_convert_to_rgba() {
    let resolution = VideoResolution { width: 2, height: 2 };
    let grey = VideoFrame::new(resolution, 0, vec![126, 126, 235, 235, 128, 128]).unwrap();
    let rgba = i420_to_rgba(&grey);
    assert_eq!(rgba.len(), 16);
    assert_eq!(&rgba[..4], &[128, 128, 128, 255]);
    assert_eq!(&rgba[8..12], &[255, 255, 255, 255]);
  }

  fn screen_frame(width: u32, height: u32, rgba: Vec<u8>, row_stride: u32) -> ScreenFrame {
    ScreenFrame {
      width,
      height,
      rgba,
      row_stride,
      timestamp_us: 0,
    }
  }

  #[allow(clippy::too_many_arguments)]
  fn camera_frame(
    width: u32,
    height: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    y_row_stride: u32,
    uv_pixel_stride: u32,
    rotation_degrees: u32,
  ) -> CameraFrame {
    let uv_row_stride = (width / 2) * uv_pixel_stride;
    CameraFrame {
      width,
      height,
      y,
      u,
      v,
      y_row_stride,
      uv_row_stride,
      uv_pixel_stride,
      rotation_degrees,
      timestamp_us: 0,
    }
  }
}
