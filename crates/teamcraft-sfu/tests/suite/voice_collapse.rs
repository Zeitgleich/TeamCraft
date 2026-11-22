//! A sender's upload that collapses while it talks with its camera on (#1889): 5 Mbit/s to
//! 120 kbit/s from one second to the next, for 20 s, three times, behind a bottleneck that holds
//! seconds at the collapsed rate (a cable modem's or a phone's buffer, as on the drop rig), then
//! again behind a home router's quarter of a second. How
//! much of the voice the viewer can play is what counts: the target is at most 1–3 s of it lost
//! per collapse, and none on the good link.
//!
//! `measure_voice_through_upload_collapses` (ignored) prints per second how much of the voice
//! spoken then arrived in time at the viewer, what the bottleneck carried (small datagrams are
//! voice and acknowledgements, large ones video) and what the presenter's engine decided, then the
//! bad seconds (more than 40 ms of voice missing) per collapse: `cargo test -p teamcraft-sfu
//! --test suite measure_voice_through -- --ignored --nocapture`.
// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::net::{Ipv6Addr, SocketAddr};
use std::time::{Duration, Instant};

use teamcraft_media::audio_frame::opus_packet_samples;
use teamcraft_media::audio_io::{AudioSink, AudioSource};
use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::call_native::NativeCall;
use teamcraft_media::video::simulcast_layers;
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::{DeviceId, VideoTrack, VideoTrackKind};

use crate::bandwidth_limits::{join, start_sfu, wait_until_secured};
use crate::common::busy_camera::{BusyCamera, FRAMERATE, HD};
use crate::common::relay::{Impairment, MAX_QUEUE_DELAY, Relay};

const GOOD_BPS: u64 = 5_000_000;
const COLLAPSED_BPS: u64 = 120_000;
/// Each collapse lasts this long, and so does the good link before, between and after them.
const PHASE: Duration = Duration::from_secs(20);
const COLLAPSES: u32 = 3;
/// What the bottleneck holds: about the drop rig's shaper at 120 kbit/s (tbf, 32 kB). The second
/// round has a home router's ([`MAX_QUEUE_DELAY`]).
const DEEP_QUEUE: Duration = Duration::from_secs(2);
/// Each way, between the presenter and the SFU.
const ONE_WAY_DELAY: (Duration, Duration) = (Duration::from_millis(12), Duration::from_millis(20));
const CAMERA_BITRATE_BPS: u32 = 2_500_000;
/// Voice that arrives this much later than the fastest misses its turn in the jitter buffer.
const MAX_LATENESS: Duration = Duration::from_millis(200);
/// A second that misses more voice than this sounds bad (the rig's measure).
const MAX_MISSING_PER_SECOND: Duration = Duration::from_millis(40);
/// Opus's clock.
const SAMPLES_PER_SECOND: u64 = 48_000;
const PRESENTER: i64 = 1;
const VIEWER: i64 = 2;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints two timelines, takes about five minutes"]
async fn measure_voice_through_upload_collapses() {
  for queue in [DEEP_QUEUE, MAX_QUEUE_DELAY] {
    let report = run(queue).await;
    println!("bottleneck queue {} ms\n{report}", queue.as_millis());
  }
}

/// Three collapses behind a bottleneck that holds `queue`: the report.
async fn run(queue: Duration) -> String {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let start = Instant::now();
  let relay = Relay::start(sfu, start);
  relay.to_sfu.set_limit(Some(GOOD_BPS));
  relay.to_sfu.set_max_queue_delay(queue);
  // A home connection's round trip to its SFU, a little unsteady; on loopback QUIC would see a
  // round trip of none, and size what it holds for that.
  for (seed, direction) in [(1, &relay.to_sfu), (2, &relay.to_client)] {
    direction.set_impairment(Some(Impairment {
      seed,
      loss_percent: (0.0, 0.0),
      delay: ONE_WAY_DELAY,
      spared_below_bytes: 0,
    }));
  }
  let mut presenter = join(relay.address, certificate_hash, PRESENTER, &[VIEWER]).await;
  let mut viewer = join(sfu, certificate_hash, VIEWER, &[PRESENTER]).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;
  viewer
    .set_audio(
      AudioSource::Tone {
        frequency_hz: 440.0,
        amplitude: 0.3,
      },
      AudioSink::Frames,
    )
    .await
    .unwrap();
  viewer.set_muted(true);
  presenter.start_video(camera(start)).await.unwrap();
  // The viewer's camera comes down the presenter's link: its acknowledgements share the upload.
  viewer.start_video(camera(start)).await.unwrap();
  let video_on_at = start.elapsed();
  let collapses: Vec<Duration> = (0..COLLAPSES)
    .map(|index| video_on_at + PHASE * (2 * index + 1))
    .collect();
  let end_at = video_on_at + PHASE * (2 * COLLAPSES + 1);
  let mut recorder = Recorder::default();
  let mut steps: Vec<(Duration, u64)> = collapses
    .iter()
    .flat_map(|at| [(*at, COLLAPSED_BPS), (*at + PHASE, GOOD_BPS)])
    .collect();
  steps.reverse();
  while start.elapsed() < end_at {
    while steps.last().is_some_and(|(at, _)| start.elapsed() >= *at) {
      let (_, bps) = steps.pop().unwrap();
      relay.to_sfu.set_limit(Some(bps));
    }
    tokio::select! {
      Some(event) = viewer.next_event() => recorder.on_viewer_event(&viewer, event, start),
      Some(event) = presenter.next_event() => recorder.on_presenter_event(&presenter, event, start),
      () = tokio::time::sleep(Duration::from_millis(10)) => {},
    }
  }
  presenter.stop_video(VideoTrackKind::Camera);
  viewer.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  viewer.leave().await.unwrap();
  let link = relay.to_sfu.log.lock().unwrap().clone();
  recorder.report(&link, video_on_at, &collapses, end_at)
}

/// What the test saw, as it came.
#[derive(Default)]
struct Recorder {
  /// The presenter's voice at the viewer: when it came, its RTP sequence number and its length in
  /// samples.
  voice: Vec<(Duration, u16, u64)>,
  /// The presenter's decisions, by when.
  decisions: Vec<(Duration, String)>,
  watching: Vec<i64>,
}

impl Recorder {
  fn on_viewer_event(&mut self, viewer: &NativeCall, event: CallEngineEvent, start: Instant) {
    match event {
      CallEngineEvent::AudioFrame {
        sender,
        sequence_number,
        opus,
        ..
      } if sender == device(PRESENTER) => {
        let samples = opus_packet_samples(&opus).map_or(960, |samples| samples as u64);
        self.voice.push((start.elapsed(), sequence_number.0, samples));
      },
      CallEngineEvent::VideoPublished(track) => self.watch(viewer, VIEWER, track),
      _ => {},
    }
  }

  fn on_presenter_event(&mut self, presenter: &NativeCall, event: CallEngineEvent, start: Instant) {
    let maybe_line = match event {
      CallEngineEvent::VideoPublished(track) => {
        self.watch(presenter, PRESENTER, track);
        None
      },
      CallEngineEvent::VideoHeldForVoice(is_held) => Some(format!("HELD {is_held}")),
      CallEngineEvent::VoiceEncoding(encoding) => Some(format!(
        "VOICE {}k {:?}",
        encoding.bitrate_bps / 1000,
        encoding.frame
      )),
      _ => None,
    };
    if let Some(line) = maybe_line {
      self.decisions.push((start.elapsed(), line));
    }
  }

  /// Each side watches the other's camera, the top layer asked for.
  fn watch(&mut self, call: &NativeCall, who: i64, track: VideoTrack) {
    if track.kind != VideoTrackKind::Camera || self.watching.contains(&who) {
      return;
    }
    self.watching.push(who);
    call.watch_video(track.stream_id, None, Box::new(|_| {}));
  }

  fn report(
    &self,
    link: &[(Duration, usize, bool)],
    from: Duration,
    collapses: &[Duration],
    end: Duration,
  ) -> String {
    let in_time = self.voice_in_time_per_second();
    // The first seconds go to the cameras' start; the last one is cut off by the end.
    let seconds = (from.as_secs() + 3)..end.as_secs();
    let is_bad = |second: u64| {
      let got = in_time.get(&second).copied().unwrap_or_default();
      Duration::from_secs(1).saturating_sub(got) > MAX_MISSING_PER_SECOND
    };
    let collapse_of = |second: u64| {
      collapses
        .iter()
        .position(|at| second >= at.as_secs() && second < (*at + PHASE).as_secs())
    };
    let mut out = String::new();
    for second in seconds.clone() {
      let (mut small, mut small_lost, mut large, mut large_lost) = (0usize, 0usize, 0usize, 0usize);
      for (_, bytes, is_dropped) in link.iter().filter(|(at, ..)| at.as_secs() == second) {
        match (*bytes < 250, *is_dropped) {
          (true, false) => small += bytes,
          (true, true) => small_lost += bytes,
          (false, false) => large += bytes,
          (false, true) => large_lost += bytes,
        }
      }
      let decisions: Vec<&str> = self
        .decisions
        .iter()
        .filter(|(at, _)| at.as_secs() == second)
        .map(|(_, line)| line.as_str())
        .collect();
      writeln!(
        out,
        "{second:3} {} {} voice {:4} ms | small {:3}k (lost {:3}k) large {:5}k (lost {:5}k) | {}",
        if collapse_of(second).is_some() {
          "LOW "
        } else {
          "    "
        },
        if is_bad(second) { "BAD" } else { "   " },
        in_time.get(&second).copied().unwrap_or_default().as_millis(),
        small * 8 / 1000,
        small_lost * 8 / 1000,
        large * 8 / 1000,
        large_lost * 8 / 1000,
        decisions.join(", ")
      )
      .unwrap();
    }
    let mut per_collapse = vec![0u32; collapses.len()];
    let mut missing_ms = vec![0u128; collapses.len()];
    let mut good_link = (0u32, 0u32);
    for second in seconds {
      match collapse_of(second) {
        Some(index) => {
          per_collapse[index] += u32::from(is_bad(second));
          let got = in_time.get(&second).copied().unwrap_or_default();
          missing_ms[index] += Duration::from_secs(1).saturating_sub(got).as_millis();
        },
        None => {
          good_link.0 += u32::from(is_bad(second));
          good_link.1 += 1;
        },
      }
    }
    writeln!(
      out,
      "bad voice seconds per collapse (of {}): {per_collapse:?}; on the good link: {} of {}",
      PHASE.as_secs(),
      good_link.0,
      good_link.1
    )
    .unwrap();
    let held_after: Vec<String> = collapses
      .iter()
      .map(|at| {
        self
          .decisions
          .iter()
          .find(|(decided, line)| decided >= at && line == "HELD true")
          .map_or("never".to_owned(), |(decided, _)| {
            format!("{} ms", (*decided - *at).as_millis())
          })
      })
      .collect();
    writeln!(
      out,
      "voice missing per collapse: {missing_ms:?} ms; video held after: {held_after:?}"
    )
    .unwrap();
    out
  }

  /// Per second the voice was spoken in (since the start), how much of it came in time.
  ///
  /// When it was spoken follows from its sequence number and length: the presenter talks without a
  /// pause. The earliest arrival relative to that is the path's own delay; what comes
  /// [`MAX_LATENESS`] after it is played as a gap.
  fn voice_in_time_per_second(&self) -> BTreeMap<u64, Duration> {
    let mut by_sequence: BTreeMap<u64, (Duration, u64)> = BTreeMap::new();
    let mut maybe_highest: Option<u64> = None;
    for (at, sequence_number, samples) in &self.voice {
      let extended = match maybe_highest {
        Some(highest) => unwrap_sequence_number(*sequence_number, highest),
        None => u64::from(*sequence_number) + (1 << 16),
      };
      maybe_highest = Some(maybe_highest.map_or(extended, |highest| highest.max(extended)));
      by_sequence.entry(extended).or_insert((*at, *samples));
    }
    // Spoken at, in samples since the first. A missing frame is taken as long as the shorter of
    // its neighbours: the voice steps to longer frames after a loss, not before it.
    let mut spoken = Vec::new();
    let mut maybe_previous: Option<(u64, u64, u64)> = None;
    for (sequence, (at, samples)) in &by_sequence {
      let position = match maybe_previous {
        Some((previous_sequence, previous_position, previous_samples)) => {
          let missing = sequence - previous_sequence - 1;
          previous_position + previous_samples + missing * previous_samples.min(*samples)
        },
        None => 0,
      };
      maybe_previous = Some((*sequence, position, *samples));
      spoken.push((
        Duration::from_micros(position * 1_000_000 / SAMPLES_PER_SECOND),
        *at,
        *samples,
      ));
    }
    let Some(offset) = spoken
      .iter()
      .map(|(spoken_at, at, _)| at.saturating_sub(*spoken_at))
      .min()
    else {
      return BTreeMap::new();
    };
    let mut in_time: BTreeMap<u64, Duration> = BTreeMap::new();
    for (spoken_at, at, samples) in spoken {
      let lateness = at.saturating_sub(spoken_at + offset);
      if lateness <= MAX_LATENESS {
        let second = (spoken_at + offset).as_secs();
        *in_time.entry(second).or_default() +=
          Duration::from_micros(samples * 1_000_000 / SAMPLES_PER_SECOND);
      }
    }
    in_time
  }
}

fn camera(start: Instant) -> VideoSendConfig {
  VideoSendConfig {
    maybe_layer_plan: None,
    kind: VideoTrackKind::Camera,
    capture: Box::new(BusyCamera::new(start)),
    layers: simulcast_layers(HD, FRAMERATE, CAMERA_BITRATE_BPS, 3),
    encoders: software_encoders(),
    maybe_filter: None,
    maybe_picture_tap: None,
    maybe_idle_refresh: None,
    maybe_screen_profile: None,
  }
}

/// `sequence_number` extended to 64 bits, nearest to `highest`.
fn unwrap_sequence_number(sequence_number: u16, highest: u64) -> u64 {
  let candidate = (highest & !0xFFFF) | u64::from(sequence_number);
  if candidate + (1 << 15) < highest {
    candidate + (1 << 16)
  } else if candidate > highest + (1 << 15) && candidate >= (1 << 16) {
    candidate - (1 << 16)
  } else {
    candidate
  }
}

fn device(raw: i64) -> DeviceId {
  raw.to_string().parse().unwrap()
}
