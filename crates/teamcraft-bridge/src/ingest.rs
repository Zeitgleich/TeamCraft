//! Streaming intake of log lines for `POST /ingest/{token}` (docs/bridge.md, "Log lines").
//!
//! A producer such as `journalctl -f | curl -T -` keeps one request open for as long as it runs, so
//! the body is never read whole: [`LineSplitter`] cuts the arriving chunks into complete lines, and
//! the endpoint hands each batch of lines to the worker as it arrives.

use std::time::Duration;

use crate::config::BridgeConfig;

/// How the log endpoint reads a body.
#[derive(Debug, Clone, Copy)]
pub struct IngestSettings {
  /// The longest line a producer may send (`listen.max_body_bytes`). There is no limit on the
  /// body as a whole: a stream is as long as its producer runs.
  pub max_line_bytes: usize,
  /// A stream that sends nothing for this long is cut off; `None` waits for as long as the
  /// connection stands (`listen.ingest_idle_timeout_secs = 0`).
  pub maybe_idle_timeout: Option<Duration>,
}

impl IngestSettings {
  pub fn from_config(config: &BridgeConfig) -> Self {
    let idle_secs = config.listen.ingest_idle_timeout_secs;
    Self {
      max_line_bytes: config.listen.max_body_bytes,
      maybe_idle_timeout: (idle_secs > 0).then(|| Duration::from_secs(idle_secs)),
    }
  }
}

/// A line without a line break for longer than the limit.
#[derive(Debug, thiserror::Error)]
#[error("a line is longer than {max_line_bytes} bytes")]
pub struct LineTooLong {
  pub max_line_bytes: usize,
}

/// Cuts a byte stream into lines. Chunks end anywhere, also inside a line or a multi-byte
/// character, so the incomplete tail of a chunk waits for the next one.
#[derive(Debug)]
pub struct LineSplitter {
  tail: Vec<u8>,
  max_line_bytes: usize,
}

impl LineSplitter {
  pub fn new(max_line_bytes: usize) -> Self {
    Self {
      tail: Vec::new(),
      max_line_bytes,
    }
  }

  /// Adds a chunk; returns the complete lines it finished (with their line breaks, possibly
  /// none), and keeps the rest back.
  pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<u8>, LineTooLong> {
    self.tail.extend_from_slice(chunk);
    let Some(last_break) = self.tail.iter().rposition(|byte| *byte == b'\n') else {
      return self.check_tail().map(|()| Vec::new());
    };
    let rest = self.tail.split_off(last_break + 1);
    let complete = std::mem::replace(&mut self.tail, rest);
    if complete
      .split(|byte| *byte == b'\n')
      .any(|line| line.len() > self.max_line_bytes)
    {
      return Err(self.too_long());
    }
    self.check_tail()?;
    Ok(complete)
  }

  /// The end of the stream: a last line without a line break is a line too.
  pub fn finish(self) -> Vec<u8> {
    self.tail
  }

  fn check_tail(&self) -> Result<(), LineTooLong> {
    if self.tail.len() > self.max_line_bytes {
      return Err(self.too_long());
    }
    Ok(())
  }

  fn too_long(&self) -> LineTooLong {
    LineTooLong {
      max_line_bytes: self.max_line_bytes,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_line_split_over_chunks_comes_out_whole() {
    let mut splitter = LineSplitter::new(64);
    assert_eq!(splitter.push(b"first\nsec").unwrap(), b"first\n");
    assert_eq!(splitter.push(b"ond").unwrap(), b"");
    assert_eq!(splitter.push(b" line\nthi").unwrap(), b"second line\n");
    assert_eq!(splitter.finish(), b"thi");
  }

  #[test]
  fn a_character_split_over_chunks_is_not_damaged() {
    let bytes = "gr\u{fc}n\n".as_bytes();
    let mut splitter = LineSplitter::new(64);
    // The two bytes of the umlaut land in different chunks.
    assert_eq!(splitter.push(&bytes[..3]).unwrap(), b"");
    let line = splitter.push(&bytes[3..]).unwrap();
    assert_eq!(String::from_utf8(line).unwrap(), "gr\u{fc}n\n");
  }

  #[test]
  fn many_lines_in_one_chunk_come_out_together() {
    let mut splitter = LineSplitter::new(64);
    assert_eq!(splitter.push(b"1\n2\n3\n").unwrap(), b"1\n2\n3\n");
    assert!(splitter.finish().is_empty());
  }

  mod limits {
    use super::*;

    #[test]
    fn a_line_over_the_limit_is_refused_while_it_arrives() {
      let mut splitter = LineSplitter::new(8);
      assert!(splitter.push(b"12345678").is_ok());
      assert!(splitter.push(b"9").is_err());
    }

    #[test]
    fn a_long_line_between_short_ones_is_refused() {
      let mut splitter = LineSplitter::new(4);
      assert!(splitter.push(b"ok\n123456789\nok\n").is_err());
    }

    #[test]
    fn a_long_unfinished_line_behind_short_ones_is_refused() {
      let mut splitter = LineSplitter::new(4);
      assert!(splitter.push(b"ok\n123456789").is_err());
    }

    #[test]
    fn the_stream_is_not_limited_as_a_whole() {
      let mut splitter = LineSplitter::new(8);
      for _ in 0..10_000 {
        assert_eq!(splitter.push(b"1234567\n").unwrap().len(), 8);
      }
    }
  }
}
