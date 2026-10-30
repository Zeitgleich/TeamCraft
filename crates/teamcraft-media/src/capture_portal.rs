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

/// The pixel formats a stream may deliver uncompressed, the first preferred.
const RAW_FORMATS: [VideoFormat; 7] = [
  VideoFormat::BGRx,
  VideoFormat::RGBx,
  VideoFormat::BGRA,
  VideoFormat::RGBA,
  VideoFormat::YUY2,
  VideoFormat::I420,
  VideoFormat::NV12,
];
/// Largest picture side a stream may deliver.
const MAX_SIDE: u32 = 8192;
/// Highest frame rate a stream may deliver.
const MAX_FRAMERATE: u32 = 120;

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
  /// How the negotiated format's pictures are converted; `None` before the format is known, or
  /// for one we can't convert.
  maybe_pixel_format: Option<PixelFormat>,
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
    maybe_pixel_format: None,
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
    .param_changed(|stream, state, id, param| {
      let Some(param) = param else {
        return;
      };
      if id != spa::param::ParamType::Format.as_raw() {
        return;
      }
      // The raw video info reads size and frame rate of compressed (MJPEG) formats too.
      match (spa::param::format_utils::parse_format(param), state.format.parse(param)) {
        (Ok((_, subtype)), Ok(_)) => {
          let size = state.format.size();
          let framerate = state.format.framerate();
          state.maybe_pixel_format = pixel_format_of(subtype, state.format.format());
          info!(
            source = ?state.source,
            ?subtype,
            format = ?state.format.format(),
            width = size.width,
            height = size.height,
            framerate = framerate.num,
            "PipeWire stream format"
          );
          if state.maybe_pixel_format.is_none() {
            warn!(?subtype, format = ?state.format.format(), "PipeWire offers a video format we can't convert");
          }
          if let Err(error) = announce_buffer_types(stream, state.source) {
            warn!(%error, source = ?state.source, "PipeWire buffer types not announced");
          }
        },
        (Err(error), _) | (_, Err(error)) => warn!(?error, "unreadable video format"),
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
      let Some(pixel_format) = state.maybe_pixel_format else {
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
  let values = format_params(source, format)?;
  let mut params = values
    .iter()
    .map(|value| Pod::from_bytes(value).ok_or(pw::Error::CreationFailed))
    .collect::<Result<Vec<_>, _>>()?;
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

/// The formats the stream takes, best first: PipeWire links the first one the source offers.
///
/// Screens: any size, any rate, the wanted ones preferred. Cameras first in the wanted size, raw
/// at the wanted frame rate or above, else MJPEG at the wanted rate, before anything they offer:
/// a USB camera lists its large raw modes first, at a few frames a second (1080p YUYV at 5 fps,
/// 720p at 8), and delivers the wanted size smoothly only as MJPEG. The V4L2 path picks the same
/// way (`capture_negotiate::rank_modes`).
fn format_params(source: PortalSource, wanted: CaptureFormat) -> Result<Vec<Vec<u8>>, pw::Error> {
  let size = spa::utils::Rectangle {
    width: u32::from(wanted.resolution.width),
    height: u32::from(wanted.resolution.height),
  };
  let rate = |num: u32| spa::utils::Fraction { num, denom: 1 };
  let wanted_rate = u32::from(wanted.framerate.max(1));
  let any_size = spa::pod::Value::Choice(spa::pod::ChoiceValue::Rectangle(spa::utils::Choice(
    spa::utils::ChoiceFlags::empty(),
    spa::utils::ChoiceEnum::Range {
      default: size,
      min: spa::utils::Rectangle { width: 2, height: 2 },
      max: spa::utils::Rectangle {
        width: MAX_SIDE,
        height: MAX_SIDE,
      },
    },
  )));
  let rate_from = |min: u32| {
    spa::pod::Value::Choice(spa::pod::ChoiceValue::Fraction(spa::utils::Choice(
      spa::utils::ChoiceFlags::empty(),
      spa::utils::ChoiceEnum::Range {
        default: rate(wanted_rate),
        min: rate(min),
        max: rate(MAX_FRAMERATE),
      },
    )))
  };
  let anything = format_object(Some(&RAW_FORMATS), any_size, rate_from(0));
  let objects = match source {
    PortalSource::Screen => vec![anything],
    PortalSource::Camera => vec![
      format_object(
        Some(&RAW_FORMATS),
        spa::pod::Value::Rectangle(size),
        rate_from(wanted_rate),
      ),
      format_object(
        None,
        spa::pod::Value::Rectangle(size),
        spa::pod::Value::Fraction(rate(wanted_rate)),
      ),
      anything,
    ],
  };
  objects
    .into_iter()
    .map(|object| {
      spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
      )
      .map(|(cursor, _)| cursor.into_inner())
      .map_err(|_| pw::Error::CreationFailed)
    })
    .collect()
}

/// One video format: raw in one of `maybe_raw_formats`, or MJPEG when `None`.
fn format_object(
  maybe_raw_formats: Option<&[VideoFormat]>,
  size: spa::pod::Value,
  framerate: spa::pod::Value,
) -> spa::pod::Object {
  use spa::param::format::{FormatProperties, MediaSubtype, MediaType};

  let property = |key: FormatProperties, value| spa::pod::Property {
    key: key.as_raw(),
    flags: spa::pod::PropertyFlags::empty(),
    value,
  };
  let subtype = match maybe_raw_formats {
    Some(_) => MediaSubtype::Raw,
    None => MediaSubtype::Mjpg,
  };
  let mut properties = vec![
    property(
      FormatProperties::MediaType,
      spa::pod::Value::Id(spa::utils::Id(MediaType::Video.as_raw())),
    ),
    property(
      FormatProperties::MediaSubtype,
      spa::pod::Value::Id(spa::utils::Id(subtype.as_raw())),
    ),
  ];
  if let Some(formats) = maybe_raw_formats {
    let ids: Vec<_> = formats
      .iter()
      .map(|format| spa::utils::Id(format.as_raw()))
      .collect();
    properties.push(property(
      FormatProperties::VideoFormat,
      spa::pod::Value::Choice(spa::pod::ChoiceValue::Id(spa::utils::Choice(
        spa::utils::ChoiceFlags::empty(),
        spa::utils::ChoiceEnum::Enum {
          default: ids[0],
          alternatives: ids,
        },
      ))),
    ));
  }
  properties.push(property(FormatProperties::VideoSize, size));
  properties.push(property(FormatProperties::VideoFramerate, framerate));
  spa::pod::Object {
    type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
    id: spa::param::ParamType::EnumFormat.as_raw(),
    properties,
  }
}

/// Tells PipeWire which buffers the stream takes, once the format is fixed. Without this a stream
/// takes only plain memory, and a camera node that hands out its V4L2 buffers as DMA-BUFs (what
/// PipeWire's V4L2 source does) can't be linked at all: "use input buffers: -22", not a single
/// frame, and the others see the camera as black (issue #683).
fn announce_buffer_types(stream: &pw::stream::Stream, source: PortalSource) -> Result<(), pw::Error> {
  let values = buffers_param(source)?;
  let mut params = [Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];
  stream.update_params(&mut params)
}

/// The `Buffers` param: the buffer types of [`buffer_types`].
fn buffers_param(source: PortalSource) -> Result<Vec<u8>, pw::Error> {
  let object = spa::pod::Object {
    type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
    id: spa::param::ParamType::Buffers.as_raw(),
    properties: vec![spa::pod::Property {
      key: spa::sys::SPA_PARAM_BUFFERS_dataType,
      flags: spa::pod::PropertyFlags::empty(),
      value: spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
        spa::utils::ChoiceFlags::empty(),
        spa::utils::ChoiceEnum::Flags {
          default: buffer_types(source),
          flags: Vec::new(),
        },
      ))),
    }],
  };
  spa::pod::serialize::PodSerializer::serialize(
    std::io::Cursor::new(Vec::new()),
    &spa::pod::Value::Object(object),
  )
  .map(|(cursor, _)| cursor.into_inner())
  .map_err(|_| pw::Error::CreationFailed)
}

/// The buffer types (a bit per `spa_data_type`) the stream maps and reads. Cameras add DMA-BUFs:
/// V4L2 exports its capture buffers that way, in plain system memory the CPU reads like any other.
/// Screens stay with shared memory: a compositor's DMA-BUF is GPU memory, often tiled, that reads
/// back as garbage or very slowly; without a modifier in the format the compositors send shared
/// memory anyway.
fn buffer_types(source: PortalSource) -> i32 {
  let bit = |data_type: spa::buffer::DataType| 1i32 << data_type.as_raw();
  let shared_memory = bit(spa::buffer::DataType::MemPtr) | bit(spa::buffer::DataType::MemFd);
  match source {
    PortalSource::Camera => shared_memory | bit(spa::buffer::DataType::DmaBuf),
    PortalSource::Screen => shared_memory,
  }
}

/// How pictures of a negotiated format are converted; `None` for one we can't convert.
fn pixel_format_of(subtype: spa::param::format::MediaSubtype, format: VideoFormat) -> Option<PixelFormat> {
  if subtype == spa::param::format::MediaSubtype::Mjpg {
    return Some(PixelFormat::Mjpeg);
  }
  if subtype != spa::param::format::MediaSubtype::Raw {
    return None;
  }
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
  use spa::param::format::{FormatProperties, MediaSubtype};
  use spa::pod::deserialize::PodDeserializer;
  use spa::pod::{ChoiceValue, Value};
  use spa::utils::{Choice, ChoiceEnum};

  use super::*;

  const HD: CaptureFormat = CaptureFormat {
    resolution: VideoResolution {
      width: 1280,
      height: 720,
    },
    framerate: 30,
  };

  #[test]
  fn screens_take_any_raw_format() {
    let params = decoded_format_params(PortalSource::Screen);
    assert_eq!(params.len(), 1);
    assert_eq!(subtype(&params[0]), MediaSubtype::Raw);
  }

  /// Issue #683: a USB camera offers 1080p raw at 5 fps first; the wanted size at its rate, raw
  /// or MJPEG, has to be asked for before it.
  #[test]
  fn cameras_ask_for_the_wanted_size_and_rate_before_anything() {
    let params = decoded_format_params(PortalSource::Camera);
    assert_eq!(params.len(), 3);
    let wanted = spa::utils::Rectangle {
      width: 1280,
      height: 720,
    };
    assert_eq!(subtype(&params[0]), MediaSubtype::Raw);
    assert_eq!(
      value(&params[0], FormatProperties::VideoSize),
      &Value::Rectangle(wanted)
    );
    assert!(matches!(
      value(&params[0], FormatProperties::VideoFramerate),
      Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { min, .. }))) if min.num == 30
    ));
    assert_eq!(subtype(&params[1]), MediaSubtype::Mjpg);
    assert_eq!(
      value(&params[1], FormatProperties::VideoSize),
      &Value::Rectangle(wanted)
    );
    assert_eq!(
      value(&params[1], FormatProperties::VideoFramerate),
      &Value::Fraction(spa::utils::Fraction { num: 30, denom: 1 })
    );
    assert_eq!(subtype(&params[2]), MediaSubtype::Raw);
    assert!(matches!(
      value(&params[2], FormatProperties::VideoSize),
      Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::Range { .. })))
    ));
  }

  #[test]
  fn cameras_take_dma_bufs_and_screens_shared_memory() {
    let dma_buf = 1 << spa::buffer::DataType::DmaBuf.as_raw();
    let mem_fd = 1 << spa::buffer::DataType::MemFd.as_raw();
    // Issue #683: PipeWire's V4L2 camera hands out DMA-BUFs; refusing them fails the link.
    assert_ne!(buffer_types(PortalSource::Camera) & dma_buf, 0);
    assert_ne!(buffer_types(PortalSource::Camera) & mem_fd, 0);
    assert_eq!(buffer_types(PortalSource::Screen) & dma_buf, 0);
    assert_ne!(buffer_types(PortalSource::Screen) & mem_fd, 0);
    for source in [PortalSource::Camera, PortalSource::Screen] {
      let bytes = buffers_param(source).unwrap();
      let (_, value) = PodDeserializer::deserialize_any_from(&bytes).unwrap();
      let spa::pod::Value::Object(object) = value else {
        panic!("not an object: {value:?}");
      };
      assert_eq!(object.id, spa::param::ParamType::Buffers.as_raw());
      let property = &object.properties[0];
      assert_eq!(property.key, spa::sys::SPA_PARAM_BUFFERS_dataType);
      assert!(matches!(
        &property.value,
        spa::pod::Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
          _,
          spa::utils::ChoiceEnum::Flags { default, .. }
        ))) if *default == buffer_types(source)
      ));
    }
  }

  #[test]
  fn pipewire_formats_map_to_converters() {
    let raw = MediaSubtype::Raw;
    assert_eq!(pixel_format_of(raw, VideoFormat::BGRx), Some(PixelFormat::Bgrx));
    assert_eq!(pixel_format_of(raw, VideoFormat::YUY2), Some(PixelFormat::Yuyv));
    assert_eq!(pixel_format_of(raw, VideoFormat::RGB), None);
    // MJPEG has no raw format.
    assert_eq!(
      pixel_format_of(MediaSubtype::Mjpg, VideoFormat::Unknown),
      Some(PixelFormat::Mjpeg)
    );
    assert_eq!(pixel_format_of(MediaSubtype::H264, VideoFormat::Unknown), None);
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

  fn decoded_format_params(source: PortalSource) -> Vec<spa::pod::Object> {
    format_params(source, HD)
      .unwrap()
      .iter()
      .map(
        |bytes| match PodDeserializer::deserialize_any_from(bytes).unwrap().1 {
          Value::Object(object) => object,
          other => panic!("not an object: {other:?}"),
        },
      )
      .collect()
  }

  fn value(object: &spa::pod::Object, key: FormatProperties) -> &Value {
    &object
      .properties
      .iter()
      .find(|property| property.key == key.as_raw())
      .unwrap()
      .value
  }

  fn subtype(object: &spa::pod::Object) -> MediaSubtype {
    match value(object, FormatProperties::MediaSubtype) {
      Value::Id(id) => MediaSubtype::from_raw(id.0),
      other => panic!("not an id: {other:?}"),
    }
  }
}
