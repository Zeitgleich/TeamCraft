//! A webhook sent as `multipart/form-data` (docs/bridge.md, "Files"), as Discord and Slack take
//! them: the message as JSON in a `payload_json` (or `payload`) part, or in plain `text` and
//! `username` fields (`curl -F text=...`), and files in parts that have a file name (`file`,
//! `files[0]`, ...; the name of the field doesn't matter).
//!
//! The limits apply while the body arrives: a file over the size limit or a file too many is
//! refused before the rest is read. Without I/O, like [`crate::multipart`]: the endpoint writes the
//! files' bytes where it keeps them.

use crate::multipart::{Event, MultipartError, MultipartParser, PartHeaders};
use crate::render::{PayloadError, WebhookMessage, WebhookPayload, render_webhook};

/// Fields besides files a form may have, the message's and unknown ones together.
pub const MAX_FORM_FIELDS: usize = 32;
/// Longest file name kept, in bytes (common file systems allow 255).
const MAX_FILE_NAME_BYTES: usize = 255;
/// Longest media type kept (RFC 6838: 127 characters each for type and subtype).
const MAX_MEDIA_TYPE_CHARS: usize = 255;
const OPAQUE_MEDIA_TYPE: &str = "application/octet-stream";
/// What a file without a usable media type is taken for, by its extension: only kinds the apps
/// show differently (pictures get a thumbnail, videos play).
const MEDIA_TYPES_BY_EXTENSION: [(&str, &str); 10] = [
  ("png", "image/png"),
  ("jpg", "image/jpeg"),
  ("jpeg", "image/jpeg"),
  ("gif", "image/gif"),
  ("webp", "image/webp"),
  ("svg", "image/svg+xml"),
  ("mp4", "video/mp4"),
  ("webm", "video/webm"),
  ("pdf", "application/pdf"),
  ("txt", "text/plain"),
];

/// How much a form may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormLimits {
  /// `0`: the form may carry no file.
  pub max_files: u32,
  pub max_file_bytes: u64,
  /// The fields besides files together (the message JSON, text, unknown fields).
  pub max_field_bytes: usize,
}

/// Reads one form; see the module documentation.
#[derive(Debug)]
pub struct FormReader {
  parser: MultipartParser,
  limits: FormLimits,
  current: Current,
  fields: FormFields,
  file_count: u32,
  field_count: usize,
  field_bytes: usize,
}

/// What happens to the files' bytes, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileAction {
  /// A file begins; its bytes follow.
  Start(FileInfo),
  Data(Vec<u8>),
  /// The current file is complete.
  End,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
  /// The sender's file name without any path, control characters or excess length.
  pub file_name: String,
  /// `type/subtype` in lower case; `application/octet-stream` when unknown.
  pub media_type: String,
}

/// The message's fields of a form, once it is read whole.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FormFields {
  maybe_payload: Option<Vec<u8>>,
  maybe_text: Option<String>,
  maybe_username: Option<String>,
}

#[derive(Debug)]
enum Current {
  Nothing,
  File { size_bytes: u64 },
  Field { kind: FieldKind, value: Vec<u8> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
  Payload,
  Text,
  Username,
  /// Unknown, or a repeated field: read and counted, but not kept.
  Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FormError {
  #[error(transparent)]
  Multipart(#[from] MultipartError),
  #[error("more than {max} files")]
  TooManyFiles { max: u32 },
  #[error("a file is larger than {max} bytes")]
  FileTooLarge { max: u64 },
  #[error("the fields besides files are larger than {max} bytes")]
  FieldsTooLarge { max: usize },
  #[error("more than {MAX_FORM_FIELDS} fields besides files")]
  TooManyFields,
  #[error("the field {0} is not UTF-8 text")]
  NotText(&'static str),
}

impl FormReader {
  /// A reader for a body of `content_type` (`multipart/form-data; boundary=...`).
  pub fn new(content_type: &str, limits: FormLimits) -> Result<Self, FormError> {
    Ok(Self {
      parser: MultipartParser::new(content_type)?,
      limits,
      current: Current::Nothing,
      fields: FormFields::default(),
      file_count: 0,
      field_count: 0,
      field_bytes: 0,
    })
  }

  /// Takes the next piece of the body and returns what to do with the files' bytes in it.
  pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<FileAction>, FormError> {
    let mut actions = Vec::new();
    for event in self.parser.feed(chunk)? {
      match event {
        Event::Part(headers) => self.start_part(headers, &mut actions)?,
        Event::Data(bytes) => self.take_data(bytes, &mut actions)?,
        Event::PartEnd => self.end_part(&mut actions)?,
      }
    }
    Ok(actions)
  }

  /// Checks that the body is complete and returns the message's fields.
  pub fn finish(self) -> Result<FormFields, FormError> {
    self.parser.finish()?;
    Ok(self.fields)
  }

  /// Whether the form is complete; the rest of the body is not read.
  pub fn is_done(&self) -> bool {
    self.parser.is_done()
  }

  /// Bytes the parser holds between two calls of [`Self::feed`].
  pub fn buffered_bytes(&self) -> usize {
    self.parser.buffered_bytes()
  }

  fn start_part(&mut self, headers: PartHeaders, actions: &mut Vec<FileAction>) -> Result<(), FormError> {
    let kind = match headers.name.as_str() {
      // Before files: `curl -F payload_json=@message.json` gives the JSON a file name.
      "payload_json" | "payload" => FieldKind::Payload,
      _ if headers.maybe_file_name.is_some() => return self.start_file(headers, actions),
      "text" => FieldKind::Text,
      "username" => FieldKind::Username,
      _ => FieldKind::Ignored,
    };
    self.field_count += 1;
    if self.field_count > MAX_FORM_FIELDS {
      return Err(FormError::TooManyFields);
    }
    let kind = if self.fields.has(kind) {
      FieldKind::Ignored
    } else {
      kind
    };
    self.current = Current::Field {
      kind,
      value: Vec::new(),
    };
    Ok(())
  }

  fn start_file(&mut self, headers: PartHeaders, actions: &mut Vec<FileAction>) -> Result<(), FormError> {
    if self.file_count >= self.limits.max_files {
      return Err(FormError::TooManyFiles {
        max: self.limits.max_files,
      });
    }
    self.file_count += 1;
    let file_name = clean_file_name(headers.maybe_file_name.as_deref().unwrap_or_default());
    let media_type = media_type(headers.maybe_content_type.as_deref(), &file_name);
    actions.push(FileAction::Start(FileInfo {
      file_name,
      media_type,
    }));
    self.current = Current::File { size_bytes: 0 };
    Ok(())
  }

  fn take_data(&mut self, bytes: Vec<u8>, actions: &mut Vec<FileAction>) -> Result<(), FormError> {
    match &mut self.current {
      Current::File { size_bytes } => {
        *size_bytes += bytes.len() as u64;
        if *size_bytes > self.limits.max_file_bytes {
          return Err(FormError::FileTooLarge {
            max: self.limits.max_file_bytes,
          });
        }
        actions.push(FileAction::Data(bytes));
      },
      Current::Field { kind, value } => {
        self.field_bytes += bytes.len();
        if self.field_bytes > self.limits.max_field_bytes {
          return Err(FormError::FieldsTooLarge {
            max: self.limits.max_field_bytes,
          });
        }
        if *kind != FieldKind::Ignored {
          value.extend_from_slice(&bytes);
        }
      },
      // The parser reports data only inside a part.
      Current::Nothing => {},
    }
    Ok(())
  }

  fn end_part(&mut self, actions: &mut Vec<FileAction>) -> Result<(), FormError> {
    match std::mem::replace(&mut self.current, Current::Nothing) {
      Current::File { .. } => actions.push(FileAction::End),
      Current::Field { kind, value } => self.fields.set(kind, value)?,
      Current::Nothing => {},
    }
    Ok(())
  }
}

impl FormFields {
  /// The message: the JSON of `payload_json` (or `payload`), otherwise `text` and `username`. With
  /// files, the message needs no text of its own.
  pub fn message(&self, danger_is_alert: bool, has_files: bool) -> Result<WebhookMessage, PayloadError> {
    let payload = match &self.maybe_payload {
      Some(json) => serde_json::from_slice(json).map_err(|error| PayloadError::Json(error.to_string()))?,
      None => WebhookPayload {
        text: self.maybe_text.clone(),
        username: self.maybe_username.clone(),
        ..WebhookPayload::default()
      },
    };
    match render_webhook(&payload, danger_is_alert) {
      Err(PayloadError::Empty) if has_files => Ok(files_only_message(&payload, danger_is_alert)),
      rendered => rendered,
    }
  }

  fn has(&self, kind: FieldKind) -> bool {
    match kind {
      FieldKind::Payload => self.maybe_payload.is_some(),
      FieldKind::Text => self.maybe_text.is_some(),
      FieldKind::Username => self.maybe_username.is_some(),
      FieldKind::Ignored => false,
    }
  }

  fn set(&mut self, kind: FieldKind, value: Vec<u8>) -> Result<(), FormError> {
    match kind {
      FieldKind::Payload => self.maybe_payload = Some(value),
      FieldKind::Text => {
        self.maybe_text = Some(String::from_utf8(value).map_err(|_| FormError::NotText("text"))?)
      },
      FieldKind::Username => {
        self.maybe_username = Some(String::from_utf8(value).map_err(|_| FormError::NotText("username"))?);
      },
      FieldKind::Ignored => {},
    }
    Ok(())
  }
}

/// A message that is only files: the sender's name, if it gave one, and its alert flags.
fn files_only_message(payload: &WebhookPayload, danger_is_alert: bool) -> WebhookMessage {
  let text = payload
    .username
    .as_deref()
    .map(|name| name.replace('*', ""))
    .filter(|name| !name.trim().is_empty())
    .map(|name| format!("**{}**", name.trim()))
    .unwrap_or_default();
  let has_danger = payload.attachments.iter().any(|attachment| {
    attachment
      .color
      .as_deref()
      .is_some_and(|color| color.eq_ignore_ascii_case("danger"))
  });
  WebhookMessage {
    text,
    is_alert: payload.mention || (danger_is_alert && has_danger),
  }
}

/// The last segment of what the sender wrote (some send a path), without control characters, cut
/// to [`MAX_FILE_NAME_BYTES`]; `file` when nothing is left.
fn clean_file_name(raw: &str) -> String {
  let last = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
  let printable: String = last.chars().filter(|character| !character.is_control()).collect();
  let mut name = printable.trim();
  if name.len() > MAX_FILE_NAME_BYTES {
    let mut end = MAX_FILE_NAME_BYTES;
    while !name.is_char_boundary(end) {
      end -= 1;
    }
    name = &name[..end];
  }
  if name.is_empty() || name == "." || name == ".." {
    "file".to_owned()
  } else {
    name.to_owned()
  }
}

/// The part's `type/subtype` (parameters dropped, lower case), or one taken from the file name's
/// extension when the part has none (or only `application/octet-stream`).
fn media_type(maybe_content_type: Option<&str>, file_name: &str) -> String {
  let maybe_given = maybe_content_type
    .map(|value| {
      value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
    })
    .filter(|essence| is_media_type(essence) && essence != OPAQUE_MEDIA_TYPE);
  if let Some(given) = maybe_given {
    return given;
  }
  let maybe_extension = file_name
    .rsplit_once('.')
    .map(|(_, extension)| extension.to_ascii_lowercase());
  maybe_extension
    .and_then(|extension| {
      MEDIA_TYPES_BY_EXTENSION
        .iter()
        .find(|(known, _)| *known == extension)
        .map(|(_, media_type)| (*media_type).to_owned())
    })
    .unwrap_or_else(|| OPAQUE_MEDIA_TYPE.to_owned())
}

fn is_media_type(essence: &str) -> bool {
  let is_token = |part: &str| {
    !part.is_empty()
      && part
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&-^_.+".contains(&byte))
  };
  essence.len() <= MAX_MEDIA_TYPE_CHARS
    && essence
      .split_once('/')
      .is_some_and(|(kind, subtype)| is_token(kind) && is_token(subtype))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::multipart::tests::{CONTENT_TYPE, PartSpec, form};

  const LIMITS: FormLimits = FormLimits {
    max_files: 3,
    max_file_bytes: 64,
    max_field_bytes: 256,
  };

  mod fields_and_files {
    use super::*;

    #[test]
    fn the_payload_json_and_the_files_come_out() {
      let body = form(&[
        (
          "files[0]",
          Some("report.pdf"),
          Some("application/pdf"),
          b"%PDF".as_slice(),
        ),
        (
          "payload_json",
          None,
          None,
          br#"{"text":"Build 7 failed","username":"ci"}"#,
        ),
        ("files[1]", Some("dir/graph.png"), None, b"png"),
      ]);
      let (fields, files) = read(&body, LIMITS).unwrap();
      assert_eq!(
        files,
        [
          (file_info("report.pdf", "application/pdf"), b"%PDF".to_vec()),
          (file_info("graph.png", "image/png"), b"png".to_vec()),
        ]
      );
      let message = fields.message(false, true).unwrap();
      assert_eq!(message.text, "**ci**\nBuild 7 failed");
    }

    #[test]
    fn plain_text_fields_work_like_curl_sends_them() {
      let body = form(&[
        (
          "file",
          Some("app.log"),
          Some("text/plain; charset=utf-8"),
          b"line".as_slice(),
        ),
        ("text", None, None, b"Nightly logs"),
        ("username", None, None, b"cron"),
        ("unknown", None, None, b"ignored"),
      ]);
      let (fields, files) = read(&body, LIMITS).unwrap();
      assert_eq!(files[0].0, file_info("app.log", "text/plain"));
      assert_eq!(
        fields.message(false, true).unwrap().text,
        "**cron**\nNightly logs"
      );
    }

    #[test]
    fn a_message_of_files_only_needs_no_text() {
      let body = form(&[("file", Some("a.bin"), None, b"x".as_slice())]);
      let (fields, _) = read(&body, LIMITS).unwrap();
      assert_eq!(fields.message(false, true).unwrap().text, "");
      assert_eq!(fields.message(false, false), Err(PayloadError::Empty));
      let named = form(&[
        (
          "payload_json",
          None,
          None,
          br#"{"username":"ci","mention":true}"#.as_slice(),
        ),
        ("file", Some("a.bin"), None, b"x"),
      ]);
      let message = read(&named, LIMITS).unwrap().0.message(false, true).unwrap();
      assert_eq!((message.text.as_str(), message.is_alert), ("**ci**", true));
    }

    #[test]
    fn the_payload_may_come_as_a_file_and_only_the_first_counts() {
      let body = form(&[
        (
          "payload_json",
          Some("message.json"),
          None,
          br#"{"text":"first"}"#.as_slice(),
        ),
        ("payload", None, None, br#"{"text":"second"}"#),
      ]);
      let (fields, files) = read(&body, LIMITS).unwrap();
      assert!(files.is_empty());
      assert_eq!(fields.message(false, false).unwrap().text, "first");
    }

    #[test]
    fn file_names_and_media_types_are_cleaned() {
      assert_eq!(clean_file_name("C:\\Users\\x\\report.pdf"), "report.pdf");
      assert_eq!(clean_file_name("../../etc/passwd"), "passwd");
      assert_eq!(clean_file_name("bad\u{0}\nname.txt"), "badname.txt");
      assert_eq!(clean_file_name(".."), "file");
      assert_eq!(clean_file_name(""), "file");
      assert_eq!(clean_file_name(&"é".repeat(200)).len(), 254);
      assert_eq!(media_type(Some("IMAGE/PNG; x=y"), "a"), "image/png");
      assert_eq!(
        media_type(Some("application/octet-stream"), "shot.JPG"),
        "image/jpeg"
      );
      assert_eq!(media_type(Some("text/html\"<script>"), "a.txt"), "text/plain");
      assert_eq!(media_type(None, "archive.tar.gz"), OPAQUE_MEDIA_TYPE);
    }
  }

  mod limits {
    use super::*;

    #[test]
    fn a_file_too_many_is_refused() {
      let body = form(&[
        ("a", Some("1"), None, b"1".as_slice()),
        ("b", Some("2"), None, b"2"),
        ("c", Some("3"), None, b"3"),
        ("d", Some("4"), None, b"4"),
      ]);
      assert_eq!(
        read(&body, LIMITS).unwrap_err(),
        FormError::TooManyFiles { max: 3 }
      );
      let no_files = FormLimits {
        max_files: 0,
        ..LIMITS
      };
      assert_eq!(
        read(&form(&[("a", Some("1"), None, b"1".as_slice())]), no_files).unwrap_err(),
        FormError::TooManyFiles { max: 0 }
      );
    }

    #[test]
    fn a_file_over_the_size_limit_is_refused_while_it_arrives() {
      let body = form(&[("file", Some("big"), None, [0u8; 65].as_slice())]);
      assert_eq!(
        read(&body, LIMITS).unwrap_err(),
        FormError::FileTooLarge { max: 64 }
      );
      let exact = form(&[("file", Some("ok"), None, [0u8; 64].as_slice())]);
      assert!(read(&exact, LIMITS).is_ok());
    }

    #[test]
    fn fields_are_limited_in_size_and_number() {
      let big = form(&[("unknown", None, None, [b'x'; 257].as_slice())]);
      assert_eq!(
        read(&big, LIMITS).unwrap_err(),
        FormError::FieldsTooLarge { max: 256 }
      );
      let names: Vec<String> = (0..=MAX_FORM_FIELDS).map(|index| format!("f{index}")).collect();
      let many: Vec<PartSpec<'_>> = names
        .iter()
        .map(|name| (name.as_str(), None, None, b"".as_slice()))
        .collect();
      assert_eq!(read(&form(&many), LIMITS).unwrap_err(), FormError::TooManyFields);
    }

    #[test]
    fn text_must_be_utf8() {
      let body = form(&[("text", None, None, [0xff, 0xfe].as_slice())]);
      assert_eq!(read(&body, LIMITS).unwrap_err(), FormError::NotText("text"));
    }
  }

  fn file_info(file_name: &str, media_type: &str) -> FileInfo {
    FileInfo {
      file_name: file_name.into(),
      media_type: media_type.into(),
    }
  }

  type Files = Vec<(FileInfo, Vec<u8>)>;

  /// Reads `body` in small pieces, collecting the files in memory.
  fn read(body: &[u8], limits: FormLimits) -> Result<(FormFields, Files), FormError> {
    let mut reader = FormReader::new(CONTENT_TYPE, limits)?;
    let mut files: Files = Vec::new();
    for chunk in body.chunks(7) {
      for action in reader.feed(chunk)? {
        match action {
          FileAction::Start(info) => files.push((info, Vec::new())),
          FileAction::Data(bytes) => files.last_mut().unwrap().1.extend(bytes),
          FileAction::End => {},
        }
      }
    }
    Ok((reader.finish()?, files))
  }
}
