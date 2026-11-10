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
/// a name from the code (`std::any::type_name` of a closure it built).
pub fn code_name(type_name: &'static str) -> &'static str {
  let mut rest = type_name;
  while let Some(trimmed) = rest.strip_suffix("::{{closure}}") {
    rest = trimmed;
  }
  // Generic arguments (`read<…>`) go with everything after the first `<`.
  let rest = rest.split('<').next().unwrap_or(rest);
  rest
    .rsplit("::")
    .next()
    .filter(|name| !name.is_empty())
    .unwrap_or("job")
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
  }
}
