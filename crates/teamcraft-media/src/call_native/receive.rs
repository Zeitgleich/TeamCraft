//! Others' video on a native client: how the app shows each track, the layer asked for it, and
//! whether this device's decoders keep up with it (#1775).
//!
//! The decoders sit where the app put them: the driver's own ([`super::VideoReceiver`]), or the
//! app's (the desktop's webview, its JPEG transcoder), which tells the call how each frame reached
//! them through a [`DecodeReporter`]. Either way a frame's lateness counts from the arrival of the
//! datagram that completed it ([`ReceivedVideoFrame::arrived_at`]), so the network's delay isn't
//! this device's load. [`ReceiveLoad`] judges them all together, as in the browser (#1762), and
//! the layers it leaves each track go to the SFU through the track's layer choice.
//!
//! A track nobody can see (its tile laid out at zero size, the window minimized) isn't decoded:
//! its frames are dropped here, and once it shows again it waits for a keyframe, which is asked
//! for at once.

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use teamcraft_proto::{MediaStreamId, VideoLayerInfo, VideoTrackKind};
use tokio::sync::mpsc;

use super::{Command, VideoCommand};
use crate::call_video::ReceivedVideoFrame;
use crate::receive_load::{DecodedFrame, FrameInterval, Importance, ReceiveLoad, WatchedStream};
use crate::rtp::RtpTimestamp;
use crate::video::{TOP_LAYER, VideoResolution, ViewChoice};

/// The size a track is weighed at while its tile's size is unknown: as large as any.
const UNKNOWN_SHOWN: VideoResolution = VideoResolution {
  width: u16::MAX,
  height: u16::MAX,
};

/// Others' tracks as this device receives and shows them.
pub(super) struct RemoteVideo {
  /// What others' tracks show, from their publication.
  kinds: HashMap<MediaStreamId, VideoTrackKind>,
  /// Others' tracks' layers, for the layer a view asks for.
  layers: HashMap<MediaStreamId, Vec<VideoLayerInfo>>,
  /// How the app shows others' tracks: their tiles' sizes and the viewer's picks (#967). Kept
  /// while a track is published, so a tile shown again asks for what it asked before.
  views: HashMap<MediaStreamId, ViewChoice>,
  /// The tracks this device receives (subscribed or watched).
  received: BTreeMap<MediaStreamId, Received>,
  load: ReceiveLoad,
}

/// One track this device receives.
#[derive(Debug, Clone)]
struct Received {
  /// The highest layer the app subscribed to it with.
  app_max_layer: u8,
  /// The layer asked of the SFU.
  asked_layer: u8,
  interval: FrameInterval,
  /// Frames waiting in its decoder when the last one was handed over; `None` before any.
  maybe_queued: Option<u32>,
  /// Frames were dropped while it was hidden: deltas wait for the next keyframe.
  is_waiting_for_keyframe: bool,
}

/// Tells a native call how the app's decoders take the frames of the tracks it subscribed to
/// ([`super::NativeCall::subscribe_video`]), so the call can ask for less where they fall behind
/// (#1775). Cheap to clone; does nothing once the call ended.
#[derive(Clone)]
pub struct DecodeReporter {
  commands: mpsc::WeakUnboundedSender<Command>,
}

/// What a decoding change asks of the call.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Asks {
  /// Tracks whose layer to ask the SFU for anew.
  pub layers: Vec<(MediaStreamId, u8)>,
  /// A track shown again after frames were dropped: it needs a keyframe.
  pub maybe_keyframe: Option<MediaStreamId>,
  /// Whether this device now holds video back for its own load, when that changed.
  pub maybe_is_limited: Option<bool>,
}

impl RemoteVideo {
  pub(super) fn new(now: Instant) -> Self {
    Self {
      kinds: HashMap::new(),
      layers: HashMap::new(),
      views: HashMap::new(),
      received: BTreeMap::new(),
      load: ReceiveLoad::new(now),
    }
  }

  /// What another device's track shows, while it is published.
  pub(super) fn kind_of(&self, stream_id: MediaStreamId) -> Option<&VideoTrackKind> {
    self.kinds.get(&stream_id)
  }

  /// Whether no other device's track is published.
  pub(super) fn is_empty(&self) -> bool {
    self.kinds.is_empty()
  }

  /// Another device published a track (or its layers changed).
  pub(super) fn on_published(
    &mut self,
    stream_id: MediaStreamId,
    kind: VideoTrackKind,
    layers: Vec<VideoLayerInfo>,
  ) -> Asks {
    self.kinds.insert(stream_id, kind);
    self.layers.insert(stream_id, layers);
    self.layer_asks()
  }

  /// A track went: what it held of this device's load goes to the others.
  pub(super) fn on_unpublished(&mut self, stream_id: MediaStreamId) -> Asks {
    self.kinds.remove(&stream_id);
    self.layers.remove(&stream_id);
    self.views.remove(&stream_id);
    self.received.remove(&stream_id);
    self.layer_asks()
  }

  /// This device receives the track, the app asking for at most `app_max_layer`: the layer to
  /// subscribe with, within what the view and this device's load leave it.
  pub(super) fn receive(&mut self, stream_id: MediaStreamId, app_max_layer: u8) -> u8 {
    let received = self.received.entry(stream_id).or_insert(Received {
      app_max_layer,
      asked_layer: app_max_layer,
      interval: FrameInterval::default(),
      maybe_queued: None,
      is_waiting_for_keyframe: false,
    });
    received.app_max_layer = app_max_layer;
    let layer = self
      .chosen_layers()
      .get(&stream_id)
      .copied()
      .unwrap_or(app_max_layer);
    if let Some(received) = self.received.get_mut(&stream_id) {
      received.asked_layer = layer;
    }
    layer
  }

  /// This device stops receiving the track: what it held goes to the others.
  pub(super) fn stop_receiving(&mut self, stream_id: MediaStreamId) -> Asks {
    if self.received.remove(&stream_id).is_none() {
      return Asks::default();
    }
    self.layer_asks()
  }

  /// The app shows the track's tile this large now (device pixels; zero: nobody can see it).
  pub(super) fn set_shown(&mut self, stream_id: MediaStreamId, shown: VideoResolution) -> Asks {
    self.views.entry(stream_id).or_default().maybe_shown = Some(shown);
    let mut asks = self.layer_asks();
    if let Some(received) = self.received.get_mut(&stream_id)
      && received.is_waiting_for_keyframe
      && !is_hidden_size(shown)
    {
      asks.maybe_keyframe = Some(stream_id);
    }
    asks
  }

  /// The viewer picked at most these lines for the track (`None`: Auto, #967).
  pub(super) fn set_quality(&mut self, stream_id: MediaStreamId, maybe_max_lines: Option<u16>) -> Asks {
    self.views.entry(stream_id).or_default().maybe_max_lines = maybe_max_lines;
    self.layer_asks()
  }

  /// Whether a received frame goes to its decoder: not while nobody can see the track, and after
  /// that not before its next keyframe.
  pub(super) fn admits(&mut self, frame: &ReceivedVideoFrame) -> bool {
    let is_hidden = self.is_hidden(frame.stream_id);
    let Some(received) = self.received.get_mut(&frame.stream_id) else {
      return true;
    };
    if is_hidden {
      received.is_waiting_for_keyframe = true;
      return false;
    }
    if received.is_waiting_for_keyframe && !frame.is_keyframe {
      return false;
    }
    received.is_waiting_for_keyframe = false;
    true
  }

  /// A frame of the track reached its decoder, behind `queued` others, `late_by` after its
  /// last datagram arrived.
  pub(super) fn on_decoding(
    &mut self,
    stream_id: MediaStreamId,
    timestamp: RtpTimestamp,
    queued: u32,
    late_by: Duration,
  ) {
    let Some(received) = self.received.get_mut(&stream_id) else {
      return;
    };
    received.maybe_queued = Some(queued);
    let interval = received.interval.next(timestamp);
    self.load.on_frame(DecodedFrame {
      queued,
      late_by,
      interval,
    });
  }

  /// Judges whether this device decodes what it receives (every timer tick): the layers to ask
  /// for anew, and whether the device started or stopped holding video back. Nothing decoded,
  /// nothing to judge.
  pub(super) fn judge(&mut self, now: Instant) -> Asks {
    let is_decoding = self.received.keys().any(|stream_id| !self.is_hidden(*stream_id));
    if !is_decoding {
      self.load.measure_afresh(now);
    }
    let was_limited = self.load.is_limited();
    let mut asks = Asks::default();
    if self.load.evaluate(&self.watched_streams(), now) {
      tracing::info!(
        is_limited = self.load.is_limited(),
        reason = ?self.load.maybe_reason(),
        "received video's decoders: layers changed for this device's load"
      );
      asks = self.layer_asks();
    }
    let is_limited = self.load.is_limited();
    asks.maybe_is_limited = (is_limited != was_limited).then_some(is_limited);
    asks
  }

  /// What this device's decoders do with each received track (#1775): the queue as last handed
  /// a frame, and the layer this device holds it to for its own load.
  #[cfg(feature = "call-view")]
  pub(super) fn add_decoding_stats(&self, report: &mut teamcraft_client_api::video_stats::VideoStatsReport) {
    use teamcraft_client_api::video_stats::{DecodeOverload, DeviceLimitStats, VideoTrackStats};

    use crate::receive_load::ReceiveOverload;

    let maybe_reason = self.load.maybe_reason().map(|reason| match reason {
      ReceiveOverload::QueuedFrames => DecodeOverload::QueuedFrames,
      ReceiveOverload::LateFrames => DecodeOverload::LateFrames,
    });
    let unlimited: HashMap<MediaStreamId, u8> = self
      .watched_streams()
      .iter()
      .map(|stream| (stream.stream_id, stream.unlimited_layer()))
      .collect();
    for track in &mut report.tracks {
      let VideoTrackStats::Received(received_stats) = track else {
        continue;
      };
      let Some(received) = self.received.get(&received_stats.stream_id) else {
        continue;
      };
      received_stats.maybe_decode_queue = received.maybe_queued;
      let is_held_back = unlimited
        .get(&received_stats.stream_id)
        .is_some_and(|layer| received.asked_layer < *layer);
      received_stats.maybe_device_limit = is_held_back.then_some(DeviceLimitStats {
        layer: received.asked_layer,
        maybe_reason,
      });
    }
  }

  /// The layers the received tracks get now, where they differ from what was asked.
  fn layer_asks(&mut self) -> Asks {
    let mut layers = Vec::new();
    for (stream_id, layer) in self.chosen_layers() {
      if let Some(received) = self.received.get_mut(&stream_id)
        && received.asked_layer != layer
      {
        tracing::debug!(
          stream_id = stream_id.0,
          layer,
          was = received.asked_layer,
          "a received track asks for another layer"
        );
        received.asked_layer = layer;
        layers.push((stream_id, layer));
      }
    }
    Asks {
      layers,
      ..Asks::default()
    }
  }

  /// What each received track's view wants, less what this device's load holds back.
  fn chosen_layers(&self) -> BTreeMap<MediaStreamId, u8> {
    self.load.layers(&self.watched_streams())
  }

  /// The received tracks as this device's load weighs them.
  fn watched_streams(&self) -> Vec<WatchedStream> {
    self
      .received
      .iter()
      .map(|(stream_id, received)| {
        let kind = self
          .kinds
          .get(stream_id)
          .cloned()
          .unwrap_or(VideoTrackKind::Camera);
        let maybe_view = self.views.get(stream_id);
        let shown = maybe_view
          .and_then(|view| view.maybe_shown)
          .unwrap_or(UNKNOWN_SHOWN);
        let layers = self.layers.get(stream_id).map(Vec::as_slice).unwrap_or_default();
        let view_layer = match maybe_view {
          Some(view) if !layers.is_empty() => view.layer(&kind, layers),
          _ => TOP_LAYER,
        };
        WatchedStream {
          stream_id: *stream_id,
          importance: Importance::new(&kind, shown),
          wanted_layer: received.app_max_layer.min(view_layer),
          offered_layers: layers.iter().map(|info| info.layer).collect(),
        }
      })
      .collect()
  }

  /// Whether the app shows the track nowhere anyone can see (it said so with a zero size).
  fn is_hidden(&self, stream_id: MediaStreamId) -> bool {
    self
      .views
      .get(&stream_id)
      .and_then(|view| view.maybe_shown)
      .is_some_and(is_hidden_size)
  }
}

impl Default for RemoteVideo {
  fn default() -> Self {
    Self::new(Instant::now())
  }
}

impl DecodeReporter {
  pub(super) fn new(commands: mpsc::WeakUnboundedSender<Command>) -> Self {
    Self { commands }
  }

  /// The app hands `frame` to its decoder now, behind `queued` frames waiting there.
  pub fn frame_decoding(&self, frame: &ReceivedVideoFrame, queued: u32) {
    let Some(commands) = self.commands.upgrade() else {
      return;
    };
    let command = VideoCommand::Decoding {
      stream_id: frame.stream_id,
      timestamp: frame.timestamp,
      queued,
      late_by: Instant::now().saturating_duration_since(frame.arrived_at),
    };
    // A closed queue means the call ended meanwhile.
    let _ = commands.send(Command::Video(command));
  }
}

impl std::fmt::Debug for DecodeReporter {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DecodeReporter").finish_non_exhaustive()
  }
}

fn is_hidden_size(shown: VideoResolution) -> bool {
  shown.width == 0 || shown.height == 0
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{DeviceId, Id, Snowflake, VideoCodec};

  use super::*;
  use crate::datagram::SimulcastLayer;
  use crate::video::simulcast_layers;

  const CAMERA: MediaStreamId = MediaStreamId(10);
  const SHARE: MediaStreamId = MediaStreamId(20);
  /// 30 fps in the RTP video clock.
  const TICKS_PER_FRAME: u32 = 3_000;
  const FRAME: Duration = Duration::from_micros(33_333);

  mod load {
    use super::*;

    #[test]
    fn decoders_that_fall_behind_step_the_least_important_track_down_and_say_so() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);
      let before = (layer_of(&video, CAMERA), layer_of(&video, SHARE));

      let now = decode_for(&mut video, start, Duration::from_secs(3), 4, Duration::ZERO);
      let asks = video.judge(now);

      assert_eq!(asks.maybe_is_limited, Some(true));
      assert_eq!(asks.layers, vec![(CAMERA, before.0 - 1)]);
      assert_eq!(
        layer_of(&video, SHARE),
        before.1,
        "the share is stepped down last"
      );
    }

    #[test]
    fn frames_late_from_a_busy_receive_path_step_down_too() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);

      let now = decode_for(&mut video, start, Duration::from_secs(3), 0, FRAME);
      let asks = video.judge(now);

      assert_eq!(asks.maybe_is_limited, Some(true));
      assert_eq!(asks.layers.len(), 1);
    }

    #[test]
    fn decoders_that_keep_up_change_nothing() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);

      let now = decode_for(
        &mut video,
        start,
        Duration::from_secs(3),
        0,
        Duration::from_millis(2),
      );

      assert_eq!(video.judge(now), Asks::default());
    }

    #[test]
    fn calm_decoders_get_their_layers_back() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);
      let wanted = layer_of(&video, CAMERA);
      let now = decode_for(&mut video, start, Duration::from_secs(3), 4, Duration::ZERO);
      assert_eq!(video.judge(now).maybe_is_limited, Some(true));

      let mut now = now;
      let mut was_lifted = false;
      for _ in 0..20 {
        now = decode_for(&mut video, now, Duration::from_secs(2), 0, Duration::ZERO);
        if video.judge(now).maybe_is_limited == Some(false) {
          was_lifted = true;
          break;
        }
      }

      assert!(was_lifted);
      assert_eq!(layer_of(&video, CAMERA), wanted);
    }

    #[test]
    fn a_new_track_starts_within_what_the_load_holds_back() {
      let start = Instant::now();
      let mut video = RemoteVideo::new(start);
      video.on_published(CAMERA, VideoTrackKind::Camera, camera_layers());
      let top = video.receive(CAMERA, TOP_LAYER);
      let now = decode_for(&mut video, start, Duration::from_secs(3), 4, Duration::ZERO);
      assert_eq!(video.judge(now).maybe_is_limited, Some(true));
      video.stop_receiving(CAMERA);

      assert!(video.receive(CAMERA, TOP_LAYER) < top);
    }
  }

  mod hidden {
    use super::*;

    #[test]
    fn a_hidden_tile_is_not_decoded_and_asks_for_a_keyframe_when_shown_again() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);
      assert!(video.admits(&frame(CAMERA, 0, true)));

      video.set_shown(CAMERA, VideoResolution { width: 0, height: 0 });
      assert!(!video.admits(&frame(CAMERA, 1, false)));
      assert!(
        !video.admits(&frame(CAMERA, 2, true)),
        "not even keyframes while hidden"
      );

      let asks = video.set_shown(CAMERA, tile());
      assert_eq!(asks.maybe_keyframe, Some(CAMERA));
      assert!(
        !video.admits(&frame(CAMERA, 3, false)),
        "deltas wait for the keyframe"
      );
      assert!(video.admits(&frame(CAMERA, 4, true)));
      assert!(video.admits(&frame(CAMERA, 5, false)));
    }

    #[test]
    fn a_tile_shown_again_without_dropped_frames_needs_no_keyframe() {
      let mut video = video_with_camera_and_share(Instant::now());

      video.set_shown(CAMERA, VideoResolution { width: 0, height: 0 });
      let asks = video.set_shown(CAMERA, tile());

      assert_eq!(asks.maybe_keyframe, None);
    }

    #[test]
    fn only_hidden_tiles_mean_nothing_to_judge() {
      let start = Instant::now();
      let mut video = video_with_camera_and_share(start);
      video.set_shown(CAMERA, VideoResolution { width: 0, height: 0 });
      video.set_shown(SHARE, VideoResolution { width: 0, height: 0 });

      let now = decode_for(&mut video, start, Duration::from_secs(3), 4, Duration::ZERO);

      assert_eq!(video.judge(now).maybe_is_limited, None);
    }

    #[test]
    fn frames_of_tracks_not_received_here_pass() {
      let mut video = RemoteVideo::new(Instant::now());

      assert!(video.admits(&frame(CAMERA, 0, false)));
    }
  }

  mod layers {
    use super::*;

    #[test]
    fn the_app_cap_and_the_view_both_bound_the_layer() {
      let mut video = RemoteVideo::new(Instant::now());
      video.on_published(CAMERA, VideoTrackKind::Camera, camera_layers());

      assert_eq!(video.receive(CAMERA, 1), 1);
      let asks = video.set_quality(CAMERA, Some(180));
      assert_eq!(asks.layers, vec![(CAMERA, 0)]);
      assert_eq!(video.set_quality(CAMERA, None).layers, vec![(CAMERA, 1)]);
    }

    #[test]
    fn tracks_not_received_ask_for_nothing() {
      let mut video = RemoteVideo::new(Instant::now());
      video.on_published(CAMERA, VideoTrackKind::Camera, camera_layers());

      assert_eq!(video.set_shown(CAMERA, tile()), Asks::default());
    }
  }

  #[cfg(feature = "call-view")]
  #[test]
  fn stats_show_the_queue_and_the_held_back_layer() {
    use teamcraft_client_api::video_stats::{DecodeOverload, VideoStatsReport, VideoTrackStats};

    let start = Instant::now();
    let mut video = video_with_camera_and_share(start);
    let now = decode_for(&mut video, start, Duration::from_secs(3), 4, Duration::ZERO);
    video.judge(now);
    let mut report = VideoStatsReport {
      tracks: vec![
        VideoTrackStats::Received(received_stats(CAMERA)),
        VideoTrackStats::Received(received_stats(SHARE)),
      ],
      ..VideoStatsReport::default()
    };

    video.add_decoding_stats(&mut report);

    let [
      VideoTrackStats::Received(camera),
      VideoTrackStats::Received(share),
    ] = &report.tracks[..]
    else {
      panic!("two received tracks");
    };
    assert_eq!(camera.maybe_decode_queue, Some(4));
    let limit = camera.maybe_device_limit.as_ref().unwrap();
    assert_eq!(limit.layer, layer_of(&video, CAMERA));
    assert_eq!(limit.maybe_reason, Some(DecodeOverload::QueuedFrames));
    assert_eq!(share.maybe_device_limit, None);
  }

  #[cfg(feature = "call-view")]
  fn received_stats(stream_id: MediaStreamId) -> teamcraft_client_api::video_stats::ReceivedVideoStats {
    teamcraft_client_api::video_stats::ReceivedVideoStats {
      stream_id,
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      layers: Vec::new(),
      maybe_layer: None,
      maybe_max_layer: None,
      packets: 0,
      bytes: 0,
      packets_lost: 0,
      packets_recovered: 0,
      frames: 0,
      frames_discarded: 0,
      maybe_jitter_ms: None,
      keyframe_requests: 0,
      maybe_longest_gap_ms: None,
      maybe_max_framerate: None,
      is_sender_capture_stalled: false,
      maybe_decode_queue: None,
      maybe_device_limit: None,
    }
  }

  /// A camera and a screen share, both received at their top layer in large tiles.
  fn video_with_camera_and_share(now: Instant) -> RemoteVideo {
    let mut video = RemoteVideo::new(now);
    video.on_published(CAMERA, VideoTrackKind::Camera, camera_layers());
    video.on_published(SHARE, VideoTrackKind::Screen, camera_layers());
    video.receive(CAMERA, TOP_LAYER);
    video.receive(SHARE, TOP_LAYER);
    video.set_shown(CAMERA, tile());
    video.set_shown(SHARE, tile());
    video
  }

  fn camera_layers() -> Vec<VideoLayerInfo> {
    simulcast_layers(
      VideoResolution {
        width: 1280,
        height: 720,
      },
      30,
      1_500_000,
      3,
    )
  }

  fn tile() -> VideoResolution {
    VideoResolution {
      width: 1280,
      height: 720,
    }
  }

  fn layer_of(video: &RemoteVideo, stream_id: MediaStreamId) -> u8 {
    video.received[&stream_id].asked_layer
  }

  /// Decodes both tracks at 30 fps for `duration` from `start`, each frame behind `queued` others
  /// and `late_by` after its arrival; returns the time after the last frame.
  fn decode_for(
    video: &mut RemoteVideo,
    start: Instant,
    duration: Duration,
    queued: u32,
    late_by: Duration,
  ) -> Instant {
    let frames = (duration.as_micros() / FRAME.as_micros()) as u32;
    for index in 0..frames {
      for stream_id in [CAMERA, SHARE] {
        video.on_decoding(stream_id, RtpTimestamp(index * TICKS_PER_FRAME), queued, late_by);
      }
    }
    start + duration
  }

  fn frame(stream_id: MediaStreamId, index: u32, is_keyframe: bool) -> ReceivedVideoFrame {
    ReceivedVideoFrame {
      sender: DeviceId::from_snowflake(Snowflake::new(2).unwrap()),
      stream_id,
      codec: VideoCodec::H264,
      layer: SimulcastLayer::BASE,
      timestamp: RtpTimestamp(index * TICKS_PER_FRAME),
      is_keyframe,
      data: vec![0, 0, 0, 1],
      arrived_at: Instant::now(),
    }
  }
}
