//! macOS: the camera stream for modes nokhwa's own stream can't deliver (#914). nokhwa asks
//! AVFoundation's video data output for the camera's own format; for an MJPEG mode (all that many
//! USB webcams offer) that is JPEG, which the output doesn't deliver, so the camera would send
//! nothing. This stream is built like nokhwa's, from its AVFoundation bindings, but asks for
//! packed YUV (`yuvs`, YUYV order) whatever the camera sends: AVFoundation decodes the camera's
//! JPEG into it. `yuvs` is also what nokhwa's stream asks for for the built-in cameras' modes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use flume::{Receiver, RecvTimeoutError, Sender};
use nokhwa::NokhwaError;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat};
use nokhwa_bindings_macos::{
  AVCaptureDevice, AVCaptureDeviceInput, AVCaptureSession, AVCaptureVideoCallback, AVCaptureVideoDataOutput,
};

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video::VideoFrame;
use crate::video_capture::CaptureError;

/// How long a read waits for a frame before the camera thread looks for a stop again.
const FRAME_WAIT: Duration = Duration::from_millis(500);
/// Packed YUV 4:2:2 has two bytes per pixel.
const YUYV_BYTES_PER_PIXEL: usize = 2;

/// What nokhwa's bindings hand over per frame: the pixel buffer's bytes, a format that is always
/// the same placeholder, and when the frame was captured.
type FrameMessage = (Vec<u8>, FrameFormat, Option<Duration>);

/// A running AVFoundation session that delivers a camera's frames as packed YUV.
pub(super) struct ConvertedStream {
  session: AVCaptureSession,
  input: AVCaptureDeviceInput,
  output: AVCaptureVideoDataOutput,
  /// The output's delegate sends through `_sender`, which it reaches by pointer: both stay as
  /// long as the stream.
  _callback: AVCaptureVideoCallback,
  _sender: Arc<Sender<FrameMessage>>,
  frames: Receiver<FrameMessage>,
  width: u32,
  height: u32,
}

impl ConvertedStream {
  /// Starts `camera` in `mode` (the camera's own format, any) with its frames as packed YUV.
  pub(super) fn start(camera: &CameraIndex, mode: CameraFormat) -> Result<Self, NokhwaError> {
    let mut device = AVCaptureDevice::new(camera)?;
    let input = AVCaptureDeviceInput::new(&device)?;
    let session = AVCaptureSession::new();
    session.begin_configuration();
    session.add_input(&input)?;
    // Adding the input sets the device to the session's preset: the mode is set again after it,
    // as nokhwa's stream does.
    device.set_all(mode)?;
    let (sender, frames) = flume::unbounded();
    let sender = Arc::new(sender);
    let callback = AVCaptureVideoCallback::new(c"teamcraft-camera", &sender)?;
    let output = AVCaptureVideoDataOutput::new();
    output.add_delegate(&callback)?;
    output.set_frame_format(FrameFormat::YUYV)?;
    session.add_output(&output)?;
    session.commit_configuration();
    session.start()?;
    let resolution = mode.resolution();
    Ok(Self {
      session,
      input,
      output,
      _callback: callback,
      _sender: sender,
      frames,
      width: resolution.width(),
      height: resolution.height(),
    })
  }

  /// The newest frame, waiting at most [`FRAME_WAIT`] for one; `Ok(None)` when none came or it
  /// couldn't be converted.
  pub(super) fn read(&mut self, start: Instant) -> Result<Option<VideoFrame>, CaptureError> {
    let (data, ..) = match self.frames.recv_timeout(FRAME_WAIT) {
      Ok(message) => message,
      // Nothing yet: the camera thread looks for a stop and reads again.
      Err(RecvTimeoutError::Timeout) => return Ok(None),
      Err(RecvTimeoutError::Disconnected) => {
        return Err(CaptureError::Device("the camera stream ended".into()));
      },
    };
    // Frames that queued up meanwhile are old: only the newest counts, as in nokhwa's stream.
    let data = self.frames.drain().last().map_or(data, |(newer, ..)| newer);
    let picture = RawPicture {
      format: PixelFormat::Yuyv,
      width: self.width,
      height: self.height,
      stride: packed_stride(data.len(), self.width, self.height),
      data: &data,
    };
    Ok(super::frame_of(&picture, start))
  }

  pub(super) fn close(&mut self) {
    self.session.stop();
    self.session.remove_output(&self.output);
    self.session.remove_input(&self.input);
  }
}

/// The row length in a packed YUV buffer of `len` bytes: AVFoundation may pad rows, and then the
/// buffer holds `height` longer rows; 0 (tight rows) otherwise.
fn packed_stride(len: usize, width: u32, height: u32) -> usize {
  let row = width as usize * YUYV_BYTES_PER_PIXEL;
  let rows = height as usize;
  if rows == 0 || !len.is_multiple_of(rows) {
    return 0;
  }
  let stride = len / rows;
  if stride > row { stride } else { 0 }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tight_rows_need_no_stride() {
    assert_eq!(packed_stride(1280 * 2 * 720, 1280, 720), 0);
  }

  #[test]
  fn padded_rows_are_read_with_their_length() {
    // 1366 pixels are 2732 bytes, padded to 2752 (a multiple of 64).
    assert_eq!(packed_stride(2752 * 768, 1366, 768), 2752);
  }

  #[test]
  fn buffers_that_are_no_whole_rows_or_too_short_count_as_tight() {
    assert_eq!(packed_stride(1280 * 2 * 720 + 7, 1280, 720), 0);
    assert_eq!(packed_stride(100, 1280, 720), 0);
    assert_eq!(packed_stride(100, 1280, 0), 0);
  }

  #[test]
  fn converted_frames_become_i420() {
    // Mid grey in YUYV order: Y U Y V.
    let data = [128_u8, 128, 128, 128].repeat(32 * 16 / 2);
    let picture = RawPicture {
      format: PixelFormat::Yuyv,
      width: 32,
      height: 16,
      stride: packed_stride(data.len(), 32, 16),
      data: &data,
    };
    let frame = super::super::frame_of(&picture, Instant::now()).unwrap();
    assert_eq!((frame.resolution().width, frame.resolution().height), (32, 16));
  }
}
