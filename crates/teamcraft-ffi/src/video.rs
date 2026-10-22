//! Video glue between the phone's camera and display and `teamcraft-media`: the app's camera
//! frames (Android `YUV_420_888`, any row and pixel stride, rotated as the sensor sits) become
//! upright I420 for the encoder, and decoded frames become RGBA for a bitmap.

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

/// A decoded frame of another device's video, ready for a bitmap.
#[derive(Clone, uniffi::Record)]
pub struct VideoFrameRgba {
  /// As in [`crate::RemoteVideoTrack`].
  pub stream_id: String,
  pub width: u32,
  pub height: u32,
  /// `width × height` pixels, 4 bytes each (R, G, B, A).
  pub rgba: Vec<u8>,
}

/// The camera as a capture source for the encoder: the app pushes frames, [`VideoCapture::start`]
/// hands over the queue they go to.
pub(crate) struct AppCamera {
  format: CaptureFormat,
  slot: CameraSlot,
}

/// Where pushed camera frames go while the camera track runs.
#[derive(Clone, Default)]
pub(crate) struct CameraSlot {
  inner: Arc<Mutex<Option<FrameSender>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rotation {
  None,
  Quarter,
  Half,
  ThreeQuarters,
}

impl AppCamera {
  pub(crate) fn new(format: CaptureFormat, slot: CameraSlot) -> Self {
    Self { format, slot }
  }
}

impl VideoCapture for AppCamera {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    *self.slot.lock() = Some(frames);
    Ok(())
  }

  fn stop(&mut self) {
    *self.slot.lock() = None;
  }
}

impl CameraSlot {
  /// Converts and queues `frame`; `false` while no camera track runs or the frame is malformed.
  pub(crate) fn push(&self, frame: &CameraFrame) -> bool {
    let Some(sender) = self.lock().clone() else {
      return false;
    };
    match camera_to_i420(frame) {
      Ok(converted) => sender.send(converted),
      Err(error) => {
        tracing::debug!(%error, "camera frame dropped");
        false
      },
    }
  }

  /// A camera track takes frames now.
  pub(crate) fn is_running(&self) -> bool {
    self.lock().is_some()
  }

  pub(crate) fn clear(&self) {
    *self.lock() = None;
  }

  fn lock(&self) -> MutexGuard<'_, Option<FrameSender>> {
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
    let slot = CameraSlot::default();
    let frame = camera_frame(2, 2, vec![0; 4], vec![128], vec![128], 2, 1, 0);
    assert!(!slot.push(&frame));
    let format = CaptureFormat {
      resolution: VideoResolution { width: 2, height: 2 },
      framerate: 15,
    };
    let mut camera = AppCamera::new(format, slot.clone());
    let (sender, receiver) = teamcraft_media::video_capture::frame_queue();
    camera.start(sender).unwrap();
    assert!(slot.push(&frame));
    assert_eq!(receiver.recv().unwrap().resolution(), format.resolution);
    camera.stop();
    assert!(!slot.push(&frame));
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
