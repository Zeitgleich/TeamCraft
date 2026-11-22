//! A JavaScript `Error.stack` as frames: what the page reports for uncaught errors, rejected
//! promises and wasm panics. The browsers write it in two shapes; anything else is skipped.
//!
//! ```text
//! Safari, Firefox:  name@https://example.com/app.js:12:34
//! Chrome:               at name (https://example.com/app.js:12:34)
//! ```
//!
//! The app's wasm has no names in the shipped files, so its frames are function indices (#2228),
//! kept as [`WasmCode`] for `scripts/symbolicate-web.py`. Each browser writes them its own way:
//!
//! ```text
//! Chrome:   at teamcraft_ui.wasm.wasm-function[1185] (https://example.com/app_bg.wasm:wasm-function[1185]:0x1234)
//!           at wasm://wasm/0a1b2c3d:wasm-function[12]:0x34      (a module compiled from bytes)
//! Firefox:  wasm-function[1185]@https://example.com/app_bg.wasm:wasm-function[1185]:0x1234
//!           wasm-function[12]@https://example.com/split.js line 30 > WebAssembly.Module:wasm-function[12]:0x34
//! WebKit:   <?>.wasm-function[17228]@[wasm code]                (no module, no offset)
//! ```

use super::event::{StackFrame, WasmCode};

/// Safari's location of a browser function: never the app's own.
const NATIVE_CODE: &str = "[native code]";
/// Safari's location of a wasm frame: the app's module, the only wasm the page runs.
const WASM_CODE: &str = "[wasm code]";
/// How every browser names a wasm function it has no name for: `wasm-function[<index>]`.
const WASM_FUNCTION: &str = "wasm-function[";
/// Firefox's location of a module compiled from bytes: the compiling script, then this.
const FIREFOX_EVAL_LINE: &str = " line ";

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
  if let Some((module, wasm)) = wasm_location(location) {
    // Every wasm module the page runs is the app's (also one compiled from bytes, `wasm://`).
    frame.is_in_app = true;
    frame.maybe_wasm = Some(wasm);
    frame.maybe_file = Some(module.to_owned()).filter(|module| !module.is_empty());
  } else {
    apply_location(&mut frame, location);
    frame.maybe_wasm = maybe_function
      .and_then(function_index)
      .map(|function_index| WasmCode {
        function_index,
        maybe_offset: None,
      });
    frame.is_in_app |= frame.maybe_wasm.is_some();
  }
  Some(frame)
}

/// Chrome's and Firefox's wasm location, `<module>:wasm-function[<index>]:0x<offset>`: the module
/// (for one compiled from bytes in Firefox, the script that compiled it) and the position.
fn wasm_location(location: &str) -> Option<(&str, WasmCode)> {
  let (module, position) = location.rsplit_once(&format!(":{WASM_FUNCTION}"))?;
  let (index, offset) = position.split_once("]:0x")?;
  let wasm = WasmCode {
    function_index: index.parse().ok()?,
    maybe_offset: Some(u64::from_str_radix(offset, 16).ok()?),
  };
  let module = module
    .split_once(FIREFOX_EVAL_LINE)
    .map_or(module, |(script, _)| script);
  Some((module, wasm))
}

/// The index in a function name the browser made up: `wasm-function[1185]`, also behind a module
/// name (`teamcraft_ui.wasm.wasm-function[1185]`, WebKit's `<?>.wasm-function[17228]`).
fn function_index(function: &str) -> Option<u32> {
  let (before, index) = function.strip_suffix(']')?.rsplit_once(WASM_FUNCTION)?;
  (before.is_empty() || before.ends_with('.'))
    .then(|| index.parse().ok())
    .flatten()
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
  fn reads_a_firefox_stack_oldest_first() {
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
    assert_eq!(innermost.maybe_wasm, None);
    assert_eq!(frames[2].maybe_function, None);
    assert_eq!(frames[1].maybe_file.as_deref(), Some(NATIVE_CODE));
    assert!(!frames[1].is_in_app);
    assert_eq!(frames[0].maybe_function.as_deref(), Some("wasm-function[1234]"));
    assert_eq!(
      frames[0].maybe_file.as_deref(),
      Some("https://chat.example.com/teamcraft-ui-1_bg.wasm")
    );
    assert_eq!(frames[0].maybe_wasm, Some(wasm(1234, Some(0x5678))));
    assert_eq!((frames[0].maybe_line, frames[0].maybe_column), (None, None));
    assert!(frames[0].is_in_app);
  }

  #[test]
  fn a_firefox_module_compiled_from_bytes_names_its_loader() {
    let frames = frames_from_script_stack(
      "wasm-function[12]@https://chat.example.com/teamcraft-ui-split-1.js line 30 > WebAssembly.Module:wasm-function[12]:0x34",
      ORIGIN,
    );
    assert_eq!(
      frames[0].maybe_file.as_deref(),
      Some("https://chat.example.com/teamcraft-ui-split-1.js")
    );
    assert_eq!(frames[0].maybe_wasm, Some(wasm(12, Some(0x34))));
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
    assert_eq!(frames[0].maybe_wasm, Some(wasm(99, Some(0x10))));
    assert!(frames.iter().all(|frame| frame.is_in_app));
  }

  #[test]
  fn reads_chrome_wasm_frames_of_split_modules() {
    let stack = "Error
    at teamcraft_ui.wasm.wasm-function[1185] (https://chat.example.com/teamcraft-ui-split-admin-1.wasm:wasm-function[1185]:0x1234)
    at wasm://wasm/0a1b2c3d:wasm-function[12]:0x34";
    let frames = frames_from_script_stack(stack, ORIGIN);
    assert_eq!(frames.len(), 2);
    // Compiled from the pack's bytes: Chrome names it by a hash, still the app's.
    assert_eq!(frames[0].maybe_function, None);
    assert_eq!(frames[0].maybe_file.as_deref(), Some("wasm://wasm/0a1b2c3d"));
    assert_eq!(frames[0].maybe_wasm, Some(wasm(12, Some(0x34))));
    assert!(frames[0].is_in_app);
    assert_eq!(
      frames[1].maybe_file.as_deref(),
      Some("https://chat.example.com/teamcraft-ui-split-admin-1.wasm")
    );
    assert_eq!(frames[1].maybe_wasm, Some(wasm(1185, Some(0x1234))));
  }

  #[test]
  fn safari_wasm_frames_are_in_app() {
    let frames = frames_from_script_stack("<?>.wasm-function[7]@[wasm code]", ORIGIN);
    assert_eq!(frames[0].maybe_function.as_deref(), Some("<?>.wasm-function[7]"));
    assert_eq!(frames[0].maybe_file.as_deref(), Some(WASM_CODE));
    assert_eq!(frames[0].maybe_wasm, Some(wasm(7, None)));
    assert!(frames[0].is_in_app);
  }

  /// WebKitGTK (the Linux desktop app, TEAMCRAFT-37): no module, no offset, only the index.
  #[test]
  fn webkit_wasm_frames_keep_their_index() {
    let stack = "<?>.wasm-function[17228]@[wasm code]
wasm-function[3]@[wasm code]
wasm-stub@[native code]";
    let frames = frames_from_script_stack(stack, ORIGIN);
    assert_eq!(frames[0].maybe_wasm, None);
    assert_eq!(frames[1].maybe_wasm, Some(wasm(3, None)));
    assert_eq!(frames[2].maybe_wasm, Some(wasm(17228, None)));
  }

  #[test]
  fn names_that_only_look_like_wasm_functions_are_no_index() {
    for function in [
      "mywasm-function[3]",
      "wasm-function[]",
      "wasm-function[x]",
      "wasm-function[3",
      "wasm-function[99999999999]",
    ] {
      assert_eq!(function_index(function), None, "{function}");
    }
    assert_eq!(wasm_location("https://x/a.wasm:wasm-function[1]:0xzz"), None);
    assert_eq!(wasm_location("https://x/a.wasm:wasm-function[1]"), None);
    assert_eq!(wasm_location("https://x/a.js:1:2"), None);
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

  fn wasm(function_index: u32, maybe_offset: Option<u64>) -> WasmCode {
    WasmCode {
      function_index,
      maybe_offset,
    }
  }
}
