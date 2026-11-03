//! Webhooks with files: `multipart/form-data` bodies as the bridge's endpoint reads them, in
//! whatever pieces the network delivers. None of it may panic; the outcome may not depend on where
//! the body is cut; the parser holds no more than its bound between pieces; and what a form yields
//! stays within the limits.
#![no_main]

use libfuzzer_sys::fuzz_target;
use teamcraft_bridge::form::{FileAction, FormError, FormLimits, FormReader};
use teamcraft_bridge::multipart::{MAX_HEADER_BYTES, boundary};

const CONTENT_TYPE: &str = "multipart/form-data; boundary=XyZ";
const LIMITS: FormLimits = FormLimits {
  max_files: 3,
  max_file_bytes: 256,
  max_field_bytes: 512,
};

/// The files of a form (name, media type, bytes) and its message (text and alert flag, or why it
/// has none).
type Outcome = (Vec<(String, String, Vec<u8>)>, Result<(String, bool), String>);

fuzz_target!(|data: &[u8]| {
  // The first two bytes say where to cut the body: alternately after that many bytes.
  let Some((cuts, body)) = data.split_first_chunk::<2>() else {
    return;
  };
  let _ = boundary(&String::from_utf8_lossy(body));
  let whole = read(body, &[body.len().max(1)]);
  let pieces = read(body, &[usize::from(cuts[0]) + 1, usize::from(cuts[1]) + 1]);
  assert_eq!(whole.is_ok(), pieces.is_ok(), "{whole:?} vs {pieces:?}");
  if let (Ok(whole), Ok(pieces)) = (&whole, &pieces) {
    assert_eq!(whole, pieces);
    let (files, _) = whole;
    assert!(files.len() <= LIMITS.max_files as usize);
    assert!(files.iter().all(|(_, _, bytes)| bytes.len() as u64 <= LIMITS.max_file_bytes));
    assert!(files.iter().all(|(name, _, _)| !name.is_empty() && !name.contains(['/', '\\'])));
  }
});

/// Reads `body` in pieces of the given sizes, in turn, as the endpoint does.
fn read(body: &[u8], sizes: &[usize]) -> Result<Outcome, FormError> {
  let mut reader = FormReader::new(CONTENT_TYPE, LIMITS)?;
  let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
  let mut rest = body;
  let mut turn = 0;
  while !rest.is_empty() && !reader.is_done() {
    let size = sizes[turn % sizes.len()].min(rest.len());
    turn += 1;
    let (chunk, after) = rest.split_at(size);
    rest = after;
    for action in reader.feed(chunk)? {
      match action {
        FileAction::Start(info) => files.push((info.file_name, info.media_type, Vec::new())),
        FileAction::Data(bytes) => {
          let (_, _, file) = files.last_mut().expect("data follows a start");
          file.extend(bytes);
        },
        FileAction::End => {},
      }
    }
    // A delimiter's length or an unfinished header block, plus a little: never a whole file.
    assert!(reader.buffered_bytes() <= MAX_HEADER_BYTES + 4, "{} bytes", reader.buffered_bytes());
  }
  let fields = reader.finish()?;
  let message = fields
    .message(true, !files.is_empty())
    .map(|message| (message.text, message.is_alert))
    .map_err(|error| error.to_string());
  Ok((files, message))
}
