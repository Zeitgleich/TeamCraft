//! `multipart/form-data` (RFC 7578, RFC 2046 §5.1) as it arrives, for webhooks with files
//! (docs/bridge.md, "Files").
//!
//! A push parser without I/O: the endpoint feeds the body chunk by chunk, in whatever pieces the
//! network delivers, and gets back where parts begin and end and the bytes of their bodies. It
//! never holds more than the chunk it was given plus a delimiter's length of a part's body, or one
//! part's headers (at most [`MAX_HEADER_BYTES`]): a large file never sits in memory whole, and
//! hostile input can't make it buffer more.

/// Longest header block of one part. Real ones (`Content-Disposition`, `Content-Type`) take a few
/// hundred bytes.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
/// Bytes before the first boundary (the preamble, which carries nothing) skipped at most.
const MAX_PREAMBLE_BYTES: usize = 8 * 1024;
/// Spaces and tabs allowed between a boundary and its line break ("transport padding").
const MAX_PADDING_BYTES: usize = 64;
/// RFC 2046: a boundary has 1 to 70 characters.
const MAX_BOUNDARY_CHARS: usize = 70;
const CRLF: &[u8] = b"\r\n";
const DASHES: &[u8] = b"--";
/// The empty line after a part's headers.
const HEADER_END: &[u8] = b"\r\n\r\n";

/// Reads one `multipart/form-data` body; see the module documentation.
#[derive(Debug)]
pub struct MultipartParser {
  /// CRLF, two dashes and the boundary: what ends the preamble and every part.
  delimiter: Vec<u8>,
  state: State,
  /// Bytes not consumed yet: the end of a body that may be the start of a delimiter, or headers
  /// that are not complete yet.
  buffer: Vec<u8>,
  preamble_bytes: usize,
  /// How far an unfinished header block was searched for its end already, so a block that arrives
  /// in many small pieces is read once, not again with every piece.
  header_scanned: usize,
}

/// What a chunk of the body contained, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
  /// A part begins.
  Part(PartHeaders),
  /// Bytes of the current part's body (a part's body may come in many pieces).
  Data(Vec<u8>),
  /// The current part is complete.
  PartEnd,
}

/// What a part's headers say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartHeaders {
  /// The form field (`name` of `Content-Disposition`).
  pub name: String,
  /// Set for files (`filename`, or the RFC 8187 `filename*`), as the sender wrote it.
  pub maybe_file_name: Option<String>,
  pub maybe_content_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
  /// Before the first boundary.
  Preamble,
  /// Right after a boundary: two dashes end the body, padding and a line break start a part.
  AfterBoundary,
  Headers,
  Body,
  /// After the closing boundary; the epilogue is ignored.
  Done,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MultipartError {
  #[error("the content type names no valid multipart boundary")]
  InvalidBoundary,
  #[error("no boundary in the first {MAX_PREAMBLE_BYTES} bytes of the body")]
  NoBoundary,
  #[error("a boundary is followed by something else than a part or the end of the body")]
  MalformedBoundary,
  #[error("a part's headers are longer than {MAX_HEADER_BYTES} bytes")]
  HeadersTooLarge,
  #[error("a part's headers are malformed: {0}")]
  MalformedHeaders(&'static str),
  #[error("the body ends before its closing boundary")]
  Truncated,
}

impl MultipartParser {
  /// A parser for the boundary of a `Content-Type: multipart/form-data; boundary=...` header.
  pub fn new(content_type: &str) -> Result<Self, MultipartError> {
    let boundary = boundary(content_type).ok_or(MultipartError::InvalidBoundary)?;
    let mut delimiter = Vec::with_capacity(boundary.len() + 4);
    delimiter.extend_from_slice(CRLF);
    delimiter.extend_from_slice(DASHES);
    delimiter.extend_from_slice(boundary.as_bytes());
    Ok(Self {
      delimiter,
      state: State::Preamble,
      // The first boundary may open the body without a line break in front: one is assumed, so
      // every boundary is found the same way.
      buffer: CRLF.to_vec(),
      preamble_bytes: 0,
      header_scanned: 0,
    })
  }

  /// Takes the next piece of the body and returns what it completed.
  pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<Event>, MultipartError> {
    let mut buffer = std::mem::take(&mut self.buffer);
    buffer.extend_from_slice(chunk);
    let mut events = Vec::new();
    let consumed = self.consume(&buffer, &mut events);
    if let Ok(consumed) = consumed {
      buffer.drain(..consumed);
    }
    self.buffer = buffer;
    consumed.map(|_| events)
  }

  /// Checks that the body ended with its closing boundary.
  pub fn finish(&self) -> Result<(), MultipartError> {
    if self.state == State::Done {
      Ok(())
    } else {
      Err(MultipartError::Truncated)
    }
  }

  /// Whether the closing boundary has come; what follows it is ignored.
  pub fn is_done(&self) -> bool {
    self.state == State::Done
  }

  /// Bytes held between two calls of [`Self::feed`].
  pub fn buffered_bytes(&self) -> usize {
    self.buffer.len()
  }

  /// Most bytes [`Self::buffered_bytes`] can reach between two calls: a partial delimiter, or a
  /// header block that is not complete yet.
  pub fn max_buffered_bytes(&self) -> usize {
    (MAX_HEADER_BYTES + HEADER_END.len() - 1)
      .max(MAX_PADDING_BYTES + CRLF.len())
      .max(self.delimiter.len())
  }

  /// Reads as far as `buffer` allows; returns how many of its bytes are done with.
  fn consume(&mut self, buffer: &[u8], events: &mut Vec<Event>) -> Result<usize, MultipartError> {
    let mut position = 0;
    while position < buffer.len() {
      let rest = &buffer[position..];
      let consumed = match self.state {
        State::Preamble => self.skip_preamble(rest)?,
        State::AfterBoundary => self.read_boundary_end(rest)?,
        State::Headers => self.read_headers(rest, events)?,
        State::Body => self.read_body(rest, events),
        State::Done => rest.len(),
      };
      if consumed == 0 {
        break;
      }
      position += consumed;
    }
    Ok(position)
  }

  /// Skips everything up to the first boundary. The limit counts the same however the body is cut.
  fn skip_preamble(&mut self, rest: &[u8]) -> Result<usize, MultipartError> {
    let (skipped, maybe_found) = match find(rest, &self.delimiter) {
      Some(at) => (at, Some(at + self.delimiter.len())),
      None => (rest.len().saturating_sub(self.delimiter.len() - 1), None),
    };
    self.preamble_bytes += skipped;
    if self.preamble_bytes > MAX_PREAMBLE_BYTES {
      return Err(MultipartError::NoBoundary);
    }
    if let Some(consumed) = maybe_found {
      self.state = State::AfterBoundary;
      return Ok(consumed);
    }
    Ok(skipped)
  }

  /// After a boundary: `--` closes the body; otherwise padding and a line break open a part.
  fn read_boundary_end(&mut self, rest: &[u8]) -> Result<usize, MultipartError> {
    if rest.len() < DASHES.len() {
      return Ok(0);
    }
    if rest.starts_with(DASHES) {
      self.state = State::Done;
      return Ok(DASHES.len());
    }
    let padding = rest
      .iter()
      .take_while(|byte| matches!(byte, b' ' | b'\t'))
      .count();
    if padding > MAX_PADDING_BYTES {
      return Err(MultipartError::MalformedBoundary);
    }
    let after = &rest[padding..];
    if after.len() < CRLF.len() {
      return Ok(0);
    }
    if !after.starts_with(CRLF) {
      return Err(MultipartError::MalformedBoundary);
    }
    self.state = State::Headers;
    self.header_scanned = 0;
    Ok(padding + CRLF.len())
  }

  /// A part's headers, up to the empty line that ends them.
  fn read_headers(&mut self, rest: &[u8], events: &mut Vec<Event>) -> Result<usize, MultipartError> {
    // The block starts where `rest` starts until it is complete; what was searched already can't
    // hold its end, apart from a part of the end cut off by the piece before.
    let from = self.header_scanned.min(rest.len());
    let (block, consumed) = if rest.starts_with(CRLF) {
      (&rest[..0], CRLF.len())
    } else if let Some(end) = find(&rest[from..], HEADER_END).map(|end| from + end) {
      (&rest[..end], end + HEADER_END.len())
    } else if rest.len() > MAX_HEADER_BYTES + HEADER_END.len() - 1 {
      // Even if the end came next, the headers before it would be too long.
      return Err(MultipartError::HeadersTooLarge);
    } else {
      self.header_scanned = rest.len().saturating_sub(HEADER_END.len() - 1);
      return Ok(0);
    };
    if block.len() > MAX_HEADER_BYTES {
      return Err(MultipartError::HeadersTooLarge);
    }
    events.push(Event::Part(parse_headers(block)?));
    self.state = State::Body;
    Ok(consumed)
  }

  /// A part's body up to the next delimiter. Without one in sight, everything but what could be
  /// the start of a delimiter goes out.
  fn read_body(&mut self, rest: &[u8], events: &mut Vec<Event>) -> usize {
    if let Some(at) = find(rest, &self.delimiter) {
      if at > 0 {
        events.push(Event::Data(rest[..at].to_vec()));
      }
      events.push(Event::PartEnd);
      self.state = State::AfterBoundary;
      return at + self.delimiter.len();
    }
    let safe = rest.len().saturating_sub(self.delimiter.len() - 1);
    if safe > 0 {
      events.push(Event::Data(rest[..safe].to_vec()));
    }
    safe
  }
}

/// The boundary of a `multipart/form-data` content type: 1 to 70 of the characters RFC 2046
/// allows, quoted or not.
pub fn boundary(content_type: &str) -> Option<String> {
  let mut parts = split_parameters(content_type);
  let media_type = parts.next()?;
  if !media_type.trim().eq_ignore_ascii_case("multipart/form-data") {
    return None;
  }
  let value = parts.find_map(|parameter| {
    let (name, value) = parameter.split_once('=')?;
    name
      .trim()
      .eq_ignore_ascii_case("boundary")
      .then(|| unquote(value.trim()))
  })?;
  let is_valid = (1..=MAX_BOUNDARY_CHARS).contains(&value.len())
    && value.bytes().all(is_boundary_char)
    && !value.ends_with(' ');
  is_valid.then_some(value)
}

/// RFC 2046 `bchars`.
fn is_boundary_char(byte: u8) -> bool {
  byte.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&byte)
}

/// The headers of one part: `Content-Disposition: form-data; name=...` is required.
fn parse_headers(block: &[u8]) -> Result<PartHeaders, MultipartError> {
  let mut maybe_disposition = None;
  let mut maybe_content_type = None;
  for line in block.split(|byte| *byte == b'\n') {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.is_empty() {
      continue;
    }
    let line = String::from_utf8_lossy(line);
    let (name, value) = line
      .split_once(':')
      .ok_or(MultipartError::MalformedHeaders("a header line without a colon"))?;
    let name = name.trim();
    if name.eq_ignore_ascii_case("content-disposition") && maybe_disposition.is_none() {
      maybe_disposition = Some(value.trim().to_owned());
    } else if name.eq_ignore_ascii_case("content-type") && maybe_content_type.is_none() {
      maybe_content_type = Some(value.trim().to_owned());
    }
  }
  let disposition = maybe_disposition.ok_or(MultipartError::MalformedHeaders("no Content-Disposition"))?;
  let (name, maybe_file_name) = parse_disposition(&disposition)?;
  Ok(PartHeaders {
    name,
    maybe_file_name,
    maybe_content_type,
  })
}

/// `form-data; name="field"; filename="report.pdf"` to the field name and the file name.
fn parse_disposition(value: &str) -> Result<(String, Option<String>), MultipartError> {
  let mut parts = split_parameters(value);
  let kind = parts.next().unwrap_or_default();
  if !kind.trim().eq_ignore_ascii_case("form-data") {
    return Err(MultipartError::MalformedHeaders(
      "the disposition is not form-data",
    ));
  }
  let mut maybe_name = None;
  let mut maybe_file_name = None;
  let mut maybe_extended_file_name = None;
  for parameter in parts {
    let Some((key, raw)) = parameter.split_once('=') else {
      continue;
    };
    let key = key.trim();
    let raw = raw.trim();
    if key.eq_ignore_ascii_case("name") && maybe_name.is_none() {
      maybe_name = Some(unquote(raw));
    } else if key.eq_ignore_ascii_case("filename") && maybe_file_name.is_none() {
      maybe_file_name = Some(unquote(raw));
    } else if key.eq_ignore_ascii_case("filename*") && maybe_extended_file_name.is_none() {
      maybe_extended_file_name = decode_extended(raw);
    }
  }
  let name = maybe_name.ok_or(MultipartError::MalformedHeaders("a part without a name"))?;
  Ok((name, maybe_extended_file_name.or(maybe_file_name)))
}

/// Splits at semicolons outside quoted strings.
fn split_parameters(value: &str) -> impl Iterator<Item = &str> {
  let mut start = 0;
  let mut is_quoted = false;
  let mut is_escaped = false;
  let mut cuts = Vec::new();
  for (index, character) in value.char_indices() {
    match character {
      _ if is_escaped => is_escaped = false,
      '\\' if is_quoted => is_escaped = true,
      '"' => is_quoted = !is_quoted,
      ';' if !is_quoted => {
        cuts.push(&value[start..index]);
        start = index + 1;
      },
      _ => {},
    }
  }
  cuts.push(&value[start..]);
  cuts.into_iter()
}

/// A token as it is, or a quoted string without its quotes and backslash escapes.
fn unquote(value: &str) -> String {
  let Some(inner) = value
    .strip_prefix('"')
    .map(|inner| inner.strip_suffix('"').unwrap_or(inner))
  else {
    return value.to_owned();
  };
  let mut out = String::with_capacity(inner.len());
  let mut characters = inner.chars();
  while let Some(character) = characters.next() {
    if character == '\\'
      && let Some(escaped) = characters.next()
    {
      out.push(escaped);
    } else {
      out.push(character);
    }
  }
  out
}

/// RFC 8187 `UTF-8''percent-encoded` (other charsets are not decoded; `filename` is used then).
fn decode_extended(raw: &str) -> Option<String> {
  let (charset, rest) = raw.split_once('\'')?;
  let (_language, encoded) = rest.split_once('\'')?;
  if !charset.eq_ignore_ascii_case("utf-8") {
    return None;
  }
  let bytes = encoded.as_bytes();
  let mut decoded = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    if bytes[index] == b'%' {
      let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
      decoded.push(u8::from_str_radix(hex, 16).ok()?);
      index += 3;
    } else {
      decoded.push(bytes[index]);
      index += 1;
    }
  }
  String::from_utf8(decoded).ok()
}

/// Where `needle` first occurs in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
  let (&first, rest) = needle.split_first()?;
  let mut from = 0;
  while let Some(offset) = haystack.get(from..)?.iter().position(|byte| *byte == first) {
    let at = from + offset;
    let end = at + needle.len();
    if end > haystack.len() {
      return None;
    }
    if &haystack[at + 1..end] == rest {
      return Some(at);
    }
    from = at + 1;
  }
  None
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  pub(crate) const CONTENT_TYPE: &str = "multipart/form-data; boundary=XyZ";

  mod parsing {
    use super::*;

    #[test]
    fn fields_and_files_come_out_with_their_headers_and_bytes() {
      let body = form(&[
        ("payload_json", None, None, br#"{"text":"hi"}"#.as_slice()),
        (
          "files[0]",
          Some("report.pdf"),
          Some("application/pdf"),
          b"%PDF-1.7\r\n--Xy",
        ),
      ]);
      let parts = parse_whole(&body).unwrap();
      assert_eq!(parts.len(), 2);
      assert_eq!(parts[0].0.name, "payload_json");
      assert_eq!(parts[0].0.maybe_file_name, None);
      assert_eq!(parts[0].1, br#"{"text":"hi"}"#);
      assert_eq!(parts[1].0.maybe_file_name.as_deref(), Some("report.pdf"));
      assert_eq!(parts[1].0.maybe_content_type.as_deref(), Some("application/pdf"));
      assert_eq!(
        parts[1].1, b"%PDF-1.7\r\n--Xy",
        "a boundary prefix in the data stays data"
      );
    }

    #[test]
    fn chunk_boundaries_do_not_matter() {
      let body = form(&[
        ("text", None, None, b"deploy done".as_slice()),
        ("file", Some("a.bin"), None, &[13, 10, 45, 45, 88, 0, 255, 13]),
      ]);
      let whole = parse_whole(&body).unwrap();
      for size in [1, 2, 3, 5, 7, 64] {
        assert_eq!(parse_in_chunks(&body, size).unwrap(), whole, "chunks of {size}");
      }
    }

    #[test]
    fn preamble_epilogue_padding_and_quoted_names_are_understood() {
      let body = b"ignored preamble\r\n--XyZ  \t\r\nContent-Disposition: form-data; name=\"a;b\"; filename=\"c\\\"d.txt\"\r\n\r\nx\r\n--XyZ--\r\nepilogue";
      let parts = parse_whole(body).unwrap();
      assert_eq!(parts[0].0.name, "a;b");
      assert_eq!(parts[0].0.maybe_file_name.as_deref(), Some("c\"d.txt"));
      assert_eq!(parts[0].1, b"x");
    }

    #[test]
    fn the_extended_file_name_wins() {
      let body = b"--XyZ\r\nContent-Disposition: form-data; name=file; filename=\"plain.txt\"; filename*=UTF-8''%C3%BCber.txt\r\n\r\n\r\n--XyZ--";
      let parts = parse_whole(body).unwrap();
      assert_eq!(parts[0].0.maybe_file_name.as_deref(), Some("über.txt"));
      assert!(parts[0].1.is_empty());
    }

    #[test]
    fn boundaries_are_read_from_the_content_type() {
      assert_eq!(
        boundary("multipart/form-data; boundary=abc").as_deref(),
        Some("abc")
      );
      assert_eq!(
        boundary("Multipart/Form-Data;charset=utf-8; BOUNDARY=\"a b'c\"").as_deref(),
        Some("a b'c")
      );
      for bad in [
        "multipart/form-data",
        "multipart/form-data; boundary=",
        "multipart/mixed; boundary=abc",
        "multipart/form-data; boundary=\"ends with space \"",
        "multipart/form-data; boundary=has\"quote",
        &format!("multipart/form-data; boundary={}", "a".repeat(71)),
      ] {
        assert_eq!(boundary(bad), None, "{bad}");
        assert!(MultipartParser::new(bad).is_err());
      }
    }
  }

  mod malformed {
    use super::*;

    #[test]
    fn an_empty_or_truncated_body_is_refused() {
      assert_eq!(parse_whole(b""), Err(MultipartError::Truncated));
      let body = form(&[("text", None, None, b"x".as_slice())]);
      for cut in [5, body.len() / 2, body.len() - 3] {
        assert_eq!(
          parse_whole(&body[..cut]),
          Err(MultipartError::Truncated),
          "cut at {cut}"
        );
      }
    }

    #[test]
    fn a_body_without_the_boundary_is_refused_early() {
      let mut parser = MultipartParser::new(CONTENT_TYPE).unwrap();
      let junk = vec![b'a'; MAX_PREAMBLE_BYTES * 2];
      assert_eq!(parser.feed(&junk), Err(MultipartError::NoBoundary));
    }

    #[test]
    fn garbage_after_a_boundary_is_refused() {
      assert_eq!(
        parse_whole(b"--XyZjunk\r\n\r\n--XyZ--"),
        Err(MultipartError::MalformedBoundary)
      );
      let padded = format!("--XyZ{}\r\n", " ".repeat(MAX_PADDING_BYTES + 1));
      assert_eq!(
        parse_whole(padded.as_bytes()),
        Err(MultipartError::MalformedBoundary)
      );
    }

    #[test]
    fn parts_without_a_disposition_or_a_name_are_refused() {
      for headers in [
        "",
        "Content-Type: text/plain",
        "Content-Disposition: attachment; name=x",
        "Content-Disposition: form-data; filename=x",
        "no colon here",
      ] {
        let body = format!("--XyZ\r\n{headers}\r\n\r\nx\r\n--XyZ--");
        assert!(
          matches!(
            parse_whole(body.as_bytes()),
            Err(MultipartError::MalformedHeaders(_))
          ),
          "{headers}"
        );
      }
    }
  }

  mod oversized {
    use super::*;

    #[test]
    fn endless_headers_are_refused_without_buffering_them() {
      let mut parser = MultipartParser::new(CONTENT_TYPE).unwrap();
      parser
        .feed(b"--XyZ\r\nContent-Disposition: form-data; name=x; filename=\"")
        .unwrap();
      let mut result = Ok(Vec::new());
      for _ in 0..100 {
        result = parser.feed(&[b'a'; 1024]);
        if result.is_err() {
          break;
        }
        assert!(parser.buffered_bytes() <= parser.max_buffered_bytes());
      }
      assert_eq!(result, Err(MultipartError::HeadersTooLarge));
    }

    #[test]
    fn a_large_body_goes_through_with_bounded_memory() {
      let mut parser = MultipartParser::new(CONTENT_TYPE).unwrap();
      let mut events = parser
        .feed(b"--XyZ\r\nContent-Disposition: form-data; name=file; filename=big\r\n\r\n")
        .unwrap();
      let mut received = 0;
      for _ in 0..1000 {
        for event in parser.feed(&[7; 4096]).unwrap() {
          if let Event::Data(bytes) = event {
            received += bytes.len();
          }
        }
        assert!(parser.buffered_bytes() <= parser.max_buffered_bytes());
      }
      events.extend(parser.feed(b"\r\n--XyZ--").unwrap());
      parser.finish().unwrap();
      for event in events {
        if let Event::Data(bytes) = event {
          received += bytes.len();
        }
      }
      assert_eq!(received, 4096 * 1000);
    }

    #[test]
    fn many_parts_are_all_reported() {
      let parts: Vec<(String, Vec<u8>)> = (0..500).map(|index| (format!("f{index}"), vec![1])).collect();
      let fields: Vec<PartSpec<'_>> = parts
        .iter()
        .map(|(name, data)| (name.as_str(), None, None, data.as_slice()))
        .collect();
      assert_eq!(parse_whole(&form(&fields)).unwrap().len(), 500);
    }

    #[test]
    fn limits_hold_the_same_however_the_body_is_cut() {
      let long_preamble = [
        vec![b'p'; MAX_PREAMBLE_BYTES + 1],
        form(&[("a", None, None, b"x")]),
      ]
      .concat();
      let headers = format!(
        "--XyZ\r\nContent-Disposition: form-data; name=a\r\nX-Pad: {}\r\n\r\nx\r\n--XyZ--",
        "p".repeat(MAX_HEADER_BYTES - 50)
      );
      for body in [long_preamble, headers.into_bytes()] {
        let whole = parse_whole(&body);
        for size in [1, 2, 3, 4, 1000] {
          assert_eq!(parse_in_chunks(&body, size), whole, "chunks of {size}");
        }
      }
    }
  }

  /// Bodies changed at a few places read the same in any pieces, ok or not (the fuzz target
  /// `bridge_multipart` does this at scale).
  #[test]
  fn changed_bodies_read_the_same_however_they_are_cut() {
    let original = form(&[
      ("payload_json", None, None, br#"{"text":"x"}"#.as_slice()),
      ("files[0]", Some("a.txt"), Some("text/plain"), b"--XyZ\r\n--Xy"),
    ]);
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for _ in 0..500 {
      let mut body = original.clone();
      for _ in 0..3 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let at = (state % body.len() as u64) as usize;
        body[at] = b"\r\n-XyZ \"a"[(state >> 32) as usize % 9];
      }
      let whole = parse_whole(&body);
      for size in [1, 5, 64] {
        assert_eq!(
          parse_in_chunks(&body, size),
          whole,
          "{body:?} in chunks of {size}"
        );
      }
    }
  }

  /// One part of a test body: name, file name, content type, data.
  pub(crate) type PartSpec<'a> = (&'a str, Option<&'a str>, Option<&'a str>, &'a [u8]);

  /// A body with the given parts.
  pub(crate) fn form(parts: &[PartSpec<'_>]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, maybe_file_name, maybe_content_type, data) in parts {
      body.extend_from_slice(b"--XyZ\r\n");
      let mut disposition = format!("Content-Disposition: form-data; name=\"{name}\"");
      if let Some(file_name) = maybe_file_name {
        disposition.push_str(&format!("; filename=\"{file_name}\""));
      }
      body.extend_from_slice(disposition.as_bytes());
      body.extend_from_slice(b"\r\n");
      if let Some(content_type) = maybe_content_type {
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
      }
      body.extend_from_slice(b"\r\n");
      body.extend_from_slice(data);
      body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(b"--XyZ--\r\n");
    body
  }

  type Parts = Vec<(PartHeaders, Vec<u8>)>;

  fn parse_whole(body: &[u8]) -> Result<Parts, MultipartError> {
    parse_in_chunks(body, body.len().max(1))
  }

  fn parse_in_chunks(body: &[u8], size: usize) -> Result<Parts, MultipartError> {
    let mut parser = MultipartParser::new(CONTENT_TYPE)?;
    let mut parts: Parts = Vec::new();
    for chunk in body.chunks(size) {
      for event in parser.feed(chunk)? {
        match event {
          Event::Part(headers) => parts.push((headers, Vec::new())),
          Event::Data(bytes) => parts.last_mut().unwrap().1.extend(bytes),
          Event::PartEnd => {},
        }
      }
    }
    parser.finish()?;
    Ok(parts)
  }
}
