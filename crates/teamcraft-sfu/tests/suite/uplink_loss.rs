//! A presenter behind a lossy uplink, on a simulated clock (#2189): does every slide reach every
//! viewer soon, and how many keyframes does it cost?
//!
//! The SFU is the real [`Forwarder`]; around it a model of the rest, deterministic and seeded:
//!
//! - **The uplink** loses 1 to 3 % of the packets in bursts and delays them by 20 to 60 ms, both
//!   walking as a real link's do ([`Link`], the relay's `Impairment` in `common::relay`).
//! - **The presenter** shares slides shown for one and for two seconds in turn: a change is a
//!   frame of [`SLIDE_PACKETS`], and a still screen sends its picture again every second in one
//!   packet (`VideoSendConfig::maybe_idle_refresh`). It keeps what it sent, sends what the SFU
//!   asks for once more, and a keyframe of [`KEYFRAME_PACKETS`] when asked, each after
//!   [`SIGNALING_DELAY`].
//! - **The viewers** get what the SFU forwards after [`DOWNLINK_DELAY`] (their links lose
//!   nothing: #1762 covers those). They decode a frame once it is whole and the frames before it
//!   since a keyframe were, as a decoder does, and ask the SFU for a keyframe when a gap held their
//!   picture for [`VIEWER_GIVES_UP_AFTER`] (their NACKs run out: the SFU never had the packets).
//!
//! The real-time end-to-end measurement through the real client, with its encoders and estimator,
//! is `screen_uplink_bursts::measure_slides_through_a_lossy_uplink`.

// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::{Duration, Instant};

use teamcraft_media::datagram::{DatagramHeader, MediaDatagram, SimulcastLayer, StreamId};
use teamcraft_media::dave::DaveParticipantId;
use teamcraft_media::fragment::FrameFragment;
use teamcraft_media::rtp::{PayloadType, RtpHeader, RtpPacket, RtpTimestamp, SequenceNumber, Ssrc};
use teamcraft_proto::{DeviceId, MediaCapabilities, VideoCodec, VideoLayerInfo, VideoTrackKind};
use teamcraft_sfu::forwarding::{Effect, Forwarder, ForwardingConfig};
use teamcraft_sfu::ids::device_id_from_dave;

/// How long each run is simulated.
const RUN: Duration = Duration::from_secs(60);
/// The first slides go out while the viewers get their first keyframe: they aren't judged.
const SETTLE: Duration = Duration::from_secs(3);
/// The simulation's step.
const TICK: Duration = Duration::from_millis(1);
/// How often the SFU's timers run (the server's `PERIODIC_INTERVAL`).
const TIMER: Duration = Duration::from_millis(100);
/// From the SFU to the presenter: signaling over a link that loses nothing here.
const SIGNALING_DELAY: Duration = Duration::from_millis(30);
/// From the SFU to a viewer.
const DOWNLINK_DELAY: Duration = Duration::from_millis(20);
/// The presenter paces its packets this far apart.
const PACING: Duration = Duration::from_millis(1);
/// Packets of a slide change and of a keyframe of the shared screen.
const SLIDE_PACKETS: u16 = 30;
const KEYFRAME_PACKETS: u16 = 40;
/// A still screen sends its picture again this often.
const IDLE_REFRESH: Duration = Duration::from_secs(1);
/// Slides shown for one and for two seconds, in turn.
const SLIDE_INTERVALS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
/// A viewer whose picture a gap held this long asks for a keyframe: its three NACKs ran out.
const VIEWER_GIVES_UP_AFTER: Duration = Duration::from_millis(300);
/// The longest from a slide change to every viewer showing the slide: a lost packet is asked for
/// at once and comes within about 0.15 s (uplink, signaling and downlink), again if that copy got
/// lost too; where that isn't enough, the keyframe asked for 0.3 s after the loss.
const MAX_SLIDE_DELAY: Duration = Duration::from_millis(700);
/// Keyframes the presenter may be asked for per second: far from one per loss.
const MAX_KEYFRAMES_PER_SEC: f64 = 0.25;
/// Seeds of the uplink: each run's losses fall elsewhere.
const SEEDS: [u64; 5] = [2189, 7, 1871, 42, 1005];
/// Camera: frames per second and packets per frame (a 720p camera at about 1 Mbit/s).
const CAMERA_FPS: u32 = 30;
const CAMERA_PACKETS: u16 = 4;
/// A camera's picture that stands longer than this counts as frozen.
const FROZEN_AFTER: Duration = Duration::from_millis(200);

const PRESENTER: u64 = 1;
const VIEWERS: [u64; 2] = [2, 3];
const VIDEO: StreamId = StreamId(100);

/// #2189: every slide, shown for one or for two seconds, reaches every viewer within
/// [`MAX_SLIDE_DELAY`] through a presenter's uplink that loses 1 to 3 % of its packets in bursts,
/// and the presenter is asked for few keyframes. Without the SFU's repair (the same runs, for
/// comparison) slides come late or not at all.
#[test]
fn slides_reach_every_viewer_through_a_lossy_uplink() {
  let mut report = Vec::new();
  for seed in SEEDS {
    let repaired = simulate(VideoTrackKind::Screen, seed, true);
    let unrepaired = simulate(VideoTrackKind::Screen, seed, false);
    report.push(format!(
      "seed {seed}: repaired {} | unrepaired {}",
      repaired.summary(),
      unrepaired.summary()
    ));
    assert!(
      (0.005..0.05).contains(&repaired.loss),
      "the uplink lost {:.1} %, not about 1 to 3 %",
      repaired.loss * 100.0
    );
    let delays = repaired.slide_delays();
    assert!(delays.len() >= 30, "{} slides judged", delays.len());
    let worst = delays.iter().max().unwrap();
    assert!(
      delays.iter().all(|delay| *delay <= MAX_SLIDE_DELAY),
      "a slide took {worst:?} to a viewer (seed {seed}):\n{}",
      repaired.summary()
    );
    assert!(
      repaired.keyframes_per_sec() <= MAX_KEYFRAMES_PER_SEC,
      "a keyframe storm (seed {seed}):\n{}",
      repaired.summary()
    );
    assert!(
      repaired.slides_late() < unrepaired.slides_late() || unrepaired.slides_late() == 0,
      "the repair made no difference (seed {seed})"
    );
  }
  println!("{}", report.join("\n"));
}

/// #2189: a camera through the same uplink isn't worse off for the repair: its picture freezes no
/// longer, and the presenter is asked for no more keyframes than without it.
#[test]
fn a_camera_through_a_lossy_uplink_does_not_regress() {
  let mut report = Vec::new();
  for seed in SEEDS {
    let repaired = simulate(VideoTrackKind::Camera, seed, true);
    let unrepaired = simulate(VideoTrackKind::Camera, seed, false);
    report.push(format!(
      "seed {seed}: repaired {} | unrepaired {}",
      repaired.summary(),
      unrepaired.summary()
    ));
    assert!(
      repaired.frozen() <= unrepaired.frozen(),
      "frozen {:?} with the repair, {:?} without (seed {seed})",
      repaired.frozen(),
      unrepaired.frozen()
    );
    assert!(
      repaired.keyframes_asked <= unrepaired.keyframes_asked,
      "{} keyframes with the repair, {} without (seed {seed})",
      repaired.keyframes_asked,
      unrepaired.keyframes_asked
    );
    assert!(repaired.keyframes_per_sec() <= MAX_KEYFRAMES_PER_SEC);
  }
  println!("{}", report.join("\n"));
}

/// What a simulated run recorded.
struct Outcome {
  /// When each slide went up (a camera: empty).
  changes: Vec<Instant>,
  /// Per viewer: when it decoded each frame, with the content (the slide) it shows.
  decoded: HashMap<DeviceId, Vec<(Instant, u32)>>,
  keyframes_asked: usize,
  /// The share of the presenter's packets the uplink lost.
  loss: f64,
  start: Instant,
}

/// One frame the presenter sent.
#[derive(Debug, Clone, Copy)]
struct Frame {
  number: u16,
  count: u16,
  is_keyframe: bool,
  /// The slide (or camera picture) it shows.
  content: u32,
}

/// What happens at a point of the simulated time.
enum Event {
  /// A packet of the presenter reaches the SFU.
  AtSfu(Vec<u8>),
  /// A packet the SFU forwarded reaches a viewer.
  AtViewer(DeviceId, Vec<u8>),
  /// The SFU's request reaches the presenter.
  AtPresenter(Request),
}

enum Request {
  Packets(Vec<u16>),
  Keyframe,
}

/// The presenter: numbers and keeps what it sends.
struct Presenter {
  next_sequence: u16,
  next_frame: u16,
  sent: HashMap<u16, Vec<u8>>,
  /// When the next packet may leave (pacing).
  next_free: Instant,
  content: u32,
}

/// A viewer's decoder: frames as they complete, decoded in order from a keyframe on.
#[derive(Default)]
struct Viewer {
  /// Per frame: its fragments so far, how many it has, whether a keyframe, its content.
  frames: BTreeMap<u16, (BTreeSet<u16>, Frame)>,
  maybe_last_decoded: Option<u16>,
  /// Since when a later frame waits for an earlier one that isn't whole.
  maybe_stuck_since: Option<Instant>,
}

/// A link that loses and delays as `common::relay::Impairment` does, on the simulated clock.
struct Link {
  rng: u64,
  stepped_at: Instant,
  loss: f64,
  delay: Duration,
  is_bad: bool,
  last_arrival: Instant,
}

fn simulate(kind: VideoTrackKind, seed: u64, is_repaired: bool) -> Outcome {
  let start = Instant::now();
  let mut config = ForwardingConfig::default();
  if !is_repaired {
    config.uplink_repair.max_requests = 0;
  }
  let mut forwarder = Forwarder::new(config);
  for n in [PRESENTER].into_iter().chain(VIEWERS) {
    forwarder.add_participant(
      device(n),
      StreamId(u32::try_from(n).unwrap()),
      capabilities(),
      start,
    );
  }
  forwarder
    .publish_video(
      device(PRESENTER),
      VIDEO,
      kind.clone(),
      VideoCodec::Vp9,
      layers(),
      None,
    )
    .unwrap();
  for viewer in VIEWERS {
    forwarder
      .subscribe(device(viewer), VIDEO, Some(0), start)
      .unwrap();
  }

  let mut sim = Sim {
    forwarder,
    events: BTreeMap::new(),
    next_event: 0,
    presenter: Presenter {
      next_sequence: 1,
      next_frame: 0,
      sent: HashMap::new(),
      next_free: start,
      content: 0,
    },
    viewers: VIEWERS
      .into_iter()
      .map(|n| (device(n), Viewer::default()))
      .collect(),
    link: Link::new(seed, start),
    sent_packets: 0,
    lost_packets: 0,
    keyframes_asked: 0,
    decoded: HashMap::new(),
  };
  // The share starts with a keyframe.
  sim.send_frame(KEYFRAME_PACKETS, true, start);

  let mut changes = Vec::new();
  let mut next_change = start + Duration::from_secs(1);
  let mut slide = 0usize;
  let mut last_sent = start;
  let mut next_timer = start;
  let mut next_camera_frame = start;
  let mut now = start;
  while now < start + RUN {
    match kind {
      VideoTrackKind::Screen => {
        if now >= next_change {
          changes.push(now);
          sim.presenter.content += 1;
          sim.send_frame(SLIDE_PACKETS, false, now);
          last_sent = now;
          next_change = now + SLIDE_INTERVALS[slide % SLIDE_INTERVALS.len()];
          slide += 1;
        } else if now.saturating_duration_since(last_sent) >= IDLE_REFRESH {
          sim.send_frame(1, false, now);
          last_sent = now;
        }
      },
      _ => {
        if now >= next_camera_frame {
          sim.presenter.content += 1;
          sim.send_frame(CAMERA_PACKETS, false, now);
          next_camera_frame = now + Duration::from_secs(1) / CAMERA_FPS;
        }
      },
    }
    sim.run_events_until(now);
    if now >= next_timer {
      sim.run_timers(now);
      next_timer = now + TIMER;
    }
    now += TICK;
  }
  Outcome {
    changes,
    decoded: sim.decoded,
    keyframes_asked: sim.keyframes_asked,
    loss: sim.lost_packets as f64 / sim.sent_packets.max(1) as f64,
    start,
  }
}

/// The simulation's state.
struct Sim {
  forwarder: Forwarder,
  /// By time, then by the order they were scheduled in.
  events: BTreeMap<(Instant, u64), Event>,
  next_event: u64,
  presenter: Presenter,
  viewers: HashMap<DeviceId, Viewer>,
  link: Link,
  sent_packets: usize,
  lost_packets: usize,
  keyframes_asked: usize,
  decoded: HashMap<DeviceId, Vec<(Instant, u32)>>,
}

impl Sim {
  fn schedule(&mut self, at: Instant, event: Event) {
    self.events.insert((at, self.next_event), event);
    self.next_event += 1;
  }

  /// The presenter sends a frame of `count` packets, paced, through the uplink.
  fn send_frame(&mut self, count: u16, is_keyframe: bool, now: Instant) {
    let frame = Frame {
      number: self.presenter.next_frame,
      count,
      is_keyframe,
      content: self.presenter.content,
    };
    self.presenter.next_frame = self.presenter.next_frame.wrapping_add(1);
    for index in 0..count {
      let sequence_number = self.presenter.next_sequence;
      self.presenter.next_sequence = sequence_number.wrapping_add(1);
      let datagram = video_datagram(sequence_number, frame, index);
      self.presenter.sent.insert(sequence_number, datagram.clone());
      self.send_on_uplink(datagram, now);
    }
  }

  fn send_on_uplink(&mut self, datagram: Vec<u8>, now: Instant) {
    let leaves = self.presenter.next_free.max(now);
    self.presenter.next_free = leaves + PACING;
    self.sent_packets += 1;
    match self.link.pass(leaves) {
      Some(arrives) => self.schedule(arrives, Event::AtSfu(datagram)),
      None => self.lost_packets += 1,
    }
  }

  fn run_events_until(&mut self, now: Instant) {
    while let Some(entry) = self.events.first_entry() {
      let &(at, _) = entry.key();
      if at > now {
        return;
      }
      let event = entry.remove();
      match event {
        Event::AtSfu(datagram) => {
          let route = self.forwarder.route(device(PRESENTER), &datagram, at).unwrap();
          for (receiver, delivery) in route.deliveries {
            self.schedule(at + DOWNLINK_DELAY, Event::AtViewer(receiver, delivery));
          }
          self.apply(route.effects, at);
        },
        Event::AtViewer(viewer, datagram) => self.receive(viewer, &datagram, at),
        Event::AtPresenter(Request::Packets(sequence_numbers)) => {
          for sequence_number in sequence_numbers {
            if let Some(datagram) = self.presenter.sent.get(&sequence_number).cloned() {
              self.send_on_uplink(datagram, at);
            }
          }
        },
        Event::AtPresenter(Request::Keyframe) => {
          self.send_frame(KEYFRAME_PACKETS, true, at);
        },
      }
    }
  }

  fn run_timers(&mut self, now: Instant) {
    for n in [PRESENTER].into_iter().chain(VIEWERS) {
      let effects = self.forwarder.on_timer(device(n), now);
      self.apply(effects, now);
    }
    let stuck: Vec<DeviceId> = self
      .viewers
      .iter_mut()
      .filter_map(|(id, viewer)| viewer.gives_up(now).then_some(*id))
      .collect();
    for viewer in stuck {
      let effects = self.forwarder.request_keyframe(viewer, VIDEO, now);
      self.apply(effects, now);
    }
  }

  /// Requests of the SFU reach the presenter after [`SIGNALING_DELAY`].
  fn apply(&mut self, effects: Vec<Effect>, now: Instant) {
    for effect in effects {
      match effect {
        Effect::RetransmitRequest { sequence_numbers, .. } => self.schedule(
          now + SIGNALING_DELAY,
          Event::AtPresenter(Request::Packets(sequence_numbers)),
        ),
        Effect::KeyframeRequest { .. } => {
          self.keyframes_asked += 1;
          self.schedule(now + SIGNALING_DELAY, Event::AtPresenter(Request::Keyframe));
        },
        _ => {},
      }
    }
  }

  fn receive(&mut self, viewer_id: DeviceId, datagram: &[u8], now: Instant) {
    let media = MediaDatagram::parse(datagram).unwrap();
    let (header, _) = RtpHeader::parse(media.rtp).unwrap();
    let fragment = FrameFragment::from_header(&header).unwrap();
    let content = u32::from(header.timestamp.0);
    let Some(viewer) = self.viewers.get_mut(&viewer_id) else {
      return;
    };
    let frame = Frame {
      number: fragment.frame_number,
      count: fragment.count,
      is_keyframe: fragment.is_keyframe,
      content,
    };
    viewer
      .frames
      .entry(frame.number)
      .or_insert_with(|| (BTreeSet::new(), frame))
      .0
      .insert(fragment.index);
    for content in viewer.decode(now) {
      self.decoded.entry(viewer_id).or_default().push((now, content));
    }
  }
}

impl Viewer {
  /// Decodes what can be decoded now; returns the contents of the frames decoded.
  fn decode(&mut self, now: Instant) -> Vec<u32> {
    let mut decoded = Vec::new();
    let is_whole = |(fragments, frame): &(BTreeSet<u16>, Frame)| fragments.len() == usize::from(frame.count);
    // A whole keyframe after the last decoded frame starts the decoder over from it.
    let after_last = |number: u16| self.maybe_last_decoded.is_none_or(|last| number > last);
    let maybe_keyframe = self
      .frames
      .iter()
      .filter(|(number, entry)| after_last(**number) && entry.1.is_keyframe && is_whole(entry))
      .map(|(number, _)| *number)
      .next_back();
    if let Some(keyframe) = maybe_keyframe {
      decoded.push(self.frames[&keyframe].1.content);
      self.maybe_last_decoded = Some(keyframe);
    }
    while let Some(last) = self.maybe_last_decoded {
      let next = last.wrapping_add(1);
      match self.frames.get(&next) {
        Some(entry) if is_whole(entry) => {
          decoded.push(entry.1.content);
          self.maybe_last_decoded = Some(next);
        },
        _ => break,
      }
    }
    if let Some(last) = self.maybe_last_decoded {
      // Frames up to the decoded one are done with.
      self.frames.retain(|number, _| *number > last);
    }
    // Fragments of a frame that isn't whole: the viewer NACKs them, and gives up after a while.
    let is_waiting = !self.frames.is_empty();
    self.maybe_stuck_since = match (is_waiting, self.maybe_stuck_since) {
      (false, _) => None,
      (true, None) => Some(now),
      (true, Some(since)) => Some(since),
    };
    decoded
  }

  /// Whether the viewer gives up waiting and asks for a keyframe now (again after as long).
  fn gives_up(&mut self, now: Instant) -> bool {
    let Some(since) = self.maybe_stuck_since else {
      return false;
    };
    if now.saturating_duration_since(since) < VIEWER_GIVES_UP_AFTER {
      return false;
    }
    self.maybe_stuck_since = Some(now);
    true
  }
}

impl Outcome {
  /// Per viewer and judged slide: from the change to the viewer showing it (or a later slide, which
  /// counts as never: [`Duration::MAX`]).
  fn slide_delays(&self) -> Vec<Duration> {
    let settled = self.start + SETTLE;
    let mut delays = Vec::new();
    for decoded in self.decoded.values() {
      for (slide, changed_at) in self.changes.iter().enumerate() {
        if *changed_at < settled || slide + 1 == self.changes.len() {
          continue;
        }
        let content = u32::try_from(slide + 1).unwrap();
        let maybe_shown = decoded
          .iter()
          .find(|(_, shown)| *shown >= content)
          .filter(|(_, shown)| *shown == content);
        delays.push(maybe_shown.map_or(Duration::MAX, |(at, _)| at.saturating_duration_since(*changed_at)));
      }
    }
    delays
  }

  /// Slides some viewer showed later than [`MAX_SLIDE_DELAY`], or not at all.
  fn slides_late(&self) -> usize {
    self
      .slide_delays()
      .iter()
      .filter(|delay| **delay > MAX_SLIDE_DELAY)
      .count()
  }

  /// A camera's frozen time summed over the viewers: gaps between decoded pictures beyond
  /// [`FROZEN_AFTER`].
  fn frozen(&self) -> Duration {
    let settled = self.start + SETTLE;
    let mut frozen = Duration::ZERO;
    for decoded in self.decoded.values() {
      for pair in decoded.windows(2) {
        let [(before, _), (after, _)] = pair else {
          continue;
        };
        let gap = after.saturating_duration_since(*before);
        if *before >= settled && gap > FROZEN_AFTER {
          frozen += gap;
        }
      }
    }
    frozen
  }

  fn keyframes_per_sec(&self) -> f64 {
    self.keyframes_asked as f64 / RUN.as_secs_f64()
  }

  fn summary(&self) -> String {
    let delays = self.slide_delays();
    let shown: Vec<&Duration> = delays.iter().filter(|delay| **delay != Duration::MAX).collect();
    let worst = shown.iter().max().map_or(0, |delay| delay.as_millis());
    let mut sorted: Vec<u128> = shown.iter().map(|delay| delay.as_millis()).collect();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    format!(
      "loss {:.1} %, slides median {median} ms, worst {worst} ms, never {}, late {}, frozen {} ms, {} keyframes",
      self.loss * 100.0,
      delays.len() - shown.len(),
      self.slides_late(),
      self.frozen().as_millis(),
      self.keyframes_asked
    )
  }
}

impl Link {
  const STEP: Duration = Duration::from_millis(500);
  const LOSS_PERCENT: (f64, f64) = (1.0, 3.0);
  const DELAY_MS: (f64, f64) = (20.0, 60.0);
  const BURST_LOSS: f64 = 0.5;
  const BURST_PACKETS: f64 = 4.0;

  fn new(seed: u64, start: Instant) -> Self {
    let mut link = Self {
      rng: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1,
      stepped_at: start,
      loss: 0.0,
      delay: Duration::ZERO,
      is_bad: false,
      last_arrival: start,
    };
    let (low, high) = Self::LOSS_PERCENT;
    link.loss = (low + link.uniform() * (high - low)) / 100.0;
    let (shortest, longest) = Self::DELAY_MS;
    link.delay = Duration::from_secs_f64((shortest + link.uniform() * (longest - shortest)) / 1000.0);
    link
  }

  /// When a packet that leaves at `leaves` arrives, or `None` if it's lost. Packets keep their order.
  fn pass(&mut self, leaves: Instant) -> Option<Instant> {
    while leaves >= self.stepped_at + Self::STEP {
      self.stepped_at += Self::STEP;
      let (low, high) = Self::LOSS_PERCENT;
      let step = (self.uniform() - 0.5) / 2.0 * (high - low) / 100.0;
      self.loss = (self.loss + step).clamp(low / 100.0, high / 100.0);
      let (shortest, longest) = Self::DELAY_MS;
      let step = (self.uniform() - 0.5) / 2.0 * (longest - shortest);
      let delay_ms = (self.delay.as_secs_f64() * 1000.0 + step).clamp(shortest, longest);
      self.delay = Duration::from_secs_f64(delay_ms / 1000.0);
    }
    let bad_share = (self.loss / Self::BURST_LOSS).min(0.9);
    let leave_bad = 1.0 / Self::BURST_PACKETS;
    let enter_bad = bad_share * leave_bad / (1.0 - bad_share);
    let roll = self.uniform();
    self.is_bad = if self.is_bad {
      roll >= leave_bad
    } else {
      roll < enter_bad
    };
    if self.is_bad && self.uniform() < Self::BURST_LOSS {
      return None;
    }
    let arrives = (leaves + self.delay).max(self.last_arrival);
    self.last_arrival = arrives;
    Some(arrives)
  }

  /// Uniform in `[0, 1)` (xorshift).
  fn uniform(&mut self) -> f64 {
    self.rng ^= self.rng << 13;
    self.rng ^= self.rng >> 7;
    self.rng ^= self.rng << 17;
    (self.rng >> 11) as f64 / (1u64 << 53) as f64
  }
}

/// A video packet of `frame`'s fragment `index`; the RTP timestamp carries the frame's content.
fn video_datagram(sequence_number: u16, frame: Frame, index: u16) -> Vec<u8> {
  let mut header = RtpHeader::new(
    PayloadType::new(98).unwrap(),
    SequenceNumber(sequence_number),
    RtpTimestamp(frame.content),
    Ssrc(VIDEO.0),
  );
  FrameFragment {
    frame_number: frame.number,
    index,
    count: frame.count,
    is_keyframe: frame.is_keyframe,
    is_capture_stalled: false,
  }
  .write_to(&mut header);
  let packet = RtpPacket {
    header,
    payload: &[0xAB; 1000],
  }
  .to_bytes()
  .unwrap();
  MediaDatagram {
    header: DatagramHeader {
      stream_id: VIDEO,
      layer: SimulcastLayer::BASE,
      maybe_transport_seq: None,
    },
    rtp: &packet,
  }
  .to_bytes(1200)
  .unwrap()
}

fn capabilities() -> MediaCapabilities {
  MediaCapabilities {
    video_encoders: vec![VideoCodec::Vp9],
    video_decoders: vec![VideoCodec::Vp9],
    max_simulcast_layers: 3,
    ..MediaCapabilities::default()
  }
}

fn layers() -> Vec<VideoLayerInfo> {
  vec![VideoLayerInfo {
    layer: 0,
    width: 1280,
    height: 720,
    max_framerate: 30,
    max_bitrate_bps: 150_000,
    maybe_codec: None,
  }]
}

fn device(raw: u64) -> DeviceId {
  device_id_from_dave(DaveParticipantId(raw)).unwrap()
}
