//! Demo pictures and a demo video in `#design`, so the files, albums and the lightbox can be shown
//! without a server. The bytes are small generated fixtures (`demo-media/`).

use teamcraft_client_api::actions::{FileInfo, FileTransfer};

use crate::model::{ConversationId, Message};

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

/// Which files go with which message, counted from the newest message of the channel.
const ATTACHED: [(usize, &[DemoFile]); 8] = [
  (0, &[REPORT]),
  (1, &[LAKE, DUNES, PEAKS]),
  (2, &[DIAGRAM]),
  (4, &[LANDSCAPE]),
  (6, &[PHOTO]),
  (3, &[BOUNCE]),
  (5, &[CLIP]),
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
    }));
  }
  files
}

/// The full bytes of a demo file, as the lightbox fetches them.
pub fn demo_bytes(file_id: &str) -> Option<&'static [u8]> {
  [
    LAKE, DUNES, PEAKS, BOUNCE, CLIP, REPORT, DIAGRAM, LANDSCAPE, PHOTO,
  ]
  .into_iter()
  .find(|file| file_id.starts_with(file.id))
  .map(|file| file.bytes)
}
