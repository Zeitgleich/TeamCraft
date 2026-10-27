//! The sound for new messages on the desktop (notifications.md, "Message sounds"; #800). The
//! webview decides when one plays (the user's choice, mutes, levels, the app's do-not-disturb, one
//! sound for a burst); the shell adds the desktop's own do-not-disturb where it can read it (Linux,
//! as for the ringtone) and plays the sound on the default output: one of `teamcraft-media`'s
//! synthesized sounds, or the user's own file.
//!
//! The own file is picked in the system's dialog, checked by decoding it once and kept in the app's
//! data directory (`sounds/`, the file as picked and its name). It never leaves this device. Every
//! sound is synthesized or decoded once per run and kept in memory; between messages nothing runs:
//! a sound plays on a thread of its own that ends with it. A missing or broken own file never keeps
//! a sound from playing: the default sound plays instead and the settings say why.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use teamcraft_client_api::message_sound::{
  BuiltInSound, MessageSound, OwnSoundInfo, OwnSoundLimits, OwnSoundPick, OwnSoundRefusal, OwnSoundStatus,
  SoundPlayed,
};
use teamcraft_media::audio_io::AudioDeviceChoice;
use teamcraft_media::{message_sounds, ringtone, sound_file};

/// The directory in the app's data directory.
const SOUNDS_DIR: &str = "sounds";
/// The own file as picked.
const OWN_FILE: &str = "own-message-sound";
/// Its name and type, next to it.
const OWN_META_FILE: &str = "own-message-sound.json";
/// What the dialog offers (and what `teamcraft_media::sound_file` reads).
const SOUND_EXTENSIONS: &[&str] = &["mp3", "ogg", "oga", "wav", "flac"];

/// The sounds decoded or synthesized so far, shared by the commands.
#[derive(Default)]
pub struct MessageSoundState {
  cache: Mutex<SoundCache>,
}

#[derive(Default)]
struct SoundCache {
  built_in: HashMap<BuiltInSound, Arc<Vec<f32>>>,
  /// The own file as last decoded or found broken; `None` until first needed, and again after a
  /// new file was picked or the file removed.
  maybe_own: Option<OwnLoad>,
}

/// The own file decoded for one length limit, or why it can't play.
#[derive(Clone)]
struct OwnLoad {
  limits: OwnSoundLimits,
  result: Result<OwnSound, OwnSoundStatus>,
}

#[derive(Clone)]
struct OwnSound {
  samples: Arc<Vec<f32>>,
  info: OwnSoundInfo,
}

/// What is kept next to the file.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OwnMeta {
  file_name: String,
  /// Helps to recognize the format when it is decoded again.
  #[serde(rename = "extension", default, skip_serializing_if = "Option::is_none")]
  maybe_extension: Option<String>,
}

/// Plays a message sound (`is_preview`: the settings' preview, which plays whatever the desktop's
/// do-not-disturb says). Returns at once; the sound plays on its own thread.
#[tauri::command(rename_all = "snake_case")]
pub async fn message_sound_play(
  app: AppHandle,
  probe: State<'_, crate::attention::OsProbe>,
  sound: MessageSound,
  limits: OwnSoundLimits,
  is_preview: bool,
) -> Result<SoundPlayed, String> {
  if !sound.is_played_by_app() {
    return Ok(SoundPlayed::Quiet);
  }
  if !is_preview && crate::ringtone::is_desktop_quiet(&probe).await {
    tracing::debug!("the desktop's do-not-disturb is on: no message sound");
    return Ok(SoundPlayed::Quiet);
  }
  let dir = sounds_dir(&app)?;
  let handle = app.clone();
  let (samples, played) = tauri::async_runtime::spawn_blocking(move || {
    handle
      .state::<MessageSoundState>()
      .samples_for(sound, &dir, limits)
  })
  .await
  .map_err(|error| error.to_string())?;
  play(samples);
  Ok(played)
}

/// "Own file…": the system's dialog, then the checks (size, decoding); a file that passes is kept
/// and plays from now on. `filter_label` names the dialog's filter in the UI's language.
#[tauri::command(rename_all = "snake_case")]
pub async fn message_sound_pick(
  window: WebviewWindow,
  limits: OwnSoundLimits,
  filter_label: String,
) -> Result<OwnSoundPick, String> {
  let Some(picked) = ask_sound_path(&window, &filter_label).await else {
    return Ok(OwnSoundPick::Cancelled);
  };
  let app = window.app_handle().clone();
  let dir = sounds_dir(&app)?;
  tauri::async_runtime::spawn_blocking(move || app.state::<MessageSoundState>().keep(&dir, &picked, limits))
    .await
    .map_err(|error| error.to_string())?
}

/// Whether this device has an own file and whether it plays, for the settings. Reads it again, so a
/// file that went missing since shows as such.
#[tauri::command(rename_all = "snake_case")]
pub async fn message_sound_status(app: AppHandle, limits: OwnSoundLimits) -> Result<OwnSoundStatus, String> {
  let dir = sounds_dir(&app)?;
  let handle = app.clone();
  tauri::async_runtime::spawn_blocking(move || {
    let state = handle.state::<MessageSoundState>();
    state.forget_own();
    state.own_status(&dir, limits)
  })
  .await
  .map_err(|error| error.to_string())
}

/// Removes the own file from this device.
#[tauri::command(rename_all = "snake_case")]
pub async fn message_sound_remove(
  app: AppHandle,
  sounds: State<'_, MessageSoundState>,
) -> Result<(), String> {
  let dir = sounds_dir(&app)?;
  sounds.forget_own();
  for name in [OWN_FILE, OWN_META_FILE] {
    match std::fs::remove_file(dir.join(name)) {
      Ok(()) => {},
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
      Err(error) => return Err(error.to_string()),
    }
  }
  Ok(())
}

impl MessageSoundState {
  /// The samples to play for `sound`, and whether the own file had to fall back.
  fn samples_for(
    &self,
    sound: MessageSound,
    dir: &Path,
    limits: OwnSoundLimits,
  ) -> (Arc<Vec<f32>>, SoundPlayed) {
    match sound {
      // Until a file is chosen the default sound plays; a file that can't play is a fallback the
      // settings mention.
      MessageSound::OwnFile => match self.own(dir, limits).result {
        Ok(own) => (own.samples, SoundPlayed::Played),
        Err(OwnSoundStatus::NotPicked) => (self.built_in(BuiltInSound::DEFAULT), SoundPlayed::Played),
        Err(_) => (self.built_in(BuiltInSound::DEFAULT), SoundPlayed::FellBack),
      },
      MessageSound::BuiltIn(built_in) => (self.built_in(built_in), SoundPlayed::Played),
      MessageSound::System | MessageSound::Silent => (Arc::new(Vec::new()), SoundPlayed::Quiet),
    }
  }

  fn built_in(&self, sound: BuiltInSound) -> Arc<Vec<f32>> {
    if let Some(samples) = self.cache().built_in.get(&sound) {
      return samples.clone();
    }
    // Synthesized outside the lock: a few milliseconds, once per sound and run.
    let samples = Arc::new(message_sounds::synthesize(sound));
    self.cache().built_in.insert(sound, samples.clone());
    samples
  }

  /// The own file for `limits`: decoded once, then from memory (also when it can't play, so a
  /// broken file isn't read again for every message).
  fn own(&self, dir: &Path, limits: OwnSoundLimits) -> OwnLoad {
    if let Some(load) = self
      .cache()
      .maybe_own
      .clone()
      .filter(|load| load.limits == limits)
    {
      return load;
    }
    let load = OwnLoad {
      limits,
      result: load_own(dir, limits),
    };
    self.cache().maybe_own = Some(load.clone());
    load
  }

  fn own_status(&self, dir: &Path, limits: OwnSoundLimits) -> OwnSoundStatus {
    match self.own(dir, limits).result {
      Ok(own) => OwnSoundStatus::Ready(own.info),
      Err(status) => status,
    }
  }

  fn forget_own(&self) {
    self.cache().maybe_own = None;
  }

  /// Checks a picked file and keeps it in `dir`; a refused file leaves the kept one as it was.
  fn keep(&self, dir: &Path, picked: &Path, limits: OwnSoundLimits) -> Result<OwnSoundPick, String> {
    let bytes = match read_at_most(picked, limits.max_bytes()) {
      Ok(Some(bytes)) => bytes,
      Ok(None) => return Ok(refused(OwnSoundRefusal::TooLarge)),
      Err(error) => {
        tracing::info!(%error, "a picked sound file could not be read");
        return Ok(refused(OwnSoundRefusal::Unreadable));
      },
    };
    let meta = OwnMeta {
      file_name: picked
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default(),
      maybe_extension: extension_of(picked),
    };
    let samples = match sound_file::decode(bytes.clone(), meta.maybe_extension.as_deref(), limits.max_play())
    {
      Ok(samples) => samples,
      Err(error) => {
        tracing::info!(%error, "a picked sound file was refused");
        return Ok(refused(OwnSoundRefusal::NotAudio));
      },
    };
    store(dir, &bytes, &meta).map_err(|error| format!("could not keep the sound file: {error}"))?;
    let own = OwnSound {
      info: OwnSoundInfo {
        file_name: meta.file_name,
        play_ms: message_sounds::play_ms(&samples),
      },
      samples: Arc::new(samples),
    };
    self.cache().maybe_own = Some(OwnLoad {
      limits,
      result: Ok(own.clone()),
    });
    Ok(OwnSoundPick::Stored(own.info))
  }

  fn cache(&self) -> std::sync::MutexGuard<'_, SoundCache> {
    self.cache.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Reads and decodes the kept file. `NotPicked` when there is none, `Unavailable` when it is gone
/// or broken.
fn load_own(dir: &Path, limits: OwnSoundLimits) -> Result<OwnSound, OwnSoundStatus> {
  let maybe_meta = std::fs::read_to_string(dir.join(OWN_META_FILE))
    .ok()
    .and_then(|text| serde_json::from_str::<OwnMeta>(&text).ok());
  let maybe_bytes = std::fs::read(dir.join(OWN_FILE)).ok();
  let (meta, bytes) = match (maybe_meta, maybe_bytes) {
    (None, None) => return Err(OwnSoundStatus::NotPicked),
    (Some(meta), Some(bytes)) => (meta, bytes),
    _ => {
      tracing::info!("the own message sound is missing a part");
      return Err(OwnSoundStatus::Unavailable);
    },
  };
  match sound_file::decode(bytes, meta.maybe_extension.as_deref(), limits.max_play()) {
    Ok(samples) => Ok(OwnSound {
      info: OwnSoundInfo {
        file_name: meta.file_name,
        play_ms: message_sounds::play_ms(&samples),
      },
      samples: Arc::new(samples),
    }),
    Err(error) => {
      tracing::info!(%error, "the own message sound can't be played");
      Err(OwnSoundStatus::Unavailable)
    },
  }
}

/// Writes the file, then its name, each through a temporary file: a crash leaves the old one or the
/// new one, never half of either.
fn store(dir: &Path, bytes: &[u8], meta: &OwnMeta) -> std::io::Result<()> {
  std::fs::create_dir_all(dir)?;
  let meta_json = serde_json::to_vec(meta).map_err(std::io::Error::other)?;
  write_replacing(&dir.join(OWN_FILE), bytes)?;
  write_replacing(&dir.join(OWN_META_FILE), &meta_json)
}

fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  let temporary = path.with_extension("partial");
  std::fs::write(&temporary, bytes)?;
  std::fs::rename(&temporary, path)
}

/// The whole file, or `None` when it is larger than `max_bytes` (checked while reading too: the
/// file may grow after the size was looked at).
fn read_at_most(path: &Path, max_bytes: u64) -> std::io::Result<Option<Vec<u8>>> {
  if std::fs::metadata(path)?.len() > max_bytes {
    return Ok(None);
  }
  let mut bytes = Vec::new();
  std::fs::File::open(path)?
    .take(max_bytes.saturating_add(1))
    .read_to_end(&mut bytes)?;
  Ok((bytes.len() as u64 <= max_bytes).then_some(bytes))
}

fn extension_of(path: &Path) -> Option<String> {
  path
    .extension()
    .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
    .filter(|extension| SOUND_EXTENSIONS.contains(&extension.as_str()))
}

fn refused(reason: OwnSoundRefusal) -> OwnSoundPick {
  OwnSoundPick::Refused { reason }
}

fn sounds_dir(app: &AppHandle) -> Result<PathBuf, String> {
  Ok(crate::core::commands::base_dir(app)?.join(SOUNDS_DIR))
}

/// Plays on a thread of its own (the audio stream may not move between threads) that ends with the
/// sound. A missing output device is logged; the notification has shown anyway.
fn play(samples: Arc<Vec<f32>>) {
  if samples.is_empty() {
    return;
  }
  let spawned = std::thread::Builder::new()
    .name("message-sound".to_owned())
    .spawn(move || {
      if let Err(error) = ringtone::play_once(&AudioDeviceChoice::Default, &samples) {
        tracing::info!(%error, "no speakers for the message sound");
      }
    });
  if let Err(error) = spawned {
    tracing::warn!(%error, "could not start the message sound");
  }
}

/// The file the user picks in the system's dialog, sound files only; `None` when cancelled.
async fn ask_sound_path(window: &WebviewWindow, filter_label: &str) -> Option<PathBuf> {
  use tauri_plugin_dialog::DialogExt;

  let (sender, receiver) = tokio::sync::oneshot::channel();
  window
    .dialog()
    .file()
    .set_parent(window)
    .add_filter(filter_label, SOUND_EXTENSIONS)
    // A dropped sender (the dialog failed to open) reads as cancelled.
    .pick_file(move |maybe_path| drop(sender.send(maybe_path)));
  let picked = receiver.await.ok().flatten()?;
  match picked.into_path() {
    Ok(path) => Some(path),
    Err(error) => {
      tracing::warn!(%error, "the file dialog answered with no local path");
      None
    },
  }
}

#[cfg(test)]
mod tests {
  // Test-only module: a failing step should abort the test with a clear message.
  #![allow(clippy::expect_used, clippy::unwrap_used)]

  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;

  #[test]
  fn a_picked_file_is_kept_with_its_name_and_plays_from_memory() {
    let dir = temporary_dir();
    let picked = dir.join("Bell Sound.WAV");
    std::fs::write(&picked, wav(0.5)).unwrap();
    let state = MessageSoundState::default();
    let sounds = dir.join(SOUNDS_DIR);
    let pick = state.keep(&sounds, &picked, OwnSoundLimits::default()).unwrap();
    let OwnSoundPick::Stored(info) = pick else {
      panic!("refused: {pick:?}")
    };
    assert_eq!(info.file_name, "Bell Sound.WAV");
    assert_eq!(info.play_ms, 500);
    assert_eq!(std::fs::read(sounds.join(OWN_FILE)).unwrap(), wav(0.5));
    // Gone from disk, it still plays from memory until the settings look again.
    std::fs::remove_file(sounds.join(OWN_FILE)).unwrap();
    let (samples, played) = state.samples_for(MessageSound::OwnFile, &sounds, OwnSoundLimits::default());
    assert_eq!(played, SoundPlayed::Played);
    assert_eq!(samples.len(), 24_000);
  }

  #[test]
  fn a_missing_or_broken_file_falls_back_to_the_default_sound() {
    let dir = temporary_dir();
    let state = MessageSoundState::default();
    assert_eq!(
      state.own_status(&dir, OwnSoundLimits::default()),
      OwnSoundStatus::NotPicked
    );
    // None chosen yet: the default sound, and nothing for the settings to warn about.
    let (samples, played) = state.samples_for(MessageSound::OwnFile, &dir, OwnSoundLimits::default());
    assert_eq!(played, SoundPlayed::Played);
    assert_eq!(*samples, message_sounds::synthesize(BuiltInSound::DEFAULT));
    store(
      &dir,
      b"not a sound",
      &OwnMeta {
        file_name: "x.mp3".to_owned(),
        maybe_extension: Some("mp3".to_owned()),
      },
    )
    .unwrap();
    state.forget_own();
    assert_eq!(
      state.own_status(&dir, OwnSoundLimits::default()),
      OwnSoundStatus::Unavailable
    );
    let (samples, played) = state.samples_for(MessageSound::OwnFile, &dir, OwnSoundLimits::default());
    assert_eq!(played, SoundPlayed::FellBack);
    assert_eq!(*samples, message_sounds::synthesize(BuiltInSound::DEFAULT));
  }

  #[test]
  fn files_too_large_unreadable_or_not_sound_are_refused_and_the_kept_one_stays() {
    let dir = temporary_dir();
    let sounds = dir.join(SOUNDS_DIR);
    let state = MessageSoundState::default();
    let good = dir.join("good.wav");
    std::fs::write(&good, wav(0.2)).unwrap();
    assert!(matches!(
      state.keep(&sounds, &good, OwnSoundLimits::default()),
      Ok(OwnSoundPick::Stored(_))
    ));
    let small = OwnSoundLimits {
      max_file_bytes: 100,
      ..OwnSoundLimits::default()
    };
    assert_eq!(
      state.keep(&sounds, &good, small),
      Ok(refused(OwnSoundRefusal::TooLarge))
    );
    let text = dir.join("notes.mp3");
    std::fs::write(&text, b"just some text, not a sound").unwrap();
    assert_eq!(
      state.keep(&sounds, &text, OwnSoundLimits::default()),
      Ok(refused(OwnSoundRefusal::NotAudio))
    );
    assert_eq!(
      state.keep(&sounds, &dir.join("gone.wav"), OwnSoundLimits::default()),
      Ok(refused(OwnSoundRefusal::Unreadable))
    );
    assert_eq!(std::fs::read(sounds.join(OWN_FILE)).unwrap(), wav(0.2));
  }

  #[test]
  fn a_longer_limit_decodes_the_kept_file_again() {
    let dir = temporary_dir();
    let picked = dir.join("long.wav");
    std::fs::write(&picked, wav(3.0)).unwrap();
    let state = MessageSoundState::default();
    let short = OwnSoundLimits {
      max_play_seconds: 2,
      ..OwnSoundLimits::default()
    };
    let Ok(OwnSoundPick::Stored(info)) = state.keep(&dir, &picked, short) else {
      panic!("not kept")
    };
    assert_eq!(info.play_ms, 2000);
    let OwnSoundStatus::Ready(info) = state.own_status(&dir, OwnSoundLimits::default()) else {
      panic!("not ready")
    };
    assert_eq!(info.play_ms, 3000);
  }

  #[test]
  fn only_sound_extensions_help_to_recognize_a_file() {
    assert_eq!(extension_of(Path::new("a/b.OGG")), Some("ogg".to_owned()));
    assert_eq!(extension_of(Path::new("a/b.exe")), None);
    assert_eq!(extension_of(Path::new("a/b")), None);
  }

  fn temporary_dir() -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-message-sound-{}-{}",
      std::process::id(),
      COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }

  /// A mono 16-bit 48 kHz WAV file with a 440 Hz tone.
  fn wav(seconds: f32) -> Vec<u8> {
    let frames = (48_000.0 * seconds) as u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    for field in [
      16u32.to_le_bytes(),
      [1, 0, 1, 0],
      48_000u32.to_le_bytes(),
      96_000u32.to_le_bytes(),
    ] {
      bytes.extend_from_slice(&field);
    }
    bytes.extend_from_slice(&[2, 0, 16, 0]);
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 2).to_le_bytes());
    for frame in 0..frames {
      let value = (0.4 * (std::f32::consts::TAU * 440.0 * frame as f32 / 48_000.0).sin() * 32_767.0) as i16;
      bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
  }
}
