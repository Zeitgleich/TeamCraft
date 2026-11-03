//! Video attachments the webview can't play (#804). On Linux WebKitGTK plays `<video>` through the
//! system's GStreamer, and a distribution without an H.264 or HEVC decoder leaves a phone video at
//! 0:00. The shell looks the decoders up in GStreamer's registry (the webview's process reads the
//! same one) so the player can name the package to install, and it can hand a video to the
//! system's own player instead.
//!
//! Opening in the system's player writes the decrypted video to a private folder: on Linux under
//! `$XDG_RUNTIME_DIR` (a tmpfs, gone at log-out), elsewhere in the app's cache folder. Only the
//! latest opened video is kept there, and the folder is emptied at every start.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager, State};
use tauri_plugin_opener::OpenerExt;
use teamcraft_client_api::remote::AttachmentPriority;
use teamcraft_client_api::video_playback::VideoPlayback;
use teamcraft_proto::{ConversationId, MessageId};

use crate::core::commands::{CoreHost, save_name};

/// The folder of videos handed to the system's player, inside the runtime or cache folder.
const OPENED_FOLDER: &str = "teamcraft-opened-videos";

/// What the registry offers for phone videos; `None` on systems whose webview doesn't play through
/// GStreamer (macOS, Windows) or where GStreamer doesn't start. Looked up once: the webview's
/// GStreamer reads its registry at start too, so a newly installed plugin needs a restart anyway.
#[tauri::command(rename_all = "snake_case")]
pub async fn video_playback() -> Option<VideoPlayback> {
  #[cfg(target_os = "linux")]
  {
    tokio::task::spawn_blocking(linux::playback).await.ok().flatten()
  }
  #[cfg(not(target_os = "linux"))]
  {
    None
  }
}

/// Decrypts a video of a message and opens it in the system's default player, for a video the
/// webview can't play. Only videos: the file gets the extension of its media type, so the system
/// can't take it for anything else (a script, a `.desktop` file).
#[tauri::command(rename_all = "snake_case")]
pub async fn open_video_externally(
  app: AppHandle,
  host: State<'_, CoreHost>,
  conversation: ConversationId,
  message: MessageId,
  file_id: String,
  file_name: String,
  media_type: String,
) -> Result<(), String> {
  let extension = video_extension(&media_type)
    .ok_or_else(|| "only videos can be opened in the system's player".to_owned())?;
  let plaintext = host
    .current()?
    .attachment(conversation, message, file_id, AttachmentPriority::default())
    .await?;
  let folder = opened_folder(&app)?;
  clear_folder(&folder);
  let path = folder.join(opened_name(&file_name, extension));
  write_private(&path, plaintext.expose_secret()).map_err(|error| {
    tracing::warn!(%error, conversation_id = %conversation, "the video could not be written for the system's player");
    "the video could not be prepared for the system's player".to_owned()
  })?;
  tracing::info!(conversation_id = %conversation, message_id = %message, "opening a video in the system's player");
  app
    .opener()
    .open_path(path.to_string_lossy(), None::<&str>)
    .map_err(|error| error.to_string())
}

/// Empties the folder of opened videos, left over from the last run.
pub fn clear_opened_videos(app: &AppHandle) {
  if let Ok(folder) = opened_folder(app) {
    clear_folder(&folder);
  }
}

/// The private folder of opened videos, created if needed.
fn opened_folder(app: &AppHandle) -> Result<PathBuf, String> {
  let base = runtime_dir()
    .or_else(|| app.path().app_cache_dir().ok())
    .ok_or_else(|| "no folder for the system's player".to_owned())?;
  let folder = base.join(OPENED_FOLDER);
  create_private_dir(&folder).map_err(|error| {
    tracing::warn!(%error, "the folder for the system's player could not be created");
    "no folder for the system's player".to_owned()
  })?;
  Ok(folder)
}

/// The tmpfs of the user's session on Linux.
fn runtime_dir() -> Option<PathBuf> {
  if cfg!(target_os = "linux") {
    std::env::var_os("XDG_RUNTIME_DIR")
      .map(PathBuf::from)
      .filter(|dir| dir.is_absolute())
  } else {
    None
  }
}

/// Removes the earlier videos. One the system's player still has open stays readable to it (Unix
/// unlinks it); on Windows it can't be removed yet and goes at the next start.
fn clear_folder(folder: &Path) {
  let Ok(entries) = fs::read_dir(folder) else {
    return;
  };
  for entry in entries.flatten() {
    // Deliberately ignored: a file still in use is removed next time.
    drop(fs::remove_file(entry.path()));
  }
}

/// The extension a video of `media_type` is saved with; `None` for anything that isn't a video.
fn video_extension(media_type: &str) -> Option<&'static str> {
  let essence = media_type
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase();
  match essence.as_str() {
    "video/mp4" => Some("mp4"),
    "video/quicktime" => Some("mov"),
    "video/webm" => Some("webm"),
    "video/x-matroska" => Some("mkv"),
    "video/3gpp" => Some("3gp"),
    "video/x-m4v" => Some("m4v"),
    _ => None,
  }
}

/// The file's own name with the extension of its media type.
fn opened_name(file_name: &str, extension: &str) -> String {
  let name = save_name(file_name);
  let stem = Path::new(&name)
    .file_stem()
    .and_then(|stem| stem.to_str())
    .filter(|stem| !stem.is_empty())
    .unwrap_or("video");
  format!("{stem}.{extension}")
}

#[cfg(unix)]
fn create_private_dir(folder: &Path) -> std::io::Result<()> {
  use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
  fs::DirBuilder::new().recursive(true).mode(0o700).create(folder)?;
  fs::set_permissions(folder, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(folder: &Path) -> std::io::Result<()> {
  fs::create_dir_all(folder)
}

/// Writes `bytes` readable by the user only.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
  let mut options = fs::OpenOptions::new();
  options.write(true).create(true).truncate(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  let mut file = options.open(path)?;
  file.write_all(bytes)?;
  file.flush()
}

#[cfg(target_os = "linux")]
mod linux {
  use teamcraft_client_api::video_playback::{CodecSource, VideoPlayback};
  use teamcraft_media::playback_gstreamer::playback_decoders;

  use crate::updates::install_kind;

  const OS_RELEASE: &str = "/etc/os-release";

  /// The registry's answer with the package names of this install, looked up once.
  pub fn playback() -> Option<VideoPlayback> {
    static PLAYBACK: std::sync::OnceLock<Option<VideoPlayback>> = std::sync::OnceLock::new();
    PLAYBACK.get_or_init(look_up).clone()
  }

  fn look_up() -> Option<VideoPlayback> {
    let decoders = playback_decoders()?;
    let maybe_os_release = std::fs::read_to_string(OS_RELEASE).ok();
    let playback = VideoPlayback {
      has_mp4_demuxer: decoders.has_mp4_demuxer,
      has_h264_decoder: decoders.has_h264_decoder,
      has_hevc_decoder: decoders.has_hevc_decoder,
      source: CodecSource::new(install_kind(), maybe_os_release.as_deref()),
    };
    let missing = playback.missing();
    if missing.is_empty() {
      tracing::info!("GStreamer can play MP4 videos with H.264 and HEVC");
    } else {
      tracing::warn!(
        ?missing,
        packages = ?playback.packages_to_install(),
        "GStreamer lacks parts to play phone videos; video attachments may not play"
      );
    }
    Some(playback)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn videos_get_the_extension_of_their_type() {
    assert_eq!(video_extension("video/mp4"), Some("mp4"));
    assert_eq!(video_extension("Video/QuickTime; codecs=hvc1"), Some("mov"));
    assert_eq!(video_extension("video/webm"), Some("webm"));
  }

  #[test]
  fn other_types_are_refused() {
    for media_type in [
      "",
      "application/x-desktop",
      "text/x-shellscript",
      "image/png",
      "video/",
    ] {
      assert_eq!(video_extension(media_type), None, "{media_type}");
    }
  }

  #[test]
  fn the_name_keeps_its_stem_and_gets_the_type_extension() {
    assert_eq!(opened_name("IMG_0001.MOV", "mov"), "IMG_0001.mov");
    assert_eq!(opened_name("run.desktop", "mp4"), "run.mp4");
    assert_eq!(opened_name("clips/a.mp4", "mp4"), "clips_a.mp4");
    assert_eq!(opened_name("", "mp4"), "file.mp4");
    assert_eq!(opened_name("noext", "webm"), "noext.webm");
  }
}
