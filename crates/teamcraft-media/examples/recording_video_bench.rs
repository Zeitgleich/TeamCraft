//! What a call recording with video costs on the desktop (#2250; docs/performance.md, "Call
//! recording"): the process's CPU time while recording a call of `TEAMCRAFT_BENCH_CAMERAS` cameras
//! (default 4, 640x360 at 30 fps, what a gallery tile receives) and one 1280x720 own camera at the
//! recording's rate, against the same recording without video. The cameras are pre-encoded test
//! patterns (two seconds, looped from a keyframe), so the senders' encoding isn't counted: what is
//! measured is the recorder's decoding, compositing, encoding and sealing, and the sound's.
//!
//! ```sh
//! cargo run --release -p teamcraft-media --example recording_video_bench --features openh264-source
//! ```
//!
//! `TEAMCRAFT_BENCH_SECONDS` sets the length of each run (default 30). The recorded files go to a
//! temporary folder and are deleted. "% core" is the CPU time against the run's wall time.

use std::time::{Duration, Instant};

use teamcraft_media::call_engine::RecordingAdmission;
use teamcraft_media::call_recording::CallRecorder;
use teamcraft_media::call_recording_video::{OwnSource, VideoRecordingConfig};
use teamcraft_media::call_video::ReceivedVideoFrame;
use teamcraft_media::datagram::SimulcastLayer;
use teamcraft_media::rtp::RtpTimestamp;
use teamcraft_media::video::VideoResolution;
use teamcraft_media::video_capture::test_pattern;
use teamcraft_media::video_codec::{OpenH264Encoder, VideoContent, VideoEncoder};
use teamcraft_proto::{DeviceId, Id, MediaStreamId, Snowflake, VideoCodec, VideoLayerInfo, VideoTrackKind};
use tracing::info;

const TILE: VideoResolution = VideoResolution {
  width: 640,
  height: 360,
};
const OWN: VideoResolution = VideoResolution {
  width: 1280,
  height: 720,
};
const FRAMERATE: u64 = 30;
const LOOP_FRAMES: u64 = 2 * FRAMERATE;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let cameras: u32 = env_number("TEAMCRAFT_BENCH_CAMERAS", 4);
  let seconds: u64 = env_number("TEAMCRAFT_BENCH_SECONDS", 30);
  let streams = (0..cameras)
    .map(encoded_loop)
    .collect::<anyhow::Result<Vec<_>>>()?;
  let folder = std::env::temp_dir().join(format!("teamcraft-recording-bench-{}", std::process::id()));
  std::fs::create_dir_all(&folder)?;
  for has_video in [false, true] {
    let (cpu, wall, size) = run(&folder, &streams, has_video, Duration::from_secs(seconds))?;
    let share = cpu.as_secs_f64() / wall.as_secs_f64() * 100.0;
    info!(
      "{} with {cameras} cameras: {:.1} % core ({:.1} s CPU in {:.1} s), {:.1} MB per hour",
      if has_video { "video" } else { "sound only" },
      share,
      cpu.as_secs_f64(),
      wall.as_secs_f64(),
      size as f64 / wall.as_secs_f64() * 3600.0 / 1_000_000.0,
    );
  }
  std::fs::remove_dir_all(&folder)?;
  Ok(())
}

/// One run: `length` of a call at real-time pace. Returns CPU time, wall time and the file's size.
fn run(
  folder: &std::path::Path,
  streams: &[Vec<(bool, Vec<u8>)>],
  has_video: bool,
  length: Duration,
) -> anyhow::Result<(Duration, Duration, u64)> {
  let own_device = device(1);
  let path = folder.join(if has_video { "bench.mp4" } else { "bench.ogg" });
  let maybe_video = has_video.then(|| (VideoRecordingConfig::default(), own_device));
  let recorder = CallRecorder::start_with_video(&path, Duration::from_secs(3600), None, maybe_video)?;
  let sink = recorder.sink();
  let maybe_video_sink = recorder.video_sink();
  let silence = vec![0.0f32; 960];
  let own_picture = test_pattern(OWN, 0, 0)?;
  let started = Instant::now();
  let cpu_before = cpu_time();
  let mut audio_frames = 0u64;
  let mut video_frames = 0u64;
  while started.elapsed() < length {
    let now = started.elapsed();
    while Duration::from_millis(audio_frames * 20) <= now {
      sink.set_admission(&RecordingAdmission::Everybody);
      sink.push_playback(&silence);
      audio_frames += 1;
    }
    while Duration::from_micros(video_frames * 1_000_000 / FRAMERATE) <= now {
      if let Some(video) = &maybe_video_sink {
        let (index, timestamp) = (video_frames % LOOP_FRAMES, video_frames * 3000);
        for (stream, frames) in streams.iter().enumerate() {
          let (is_keyframe, data) = &frames[index as usize];
          let frame = ReceivedVideoFrame {
            sender: device(stream as i64 + 2),
            stream_id: MediaStreamId(stream as u32 + 1),
            codec: VideoCodec::H264,
            layer: SimulcastLayer::BASE,
            timestamp: RtpTimestamp(timestamp as u32),
            is_keyframe: *is_keyframe,
            data: data.clone(),
            arrived_at: Instant::now(),
          };
          video.push_received(&frame, &VideoTrackKind::Camera);
        }
        video.push_own(OwnSource::Camera, &own_picture, Duration::from_millis(66));
      }
      video_frames += 1;
    }
    std::thread::sleep(Duration::from_millis(5));
  }
  let take = recorder.finish()?;
  let (cpu, wall) = (cpu_time() - cpu_before, started.elapsed());
  let size = take.size_bytes;
  teamcraft_media::call_recording::delete_recording(&take.path);
  Ok((cpu, wall, size))
}

/// Two seconds of a camera, encoded once: the bench replays them.
fn encoded_loop(index: u32) -> anyhow::Result<Vec<(bool, Vec<u8>)>> {
  let layer = VideoLayerInfo {
    layer: 0,
    width: TILE.width,
    height: TILE.height,
    max_framerate: FRAMERATE as u8,
    max_bitrate_bps: 500_000,
    maybe_codec: None,
  };
  let mut encoder = OpenH264Encoder::new(&layer, VideoContent::Camera)?;
  let mut frames = Vec::new();
  for frame in 0..LOOP_FRAMES {
    let picture = test_pattern(TILE, frame + u64::from(index) * 7, frame * 33_333)?;
    let encoded = encoder.encode(&picture, frame == 0)?;
    frames.push(encoded.map_or((false, Vec::new()), |picture| (picture.is_keyframe, picture.data)));
  }
  Ok(frames)
}

fn device(raw: i64) -> DeviceId {
  DeviceId::from_snowflake(Snowflake::new(raw).unwrap_or_else(|_| unreachable!("a positive id")))
}

fn env_number<T: std::str::FromStr>(name: &str, default: T) -> T {
  std::env::var(name)
    .ok()
    .and_then(|value| value.parse().ok())
    .unwrap_or(default)
}

#[cfg(unix)]
fn cpu_time() -> Duration {
  use nix::sys::resource::{UsageWho, getrusage};
  getrusage(UsageWho::RUSAGE_SELF).map_or(Duration::ZERO, |usage| {
    let to_duration = |time: nix::sys::time::TimeVal| {
      Duration::new(time.tv_sec().max(0) as u64, (time.tv_usec().max(0) * 1000) as u32)
    };
    to_duration(usage.user_time()) + to_duration(usage.system_time())
  })
}

/// Not measured outside Unix: the CPU column stays 0.
#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}
