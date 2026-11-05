//! Voice messages in the desktop app (#204). The microphone is the shell's, as in calls (the
//! webview doesn't capture: WebKitGTK media is unreliable, ARCHITECTURE.md §7), so the shell
//! records with `teamcraft-media` into a private folder and the composer polls its level and
//! length while it shows the recording.
//!
//! - A finished take stays in the recording folder until the user sends or discards it. Sending
//!   moves it into the core's upload staging folder, which the core empties once the upload is
//!   done; the recording folder is emptied at every start, so a take left by a crash is never
//!   sent.
//! - Voice messages the webview can't play (Ogg or WebM Opus in the macOS webview) are decoded
//!   here from memory to WAV ([`voice_playback_prepare`]), no longer than the workspace's limit
//!   and than the file's size can hold, into a private folder; the webview reads it in parts and
//!   lets go of it ([`voice_playback_release`]). The decrypted Opus never touches the disk.
//! - Sign-out ([`stop_for_sign_out`]) cancels a recording and deletes every take.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager, State};
use teamcraft_client_api::remote::AttachmentPriority;
use teamcraft_client_api::voice::{RecordingRefusal, RecordingStatus, RecordingStop, ShellTake};
use teamcraft_media::audio_io::AudioDeviceChoice;
use teamcraft_media::voice_decode;
use teamcraft_media::voice_recording::{VoiceRecorder, VoiceRecordingError, clear_takes};
use teamcraft_proto::{ConversationId, MAX_VOICE_MESSAGE_MINUTES, MessageId};

use crate::core::commands::CoreHost;
use crate::video_playback::create_private_dir;

/// The private folder of takes, in the app's cache folder.
const TAKES_FOLDER: &str = "voice-takes";
/// The private folder of voice messages decoded for playback.
const PLAYBACK_FOLDER: &str = "voice-playback";
/// The most of a decoded file one read hands the webview.
const PLAYBACK_CHUNK_BYTES: u64 = 4 * 1024 * 1024;

/// The recording under way, if any; one at a time for the whole app.
#[derive(Default)]
pub struct VoiceState {
  maybe_recorder: Mutex<Option<VoiceRecorder>>,
  takes: AtomicU64,
  /// Numbers the decoded files, so two decodes never share one.
  decodes: AtomicU64,
}

/// A decoded voice message ready to be read.
#[derive(Debug, serde::Serialize)]
pub struct DecodedVoice {
  token: String,
  size_bytes: u64,
}

/// Empties the recording and playback folders, left over from the last run.
pub fn clear_voice_folders(app: &AppHandle) {
  for name in [TAKES_FOLDER, PLAYBACK_FOLDER] {
    if let Ok(folder) = private_folder(app, name) {
      clear_takes(&folder);
    }
  }
}

/// Sign-out: a recording ends without a take and every take and decoded file is deleted.
pub fn stop_for_sign_out(app: &AppHandle) {
  let state = app.state::<VoiceState>();
  if let Some(recorder) = take_recorder(&state) {
    recorder.cancel();
  }
  clear_voice_folders(app);
}

/// Starts recording from the system's default microphone, for at most `max_seconds`.
#[tauri::command(rename_all = "snake_case")]
pub async fn voice_record_start(
  app: AppHandle,
  state: State<'_, VoiceState>,
  max_seconds: u32,
) -> Result<(), String> {
  let folder =
    private_folder(&app, TAKES_FOLDER).map_err(|_| RecordingRefusal::Failed.as_str().to_owned())?;
  let path = folder.join(format!(
    "take-{}.ogg",
    state.takes.fetch_add(1, Ordering::Relaxed)
  ));
  let max = Duration::from_secs(u64::from(
    max_seconds.min(u32::from(MAX_VOICE_MESSAGE_MINUTES) * 60),
  ));
  if is_recording(&state) {
    return Err(RecordingRefusal::Busy.as_str().to_owned());
  }
  // Opening the microphone takes a moment; the composer waits for it to show the recording.
  let started =
    tokio::task::spawn_blocking(move || VoiceRecorder::start(&path, &AudioDeviceChoice::Default, max))
      .await
      .map_err(|_| RecordingRefusal::Failed.as_str().to_owned())?;
  let recorder = started.map_err(|error| {
    tracing::warn!(%error, "the voice recording did not start");
    match error {
      VoiceRecordingError::Microphone(_) => RecordingRefusal::Microphone.as_str().to_owned(),
      _ => RecordingRefusal::Failed.as_str().to_owned(),
    }
  })?;
  let mut slot = state
    .maybe_recorder
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if slot.is_some() {
    // Another window started one meanwhile: this one goes.
    drop(slot);
    recorder.cancel();
    return Err(RecordingRefusal::Busy.as_str().to_owned());
  }
  *slot = Some(recorder);
  Ok(())
}

fn is_recording(state: &VoiceState) -> bool {
  state
    .maybe_recorder
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .is_some()
}

/// The recording's level and length; `None` without one.
#[tauri::command(rename_all = "snake_case")]
pub fn voice_record_status(state: State<'_, VoiceState>) -> Option<RecordingStatus> {
  let slot = state
    .maybe_recorder
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  slot.as_ref().map(|recorder| RecordingStatus {
    level: recorder.level(),
    elapsed_ms: recorder.elapsed().as_millis() as u64,
    maybe_stop: recorder.maybe_stop(),
  })
}

/// Stops the recording and keeps the take for the user to send or discard. `stop`: why the app
/// stopped it, when the user didn't.
#[tauri::command(rename_all = "snake_case")]
pub async fn voice_record_finish(
  state: State<'_, VoiceState>,
  stop: Option<RecordingStop>,
) -> Result<ShellTake, String> {
  let recorder = take_recorder(&state).ok_or_else(|| RecordingRefusal::Failed.as_str().to_owned())?;
  let take = tokio::task::spawn_blocking(move || recorder.finish(stop))
    .await
    .map_err(|_| RecordingRefusal::Failed.as_str().to_owned())?
    .map_err(|error| {
      tracing::debug!(%error, "the voice recording ended without a take");
      RecordingRefusal::Failed.as_str().to_owned()
    })?;
  Ok(ShellTake {
    path: take.path.to_string_lossy().into_owned(),
    duration_ms: take.duration_ms,
    waveform: take.waveform,
    maybe_stop: take.maybe_stop,
  })
}

/// Stops the recording and deletes it.
#[tauri::command(rename_all = "snake_case")]
pub async fn voice_record_cancel(state: State<'_, VoiceState>) -> Result<(), String> {
  if let Some(recorder) = take_recorder(&state) {
    drop(tokio::task::spawn_blocking(move || recorder.cancel()).await);
  }
  Ok(())
}

/// Moves a take into the core's upload staging folder under `file_name`; returns the path the
/// message names.
#[tauri::command(rename_all = "snake_case")]
pub fn voice_take_stage(
  app: AppHandle,
  host: State<'_, CoreHost>,
  path: String,
  file_name: String,
) -> Result<String, String> {
  let take = own_take(&app, &path)?;
  let staging = host.current()?.staging_dir().to_path_buf();
  create_private_dir(&staging).map_err(|error| error.to_string())?;
  let target = staging.join(format!("voice-{}-{}", std::process::id(), plain_name(&file_name)));
  // Across file systems a rename fails: copy, then delete.
  if fs::rename(&take, &target).is_err() {
    fs::copy(&take, &target).map_err(|error| error.to_string())?;
    drop(fs::remove_file(&take));
  }
  Ok(target.to_string_lossy().into_owned())
}

/// Deletes a take the user discarded.
#[tauri::command(rename_all = "snake_case")]
pub fn voice_take_discard(app: AppHandle, path: String) -> Result<(), String> {
  let take = own_take(&app, &path)?;
  fs::remove_file(take).map_err(|error| error.to_string())
}

/// Decodes a voice message to WAV for a webview that can't play its Opus (macOS), at most
/// `max_seconds` (the workspace's limit) long. The webview then reads it with
/// [`voice_playback_read`] and lets go of it with [`voice_playback_release`].
#[tauri::command(rename_all = "snake_case")]
pub async fn voice_playback_prepare(
  app: AppHandle,
  host: State<'_, CoreHost>,
  state: State<'_, VoiceState>,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  max_seconds: u32,
) -> Result<DecodedVoice, String> {
  let plaintext = host
    .current()?
    .attachment(conversation, message, file_id, AttachmentPriority::Now)
    .await?;
  let folder = private_folder(&app, PLAYBACK_FOLDER).map_err(|error| error.to_string())?;
  let token = format!("{:016x}", state.decodes.fetch_add(1, Ordering::Relaxed));
  let output = folder.join(format!("{token}.wav"));
  let max_minutes = u32::from(MAX_VOICE_MESSAGE_MINUTES);
  let max = Duration::from_secs(u64::from(max_seconds.clamp(1, max_minutes * 60)));
  let bytes = plaintext.expose_secret().to_vec();
  let decoded = tokio::task::spawn_blocking({
    let output = output.clone();
    move || voice_decode::decode_bytes_to_wav(bytes, &output, max)
  })
  .await;
  match decoded {
    Ok(Ok(_)) => {
      let size_bytes = fs::metadata(&output).map_err(|error| error.to_string())?.len();
      Ok(DecodedVoice { token, size_bytes })
    },
    Ok(Err(error)) => {
      tracing::debug!(%error, conversation_id = %conversation, "a voice message did not decode");
      Err("not playable".to_owned())
    },
    Err(_) => Err("not playable".to_owned()),
  }
}

/// Up to 4 MiB of a decoded voice message from `offset`.
#[tauri::command(rename_all = "snake_case")]
pub fn voice_playback_read(
  app: AppHandle,
  token: String,
  offset: u64,
) -> Result<tauri::ipc::Response, String> {
  use std::io::{Read, Seek, SeekFrom};

  let path = decoded_path(&app, &token)?;
  let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
  file
    .seek(SeekFrom::Start(offset))
    .map_err(|error| error.to_string())?;
  let mut chunk = Vec::new();
  file
    .take(PLAYBACK_CHUNK_BYTES)
    .read_to_end(&mut chunk)
    .map_err(|error| error.to_string())?;
  Ok(tauri::ipc::Response::new(chunk))
}

/// Deletes a decoded voice message once the webview holds it.
#[tauri::command(rename_all = "snake_case")]
pub fn voice_playback_release(app: AppHandle, token: String) -> Result<(), String> {
  let path = decoded_path(&app, &token)?;
  match fs::remove_file(path) {
    Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
    _ => Ok(()),
  }
}

/// The decoded file of `token`; only tokens [`voice_playback_prepare`] hands out (hex digits).
fn decoded_path(app: &AppHandle, token: &str) -> Result<PathBuf, String> {
  if token.len() != 16 || !token.chars().all(|char| char.is_ascii_hexdigit()) {
    return Err("not a decoded voice message".to_owned());
  }
  let folder = private_folder(app, PLAYBACK_FOLDER).map_err(|error| error.to_string())?;
  Ok(folder.join(format!("{token}.wav")))
}

fn take_recorder(state: &VoiceState) -> Option<VoiceRecorder> {
  state
    .maybe_recorder
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
    .take()
}

/// A take path from the webview, if it names a file directly in the recording folder.
fn own_take(app: &AppHandle, path: &str) -> Result<PathBuf, String> {
  let folder = private_folder(app, TAKES_FOLDER).map_err(|error| error.to_string())?;
  let path = Path::new(path);
  let is_own = path.parent() == Some(folder.as_path())
    && path
      .file_name()
      .is_some_and(|name| name.to_string_lossy().starts_with("take-"));
  match is_own {
    true => Ok(path.to_path_buf()),
    false => Err("not a voice recording".to_owned()),
  }
}

/// `file_name` without anything that could leave the folder.
fn plain_name(file_name: &str) -> String {
  let name: String = file_name
    .chars()
    .map(|char| match char {
      '/' | '\\' | ':' => '-',
      other => other,
    })
    .collect();
  match name.trim_start_matches('.') {
    "" => "voice-message.ogg".to_owned(),
    name => name.to_owned(),
  }
}

/// A private folder in the app's cache folder, created if needed.
fn private_folder(app: &AppHandle, name: &str) -> Result<PathBuf, std::io::Error> {
  let base = app
    .path()
    .app_cache_dir()
    .map_err(|error| std::io::Error::other(error.to_string()))?;
  let folder = base.join(name);
  create_private_dir(&folder)?;
  Ok(folder)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn file_names_stay_in_the_folder() {
    assert_eq!(plain_name("../../etc/passwd"), "-..-etc-passwd");
    assert_eq!(
      plain_name("voice-message-20261105-140312.ogg"),
      "voice-message-20261105-140312.ogg"
    );
    assert_eq!(plain_name(".."), "voice-message.ogg");
  }
}
