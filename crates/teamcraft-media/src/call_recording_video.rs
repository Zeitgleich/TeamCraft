//! The picture of a call recording (issue #2250, e2ee.md §8.3): a thread of its own decodes the
//! tracks the recording device shows, composites them as its call screen arranges them
//! ([`crate::recording_composite`]) at a fixed size and rate, and encodes the result as H.264 for
//! the recording's file (`call_recording`, a fragmented MP4).
//!
//! - **Inputs:** others' frames as they arrive ([`VideoRecordingSink::push_received`], H.264
//!   only; a track in another codec stays an empty tile), the own camera and screen as raw pictures
//!   ([`VideoRecordingSink::push_own`], at most one kept per source), and what the call screen
//!   shows ([`VideoRecordingSink::set_layout`], [`VideoRecordingSink::set_focus`],
//!   [`VideoRecordingSink::forget`]).
//! - **Bounded:** received frames wait in a queue of [`MAX_QUEUED_INPUTS`]; a frame that doesn't
//!   fit is dropped, counted, and its track skips to its next keyframe (a decoder fed a gap shows
//!   garbage). Each track keeps one decoded picture; the encoded pictures wait for the file in a
//!   queue of their own ([`MAX_QUEUED_PICTURES`]), beyond which they are dropped and counted, and
//!   the next picture is a keyframe. Nothing grows with the recording's length.
//! - **Consent (#2115):** a tile of a device the recording doesn't take in is blanked and carries
//!   the app's label; its picture is never sampled ([`VideoAdmission`]). The own device is always
//!   in: the recorder consents by recording.
//! - **Hardware first:** the encoder comes from `video_pipeline::preferred_encoders` (VA-API or
//!   VideoToolbox where the app has them, OpenH264 otherwise), with its fallback to software.
//! - Nothing runs while the recording waits for consent (it records nothing then), and nothing is
//!   logged about what the pictures show.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use teamcraft_proto::{DeviceId, MediaStreamId, VideoCodec, VideoLayerInfo, VideoTrackKind};
use tracing::{debug, info, warn};

use crate::call_engine::RecordingAdmission;
use crate::call_video::ReceivedVideoFrame;
use crate::recording_composite::{Compositor, LabelMask, SceneLayout, SceneTile};
use crate::video::{VideoFrame, VideoResolution};
use crate::video_codec::{EncodedPicture, OpenH264Decoder, VideoContent, VideoDecoder, VideoEncoder};
use crate::video_pipeline::preferred_encoders;

/// Received frames waiting for the decoders, at most (about two seconds of four tracks at 30 fps).
pub const MAX_QUEUED_INPUTS: usize = 256;
/// Encoded pictures waiting for the file, at most (two seconds at 15 fps).
pub const MAX_QUEUED_PICTURES: usize = 30;
/// A tile that got nothing for this long is gone (its track ended without being forgotten).
/// Longer than a still screen share's refresh.
const STALE_TILE: Duration = Duration::from_secs(10);
/// A keyframe at least this often: the file's fragments start at keyframes.
const KEYFRAME_INTERVAL: Duration = Duration::from_secs(2);
/// How long the encoder may take to give its first picture before the recording goes on without
/// video (the writer's side waits this long for the parameter sets).
pub const FIRST_PICTURE_WAIT: Duration = Duration::from_secs(3);

/// The size, rate and bitrate of the recorded picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoRecordingConfig {
  pub size: VideoResolution,
  pub framerate: u8,
  pub bitrate_bps: u32,
}

/// Where the call hands the recording its pictures. Any number of copies; all go quiet once the
/// recording ends.
#[derive(Clone)]
pub struct VideoRecordingSink {
  inputs: SyncSender<Input>,
  shared: Arc<VideoShared>,
}

/// Which devices' tiles a recording shows (the same rule as its sound, #2115).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VideoAdmission {
  /// Nobody yet: the recording waits for consent and records nothing.
  #[default]
  Nobody,
  Everybody,
  /// These devices, and this device itself.
  Only(Vec<DeviceId>),
}

/// An own capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnSource {
  Camera,
  Screen,
}

/// One encoded picture for the file.
pub struct RecordedPicture {
  /// When it was composited: the file places it by this.
  pub at: Instant,
  pub is_keyframe: bool,
  /// H.264, Annex B.
  pub data: Vec<u8>,
}

/// The thread; stopping it is the recorder's.
pub(crate) struct VideoRecording {
  sink: VideoRecordingSink,
  maybe_thread: Option<JoinHandle<()>>,
}

/// What the recorder's writer and the compositing thread share.
#[derive(Default)]
pub(crate) struct VideoShared {
  is_stopping: AtomicBool,
  /// The writer took its first sound: compositing starts (nothing is recorded before).
  pub(crate) is_audio_started: AtomicBool,
  /// The writer dropped a picture or waits for a first one: the next is a keyframe.
  pub(crate) is_keyframe_needed: AtomicBool,
  pub(crate) queued_pictures: AtomicUsize,
  pub(crate) dropped_pictures: AtomicU64,
  dropped_inputs: AtomicU64,
  own: Mutex<OwnPictures>,
  scene: Mutex<Scene>,
  /// Tracks whose frames were dropped: skipped until their next keyframe.
  broken: Mutex<HashSet<MediaStreamId>>,
}

#[derive(Default)]
struct OwnPictures {
  maybe_camera: Option<(VideoFrame, Instant)>,
  maybe_screen: Option<(VideoFrame, Instant)>,
}

#[derive(Default)]
struct Scene {
  layout: SceneLayout,
  maybe_focus: Option<DeviceId>,
  admission: VideoAdmission,
  maybe_label: Option<Arc<LabelMask>>,
  forgotten: Vec<MediaStreamId>,
}

enum Input {
  Received {
    frame: ReceivedVideoFrame,
    is_screen: bool,
  },
}

/// One received track on the compositing thread.
struct Tile {
  sender: DeviceId,
  is_screen: bool,
  decoder: OpenH264Decoder,
  maybe_picture: Option<VideoFrame>,
  /// Skips frames until a keyframe: after a decode error or a gap.
  is_waiting_for_keyframe: bool,
  last_input: Instant,
}

impl Default for VideoRecordingConfig {
  /// 1280x720 at 15 frames a second and 350 kbit/s: about 160 MB an hour, slides and faces stay
  /// readable.
  fn default() -> Self {
    Self {
      size: VideoResolution {
        width: 1280,
        height: 720,
      },
      framerate: 15,
      bitrate_bps: 350_000,
    }
  }
}

impl VideoRecording {
  /// Starts the compositing thread; each encoded picture goes to `deliver`, which answers whether
  /// the file took it.
  pub(crate) fn start(
    config: VideoRecordingConfig,
    own_device: DeviceId,
    shared: Arc<VideoShared>,
    deliver: impl Fn(RecordedPicture) -> bool + Send + 'static,
  ) -> std::io::Result<Self> {
    let (inputs, input_rx) = mpsc::sync_channel(MAX_QUEUED_INPUTS);
    let thread_shared = Arc::clone(&shared);
    let thread = std::thread::Builder::new()
      .name("call-recording-video".to_owned())
      .spawn(move || composite(&input_rx, config, own_device, &thread_shared, &deliver))?;
    info!(
      width = config.size.width,
      height = config.size.height,
      framerate = config.framerate,
      "call recording video started"
    );
    Ok(Self {
      sink: VideoRecordingSink { inputs, shared },
      maybe_thread: Some(thread),
    })
  }

  pub(crate) fn sink(&self) -> VideoRecordingSink {
    self.sink.clone()
  }

  /// Stops the thread.
  pub(crate) fn stop(&mut self) {
    self.sink.shared.is_stopping.store(true, Ordering::Relaxed);
    if let Some(thread) = self.maybe_thread.take()
      && thread.join().is_err()
    {
      warn!("the call recording's video thread panicked");
    }
    info!(
      dropped_pictures = self.sink.shared.dropped_pictures.load(Ordering::Relaxed),
      dropped_frames = self.sink.shared.dropped_inputs.load(Ordering::Relaxed),
      "call recording video ended"
    );
  }
}

impl Drop for VideoRecording {
  fn drop(&mut self) {
    if self.maybe_thread.is_some() {
      self.stop();
    }
  }
}

impl VideoShared {
  /// Which devices' tiles the recording shows: set by the call with the sound's rule, every
  /// played frame; it allocates only when the rule changed.
  pub(crate) fn set_admission(&self, admission: &RecordingAdmission<'_>) {
    let mut scene = lock(&self.scene);
    let is_same = match (admission, &scene.admission) {
      (RecordingAdmission::Nobody, VideoAdmission::Nobody)
      | (RecordingAdmission::Everybody, VideoAdmission::Everybody) => true,
      (RecordingAdmission::Only(devices), VideoAdmission::Only(known)) => *devices == known.as_slice(),
      _ => false,
    };
    if !is_same {
      scene.admission = match admission {
        RecordingAdmission::Nobody => VideoAdmission::Nobody,
        RecordingAdmission::Everybody => VideoAdmission::Everybody,
        RecordingAdmission::Only(devices) => VideoAdmission::Only(devices.to_vec()),
      };
    }
  }
}

impl VideoRecordingSink {
  /// Another device's frame of a track the call screen shows. `kind` is what the track shows.
  pub fn push_received(&self, frame: &ReceivedVideoFrame, kind: &VideoTrackKind) {
    if self.is_ended() || frame.codec != VideoCodec::H264 {
      return;
    }
    {
      let mut broken = lock(&self.shared.broken);
      if broken.contains(&frame.stream_id) {
        if !frame.is_keyframe {
          return;
        }
        broken.remove(&frame.stream_id);
      }
    }
    let input = Input::Received {
      frame: frame.clone(),
      is_screen: *kind == VideoTrackKind::Screen,
    };
    match self.inputs.try_send(input) {
      Ok(()) => {},
      Err(TrySendError::Full(_)) => {
        self.shared.dropped_inputs.fetch_add(1, Ordering::Relaxed);
        lock(&self.shared.broken).insert(frame.stream_id);
        debug!("call recording video queue full, frame dropped");
      },
      Err(TrySendError::Disconnected(_)) => {},
    }
  }

  /// A picture of the own camera (after its effects) or screen share. Only the latest is kept;
  /// at most one copy per frame interval is taken (`interval`).
  pub fn push_own(&self, source: OwnSource, frame: &VideoFrame, interval: Duration) {
    if self.is_ended() || !self.shared.is_audio_started.load(Ordering::Relaxed) {
      return;
    }
    let now = Instant::now();
    let mut own = lock(&self.shared.own);
    let slot = match source {
      OwnSource::Camera => &mut own.maybe_camera,
      OwnSource::Screen => &mut own.maybe_screen,
    };
    if slot
      .as_ref()
      .is_some_and(|(_, at)| now.duration_since(*at) < interval)
    {
      return;
    }
    *slot = Some((frame.clone(), now));
  }

  /// The own capture ended: its tile goes.
  pub fn forget_own(&self, source: OwnSource) {
    let mut own = lock(&self.shared.own);
    match source {
      OwnSource::Camera => own.maybe_camera = None,
      OwnSource::Screen => own.maybe_screen = None,
    }
  }

  /// The call screen stopped showing a track: its tile goes.
  pub fn forget(&self, stream: MediaStreamId) {
    lock(&self.shared.scene).forgotten.push(stream);
  }

  /// The call screen's layout.
  pub fn set_layout(&self, layout: SceneLayout) {
    lock(&self.shared.scene).layout = layout;
  }

  /// The active speaker, large in the active speaker layout.
  pub fn set_focus(&self, maybe_device: Option<DeviceId>) {
    lock(&self.shared.scene).maybe_focus = maybe_device;
  }

  /// The label of a blanked tile, as the app drew it in its language.
  pub fn set_label(&self, maybe_label: Option<LabelMask>) {
    lock(&self.shared.scene).maybe_label = maybe_label.map(Arc::new);
  }

  /// The recording takes no more pictures: it ended, or it stops.
  pub fn is_ended(&self) -> bool {
    self.shared.is_stopping.load(Ordering::Relaxed)
  }
}

/// The compositing thread: takes inputs, and on every tick of the frame rate composites, encodes
/// and delivers a picture, until the recording stops.
fn composite(
  inputs: &Receiver<Input>,
  config: VideoRecordingConfig,
  own_device: DeviceId,
  shared: &VideoShared,
  deliver: &dyn Fn(RecordedPicture) -> bool,
) {
  let mut compositor = match Compositor::new(config.size) {
    Ok(compositor) => compositor,
    Err(error) => {
      warn!(%error, "no compositor for the call recording's video");
      return;
    },
  };
  let mut encoder = match encoder(config) {
    Ok(encoder) => encoder,
    Err(error) => {
      warn!(%error, "no encoder for the call recording's video; it records the sound only");
      return;
    },
  };
  let interval = Duration::from_micros(1_000_000 / u64::from(config.framerate.max(1)));
  let started = Instant::now();
  let mut tiles: HashMap<MediaStreamId, Tile> = HashMap::new();
  let mut next_tick = started + interval;
  let mut maybe_last_keyframe: Option<Instant> = None;
  while !shared.is_stopping.load(Ordering::Relaxed) {
    let wait = next_tick.saturating_duration_since(Instant::now());
    match inputs.recv_timeout(wait) {
      Ok(input) => {
        take_input(&mut tiles, input);
        continue;
      },
      Err(RecvTimeoutError::Timeout) => {},
      Err(RecvTimeoutError::Disconnected) => break,
    }
    let now = Instant::now();
    next_tick += interval;
    if next_tick < now {
      // Behind (a slow machine): skip the missed ticks rather than catch up.
      next_tick = now + interval;
    }
    if !shared.is_audio_started.load(Ordering::Relaxed) {
      continue;
    }
    drop_gone_tiles(&mut tiles, shared, now);
    let is_keyframe_forced = shared.is_keyframe_needed.swap(false, Ordering::Relaxed)
      || maybe_last_keyframe.is_none_or(|last| now.duration_since(last) >= KEYFRAME_INTERVAL);
    let timestamp_us = u64::try_from(now.duration_since(started).as_micros()).unwrap_or(u64::MAX);
    let frame = match scene_frame(&mut compositor, &tiles, shared, own_device, timestamp_us) {
      Ok(frame) => frame,
      Err(error) => {
        warn!(%error, "call recording picture not composited");
        continue;
      },
    };
    match encoder.encode(&frame, is_keyframe_forced) {
      Ok(Some(EncodedPicture { is_keyframe, data })) => {
        if is_keyframe {
          maybe_last_keyframe = Some(now);
        }
        hand_over(
          shared,
          deliver,
          RecordedPicture {
            at: now,
            is_keyframe,
            data,
          },
        );
      },
      // Rate control skipped it.
      Ok(None) => {},
      Err(error) => {
        warn!(%error, "call recording picture not encoded");
        shared.is_keyframe_needed.store(true, Ordering::Relaxed);
      },
    }
  }
}

fn encoder(
  config: VideoRecordingConfig,
) -> Result<Box<dyn VideoEncoder>, crate::video_codec::VideoCodecError> {
  let layer = VideoLayerInfo {
    layer: 0,
    width: config.size.width,
    height: config.size.height,
    max_framerate: config.framerate,
    max_bitrate_bps: config.bitrate_bps,
    maybe_codec: None,
  };
  let encoder = (preferred_encoders())(&layer, VideoContent::Camera)?;
  info!(encoder = encoder.name(), "call recording video encoder");
  Ok(encoder)
}

fn take_input(tiles: &mut HashMap<MediaStreamId, Tile>, input: Input) {
  let Input::Received { frame, is_screen } = input;
  let tile = match tiles.entry(frame.stream_id) {
    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
    std::collections::hash_map::Entry::Vacant(entry) => match OpenH264Decoder::new() {
      Ok(decoder) => entry.insert(Tile {
        sender: frame.sender,
        is_screen,
        decoder,
        maybe_picture: None,
        is_waiting_for_keyframe: true,
        last_input: Instant::now(),
      }),
      Err(error) => {
        warn!(%error, "no decoder for a recorded track");
        return;
      },
    },
  };
  tile.last_input = Instant::now();
  if frame.is_keyframe {
    tile.is_waiting_for_keyframe = false;
  }
  if tile.is_waiting_for_keyframe {
    return;
  }
  let timestamp_us = u64::from(frame.timestamp.0) * 1_000_000 / u64::from(crate::video::VIDEO_CLOCK_RATE_HZ);
  match tile.decoder.decode(&frame.data, timestamp_us) {
    Ok(Some(picture)) => tile.maybe_picture = Some(picture),
    Ok(None) => {},
    Err(error) => {
      debug!(%error, "recorded frame not decodable; waiting for a keyframe");
      tile.is_waiting_for_keyframe = true;
    },
  }
}

fn drop_gone_tiles(tiles: &mut HashMap<MediaStreamId, Tile>, shared: &VideoShared, now: Instant) {
  let forgotten = std::mem::take(&mut lock(&shared.scene).forgotten);
  for stream in forgotten {
    tiles.remove(&stream);
  }
  tiles.retain(|_, tile| now.duration_since(tile.last_input) < STALE_TILE);
  let mut guard = lock(&shared.own);
  let own = &mut *guard;
  for slot in [&mut own.maybe_camera, &mut own.maybe_screen] {
    if slot
      .as_ref()
      .is_some_and(|(_, at)| now.duration_since(*at) >= STALE_TILE)
    {
      *slot = None;
    }
  }
}

/// The frame of the current scene.
fn scene_frame(
  compositor: &mut Compositor,
  tiles: &HashMap<MediaStreamId, Tile>,
  shared: &VideoShared,
  own_device: DeviceId,
  timestamp_us: u64,
) -> Result<VideoFrame, crate::video::VideoError> {
  let (layout, maybe_focus, admission, maybe_label) = {
    let scene = lock(&shared.scene);
    (
      scene.layout,
      scene.maybe_focus,
      scene.admission.clone(),
      scene.maybe_label.clone(),
    )
  };
  let own = lock(&shared.own);
  let mut ordered: Vec<(&MediaStreamId, &Tile)> = tiles.iter().collect();
  // A stable order: by track, as the call screen keeps tiles where they are.
  ordered.sort_by_key(|(stream, _)| stream.0);
  let mut scene: Vec<SceneTile<'_>> = Vec::with_capacity(ordered.len() + 2);
  if let Some((picture, _)) = &own.maybe_screen {
    scene.push(own_tile(picture, true));
  }
  for (_, tile) in ordered {
    scene.push(SceneTile {
      maybe_picture: tile.maybe_picture.as_ref(),
      is_screen: tile.is_screen,
      is_focused: maybe_focus == Some(tile.sender),
      is_left_out: !is_admitted(&admission, tile.sender),
    });
  }
  if let Some((picture, _)) = &own.maybe_camera {
    scene.push(SceneTile {
      is_focused: maybe_focus == Some(own_device),
      ..own_tile(picture, false)
    });
  }
  compositor.compose(&scene, layout, maybe_label.as_deref(), timestamp_us)
}

fn own_tile(picture: &VideoFrame, is_screen: bool) -> SceneTile<'_> {
  SceneTile {
    maybe_picture: Some(picture),
    is_screen,
    is_focused: false,
    is_left_out: false,
  }
}

fn is_admitted(admission: &VideoAdmission, device: DeviceId) -> bool {
  match admission {
    VideoAdmission::Nobody => false,
    VideoAdmission::Everybody => true,
    VideoAdmission::Only(devices) => devices.contains(&device),
  }
}

/// Hands a picture to the file, within its queue's bound; a picture dropped makes the next one a
/// keyframe, so the file's video never builds on a missing picture.
fn hand_over(shared: &VideoShared, deliver: &dyn Fn(RecordedPicture) -> bool, picture: RecordedPicture) {
  if shared.queued_pictures.load(Ordering::Relaxed) >= MAX_QUEUED_PICTURES {
    shared.dropped_pictures.fetch_add(1, Ordering::Relaxed);
    shared.is_keyframe_needed.store(true, Ordering::Relaxed);
    return;
  }
  shared.queued_pictures.fetch_add(1, Ordering::Relaxed);
  if !deliver(picture) {
    shared.queued_pictures.fetch_sub(1, Ordering::Relaxed);
    shared.dropped_pictures.fetch_add(1, Ordering::Relaxed);
    shared.is_keyframe_needed.store(true, Ordering::Relaxed);
  }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn admission_follows_the_sound_rule() {
    let seven = device(7);
    assert!(!is_admitted(&VideoAdmission::Nobody, seven));
    assert!(is_admitted(&VideoAdmission::Everybody, seven));
    assert!(is_admitted(&VideoAdmission::Only(vec![seven]), seven));
    assert!(!is_admitted(&VideoAdmission::Only(vec![device(8)]), seven));
  }

  #[test]
  fn a_full_picture_queue_drops_and_asks_for_a_keyframe() {
    let shared = VideoShared::default();
    shared
      .queued_pictures
      .store(MAX_QUEUED_PICTURES, Ordering::Relaxed);
    let delivered = AtomicUsize::new(0);
    let deliver = |_: RecordedPicture| {
      delivered.fetch_add(1, Ordering::Relaxed);
      true
    };
    hand_over(&shared, &deliver, picture());
    assert_eq!(delivered.load(Ordering::Relaxed), 0);
    assert_eq!(shared.dropped_pictures.load(Ordering::Relaxed), 1);
    assert!(shared.is_keyframe_needed.load(Ordering::Relaxed));
    shared.queued_pictures.store(0, Ordering::Relaxed);
    shared.is_keyframe_needed.store(false, Ordering::Relaxed);
    hand_over(&shared, &deliver, picture());
    assert_eq!(delivered.load(Ordering::Relaxed), 1);
    assert_eq!(shared.queued_pictures.load(Ordering::Relaxed), 1);
    hand_over(&shared, &|_| false, picture());
    assert_eq!(shared.queued_pictures.load(Ordering::Relaxed), 1);
    assert!(shared.is_keyframe_needed.load(Ordering::Relaxed));
  }

  #[test]
  fn a_full_input_queue_drops_and_skips_the_track_to_its_next_keyframe() {
    let (inputs, input_rx) = mpsc::sync_channel(1);
    let sink = VideoRecordingSink {
      inputs,
      shared: Arc::default(),
    };
    let camera = VideoTrackKind::Camera;
    sink.push_received(&received(1, true), &camera);
    sink.push_received(&received(1, false), &camera);
    assert_eq!(sink.shared.dropped_inputs.load(Ordering::Relaxed), 1);
    drop(input_rx.try_recv().unwrap());
    // The gap: deltas are skipped until the track's next keyframe.
    sink.push_received(&received(1, false), &camera);
    assert!(input_rx.try_recv().is_err());
    sink.push_received(&received(1, true), &camera);
    assert!(input_rx.try_recv().is_ok());
    // Other codecs aren't decoded here.
    let mut av1 = received(2, true);
    av1.codec = VideoCodec::Av1;
    sink.push_received(&av1, &camera);
    assert!(input_rx.try_recv().is_err());
  }

  #[test]
  fn own_pictures_keep_the_latest_and_only_once_recording() {
    let (inputs, _input_rx) = mpsc::sync_channel(1);
    let sink = VideoRecordingSink {
      inputs,
      shared: Arc::default(),
    };
    let frame = VideoFrame::new(VideoResolution { width: 4, height: 4 }, 0, vec![0; 24]).unwrap();
    sink.push_own(OwnSource::Camera, &frame, Duration::ZERO);
    assert!(
      lock(&sink.shared.own).maybe_camera.is_none(),
      "nothing while waiting for consent"
    );
    sink.shared.is_audio_started.store(true, Ordering::Relaxed);
    sink.push_own(OwnSource::Camera, &frame, Duration::from_secs(60));
    sink.push_own(
      OwnSource::Camera,
      &frame.clone().with_timestamp_us(5),
      Duration::from_secs(60),
    );
    assert_eq!(
      lock(&sink.shared.own)
        .maybe_camera
        .as_ref()
        .map(|(frame, _)| frame.timestamp_us()),
      Some(0),
      "within the interval the copy is skipped"
    );
    sink.forget_own(OwnSource::Camera);
    assert!(lock(&sink.shared.own).maybe_camera.is_none());
  }

  fn device(raw: i64) -> DeviceId {
    use teamcraft_proto::{Id, Snowflake};
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn picture() -> RecordedPicture {
    RecordedPicture {
      at: Instant::now(),
      is_keyframe: false,
      data: vec![1],
    }
  }

  fn received(stream: u32, is_keyframe: bool) -> ReceivedVideoFrame {
    ReceivedVideoFrame {
      sender: device(1),
      stream_id: MediaStreamId(stream),
      codec: VideoCodec::H264,
      layer: crate::datagram::SimulcastLayer::BASE,
      timestamp: crate::rtp::RtpTimestamp(0),
      is_keyframe,
      data: vec![0, 0, 0, 1, 0x65],
      arrived_at: web_time::Instant::now(),
    }
  }
}
