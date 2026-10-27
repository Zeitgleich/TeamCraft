//! Terminal escape sequences in logs (ANSI/ECMA-48): the colours and styles they select are kept as
//! runs over the text, everything else is dropped. Nothing in them is ever acted on: cursor moves,
//! title changes, hyperlinks (OSC 8, whose text stays and whose address goes) and device controls
//! are removed, and a colour only ever becomes one of the sixteen the stylesheet defines for light
//! and dark (256-colour and true-colour codes take the nearest of them).

use super::Span;

const ESC: u8 = 0x1B;
const BEL: u8 = 0x07;
/// An OSC or device control string longer than this without its end is no sequence: its
/// introducer is dropped and the rest shown, so no unterminated sequence hides a line.
const MAX_STRING_BYTES: usize = 2048;
/// The sixteen basic colours as xterm draws them, for the nearest match of other colours.
const BASIC_RGB: [(u8, u8, u8); 16] = [
  (0, 0, 0),
  (205, 0, 0),
  (0, 205, 0),
  (205, 205, 0),
  (0, 0, 238),
  (205, 0, 205),
  (0, 205, 205),
  (229, 229, 229),
  (127, 127, 127),
  (255, 0, 0),
  (0, 255, 0),
  (255, 255, 0),
  (92, 92, 255),
  (255, 0, 255),
  (0, 255, 255),
  (255, 255, 255),
];
/// The steps of the 6×6×6 colour cube of the 256-colour palette.
const CUBE_STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// A text without its escape sequences, and the styles they set as runs over it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Stripped {
  pub text: String,
  pub runs: Vec<StyledRun>,
}

/// A stretch of text in one style other than the default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyledRun {
  pub span: Span,
  pub style: AnsiStyle,
}

/// Colours (0 to 15: the eight basic colours, then their bright forms) and text attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnsiStyle {
  pub maybe_fg: Option<u8>,
  pub maybe_bg: Option<u8>,
  pub attributes: Attributes,
}

/// Text attributes of a style, as bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attributes(u8);

impl Attributes {
  pub const BOLD: Self = Self(1);
  pub const DIM: Self = Self(1 << 1);
  pub const ITALIC: Self = Self(1 << 2);
  pub const UNDERLINE: Self = Self(1 << 3);
  pub const INVERSE: Self = Self(1 << 4);
  pub const STRIKE: Self = Self(1 << 5);

  pub fn contains(self, other: Self) -> bool {
    self.0 & other.0 == other.0
  }

  fn with(self, other: Self) -> Self {
    Self(self.0 | other.0)
  }

  fn without(self, other: Self) -> Self {
    Self(self.0 & !other.0)
  }
}

/// Whether `text` has escape sequences at all (most files don't, and skip [`strip`]).
pub fn has_escapes(text: &str) -> bool {
  text.as_bytes().contains(&ESC)
}

/// Removes every escape sequence from `text` and keeps the styles of SGR sequences (`ESC [ … m`)
/// as runs over the result. A style carries over line breaks, as in a terminal.
pub fn strip(text: &str) -> Stripped {
  let bytes = text.as_bytes();
  let mut out = String::with_capacity(text.len());
  let mut runs = Vec::new();
  let mut style = AnsiStyle::default();
  let mut run_start = 0;
  let mut pos = 0;
  while let Some(found) = bytes[pos..].iter().position(|&byte| byte == ESC) {
    let esc = pos + found;
    // Sequences are ASCII, so every position here is a character boundary.
    out.push_str(text.get(pos..esc).unwrap_or_default());
    let (end, maybe_sgr) = sequence(bytes, esc);
    if let Some(params) = maybe_sgr {
      let next = apply_sgr(style, params);
      if next != style {
        close_run(&mut runs, style, run_start, out.len());
        style = next;
        run_start = out.len();
      }
    }
    pos = end;
  }
  out.push_str(text.get(pos..).unwrap_or_default());
  close_run(&mut runs, style, run_start, out.len());
  Stripped { text: out, runs }
}

fn close_run(runs: &mut Vec<StyledRun>, style: AnsiStyle, start: usize, end: usize) {
  if style != AnsiStyle::default() && end > start {
    runs.push(StyledRun {
      span: Span::new(start, end),
      style,
    });
  }
}

/// Where the sequence starting with the escape at `esc` ends, and the parameters if it is an SGR
/// sequence. Malformed sequences end where they stop making sense; what follows is text.
fn sequence(bytes: &[u8], esc: usize) -> (usize, Option<&[u8]>) {
  match bytes.get(esc + 1) {
    Some(b'[') => control_sequence(bytes, esc + 2),
    // OSC, DCS, SOS, PM and APC: a string up to its terminator.
    Some(b']' | b'P' | b'X' | b'^' | b'_') => (string_end(bytes, esc + 2).unwrap_or(esc + 2), None),
    // nF: intermediates, then a final byte.
    Some(0x20..=0x2F) => {
      let mut end = esc + 1;
      while matches!(bytes.get(end), Some(0x20..=0x2F)) {
        end += 1;
      }
      if matches!(bytes.get(end), Some(0x30..=0x7E)) {
        end += 1;
      }
      (end, None)
    },
    // Two-byte sequences (Fp, Fe, Fs).
    Some(0x30..=0x7E) => (esc + 2, None),
    _ => (esc + 1, None),
  }
}

/// A control sequence after `ESC [`: parameters, intermediates and a final byte.
fn control_sequence(bytes: &[u8], start: usize) -> (usize, Option<&[u8]>) {
  let mut end = start;
  while matches!(bytes.get(end), Some(0x30..=0x3F)) {
    end += 1;
  }
  let params = &bytes[start..end];
  while matches!(bytes.get(end), Some(0x20..=0x2F)) {
    end += 1;
  }
  // Private sequences (`ESC [ ? …`, `ESC [ > … m`) are no colours.
  let is_private = matches!(params.first(), Some(b'<' | b'=' | b'>' | b'?'));
  match bytes.get(end) {
    Some(b'm') if !is_private && end == start + params.len() => (end + 1, Some(params)),
    Some(0x40..=0x7E) => (end + 1, None),
    _ => (end, None),
  }
}

/// The end of a control string: after BEL or `ESC \`. `None` when a line break or the end of the
/// text comes first, or the string is implausibly long.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
  for (offset, &byte) in bytes[start.min(bytes.len())..]
    .iter()
    .enumerate()
    .take(MAX_STRING_BYTES)
  {
    match byte {
      BEL => return Some(start + offset + 1),
      ESC if bytes.get(start + offset + 1) == Some(&b'\\') => return Some(start + offset + 2),
      b'\n' | b'\r' => return None,
      _ => {},
    }
  }
  None
}

/// The style after an SGR sequence with `params` (`1;31`, `38;5;208`, `38:2::255:128:0`).
fn apply_sgr(mut style: AnsiStyle, params: &[u8]) -> AnsiStyle {
  let groups: Vec<Vec<u16>> = params
    .split(|&byte| byte == b';')
    .map(|group| group.split(|&byte| byte == b':').map(number).collect())
    .collect();
  let mut index = 0;
  while let Some(group) = groups.get(index) {
    let code = group.first().copied().unwrap_or(0);
    match code {
      0 => style = AnsiStyle::default(),
      1 => style.attributes = style.attributes.with(Attributes::BOLD),
      2 => style.attributes = style.attributes.with(Attributes::DIM),
      3 => style.attributes = style.attributes.with(Attributes::ITALIC),
      4 => style.attributes = style.attributes.with(Attributes::UNDERLINE),
      7 => style.attributes = style.attributes.with(Attributes::INVERSE),
      9 => style.attributes = style.attributes.with(Attributes::STRIKE),
      22 => {
        style.attributes = style
          .attributes
          .without(Attributes::BOLD)
          .without(Attributes::DIM)
      },
      23 => style.attributes = style.attributes.without(Attributes::ITALIC),
      24 => style.attributes = style.attributes.without(Attributes::UNDERLINE),
      27 => style.attributes = style.attributes.without(Attributes::INVERSE),
      29 => style.attributes = style.attributes.without(Attributes::STRIKE),
      30..=37 => style.maybe_fg = Some(basic(code - 30)),
      39 => style.maybe_fg = None,
      40..=47 => style.maybe_bg = Some(basic(code - 40)),
      49 => style.maybe_bg = None,
      90..=97 => style.maybe_fg = Some(basic(code - 90 + 8)),
      100..=107 => style.maybe_bg = Some(basic(code - 100 + 8)),
      38 | 48 => {
        let (maybe_color, consumed) = match group.get(1..) {
          Some(colon_form) if !colon_form.is_empty() => (extended_colon(colon_form), 0),
          _ => extended_semicolon(&groups[index + 1..]),
        };
        if let Some(color) = maybe_color {
          match code {
            38 => style.maybe_fg = Some(color),
            _ => style.maybe_bg = Some(color),
          }
        }
        index += consumed;
      },
      _ => {},
    }
    index += 1;
  }
  style
}

/// `38;5;n` and `38;2;r;g;b`: the colour and how many groups after the 38 it took.
fn extended_semicolon(rest: &[Vec<u16>]) -> (Option<u8>, usize) {
  let value = |at: usize| rest.get(at).and_then(|group| group.first().copied());
  match value(0) {
    Some(5) => (value(1).map(palette), 2),
    Some(2) => match (value(1), value(2), value(3)) {
      (Some(red), Some(green), Some(blue)) => (Some(nearest(channel(red), channel(green), channel(blue))), 4),
      _ => (None, rest.len()),
    },
    _ => (None, 0),
  }
}

/// `38:5:n`, `38:2:r:g:b` and `38:2:colour-space:r:g:b`.
fn extended_colon(values: &[u16]) -> Option<u8> {
  match values {
    [5, index, ..] => Some(palette(*index)),
    [2, _, red, green, blue, ..] | [2, red, green, blue] => {
      Some(nearest(channel(*red), channel(*green), channel(*blue)))
    },
    _ => None,
  }
}

/// A colour of the 256-colour palette as one of the sixteen.
fn palette(index: u16) -> u8 {
  match index {
    0..=15 => basic(index),
    16..=231 => {
      let cube = index - 16;
      let step = |value: u16| CUBE_STEPS[usize::from(value % 6)];
      nearest(step(cube / 36), step(cube / 6), step(cube))
    },
    _ => {
      let gray = channel(8 + 10 * (index.min(255) - 232));
      nearest(gray, gray, gray)
    },
  }
}

fn nearest(red: u8, green: u8, blue: u8) -> u8 {
  let distance = |(r, g, b): (u8, u8, u8)| {
    let delta = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
    delta(r, red) + delta(g, green) + delta(b, blue)
  };
  (0..16u8)
    .min_by_key(|&color| distance(BASIC_RGB[usize::from(color)]))
    .unwrap_or(7)
}

fn basic(code: u16) -> u8 {
  u8::try_from(code.min(15)).unwrap_or(15)
}

fn channel(value: u16) -> u8 {
  u8::try_from(value.min(255)).unwrap_or(u8::MAX)
}

fn number(digits: &[u8]) -> u16 {
  digits.iter().fold(0u16, |value, &digit| {
    if digit.is_ascii_digit() {
      value.saturating_mul(10).saturating_add(u16::from(digit - b'0'))
    } else {
      value
    }
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod stripping {
    use super::*;

    #[test]
    fn text_without_escapes_stays_as_it_is() {
      let stripped = strip("plain\ntext");
      assert_eq!(stripped.text, "plain\ntext");
      assert!(stripped.runs.is_empty());
      assert!(!has_escapes("plain"));
      assert!(has_escapes("a\x1b[0m"));
    }

    #[test]
    fn colours_become_runs_over_the_plain_text() {
      let stripped = strip("ok \x1b[1;31mfailed\x1b[0m done");
      assert_eq!(stripped.text, "ok failed done");
      assert_eq!(
        stripped.runs,
        [StyledRun {
          span: Span::new(3, 9),
          style: AnsiStyle {
            maybe_fg: Some(1),
            maybe_bg: None,
            attributes: Attributes::BOLD,
          },
        }]
      );
    }

    #[test]
    fn a_style_carries_over_a_line_break() {
      let stripped = strip("\x1b[33ma\nb\x1b[39mc");
      assert_eq!(stripped.text, "a\nbc");
      assert_eq!(stripped.runs.len(), 1);
      assert_eq!(stripped.runs[0].span, Span::new(0, 3));
    }

    #[test]
    fn other_sequences_are_dropped_without_effect() {
      // Cursor moves, erase, a private mode, a window title, a hyperlink with its text kept,
      // a charset selection and a lone escape at the end.
      let text = "\x1b[2J\x1b[1;1H\x1b[?25ltop\x1b]0;title\x07 \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ \x1b(Bend\x1b";
      let stripped = strip(text);
      assert_eq!(stripped.text, "top link end");
      assert!(stripped.runs.is_empty());
    }

    #[test]
    fn an_unterminated_string_hides_nothing_after_its_line() {
      let stripped = strip("a\x1b]0;never ends\nsecond line");
      assert_eq!(stripped.text, "a0;never ends\nsecond line");
    }

    #[test]
    fn a_malformed_sequence_keeps_the_text_after_it() {
      assert_eq!(strip("\x1b[12\u{e9}t\u{e9}").text, "\u{e9}t\u{e9}");
      assert_eq!(strip("x\x1b\x01y").text, "x\x01y");
    }
  }

  mod colours {
    use super::*;

    #[test]
    fn basic_bright_and_background_colours() {
      let style = apply_sgr(AnsiStyle::default(), b"92;44;4");
      assert_eq!(style.maybe_fg, Some(10));
      assert_eq!(style.maybe_bg, Some(4));
      assert!(style.attributes.contains(Attributes::UNDERLINE));
      let reset = apply_sgr(style, b"39;49;24");
      assert_eq!(reset, AnsiStyle::default());
      assert_eq!(apply_sgr(style, b""), AnsiStyle::default());
    }

    #[test]
    fn palette_and_true_colours_take_the_nearest_basic_colour() {
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38;5;196").maybe_fg, Some(9));
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38;5;3").maybe_fg, Some(3));
      assert_eq!(apply_sgr(AnsiStyle::default(), b"48;5;255").maybe_bg, Some(7));
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38;2;0;0;250").maybe_fg, Some(4));
      assert_eq!(
        apply_sgr(AnsiStyle::default(), b"38:2::250:250:0").maybe_fg,
        Some(11)
      );
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38:5:46").maybe_fg, Some(10));
    }

    #[test]
    fn codes_after_an_extended_colour_still_apply() {
      let style = apply_sgr(AnsiStyle::default(), b"38;5;1;1");
      assert_eq!(style.maybe_fg, Some(1));
      assert!(style.attributes.contains(Attributes::BOLD));
    }

    #[test]
    fn truncated_and_huge_parameters_are_harmless() {
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38;2;1").maybe_fg, None);
      assert_eq!(apply_sgr(AnsiStyle::default(), b"38;5").maybe_fg, None);
      assert_eq!(
        apply_sgr(AnsiStyle::default(), b"99999999;38;5;99999").maybe_fg,
        Some(7)
      );
    }
  }
}
