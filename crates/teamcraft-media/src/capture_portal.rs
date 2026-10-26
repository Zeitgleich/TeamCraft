//! Cameras and screens through the xdg-desktop-portal and PipeWire (Linux): the desktop asks the
//! user (a screen/window picker, or camera permission), then hands out a PipeWire remote with
//! just that source. Works in Flatpak and on Wayland, where nothing else can capture the screen.
//!
//! The portal talk is D-Bus, async on one runtime of its own that lives as long as the process;
//! the frames come from a PipeWire stream on a thread of its own, converted to I420 and queued.
//!
//! One runtime for every portal request, not one per capture: the portal library keeps a single
//! D-Bus connection for the whole process, and that connection reads its messages on a task of
//! the runtime it was opened on. A runtime that is idle (or gone) after the first request leaves
//! every later request, the next screen picker among them, waiting for an answer nobody reads
//! (issue #683).

use std::os::fd::OwnedFd;
use std::sync::OnceLock;
use std::thread::JoinHandle;
use std::time::Instant;

use ashpd::desktop::PersistMode;
use ashpd::desktop::camera::Camera;
use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};
use pw::spa::pod::Pod;
use tracing::{debug, info, trace, warn};

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video::VideoResolution;
use crate::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};
use crate::video_pipeline::RepeatedWarning;

/// What to capture through the portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSource {
  Camera,
  /// A monitor or a window, as the user picks.
  Screen,
}

/// Worker threads of the portal runtime: D-Bus messages only, so one is plenty.
const PORTAL_WORKER_THREADS: usize = 1;

/// The runtime of all portal requests (see the module docs); built on first use.
static PORTAL_RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

/// A portal-granted camera or screen.
pub struct PortalCapture {
  source: PortalSource,
  format: CaptureFormat,
  /// Keeps the screencast session (and with it the share) open.
  maybe_session: Option<ScreencastSession>,
  maybe_remote: Option<Remote>,
  maybe_running: Option<(pw::channel::Sender<()>, JoinHandle<()>)>,
}

struct ScreencastSession {
  _proxy: Screencast,
  session: ashpd::desktop::Session<Screencast>,
}

/// A PipeWire remote and the node to read from (`None`: let PipeWire pick, e.g. the default
/// camera on the camera portal's remote).
struct Remote {
  fd: OwnedFd,
  maybe_node: Option<u32>,
}

/// State of the PipeWire stream callbacks.
struct StreamState {
  source: PortalSource,
  format: VideoInfoRaw,
  frames: FrameSender,
  start: Instant,
  delivered: u64,
  /// Pictures that can't be used, logged at most every few seconds.
  warning: RepeatedWarning,
}

/// What a PipeWire buffer's chunk holds.
#[derive(Debug, PartialEq, Eq)]
enum ChunkContent {
  /// The picture's bytes within the mapped data.
  Picture(std::ops::Range<usize>),
  /// No picture: a metadata-only buffer (some compositors send these when only the cursor moved)
  /// or one the producer marked as damaged.
  Empty,
  /// The chunk claims bytes beyond the mapped data.
  OutOfRange,
}

impl PortalCapture {
  /// Asks for camera access. `wanted` is a hint; the camera's offer decides.
  pub fn camera(wanted: CaptureFormat) -> Result<Self, CaptureError> {
    let fd = portal_runtime()?.block_on(async {
      let camera = Camera::new().await.map_err(portal_error)?;
      if !camera.is_present().await.map_err(portal_error)? {
        return Err(CaptureError::NoDevice("camera"));
      }
      camera
        .request_access(Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .map_err(|_| CaptureError::Denied("camera"))?;
      camera
        .open_pipe_wire_remote(Default::default())
        .await
        .map_err(portal_error)
    })?;
    debug!("camera portal granted a PipeWire remote");
    Ok(Self {
      source: PortalSource::Camera,
      format: wanted,
      maybe_session: None,
      maybe_remote: Some(Remote { fd, maybe_node: None }),
      maybe_running: None,
    })
  }

  /// Lets the user pick a monitor or window. With `maybe_restore_token` from an earlier share the
  /// portal may skip the picker; the returned token restores this choice next time.
  pub fn screen(
    wanted: CaptureFormat,
    maybe_restore_token: Option<&str>,
  ) -> Result<(Self, Option<String>), CaptureError> {
    let (proxy, session, node, size, fd, maybe_token) = portal_runtime()?.block_on(async {
      let proxy = Screencast::new().await.map_err(portal_error)?;
      let session = proxy
        .create_session(Default::default())
        .await
        .map_err(portal_error)?;
      proxy
        .select_sources(
          &session,
          SelectSourcesOptions::default()
            .set_cursor_mode(CursorMode::Embedded)
            .set_sources(SourceType::Monitor | SourceType::Window)
            .set_multiple(false)
            .set_persist_mode(PersistMode::ExplicitlyRevoked)
            .set_restore_token(maybe_restore_token),
        )
        .await
        .map_err(portal_error)?
        .response()
        .map_err(picker_error)?;
      let streams = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .map_err(picker_error)?;
      let stream = streams
        .streams()
        .first()
        .ok_or(CaptureError::NoDevice("screen"))?;
      let (node, size) = (stream.pipe_wire_node_id(), stream.size());
      let maybe_token = streams.restore_token().map(str::to_owned);
      let fd = proxy
        .open_pipe_wire_remote(&session, Default::default())
        .await
        .map_err(portal_error)?;
      Ok::<_, CaptureError>((proxy, session, node, size, fd, maybe_token))
    })?;
    // The portal reports the source's size; screens are captured at it (up to the wanted size).
    let format = match size {
      Some((width, height)) if width > 0 && height > 0 => CaptureFormat {
        resolution: VideoResolution {
          width: u16::try_from(width & !1).unwrap_or(wanted.resolution.width),
          height: u16::try_from(height & !1).unwrap_or(wanted.resolution.height),
        },
        framerate: wanted.framerate,
      },
      _ => wanted,
    };
    debug!(node, ?size, "screencast portal granted a stream");
    let capture = Self {
      source: PortalSource::Screen,
      format,
      maybe_session: Some(ScreencastSession {
        _proxy: proxy,
        session,
      }),
      maybe_remote: Some(Remote {
        fd,
        maybe_node: Some(node),
      }),
      maybe_running: None,
    };
    Ok((capture, maybe_token))
  }
}

impl VideoCapture for PortalCapture {
  fn format(&self) -> CaptureFormat {
    self.format
  }

  /// The portal grants one PipeWire remote, so a portal capture starts once.
  fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
    let remote = self
      .maybe_remote
      .take()
      .ok_or_else(|| CaptureError::Device("a portal capture can only be started once".into()))?;
    let (stop, stop_rx) = pw::channel::channel::<()>();
    let source = self.source;
    let format = self.format;
    let thread = std::thread::Builder::new()
      .name("pipewire-capture".into())
      .spawn(move || {
        if let Err(error) = run_stream(remote, source, format, frames, stop_rx) {
          warn!(%error, ?source, "PipeWire capture ended");
        }
      })
      .map_err(|error| CaptureError::Device(error.to_string()))?;
    self.maybe_running = Some((stop, thread));
    Ok(())
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      // A closed channel means the loop already ended.
      let _ = stop.send(());
      if thread.join().is_err() {
        warn!("PipeWire capture thread panicked");
      }
    }
  }
}

impl Drop for PortalCapture {
  fn drop(&mut self) {
    self.stop();
    if let Some(screencast) = self.maybe_session.take() {
      // Ends the share in the desktop's indicator too; failure just leaves it to time out.
      match portal_runtime() {
        Ok(runtime) => {
          if let Err(error) = runtime.block_on(screencast.session.close()) {
            debug!(%error, "screencast session not closed");
          }
        },
        Err(error) => debug!(%error, "screencast session not closed"),
      }
    }
  }
}

/// Runs the PipeWire loop until `stop` fires or the stream breaks.
fn run_stream(
  remote: Remote,
  source: PortalSource,
  format: CaptureFormat,
  frames: FrameSender,
  stop: pw::channel::Receiver<()>,
) -> Result<(), pw::Error> {
  pw::init();
  let main_loop = pw::main_loop::MainLoopRc::new(None)?;
  let context = pw::context::ContextRc::new(&main_loop, None)?;
  let core = context.connect_fd_rc(remote.fd, None)?;
  let role = match source {
    PortalSource::Camera => "Camera",
    PortalSource::Screen => "Screen",
  };
  let stream = pw::stream::StreamBox::new(
    &core,
    "teamcraft-capture",
    properties! {
      *pw::keys::MEDIA_TYPE => "Video",
      *pw::keys::MEDIA_CATEGORY => "Capture",
      *pw::keys::MEDIA_ROLE => role,
    },
  )?;
  let state = StreamState {
    source,
    format: VideoInfoRaw::default(),
    frames,
    start: Instant::now(),
    delivered: 0,
    warning: RepeatedWarning::default(),
  };
  let loop_for_quit = main_loop.clone();
  let loop_for_error = main_loop.clone();
  let _listener = stream
    .add_local_listener_with_user_data(state)
    .state_changed(move |_, state, old, new| {
      debug!(source = ?state.source, ?old, ?new, "PipeWire stream state");
      if let pw::stream::StreamState::Error(error) = new {
        // The encoder thread sees the queue close and reports the capture as ended.
        warn!(source = ?state.source, %error, "PipeWire stream failed");
        loop_for_error.quit();
      }
    })
    .param_changed(|_, state, id, param| {
      let Some(param) = param else {
        return;
      };
      if id != spa::param::ParamType::Format.as_raw() {
        return;
      }
      match state.format.parse(param) {
        Ok(_) => {
          let size = state.format.size();
          let framerate = state.format.framerate();
          info!(
            source = ?state.source,
            format = ?state.format.format(),
            width = size.width,
            height = size.height,
            framerate = framerate.num,
            "PipeWire stream format"
          );
          if pixel_format_of(state.format.format()).is_none() {
            warn!(format = ?state.format.format(), "PipeWire offers a video format we can't convert");
          }
        },
        Err(error) => warn!(?error, "unreadable video format"),
      }
    })
    .process(move |stream, state| {
      let Some(mut buffer) = stream.dequeue_buffer() else {
        trace!("process without a buffer");
        return;
      };
      let datas = buffer.datas_mut();
      let Some(data) = datas.first_mut() else {
        trace!("buffer without data");
        return;
      };
      let chunk = data.chunk();
      let (offset, size, stride) = (chunk.offset() as usize, chunk.size() as usize, chunk.stride());
      trace!(offset, size, stride, "buffer");
      let is_corrupted = chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED);
      let Some(pixel_format) = pixel_format_of(state.format.format()) else {
        state.warning.warn(|suppressed| {
          warn!(format = ?state.format.format(), suppressed, "PipeWire buffer in a format we can't convert");
        });
        return;
      };
      let Some(mapped) = data.data() else {
        // A DMA-BUF the stream can't map: no picture ever comes this way.
        state
          .warning
          .warn(|suppressed| warn!(source = ?state.source, suppressed, "PipeWire buffer not mapped"));
        return;
      };
      let bytes = match picture_range(mapped.len(), offset, size, is_corrupted) {
        ChunkContent::Picture(range) => &mapped[range],
        // Metadata-only buffers (a cursor move, nothing new on screen) and damaged ones.
        ChunkContent::Empty => return,
        ChunkContent::OutOfRange => {
          let mapped_len = mapped.len();
          state.warning.warn(|suppressed| {
            warn!(
              offset,
              size,
              mapped = mapped_len,
              suppressed,
              "PipeWire buffer chunk out of range"
            );
          });
          return;
        },
      };
      let picture = RawPicture {
        format: pixel_format,
        width: state.format.size().width,
        height: state.format.size().height,
        stride: usize::try_from(stride).unwrap_or(0),
        data: bytes,
      };
      let timestamp_us = u64::try_from(state.start.elapsed().as_micros()).unwrap_or(u64::MAX);
      match picture.to_i420(timestamp_us) {
        Ok(frame) => {
          if state.delivered == 0 {
            debug!(source = ?state.source, "first PipeWire frame");
          }
          state.delivered += 1;
          if !state.frames.send(frame) {
            loop_for_quit.quit();
          }
        },
        Err(error) => {
          let source = state.source;
          state
            .warning
            .warn(|suppressed| warn!(%error, ?source, suppressed, "captured frame dropped"));
        },
      }
    })
    .register()?;
  let values = format_params(format)?;
  let mut params = [Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];
  stream.connect(
    spa::utils::Direction::Input,
    remote.maybe_node,
    pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
    &mut params,
  )?;
  let loop_for_stop = main_loop.clone();
  let _stop = stop.attach(main_loop.loop_(), move |()| loop_for_stop.quit());
  main_loop.run();
  Ok(())
}

/// The formats we convert, the wanted size (any size allowed) and frame rate.
fn format_params(format: CaptureFormat) -> Result<Vec<u8>, pw::Error> {
  let size = spa::utils::Rectangle {
    width: u32::from(format.resolution.width),
    height: u32::from(format.resolution.height),
  };
  let object = spa::pod::object!(
    spa::utils::SpaTypes::ObjectParamFormat,
    spa::param::ParamType::EnumFormat,
    spa::pod::property!(
      spa::param::format::FormatProperties::MediaType,
      Id,
      spa::param::format::MediaType::Video
    ),
    spa::pod::property!(
      spa::param::format::FormatProperties::MediaSubtype,
      Id,
      spa::param::format::MediaSubtype::Raw
    ),
    spa::pod::property!(
      spa::param::format::FormatProperties::VideoFormat,
      Choice,
      Enum,
      Id,
      VideoFormat::BGRx,
      VideoFormat::BGRx,
      VideoFormat::RGBx,
      VideoFormat::BGRA,
      VideoFormat::RGBA,
      VideoFormat::YUY2,
      VideoFormat::I420,
      VideoFormat::NV12
    ),
    spa::pod::property!(
      spa::param::format::FormatProperties::VideoSize,
      Choice,
      Range,
      Rectangle,
      size,
      spa::utils::Rectangle { width: 2, height: 2 },
      spa::utils::Rectangle {
        width: 8192,
        height: 8192
      }
    ),
    spa::pod::property!(
      spa::param::format::FormatProperties::VideoFramerate,
      Choice,
      Range,
      Fraction,
      spa::utils::Fraction {
        num: u32::from(format.framerate.max(1)),
        denom: 1
      },
      spa::utils::Fraction { num: 0, denom: 1 },
      spa::utils::Fraction { num: 120, denom: 1 }
    ),
  );
  spa::pod::serialize::PodSerializer::serialize(
    std::io::Cursor::new(Vec::new()),
    &spa::pod::Value::Object(object),
  )
  .map(|(cursor, _)| cursor.into_inner())
  .map_err(|_| pw::Error::CreationFailed)
}

fn pixel_format_of(format: VideoFormat) -> Option<PixelFormat> {
  match format {
    VideoFormat::BGRx | VideoFormat::BGRA => Some(PixelFormat::Bgrx),
    VideoFormat::RGBx | VideoFormat::RGBA => Some(PixelFormat::Rgbx),
    VideoFormat::YUY2 => Some(PixelFormat::Yuyv),
    VideoFormat::I420 => Some(PixelFormat::I420),
    VideoFormat::NV12 => Some(PixelFormat::Nv12),
    _ => None,
  }
}

/// Where the picture of a buffer is: `size` bytes from `offset` of `mapped_len` mapped bytes.
fn picture_range(mapped_len: usize, offset: usize, size: usize, is_corrupted: bool) -> ChunkContent {
  if size == 0 || is_corrupted {
    return ChunkContent::Empty;
  }
  match offset.checked_add(size) {
    Some(end) if end <= mapped_len => ChunkContent::Picture(offset..end),
    _ => ChunkContent::OutOfRange,
  }
}

/// The portal runtime. Its worker keeps reading the shared D-Bus connection between requests,
/// so a request made from any thread later is answered. Callers block on it from plain threads
/// (the capture's blocking task, the encoder thread), never from async code.
fn portal_runtime() -> Result<&'static tokio::runtime::Runtime, CaptureError> {
  PORTAL_RUNTIME
    .get_or_init(|| {
      tokio::runtime::Builder::new_multi_thread()
        .worker_threads(PORTAL_WORKER_THREADS)
        .thread_name("xdg-portal")
        .enable_all()
        .build()
        .map_err(|error| error.to_string())
    })
    .as_ref()
    .map_err(|error| CaptureError::Device(format!("portal runtime: {error}")))
}

/// The screen picker's answer: closed without a choice, or refused.
fn picker_error(error: ashpd::Error) -> CaptureError {
  match error {
    ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => CaptureError::Cancelled("screen"),
    _ => CaptureError::Denied("screen"),
  }
}

fn portal_error(error: ashpd::Error) -> CaptureError {
  match error {
    ashpd::Error::Response(_) => CaptureError::Denied("capture"),
    other => CaptureError::Device(format!("portal: {other}")),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn format_params_serialize() {
    let bytes = format_params(CaptureFormat {
      resolution: VideoResolution {
        width: 1280,
        height: 720,
      },
      framerate: 30,
    })
    .unwrap();
    assert!(Pod::from_bytes(&bytes).is_some());
  }

  #[test]
  fn pipewire_formats_map_to_converters() {
    assert_eq!(pixel_format_of(VideoFormat::BGRx), Some(PixelFormat::Bgrx));
    assert_eq!(pixel_format_of(VideoFormat::YUY2), Some(PixelFormat::Yuyv));
    assert_eq!(pixel_format_of(VideoFormat::RGB), None);
  }

  #[test]
  fn buffers_without_a_usable_picture_are_told_apart() {
    assert_eq!(picture_range(100, 10, 50, false), ChunkContent::Picture(10..60));
    assert_eq!(picture_range(100, 0, 100, false), ChunkContent::Picture(0..100));
    // A cursor move only, and a damaged buffer: nothing to convert, nothing to warn about.
    assert_eq!(picture_range(100, 0, 0, false), ChunkContent::Empty);
    assert_eq!(picture_range(100, 0, 100, true), ChunkContent::Empty);
    assert_eq!(picture_range(100, 60, 50, false), ChunkContent::OutOfRange);
    assert_eq!(picture_range(100, usize::MAX, 2, false), ChunkContent::OutOfRange);
  }

  #[test]
  fn a_closed_picker_is_a_cancel_and_anything_else_a_refusal() {
    use ashpd::desktop::ResponseError;

    assert!(matches!(
      picker_error(ashpd::Error::Response(ResponseError::Cancelled)),
      CaptureError::Cancelled("screen")
    ));
    assert!(matches!(
      picker_error(ashpd::Error::Response(ResponseError::Other)),
      CaptureError::Denied("screen")
    ));
  }

  #[test]
  fn the_portal_runtime_is_shared_and_keeps_running() {
    let first = portal_runtime().unwrap();
    let second = portal_runtime().unwrap();
    assert!(std::ptr::eq(first, second), "one runtime for every request");
    // A task spawned during one request still runs after it, while nobody blocks on the
    // runtime: what the shared D-Bus connection's reader needs (issue #683).
    let (sent, received) = std::sync::mpsc::channel();
    first.block_on(async move {
      tokio::spawn(async move {
        tokio::task::yield_now().await;
        let _ = sent.send(());
      });
    });
    assert!(received.recv_timeout(std::time::Duration::from_secs(10)).is_ok());
  }
}
