//! Timed steps of the core (issues #1521, #1640; docs/tracing.md): named stretches of work
//! (`gateway_resume`, `catch_up_conversation`, `snapshot_build`) and how long the local store held
//! a thread up (a wait for another thread's group of writes, a group of writes), as `tracing`
//! spans. The trace layer of `teamcraft_client_api::crash_reporting::trace` times them, so the
//! measures, crash breadcrumbs and slow-root reports of every platform read the same spans. Until a
//! process turns collection on, a step costs one atomic load.
//!
//! Only static names leave this module: no table, key, ID or value.

/// A step that is timed until it is dropped; `None` while nothing collects spans.
#[must_use = "the step is timed until this is dropped"]
pub struct Step {
  _span: tracing::Span,
}

/// A wait of the local store, timed until [`Mark::note`] or a drop.
pub(crate) struct Mark {
  _span: tracing::Span,
}

/// Times the rest of the scope as `name`: `let _step = timing::step("gateway_resume");`
pub fn step(name: &'static str) -> Option<Step> {
  let span = tracing::trace_span!("core.step", name);
  (!span.is_disabled()).then_some(Step { _span: span })
}

/// `crate::module::function::{{closure}}` → `function`: the function a job or request came from,
/// a name from the code (`std::any::type_name` of a closure it built). Generic arguments and the
/// `<Type as Trait>` of a trait method's path go; a name that leaves nothing is `job`.
pub fn code_name(type_name: &'static str) -> &'static str {
  // The last path segment outside any `<…>` that isn't a closure: `<A as B>::run::{{closure}}`
  // is `run`, not nothing (issue #1759: such jobs all showed as `job`).
  let bytes = type_name.as_bytes();
  let mut depth = 0usize;
  let mut segment_start = 0usize;
  let mut found: Option<(usize, usize)> = None;
  let mut index = 0;
  while index <= bytes.len() {
    let at_end = index == bytes.len();
    let is_separator = at_end || (depth == 0 && bytes[index] == b':' && bytes.get(index + 1) == Some(&b':'));
    if !at_end {
      match bytes[index] {
        b'<' => depth += 1,
        b'>' => depth = depth.saturating_sub(1),
        _ => {},
      }
    }
    if is_separator {
      let segment = &type_name[segment_start..index];
      // `a::b<c>` is `b`; `<A as B>` and `{{closure}}` are not names.
      let name_len = segment.find('<').unwrap_or(segment.len());
      if name_len > 0 && !segment.starts_with("{{") {
        found = Some((segment_start, segment_start + name_len));
      }
      segment_start = index + 2;
      index += 1;
    }
    index += 1;
  }
  found.map_or("job", |(from, to)| &type_name[from..to])
}

impl Mark {
  /// A thread waits for another thread's group of writes (or reads) to end.
  pub(crate) fn lock_wait() -> Option<Self> {
    Self::from_span(tracing::trace_span!("store.lock_wait", name = "store_lock"))
  }

  /// A group of writes holds the store, from its first write until it commits.
  pub(crate) fn write_group() -> Option<Self> {
    Self::from_span(tracing::trace_span!("store.write_group", name = "write_group"))
  }

  /// The wait ended.
  pub(crate) fn note(self) {
    drop(self);
  }

  fn from_span(span: tracing::Span) -> Option<Self> {
    (!span.is_disabled()).then_some(Self { _span: span })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn code_names_are_the_function() {
    assert_eq!(
      code_name("teamcraft_ffi::TeamCraft::messages::{{closure}}"),
      "messages"
    );
    assert_eq!(
      code_name("a::b::conversations::{{closure}}::{{closure}}"),
      "conversations"
    );
    assert_eq!(code_name("a::TeamCraft::read<alloc::vec::Vec<u8>>"), "read");
    assert_eq!(code_name(""), "job");
    // A trait method's path starts with `<Type as Trait>` (issue #1759).
    assert_eq!(
      code_name("<teamcraft_ffi::TeamCraft as teamcraft_ffi::Settings>::settings_mark::{{closure}}"),
      "settings_mark"
    );
    assert_eq!(
      code_name("<alloc::vec::Vec<a::B> as c::D<e::F>>::run::{{closure}}::{{closure}}"),
      "run"
    );
    assert_eq!(code_name("a::b<c::d>::e::{{closure}}"), "e");
    assert_eq!(code_name("{{closure}}"), "job");
  }
}
