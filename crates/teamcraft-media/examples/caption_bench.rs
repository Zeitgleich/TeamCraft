//! What live captions cost on the desktop (issue #215): speech played through the caption worker
//! at the call's pace (a 20 ms frame every 20 ms), as the call's playback hands it over, with
//! whisper.cpp and the downloaded model.
//!
//! ```sh
//! ffmpeg -i speech.wav -f f32le -ar 48000 -ac 1 speech.f32
//! cargo run -p teamcraft-media --features captions-whisper --example caption_bench -- \
//!   --model ggml-base-q5_1.bin --audio speech.f32 [--language de] [--threads 4] [--speakers 1] \
//!   [--show-text]
//! ```
//!
//! `--audio` is raw 32-bit float mono samples at 48 kHz. `--speakers 2` plays the same speech as two
//! people talking at once (the worst case for the queue). whisper.cpp is built optimized also in a
//! debug build (whisper-rs-sys), so this needs no release build.
//!
//! The report gives the process's CPU time over all threads as a share of one core, averaged over
//! the speech's length, the worker's real-time factor per final sentence (how long it took over how
//! long the sentence was), how long after a sentence's end its text came, and whether captions
//! stopped as overloaded. `--show-text` also logs the recognized text, to judge it by ear: only for
//! speech that may be logged (never a real call's).

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use teamcraft_client_api::calls::CaptionLanguage;
use teamcraft_media::captions::{CaptionSettings, CaptionUpdate, CaptionWorker};
use teamcraft_media::captions_whisper::WhisperRecognizer;
use teamcraft_proto::{DeviceId, Id, Snowflake};
use tracing::info;

const FRAME_SAMPLES: usize = 960;
const FRAME_INTERVAL: Duration = Duration::from_millis(20);

struct Args {
  model: PathBuf,
  audio: PathBuf,
  language: CaptionLanguage,
  threads: u16,
  speakers: usize,
  shows_text: bool,
}

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt().with_target(false).init();
  let args = parse_args()?;
  let bytes = std::fs::read(&args.audio)?;
  let samples: Vec<f32> = bytes
    .as_chunks::<4>()
    .0
    .iter()
    .map(|chunk| f32::from_le_bytes(*chunk))
    .collect();
  let audio_length = Duration::from_secs_f64(samples.len() as f64 / 48_000.0);
  let load_started = Instant::now();
  let recognizer = WhisperRecognizer::load(&args.model, args.threads)?;
  info!(took = ?load_started.elapsed(), "model loaded");
  let settings = CaptionSettings {
    language: args.language,
    ..CaptionSettings::default()
  };
  let (sender, updates) = mpsc::channel();
  let (worker, tap) = CaptionWorker::spawn(Box::new(recognizer), settings, move |update| {
    // The bench may be over.
    let _ = sender.send((Instant::now(), update));
  })?;
  let speakers = (1..=args.speakers as i64)
    .map(|raw| Snowflake::new(raw).map(DeviceId::from_snowflake))
    .collect::<Result<Vec<_>, _>>()?;
  let cpu_start = cpu_time();
  let started = Instant::now();
  for (index, frame) in samples.chunks(FRAME_SAMPLES).enumerate() {
    for speaker in &speakers {
      tap.push(*speaker, frame);
    }
    // The call's pace: frame n is played at n × 20 ms.
    let due = started + FRAME_INTERVAL * (index as u32 + 1);
    if let Some(wait) = due.checked_duration_since(Instant::now()) {
      std::thread::sleep(wait);
    }
  }
  let played = started.elapsed();
  for speaker in &speakers {
    tap.end(*speaker);
  }
  drop(tap);
  let mut finals = 0;
  let mut partials = 0;
  let mut is_overloaded = false;
  let mut last_final_at = started;
  while let Ok((at, update)) = updates.recv_timeout(Duration::from_secs(30)) {
    match update {
      CaptionUpdate::Text { text, is_final, .. } => {
        if is_final {
          finals += 1;
          last_final_at = at;
          if args.shows_text {
            info!(at_secs = at.duration_since(started).as_secs_f64(), %text, "final");
          }
        } else {
          partials += 1;
        }
      },
      CaptionUpdate::Overloaded => is_overloaded = true,
      CaptionUpdate::Failed => anyhow::bail!("the recognizer failed"),
    }
  }
  let cpu = cpu_time().saturating_sub(cpu_start);
  drop(worker);
  info!(
    speech_secs = audio_length.as_secs_f64(),
    speakers = args.speakers,
    threads = args.threads,
    finals,
    partials,
    cpu_secs = cpu.as_secs_f64(),
    played_secs = played.as_secs_f64(),
    cpu_percent_of_one_core = 100.0 * cpu.as_secs_f64() / played.as_secs_f64(),
    last_text_after_end_secs = last_final_at
      .saturating_duration_since(started + played)
      .as_secs_f64(),
    is_overloaded,
    "captions bench"
  );
  Ok(())
}

fn parse_args() -> anyhow::Result<Args> {
  let mut args = Args {
    model: PathBuf::new(),
    audio: PathBuf::new(),
    language: CaptionLanguage::Auto,
    threads: 4,
    speakers: 1,
    shows_text: false,
  };
  let mut words = std::env::args().skip(1);
  while let Some(flag) = words.next() {
    if flag == "--show-text" {
      args.shows_text = true;
      continue;
    }
    let value = words
      .next()
      .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))?;
    match flag.as_str() {
      "--model" => args.model = value.into(),
      "--audio" => args.audio = value.into(),
      "--language" => args.language = CaptionLanguage::from_str_or_auto(&value),
      "--threads" => args.threads = value.parse()?,
      "--speakers" => args.speakers = value.parse()?,
      other => anyhow::bail!("unknown flag {other}"),
    }
  }
  anyhow::ensure!(!args.model.as_os_str().is_empty(), "--model is required");
  anyhow::ensure!(!args.audio.as_os_str().is_empty(), "--audio is required");
  Ok(args)
}

/// User plus system CPU time of the whole process so far.
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

/// Not measured outside Unix: the CPU line says 0.
#[cfg(not(unix))]
fn cpu_time() -> Duration {
  Duration::ZERO
}
