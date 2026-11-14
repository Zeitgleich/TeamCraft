//! A participant on a thin uplink with its camera on (#1893): a phone on LTE that carries 64 kbit/s,
//! talking in turns. Its voice has to get through; the camera has to make room for it.
//!
//! `measure_voice_and_camera_on_a_thin_uplink` (ignored) prints per second what the presenter's
//! engine decided (uplink estimate, video held for the voice, voice step, camera layers) and what
//! its link carried (small datagrams are voice, large ones video), and how much of its voice the
//! viewer played: `cargo test -p teamcraft-sfu --test suite measure_voice_and_camera -- --ignored
//! --nocapture`.
// clippy.toml only relaxes these lints inside #[test] functions; the helpers below are test code too.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fmt::Write as _;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use teamcraft_media::call_engine::CallEngineEvent;
use teamcraft_media::video::simulcast_layers;
use teamcraft_media::video_pipeline::{VideoSendConfig, software_encoders};
use teamcraft_proto::VideoTrackKind;

use crate::bandwidth_limits::{join, start_sfu, wait_until_secured};
use crate::common::busy_camera::{BusyCamera, FRAMERATE};
use crate::common::relay::Relay;

/// The uplink: a steady 64 kbit/s, from the join on.
const UPLINK_BPS: u64 = 64_000;
/// A phone's camera: 640×360 in three layers, about what a browser on a phone publishes.
const CAMERA_BITRATE_BPS: u32 = 500_000;
/// The presenter talks this long, then is muted this long, and again.
const TURN: Duration = Duration::from_secs(8);
const TURNS: u32 = 4;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "measurement: prints a timeline, takes about a minute"]
async fn measure_voice_and_camera_on_a_thin_uplink() {
  let (port, certificate_hash) = start_sfu().await;
  let sfu = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
  let start = Instant::now();
  let relay = Relay::start(sfu, start);
  relay.to_sfu.set_limit(Some(UPLINK_BPS));
  let mut presenter = join(relay.address, certificate_hash, 1, &[2]).await;
  let mut viewer = join(sfu, certificate_hash, 2, &[1]).await;
  wait_until_secured(&mut presenter).await;
  wait_until_secured(&mut viewer).await;
  presenter.set_muted(true);
  let camera = BusyCamera::new(start);
  let size = teamcraft_media::video::VideoResolution {
    width: 640,
    height: 360,
  };
  presenter
    .start_video(VideoSendConfig {
      maybe_layer_plan: None,
      kind: VideoTrackKind::Camera,
      capture: Box::new(camera),
      layers: simulcast_layers(size, FRAMERATE, CAMERA_BITRATE_BPS, 3),
      encoders: software_encoders(),
      maybe_filter: None,
      maybe_picture_tap: None,
      maybe_idle_refresh: None,
      maybe_screen_profile: None,
    })
    .await
    .unwrap();
  let heard: Arc<Mutex<Vec<(Duration, String)>>> = Arc::default();
  let mut talks = Vec::new();
  let video_on_at = start.elapsed();
  for turn in 0..2 * TURNS {
    let is_talking = turn % 2 == 1;
    presenter.set_muted(!is_talking);
    if is_talking {
      talks.push(start.elapsed());
    }
    let until = tokio::time::Instant::now() + TURN;
    while tokio::time::Instant::now() < until {
      if let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(20), presenter.next_event()).await {
        let maybe_line = match event {
          CallEngineEvent::UplinkEstimate { bps, loss_percent } => {
            Some(format!("est {}k loss {loss_percent}", bps / 1000))
          },
          CallEngineEvent::VideoHeldForVoice(is_held) => Some(format!("HELD {is_held}")),
          CallEngineEvent::VoiceEncoding(encoding) => Some(format!(
            "VOICE {}k {:?}",
            encoding.bitrate_bps / 1000,
            encoding.frame
          )),
          CallEngineEvent::LayersNeeded { layers, .. } => Some(format!("layers {layers:?}")),
          _ => None,
        };
        if let Some(line) = maybe_line {
          heard.lock().unwrap().push((start.elapsed(), line));
        }
      }
    }
  }
  let end = start.elapsed();
  presenter.stop_video(VideoTrackKind::Camera);
  presenter.leave().await.unwrap();
  let stats = viewer.leave().await.unwrap();
  let talk_frames = u64::from(TURNS) * TURN.as_secs() * 50;
  let mut out = String::new();
  writeln!(
    out,
    "viewer played {} audible frames of {talk_frames} the presenter talked ({} %)",
    stats.audible_frames_played,
    stats.audible_frames_played * 100 / talk_frames
  )
  .unwrap();
  let link = relay.to_sfu.log.lock().unwrap().clone();
  let heard = heard.lock().unwrap().clone();
  for second in video_on_at.as_secs()..=end.as_secs() {
    let in_second = |at: &Duration| at.as_secs() == second;
    let (mut small, mut small_lost, mut large, mut large_lost) = (0usize, 0usize, 0usize, 0usize);
    for (_, bytes, is_dropped) in link.iter().filter(|(at, ..)| in_second(at)) {
      match (*bytes < 250, *is_dropped) {
        (true, false) => small += bytes,
        (true, true) => small_lost += bytes,
        (false, false) => large += bytes,
        (false, true) => large_lost += bytes,
      }
    }
    let is_talking = talks
      .iter()
      .any(|talk| second >= talk.as_secs() && second < (*talk + TURN).as_secs());
    let events: Vec<&str> = heard
      .iter()
      .filter(|(at, line)| in_second(at) && !line.starts_with("est"))
      .map(|(_, line)| line.as_str())
      .collect();
    let estimate = heard
      .iter()
      .rev()
      .find(|(at, line)| in_second(at) && line.starts_with("est"))
      .map_or("", |(_, line)| line.as_str());
    writeln!(
      out,
      "{second:3} {} small {:3}k (lost {:3}k) large {:3}k (lost {:3}k) | {estimate} | {}",
      if is_talking { "TALK" } else { "    " },
      small * 8 / 1000,
      small_lost * 8 / 1000,
      large * 8 / 1000,
      large_lost * 8 / 1000,
      events.join(", ")
    )
    .unwrap();
  }
  println!("{out}");
}
