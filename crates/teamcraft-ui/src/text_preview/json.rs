//! JSON formatted on demand: checked by `serde_json`, then indented token by token. Nothing is
//! parsed into values, so keys keep their order and numbers their exact spelling, and the result
//! has the same content as the file.
//!
//! Indentation grows with nesting, so a small hostile file (thousands of `[`) could format into
//! gigabytes: nesting and the formatted size are bounded.

/// One level of indentation.
const INDENT: &str = "  ";
/// Nesting deeper than this is not formatted (as deep as `serde_json` parses into values).
pub const MAX_DEPTH: usize = 128;
/// The formatted text at most.
pub const MAX_FORMATTED_BYTES: usize = 64 * 1024 * 1024;

/// Why a file is not formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JsonFormatError {
  /// Where it stops being JSON (1-based, as editors count).
  #[error("not valid JSON at line {line}, column {column}")]
  Invalid { line: usize, column: usize },
  #[error("JSON nested deeper than {MAX_DEPTH} levels")]
  TooDeep,
  #[error("formatted JSON larger than {MAX_FORMATTED_BYTES} bytes")]
  TooLarge,
}

/// `text` indented by two spaces per level, one value per line.
pub fn pretty(text: &str) -> Result<String, JsonFormatError> {
  if let Err(error) = serde_json::from_str::<serde::de::IgnoredAny>(text) {
    return Err(JsonFormatError::Invalid {
      line: error.line(),
      column: error.column(),
    });
  }
  reindent(text, MAX_FORMATTED_BYTES)
}

/// Indents valid JSON into at most `max_bytes`. Structure and whitespace outside strings are ASCII,
/// so every cut is at a character boundary.
fn reindent(text: &str, max_bytes: usize) -> Result<String, JsonFormatError> {
  let bytes = text.as_bytes();
  let mut out = String::with_capacity((text.len() + text.len() / 2).min(max_bytes));
  let mut depth = 0usize;
  let mut at = 0;
  while at < bytes.len() {
    match bytes[at] {
      b'"' => {
        let end = string_end(bytes, at);
        out.push_str(text.get(at..end).unwrap_or_default());
        at = end;
        continue;
      },
      open @ (b'{' | b'[') => {
        out.push(char::from(open));
        let next = skip_whitespace(bytes, at + 1);
        if let Some(&close @ (b'}' | b']')) = bytes.get(next) {
          out.push(char::from(close));
          at = next + 1;
          continue;
        }
        depth += 1;
        if depth > MAX_DEPTH {
          return Err(JsonFormatError::TooDeep);
        }
        new_line(&mut out, depth);
      },
      close @ (b'}' | b']') => {
        depth = depth.saturating_sub(1);
        new_line(&mut out, depth);
        out.push(char::from(close));
      },
      b',' => {
        out.push(',');
        new_line(&mut out, depth);
      },
      b':' => out.push_str(": "),
      b' ' | b'\t' | b'\n' | b'\r' => {},
      _ => {
        let end = token_end(bytes, at);
        out.push_str(text.get(at..end).unwrap_or_default());
        at = end;
        continue;
      },
    }
    if out.len() > max_bytes {
      return Err(JsonFormatError::TooLarge);
    }
    at += 1;
  }
  Ok(out)
}

fn new_line(out: &mut String, depth: usize) {
  out.push('\n');
  for _ in 0..depth {
    out.push_str(INDENT);
  }
}

/// After the closing quote of the string starting at `start`.
fn string_end(bytes: &[u8], start: usize) -> usize {
  let mut at = start + 1;
  while at < bytes.len() {
    match bytes[at] {
      b'\\' => at += 2,
      b'"' => return at + 1,
      _ => at += 1,
    }
  }
  bytes.len()
}

fn skip_whitespace(bytes: &[u8], start: usize) -> usize {
  start
    + bytes[start.min(bytes.len())..]
      .iter()
      .take_while(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
      .count()
}

/// The end of a number, `true`, `false` or `null`.
fn token_end(bytes: &[u8], start: usize) -> usize {
  start
    + bytes[start..]
      .iter()
      .take_while(|byte| {
        !matches!(
          byte,
          b',' | b':' | b'{' | b'}' | b'[' | b']' | b'"' | b' ' | b'\t' | b'\n' | b'\r'
        )
      })
      .count()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn objects_and_arrays_get_a_line_per_value() {
    assert_eq!(
      pretty(r#"{"b":1,"a":[true,null,{"x":"y"}],"e":{},"f":[ ]}"#).unwrap(),
      "{\n  \"b\": 1,\n  \"a\": [\n    true,\n    null,\n    {\n      \"x\": \"y\"\n    }\n  ],\n  \"e\": {},\n  \"f\": []\n}"
    );
  }

  #[test]
  fn strings_numbers_and_key_order_stay_exactly_as_they_were() {
    let text = r#"{"z":"a, b:{c} [d] \"q\" \\","a":1.50e+10,"m":-0.0,"u":"\u00fc ü"}"#;
    let formatted = pretty(text).unwrap();
    assert!(formatted.contains(r#""z": "a, b:{c} [d] \"q\" \\""#));
    assert!(formatted.contains("1.50e+10"));
    assert!(formatted.contains("-0.0"));
    assert!(formatted.find("\"z\"").unwrap() < formatted.find("\"a\"").unwrap());
    let compact: String = formatted.lines().map(str::trim).collect::<Vec<_>>().join("");
    assert_eq!(compact.replace(": ", ":"), text);
  }

  #[test]
  fn formatting_twice_changes_nothing() {
    let once = pretty("[1,[2,[3]],{\"a\":{\"b\":[]}}]").unwrap();
    assert_eq!(pretty(&once).unwrap(), once);
  }

  #[test]
  fn a_scalar_is_its_own_formatting() {
    assert_eq!(pretty("  42 ").unwrap(), "42");
    assert_eq!(pretty("\"x\"").unwrap(), "\"x\"");
  }

  #[test]
  fn invalid_json_says_where() {
    assert_eq!(
      pretty("{\n  \"a\": 1,\n}"),
      Err(JsonFormatError::Invalid { line: 3, column: 1 })
    );
    assert!(matches!(
      pretty("{\"a\":1}\n{\"b\":2}"),
      Err(JsonFormatError::Invalid { .. })
    ));
    assert!(pretty("").is_err());
  }

  #[test]
  fn deep_nesting_is_refused_not_blown_up() {
    let nested = |depth: usize| format!("{}1{}", "[".repeat(depth), "]".repeat(depth));
    assert!(pretty(&nested(MAX_DEPTH)).is_ok());
    assert_eq!(pretty(&nested(MAX_DEPTH + 1)), Err(JsonFormatError::TooDeep));
    // Valid JSON far deeper than the stack would take.
    assert_eq!(pretty(&nested(100_000)), Err(JsonFormatError::TooDeep));
  }

  #[test]
  fn the_formatted_size_is_bounded() {
    let many = format!("[[[[{}1]]]]", "1,".repeat(1000));
    assert!(reindent(&many, 1_000_000).is_ok());
    assert_eq!(reindent(&many, 4_000), Err(JsonFormatError::TooLarge));
  }
}
