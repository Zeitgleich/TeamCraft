//! A JavaScript `Error.stack` as frames: what the page reports for uncaught errors and rejected
//! promises. The browsers write it in two shapes; anything else is skipped.
//!
//! ```text
//! Safari, Firefox:  name@https://example.com/app.js:12:34
//! Chrome:               at name (https://example.com/app.js:12:34)
//! ```

use super::event::StackFrame;

/// Safari's location of a browser function: never the app's own.
const NATIVE_CODE: &str = "[native code]";
/// Safari's location of a wasm frame: the app's module, the only wasm the page runs.
const WASM_CODE: &str = "[wasm code]";

/// Frames from a script error's `stack`, oldest call first (as Sentry wants them). `origin` is the
/// page's (`https://example.com`): frames from there, and wasm frames, are the app's own.
pub fn frames_from_script_stack(stack: &str, origin: &str) -> Vec<StackFrame> {
  let mut frames: Vec<StackFrame> = stack
    .lines()
    .map(str::trim)
    .filter_map(|line| frame_of(line, origin))
    .collect();
  frames.reverse();
  frames
}

fn frame_of(line: &str, origin: &str) -> Option<StackFrame> {
  let (maybe_function, location) = if let Some(rest) = line.strip_prefix("at ") {
    match rest.strip_suffix(')').and_then(|rest| rest.rsplit_once(" (")) {
      Some((function, location)) => (Some(function), location),
      None => (None, rest),
    }
  } else if let Some((function, location)) = line.rsplit_once('@') {
    (Some(function).filter(|function| !function.is_empty()), location)
  } else if line == NATIVE_CODE || line.contains("://") {
    // Safari writes a frame without a function name as the bare location.
    (None, line)
  } else {
    // The first line of Chrome's stack is the message, not a frame.
    return None;
  };
  let mut frame = StackFrame {
    maybe_function: maybe_function.map(str::to_owned),
    is_in_app: location == WASM_CODE || (!origin.is_empty() && location.starts_with(origin)),
    ..StackFrame::default()
  };
  apply_location(&mut frame, location);
  Some(frame)
}

/// `url:line:column`; a location without numbers (`[native code]`) stays the file.
fn apply_location(frame: &mut StackFrame, location: &str) {
  let mut parts = location.rsplitn(3, ':');
  let maybe_column = parts.next().and_then(|part| part.parse().ok());
  let maybe_line = parts.next().and_then(|part| part.parse().ok());
  match (maybe_column, maybe_line, parts.next()) {
    (Some(column), Some(line), Some(file)) => {
      frame.maybe_file = Some(file.to_owned());
      frame.maybe_line = Some(line);
      frame.maybe_column = Some(column);
    },
    _ => frame.maybe_file = Some(location.to_owned()).filter(|file| !file.is_empty()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const ORIGIN: &str = "https://chat.example.com";

  #[test]
  fn reads_a_safari_stack_oldest_first() {
    let stack = "__wbg_adapter_12@https://chat.example.com/teamcraft-ui-1.js:3890:14
@https://chat.example.com/teamcraft-ui-1.js:3912:20
[native code]
wasm-function[1234]@https://chat.example.com/teamcraft-ui-1_bg.wasm:wasm-function[1234]:0x5678";
    let frames = frames_from_script_stack(stack, ORIGIN);
    assert_eq!(frames.len(), 4);
    let innermost = frames.last().unwrap();
    assert_eq!(innermost.maybe_function.as_deref(), Some("__wbg_adapter_12"));
    assert_eq!(
      innermost.maybe_file.as_deref(),
      Some("https://chat.example.com/teamcraft-ui-1.js")
    );
    assert_eq!(
      (innermost.maybe_line, innermost.maybe_column),
      (Some(3890), Some(14))
    );
    assert!(innermost.is_in_app);
    assert_eq!(frames[2].maybe_function, None);
    assert_eq!(frames[1].maybe_file.as_deref(), Some(NATIVE_CODE));
    assert!(!frames[1].is_in_app);
    assert_eq!(frames[0].maybe_function.as_deref(), Some("wasm-function[1234]"));
    assert!(frames[0].is_in_app);
  }

  #[test]
  fn reads_a_chrome_stack_without_the_message_line() {
    let stack = "Error: closure invoked recursively or after being dropped
    at __wbg_adapter_12 (https://chat.example.com/teamcraft-ui-1.js:3890:14)
    at https://chat.example.com/teamcraft-ui-1.js:3912:20
    at teamcraft_ui.wasm.run (https://chat.example.com/teamcraft-ui-1_bg.wasm:wasm-function[99]:0x10)";
    let frames = frames_from_script_stack(stack, ORIGIN);
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[2].maybe_function.as_deref(), Some("__wbg_adapter_12"));
    assert_eq!(frames[2].maybe_line, Some(3890));
    assert_eq!(frames[1].maybe_function, None);
    assert_eq!(frames[1].maybe_column, Some(20));
    assert_eq!(frames[0].maybe_function.as_deref(), Some("teamcraft_ui.wasm.run"));
    assert!(frames.iter().all(|frame| frame.is_in_app));
  }

  #[test]
  fn safari_wasm_frames_are_in_app() {
    let frames = frames_from_script_stack("<?>.wasm-function[7]@[wasm code]", ORIGIN);
    assert_eq!(frames[0].maybe_function.as_deref(), Some("<?>.wasm-function[7]"));
    assert_eq!(frames[0].maybe_file.as_deref(), Some(WASM_CODE));
    assert!(frames[0].is_in_app);
  }

  #[test]
  fn other_origins_are_not_in_app() {
    let frames = frames_from_script_stack("f@https://cdn.example.org/x.js:1:2", ORIGIN);
    assert!(!frames[0].is_in_app);
  }

  #[test]
  fn empty_and_malformed_stacks_give_no_frames() {
    assert!(frames_from_script_stack("", ORIGIN).is_empty());
    assert!(frames_from_script_stack("TypeError: x is not a function", ORIGIN).is_empty());
    let frames = frames_from_script_stack("f@", ORIGIN);
    assert_eq!(frames[0].maybe_file, None);
  }
}
