//! Captures the computer's sound like a screen share with audio does, and checks that this
//! process's own playback is left out (Linux, PipeWire).
//!
//! The example plays a 300 Hz tone itself (a PipeWire playback stream in this process, like the
//! call's speakers) while capturing; another program plays a 1 kHz tone. `portal-test/run.sh`
//! does this in a headless container with a null sink, so nothing is audible.
//!
//! It prints the level of both tones in the capture and exits non-zero when its own tone is there
//! or nothing was captured.

use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, bail};
use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use teamcraft_media::system_audio::{SystemAudioCapture, system_audio_queue};
use teamcraft_media::system_audio_pipewire::PipeWireSystemAudio;
use tracing::info;

const OWN_TONE_HZ: f32 = 300.0;
const OTHER_TONE_HZ: f32 = 1000.0;
const DURATION: Duration = Duration::from_secs(3);
/// Our tone counts as leaked above this share of the other tone's level.
const MAX_LEAK_RATIO: f32 = 0.05;

fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt()
    .with_target(false)
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
    .init();
  let runtime = tokio::runtime::Builder::new_current_thread()
    .enable_all()
    .build()?;

  let own_tone = OwnTone::play()?;
  let mut capture = PipeWireSystemAudio::new();
  let (sender, mut samples) = system_audio_queue();
  capture.start(sender).context("PipeWire system audio")?;

  let captured = runtime.block_on(async {
    let mut captured = Vec::new();
    let deadline = tokio::time::Instant::now() + DURATION;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, samples.recv()).await {
      captured.extend(chunk);
    }
    captured
  });
  capture.stop();
  own_tone.stop();

  let own = level(&captured, OWN_TONE_HZ);
  let other = level(&captured, OTHER_TONE_HZ);
  info!(
    samples = captured.len(),
    own_tone = format!("{own:.4}"),
    other_tone = format!("{other:.4}"),
    "captured"
  );
  if captured.is_empty() {
    bail!("nothing was captured");
  }
  if own > MAX_LEAK_RATIO * other.max(0.01) {
    bail!("our own playback is in the capture");
  }
  Ok(())
}

/// Amplitude of one frequency in `samples` (Goertzel), 0..1 for a full-scale sine.
fn level(samples: &[f32], frequency_hz: f32) -> f32 {
  if samples.is_empty() {
    return 0.0;
  }
  let omega = 2.0 * std::f32::consts::PI * frequency_hz / 48_000.0;
  let coefficient = 2.0 * omega.cos();
  let (mut previous, mut before) = (0.0f32, 0.0f32);
  for &sample in samples {
    let current = sample + coefficient * previous - before;
    before = previous;
    previous = current;
  }
  let power = previous * previous + before * before - coefficient * previous * before;
  2.0 * power.max(0.0).sqrt() / samples.len() as f32
}

/// A tone this process plays through PipeWire, until stopped.
struct OwnTone {
  stop: pw::channel::Sender<()>,
  thread: JoinHandle<()>,
}

impl OwnTone {
  fn play() -> anyhow::Result<Self> {
    let (stop, stop_rx) = pw::channel::channel::<()>();
    let thread = std::thread::spawn(move || {
      if let Err(error) = run_tone(stop_rx) {
        tracing::warn!(%error, "own tone failed");
      }
    });
    Ok(Self { stop, thread })
  }

  fn stop(self) {
    let _ = self.stop.send(());
    let _ = self.thread.join();
  }
}

fn run_tone(stop: pw::channel::Receiver<()>) -> Result<(), pw::Error> {
  pw::init();
  let main_loop = pw::main_loop::MainLoopRc::new(None)?;
  let context = pw::context::ContextRc::new(&main_loop, None)?;
  let core = context.connect_rc(None)?;
  let stream = pw::stream::StreamBox::new(
    &core,
    "own-tone",
    properties! {
      *pw::keys::MEDIA_TYPE => "Audio",
      *pw::keys::MEDIA_CATEGORY => "Playback",
      *pw::keys::MEDIA_ROLE => "Communication",
    },
  )?;
  let _listener = stream
    .add_local_listener_with_user_data(0u64)
    .process(|stream, position| {
      let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
      };
      let Some(data) = buffer.datas_mut().first_mut() else {
        return;
      };
      let mut frames = 0;
      if let Some(bytes) = data.data() {
        for sample in bytes.chunks_exact_mut(4) {
          let t = *position as f32 / 48_000.0;
          let value = 0.3 * (2.0 * std::f32::consts::PI * OWN_TONE_HZ * t).sin();
          sample.copy_from_slice(&value.to_le_bytes());
          *position += 1;
          frames += 1;
        }
      }
      let chunk = data.chunk_mut();
      *chunk.offset_mut() = 0;
      *chunk.stride_mut() = 4;
      *chunk.size_mut() = frames * 4;
    })
    .register()?;
  let mut info = spa::param::audio::AudioInfoRaw::new();
  info.set_format(spa::param::audio::AudioFormat::F32LE);
  info.set_rate(48_000);
  info.set_channels(1);
  let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
  position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
  info.set_position(position);
  let values = spa::pod::serialize::PodSerializer::serialize(
    std::io::Cursor::new(Vec::new()),
    &spa::pod::Value::Object(spa::pod::Object {
      type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
      id: spa::param::ParamType::EnumFormat.as_raw(),
      properties: info.into(),
    }),
  )
  .map_err(|_| pw::Error::CreationFailed)?
  .0
  .into_inner();
  let mut params = [spa::pod::Pod::from_bytes(&values).ok_or(pw::Error::CreationFailed)?];
  stream.connect(
    spa::utils::Direction::Output,
    None,
    pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
    &mut params,
  )?;
  let loop_for_stop = main_loop.clone();
  let _stop = stop.attach(main_loop.loop_(), move |()| loop_for_stop.quit());
  main_loop.run();
  Ok(())
}
