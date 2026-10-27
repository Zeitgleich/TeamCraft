//! Demo pictures, a demo video and text files in `#design`, so the files, albums, the lightbox and
//! the previews can be shown without a server. The bytes are small generated fixtures
//! (`demo-media/`), and one large log made when first asked for.

use std::fmt::Write as _;
use std::sync::LazyLock;

use teamcraft_client_api::actions::{FileInfo, FileTransfer};

use crate::model::{ConversationId, Message};

/// A generated log of this many lines (about 9 MB, below the default preview limit), for the text
/// viewer's virtualized rows and finding in a large file (#821).
const LARGE_LOG_LINES: usize = 100_000;
const LARGE_LOG_ID: &str = "9000013";
/// The message the large log is attached to, counted from the newest.
const LARGE_LOG_AT: usize = 9;
static LARGE_LOG: LazyLock<Vec<u8>> = LazyLock::new(large_log);

/// A file of the demo, its full bytes and its thumbnail.
struct DemoFile {
  id: &'static str,
  name: &'static str,
  media_type: &'static str,
  bytes: &'static [u8],
  maybe_thumbnail: Option<&'static [u8]>,
}

const LAKE: DemoFile = DemoFile {
  id: "9000001",
  name: "lake.jpg",
  media_type: "image/jpeg",
  bytes: include_bytes!("../demo-media/lake.jpg"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/lake-thumb.png")),
};
const DUNES: DemoFile = DemoFile {
  id: "9000002",
  name: "dunes.jpg",
  media_type: "image/jpeg",
  bytes: include_bytes!("../demo-media/dunes.jpg"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/dunes-thumb.png")),
};
const PEAKS: DemoFile = DemoFile {
  id: "9000003",
  name: "peaks.jpg",
  media_type: "image/jpeg",
  bytes: include_bytes!("../demo-media/peaks.jpg"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/peaks-thumb.png")),
};
const BOUNCE: DemoFile = DemoFile {
  id: "9000004",
  name: "bounce.gif",
  media_type: "image/gif",
  bytes: include_bytes!("../demo-media/bounce.gif"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/bounce-thumb.png")),
};
const REPORT: DemoFile = DemoFile {
  id: "9000006",
  name: "quarterly-report.pdf",
  media_type: "application/pdf",
  bytes: b"%PDF-1.4 demo",
  maybe_thumbnail: None,
};
/// The script in it must never run: SVG is only shown drawn into a PNG.
const DIAGRAM: DemoFile = DemoFile {
  id: "9000007",
  name: "diagram.svg",
  media_type: "image/svg+xml",
  bytes: include_bytes!("../demo-media/diagram.svg"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/diagram-thumb.png")),
};
/// Thumbnail made by the sender's browser (JPEG), as for every format the core can't decode.
const LANDSCAPE: DemoFile = DemoFile {
  id: "9000008",
  name: "landscape.avif",
  media_type: "image/avif",
  bytes: include_bytes!("../demo-media/landscape.avif"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/landscape-thumb.jpg")),
};
/// A format this browser can't decode, without a thumbnail: shown as a file card.
const PHOTO: DemoFile = DemoFile {
  id: "9000009",
  name: "IMG_0042.heic",
  media_type: "image/heic",
  bytes: b"not decodable in this demo",
  maybe_thumbnail: None,
};
const CLIP: DemoFile = DemoFile {
  id: "9000005",
  name: "clip.webm",
  media_type: "video/webm",
  bytes: include_bytes!("../demo-media/clip.webm"),
  maybe_thumbnail: Some(include_bytes!("../demo-media/clip-thumb.png")),
};

/// A log with levels, terminal colours and a stack trace, for the text viewer (#821). IDs of the
/// demo files must not start with one another (`demo_bytes`).
const SERVER_LOG: DemoFile = DemoFile {
  id: "9000011",
  name: "server.log",
  media_type: "text/plain",
  bytes: include_bytes!("../demo-media/server.log"),
  maybe_thumbnail: None,
};
/// Minified JSON, to format in the text viewer.
const SETTINGS_JSON: DemoFile = DemoFile {
  id: "9000012",
  name: "settings.json",
  media_type: "application/json",
  bytes: include_bytes!("../demo-media/settings.json"),
  maybe_thumbnail: None,
};

/// Which files go with which message, counted from the newest message of the channel.
const ATTACHED: [(usize, &[DemoFile]); 9] = [
  (0, &[REPORT]),
  (1, &[LAKE, DUNES, PEAKS]),
  (2, &[DIAGRAM]),
  (4, &[LANDSCAPE]),
  (6, &[PHOTO]),
  (3, &[BOUNCE]),
  (5, &[CLIP]),
  (7, &[SERVER_LOG, SETTINGS_JSON]),
  (8, &[DUNES]),
];

/// The files of the demo messages, newest first like the real backend.
pub(super) fn demo_files(conversation: ConversationId, messages: &[Message]) -> Vec<FileInfo> {
  let mut files = Vec::new();
  for (from_newest, attached) in ATTACHED {
    let Some(message) = messages
      .len()
      .checked_sub(from_newest + 1)
      .and_then(|index| messages.get(index))
    else {
      continue;
    };
    files.extend(attached.iter().map(|file| FileInfo {
      conversation,
      message: message.id,
      // The single dunes picture repeats the album's file under its own ID.
      file_id: format!("{}{from_newest}", file.id),
      file_name: file.name.to_owned(),
      media_type: file.media_type.to_owned(),
      size_bytes: file.bytes.len() as u64,
      maybe_thumbnail_png: file.maybe_thumbnail.map(<[u8]>::to_vec),
      transfer: FileTransfer::Remote,
      maybe_sender: Some(message.author),
      maybe_sent_at: Some(message.sent_at),
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_content_sha256: None,
    }));
  }
  let maybe_large_log_message = messages
    .len()
    .checked_sub(LARGE_LOG_AT + 1)
    .and_then(|index| messages.get(index));
  if let Some(message) = maybe_large_log_message {
    files.push(FileInfo {
      conversation,
      message: message.id,
      file_id: format!("{LARGE_LOG_ID}{LARGE_LOG_AT}"),
      file_name: "large.log".to_owned(),
      media_type: "text/plain".to_owned(),
      size_bytes: LARGE_LOG.len() as u64,
      maybe_thumbnail_png: None,
      transfer: FileTransfer::Remote,
      maybe_sender: Some(message.author),
      maybe_sent_at: Some(message.sent_at),
      maybe_dimensions: None,
      maybe_duration_ms: None,
      maybe_content_sha256: None,
    });
  }
  files
}

/// The full bytes of a demo file, as the lightbox fetches them.
pub fn demo_bytes(file_id: &str) -> Option<&'static [u8]> {
  if file_id.starts_with(LARGE_LOG_ID) {
    return Some(LARGE_LOG.as_slice());
  }
  [
    LAKE,
    DUNES,
    PEAKS,
    BOUNCE,
    CLIP,
    REPORT,
    DIAGRAM,
    LANDSCAPE,
    PHOTO,
    SERVER_LOG,
    SETTINGS_JSON,
  ]
  .into_iter()
  .find(|file| file_id.starts_with(file.id))
  .map(|file| file.bytes)
}

/// The large log: a line per processed item, every seventh an error, and now and then a very long
/// line (a dumped payload) that the viewer shows in parts.
fn large_log() -> Vec<u8> {
  const LEVELS: [&str; 5] = ["INFO", "DEBUG", "INFO", "WARN", "ERROR"];
  let mut text = String::with_capacity(LARGE_LOG_LINES * 90);
  for line in 0..LARGE_LOG_LINES {
    let seconds = line / 10;
    let level = LEVELS[line % 7 % LEVELS.len()];
    // Writing to a String can't fail.
    let _ = writeln!(
      text,
      "2026-10-27T{:02}:{:02}:{:02}.{:03}Z {level:>5} app::worker: processed item {line} of batch {} in {} ms",
      seconds / 3600 % 24,
      seconds / 60 % 60,
      seconds % 60,
      line % 1000,
      line / 500,
      line % 97,
    );
    if line % 25_000 == 12_345 {
      let _ = writeln!(text, "payload of item {line}: {}", "abcdefghij".repeat(2_000));
    }
  }
  text.into_bytes()
}
