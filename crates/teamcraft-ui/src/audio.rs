//! Audio attachments (#802), apart from the DOM so it can be tested: which files play in the
//! conversation, as what, and where a player stands. The player itself is
//! [`crate::audio_player`], its card `components::audio_card`.
//!
//! A file is audio by its media type (`audio/*`), else by its extension when the sender's system
//! handed over no specific type (`application/octet-stream`), like [`crate::preview::PreviewKind`].
//! A specific type is never overruled by the extension: `song.mp3` sent as `application/pdf` is not
//! audio. The decrypted file is given to the browser as a `blob:` of a known audio type only
//! ([`AudioFormat::blob_type`]); an audio type this list doesn't know stays opaque bytes, which the
//! browser may still sniff and play, or the card says it can't and offers the download.

use crate::media_formats::essence;

/// The audio formats the app names for the browser. What a webview can actually play differs
/// (WebKitGTK depends on the installed GStreamer plugins); a file that doesn't play says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
  /// MP3.
  Mpeg,
  /// M4A: AAC or ALAC in an MP4 container.
  Mp4,
  /// Raw AAC (ADTS).
  Aac,
  /// Ogg Vorbis or Ogg Opus.
  Ogg,
  Wav,
  Flac,
  Webm,
  /// Another `audio/*` type: tried as opaque bytes.
  Other,
}

/// Where the one player of the app stands with the attachment it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioPhase {
  /// Downloading and decrypting, after the first press on play.
  Loading,
  /// Loaded and not playing (paused, ended, or the browser refused to start without a new press).
  Paused,
  Playing,
  Failed(AudioFailure),
}

/// Why an attachment doesn't play.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFailure {
  /// Not downloaded or decrypted (offline, server trouble): worth trying again.
  Unavailable,
  /// Here, but this webview can't play the format: the download is offered instead (#322).
  Unplayable,
}

impl AudioFormat {
  /// What the file plays as, by its media type, else by its extension when the type says nothing.
  /// `None`: not audio, a file card.
  pub fn of(media_type: &str, file_name: &str) -> Option<Self> {
    let essence = essence(media_type);
    if let Some(format) = Self::of_media_type(&essence) {
      return Some(format);
    }
    let is_unspecific = matches!(essence.as_str(), "" | "application/octet-stream");
    if !is_unspecific {
      return None;
    }
    file_name
      .rsplit_once('.')
      .and_then(|(_, extension)| Self::of_extension(&extension.to_ascii_lowercase()))
  }

  /// The type of the `blob:` the player gets; `None` leaves it opaque. WebKit picks its decoder by
  /// this type, so a known format always gets one, also when the sender gave none.
  pub fn blob_type(self) -> Option<&'static str> {
    match self {
      Self::Mpeg => Some("audio/mpeg"),
      Self::Mp4 => Some("audio/mp4"),
      Self::Aac => Some("audio/aac"),
      Self::Ogg => Some("audio/ogg"),
      Self::Wav => Some("audio/wav"),
      Self::Flac => Some("audio/flac"),
      Self::Webm => Some("audio/webm"),
      Self::Other => None,
    }
  }

  fn of_media_type(essence: &str) -> Option<Self> {
    match essence {
      "audio/mpeg" | "audio/mp3" | "audio/mpeg3" | "audio/x-mpeg" | "audio/x-mp3" => Some(Self::Mpeg),
      "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/x-mp4" => Some(Self::Mp4),
      "audio/aac" | "audio/x-aac" | "audio/aacp" => Some(Self::Aac),
      "audio/ogg" | "audio/opus" | "audio/vorbis" | "audio/x-ogg" | "application/ogg" => Some(Self::Ogg),
      "audio/wav" | "audio/x-wav" | "audio/wave" | "audio/vnd.wave" => Some(Self::Wav),
      "audio/flac" | "audio/x-flac" => Some(Self::Flac),
      "audio/webm" => Some(Self::Webm),
      other if other.starts_with("audio/") => Some(Self::Other),
      _ => None,
    }
  }

  fn of_extension(extension: &str) -> Option<Self> {
    match extension {
      "mp3" => Some(Self::Mpeg),
      "m4a" | "m4b" => Some(Self::Mp4),
      "aac" => Some(Self::Aac),
      "ogg" | "oga" | "opus" => Some(Self::Ogg),
      "wav" | "wave" => Some(Self::Wav),
      "flac" => Some(Self::Flac),
      "weba" => Some(Self::Webm),
      _ => None,
    }
  }
}

impl AudioPhase {
  /// The play button shows "pause" (pressing it pauses).
  pub fn is_playing(self) -> bool {
    self == Self::Playing
  }

  /// The file is in the player: the scrubber and the volume are shown.
  pub fn is_loaded(self) -> bool {
    matches!(self, Self::Paused | Self::Playing)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn audio_types_are_audio_whatever_the_name() {
    let cases = [
      ("audio/mpeg", "voice.mp3", AudioFormat::Mpeg),
      ("audio/mp3", "voice", AudioFormat::Mpeg),
      ("Audio/MPEG; charset=binary", "x", AudioFormat::Mpeg),
      ("audio/x-m4a", "memo.m4a", AudioFormat::Mp4),
      ("audio/mp4", "memo.m4a", AudioFormat::Mp4),
      ("audio/aac", "a.aac", AudioFormat::Aac),
      ("audio/ogg", "a.ogg", AudioFormat::Ogg),
      ("audio/opus", "a.opus", AudioFormat::Ogg),
      ("application/ogg", "a.ogg", AudioFormat::Ogg),
      ("audio/x-wav", "a.wav", AudioFormat::Wav),
      ("audio/wave", "a.wav", AudioFormat::Wav),
      ("audio/flac", "a.flac", AudioFormat::Flac),
      ("audio/webm", "a.weba", AudioFormat::Webm),
      ("audio/amr", "call.amr", AudioFormat::Other),
      ("audio/x-ms-wma", "old.wma", AudioFormat::Other),
    ];
    for (media_type, name, format) in cases {
      assert_eq!(
        AudioFormat::of(media_type, name),
        Some(format),
        "{media_type} {name}"
      );
    }
  }

  #[test]
  fn an_unspecific_type_is_told_by_the_extension() {
    let cases = [
      ("application/octet-stream", "song.mp3", AudioFormat::Mpeg),
      ("", "Song.MP3", AudioFormat::Mpeg),
      ("application/octet-stream", "book.m4b", AudioFormat::Mp4),
      ("", "note.opus", AudioFormat::Ogg),
      ("", "take.wav", AudioFormat::Wav),
      ("", "master.flac", AudioFormat::Flac),
      ("", "invoice.pdf.mp3", AudioFormat::Mpeg),
    ];
    for (media_type, name, format) in cases {
      assert_eq!(
        AudioFormat::of(media_type, name),
        Some(format),
        "{media_type:?} {name}"
      );
    }
  }

  #[test]
  fn a_specific_type_is_never_overruled_by_the_name() {
    let cases = [
      ("application/pdf", "song.mp3"),
      ("text/html", "song.mp3"),
      ("text/plain", "song.mp3"),
      ("image/svg+xml", "song.mp3"),
      ("video/mp4", "song.m4a"),
      ("application/x-msdownload", "song.mp3"),
    ];
    for (media_type, name) in cases {
      assert_eq!(AudioFormat::of(media_type, name), None, "{media_type} {name}");
    }
  }

  #[test]
  fn names_that_only_look_like_audio_are_not() {
    for name in ["song.mp3.exe", "mp3", "song", "song.mp3 ", "archive.mp3.zip"] {
      assert_eq!(
        AudioFormat::of("application/octet-stream", name),
        None,
        "{name:?}"
      );
    }
    assert_eq!(AudioFormat::of("", ""), None);
  }

  #[test]
  fn known_formats_get_an_audio_blob_and_unknown_ones_stay_opaque() {
    assert_eq!(AudioFormat::Mpeg.blob_type(), Some("audio/mpeg"));
    assert_eq!(
      AudioFormat::of("", "song.mp3").and_then(AudioFormat::blob_type),
      Some("audio/mpeg"),
      "a file sent without a type still gets one"
    );
    assert_eq!(AudioFormat::Other.blob_type(), None);
    for format in [
      AudioFormat::Mpeg,
      AudioFormat::Mp4,
      AudioFormat::Aac,
      AudioFormat::Ogg,
      AudioFormat::Wav,
      AudioFormat::Flac,
      AudioFormat::Webm,
    ] {
      let blob_type = format.blob_type().unwrap_or_default();
      assert_eq!(
        crate::media_formats::displayable_type(blob_type),
        Some(blob_type),
        "the blob keeps {blob_type}"
      );
    }
  }

  #[test]
  fn only_a_loaded_file_shows_the_player() {
    assert!(AudioPhase::Playing.is_playing());
    assert!(!AudioPhase::Paused.is_playing());
    assert!(AudioPhase::Paused.is_loaded());
    assert!(!AudioPhase::Loading.is_loaded());
    assert!(!AudioPhase::Failed(AudioFailure::Unplayable).is_loaded());
  }
}
