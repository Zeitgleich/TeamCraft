//! Cameras and screens through the xdg-desktop-portal and PipeWire (Linux): the desktop asks the
//! user (a screen/window picker, or camera permission), then hands out a PipeWire remote with
//! just that source. Works in Flatpak and on Wayland, where nothing else can capture the screen.
//!
//! The portal talk is D-Bus (async, on a small runtime the capture keeps for its session); the
//! frames come from a PipeWire stream on a thread of its own, converted to I420 and queued.

use std::os::fd::OwnedFd;
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
use tracing::{debug, trace, warn};

use crate::capture_convert::{PixelFormat, RawPicture};
use crate::video::VideoResolution;
use crate::video_capture::{CaptureError, CaptureFormat, FrameSender, VideoCapture};

/// What to capture through the portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortalSource {
  Camera,
  /// A monitor or a window, as the user picks.
  Screen,
}

/// A portal-granted camera or screen.
pub struct PortalCapture {
  source: PortalSource,
  format: CaptureFormat,
  /// Keeps the D-Bus connection (and with it the screencast session) alive.
  runtime: tokio::runtime::Runtime,
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
  format: VideoInfoRaw,
  frames: FrameSender,
  start: Instant,
}

impl PortalCapture {
  /// Asks for camera access. `wanted` is a hint; the camera's offer decides.
  pub fn camera(wanted: CaptureFormat) -> Result<Self, CaptureError> {
    let runtime = runtime()?;
    let fd = runtime.block_on(async {
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
    Ok(Self {
      source: PortalSource::Camera,
      format: wanted,
      runtime,
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
    let runtime = runtime()?;
    let (proxy, session, node, size, fd, maybe_token) = runtime.block_on(async {
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
        .map_err(|_| CaptureError::Denied("screen"))?;
      let streams = proxy
        .start(&session, None, Default::default())
        .await
        .map_err(portal_error)?
        .response()
        .map_err(|_| CaptureError::Denied("screen"))?;
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
    let capture = Self {
      source: PortalSource::Screen,
      format,
      runtime,
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
      if let Err(error) = self.runtime.block_on(screencast.session.close()) {
        debug!(%error, "screencast session not closed");
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
    format: VideoInfoRaw::default(),
    frames,
    start: Instant::now(),
  };
  let loop_for_quit = main_loop.clone();
  let _listener = stream
    .add_local_listener_with_user_data(state)
    .param_changed(|_, state, id, param| {
      let Some(param) = param else {
        return;
      };
      if id != spa::param::ParamType::Format.as_raw() {
        return;
      }
      if let Err(error) = state.format.parse(param) {
        warn!(?error, "unreadable video format");
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
      let Some(pixel_format) = pixel_format_of(state.format.format()) else {
        trace!(format = ?state.format.format(), "buffer in an unconverted format");
        return;
      };
      let Some(bytes) = data.data() else {
        trace!("buffer not mapped");
        return;
      };
      let Some(bytes) = bytes.get(offset..offset + size) else {
        trace!(offset, size, mapped = bytes.len(), "buffer chunk out of range");
        return;
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
          if !state.frames.send(frame) {
            loop_for_quit.quit();
          }
        },
        Err(error) => debug!(%error, "captured frame dropped"),
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

fn runtime() -> Result<tokio::runtime::Runtime, CaptureError> {
  tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()
    .map_err(|error| CaptureError::Device(error.to_string()))
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
}
