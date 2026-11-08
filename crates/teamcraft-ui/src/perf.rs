//! Performance data of the page (issue #1521, docs/crash-reports.md "Performance data"): how long
//! the start, a channel switch, a send, a resync, a transfer and a call setup take for real users,
//! sent as Sentry transactions with the crash reports (`crate::crash_reports`).
//!
//! Whether an operation is measured is decided when it starts (the instance's trace rate, the
//! user's choice): an unmeasured one costs a random number and nothing else. Names, span names and
//! tags are `&'static str` from the code, so a measure can't carry content (transaction.rs).
//!
//! Natively (unit tests, the desktop shell's own crates) nothing is measured.

use std::cell::RefCell;

pub use teamcraft_client_api::crash_reporting::{PerfOperation, PerfStatus, PerfUnit};
use teamcraft_client_api::crash_reporting::{PerfSpan, Transaction};
use teamcraft_client_api::remote::CoreConnection;

/// Measures that may run side by side (sends, one per message) at most; a further one drops the
/// oldest, so a measure whose end never comes (the row moved, the page lost it) can't pile up.
const MAX_PENDING: usize = 16;

thread_local! {
  /// Measures that start in one place and end in another (a click and the render it leads to),
  /// by operation and a local key (a conversation's or a pending message's ID). The key never
  /// leaves the page: it only finds the measure again.
  static PENDING: RefCell<Vec<(PerfOperation, String, Measure)>> = const { RefCell::new(Vec::new()) };
}

/// One operation being measured, or not (sampled out, reports off).
#[derive(Debug, Default)]
#[must_use = "a measure is sent when finished"]
pub struct Measure {
  maybe_transaction: Option<Transaction>,
}

/// Starts measuring `operation` now.
pub fn begin(operation: PerfOperation) -> Measure {
  begin_at(operation, now_ms())
}

/// Starts measuring `operation` from `start_ms` (Unix time in milliseconds), such as the page's
/// navigation for the start.
pub fn begin_at(operation: PerfOperation, start_ms: f64) -> Measure {
  Measure {
    maybe_transaction: platform::should_trace()
      .then(|| Transaction::start(operation, start_ms, platform::random40())),
  }
}

/// Starts a measure that [`end_pending`] finishes elsewhere; one that runs for the same `key`
/// is dropped unsent (the user moved on before it ended, which says nothing about its speed).
pub fn begin_pending(operation: PerfOperation, key: &str) {
  let measure = begin(operation);
  PENDING.with(|pending| {
    let mut pending = pending.borrow_mut();
    pending.retain(|(running, running_key, _)| *running != operation || running_key != key);
    if measure.is_on() {
      if pending.len() >= MAX_PENDING {
        drop(pending.remove(0));
      }
      pending.push((operation, key.to_owned(), measure));
    }
  });
}

/// Drops every running measure of `operation` but the one for `key` (a channel switch that the
/// next one replaced).
pub fn keep_only_pending(operation: PerfOperation, key: &str) {
  PENDING.with(|pending| {
    pending
      .borrow_mut()
      .retain(|(running, running_key, _)| *running != operation || running_key == key);
  });
}

/// Changes the running measure of `operation` for `key`, if there is one.
pub fn with_pending(operation: PerfOperation, key: &str, change: impl FnOnce(&mut Measure)) {
  PENDING.with(|pending| {
    if let Some((_, _, measure)) = pending
      .borrow_mut()
      .iter_mut()
      .find(|(running, running_key, _)| *running == operation && running_key == key)
    {
      change(measure);
    }
  });
}

/// Whether a measure of `operation` runs (for any key).
pub fn is_pending(operation: PerfOperation) -> bool {
  PENDING.with(|pending| {
    pending
      .borrow()
      .iter()
      .any(|(running, _, _)| *running == operation)
  })
}

/// Finishes the running measure of `operation` for `key`, if there is one.
pub fn end_pending(operation: PerfOperation, key: &str, status: PerfStatus) {
  let maybe_measure = PENDING.with(|pending| {
    let mut pending = pending.borrow_mut();
    let position = pending
      .iter()
      .position(|(running, running_key, _)| *running == operation && running_key == key)?;
    Some(pending.remove(position).2)
  });
  if let Some(measure) = maybe_measure {
    measure.finish(status);
  }
}

/// Drops the running measures of `operation` unsent.
pub fn cancel_pending(operation: PerfOperation) {
  PENDING.with(|pending| {
    pending
      .borrow_mut()
      .retain(|(running, _, _)| *running != operation)
  });
}

/// The signed-in start is done (the channel list is on screen): measures it from the page's
/// navigation, with the start's parts (`crate::start_timing`) as its steps. Once per page.
pub fn finish_start() {
  thread_local! {
    static IS_DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
  }
  if IS_DONE.with(|done| done.replace(true)) {
    return;
  }
  let origin = navigation_start_ms();
  let mut measure = begin_at(PerfOperation::StartToChannelList, origin);
  if !measure.is_on() {
    return;
  }
  let parts = crate::start_timing::parts();
  let has_restore = parts
    .iter()
    .any(|(part, _, _)| *part == crate::start_timing::Part::Restore);
  for (part, start_ms, end_ms) in parts {
    measure.span_between("app.start", part.name(), origin + start_ms, origin + end_ms);
  }
  measure.tag("account", if has_restore { "restored" } else { "cached" });
  measure.finish(PerfStatus::Ok);
}

/// The connection to the server changed (the offline banner sees every change): a reconnect is
/// measured from the core starting to connect again until it is connected, which covers the
/// gateway's resume or fresh start; an attempt that fails ends as unavailable.
pub fn connection_changed(previous: CoreConnection, current: CoreConnection) {
  const KEY: &str = "";
  match current {
    CoreConnection::Connecting if !matches!(previous, CoreConnection::Connecting) => {
      begin_pending(PerfOperation::Resync, KEY);
    },
    CoreConnection::Connected | CoreConnection::Slow => {
      if matches!(current, CoreConnection::Slow) {
        with_pending(PerfOperation::Resync, KEY, |measure| measure.tag("link", "slow"));
      }
      end_pending(PerfOperation::Resync, KEY, PerfStatus::Ok);
    },
    CoreConnection::Offline { .. } => end_pending(PerfOperation::Resync, KEY, PerfStatus::Unavailable),
    CoreConnection::SignInRequired { .. } => cancel_pending(PerfOperation::Resync),
    CoreConnection::Connecting => {},
  }
}

/// Unix time in milliseconds, with a fraction: the page's monotonic clock from its time origin,
/// so a duration never jumps with the wall clock.
pub fn now_ms() -> f64 {
  platform::now_ms()
}

/// When the page's navigation started, Unix time in milliseconds.
pub fn navigation_start_ms() -> f64 {
  platform::time_origin_ms()
}

impl Measure {
  pub fn is_on(&self) -> bool {
    self.maybe_transaction.is_some()
  }

  /// A step that started at `start_ms` and ends now.
  pub fn span(&mut self, op: &'static str, description: &'static str, start_ms: f64, status: PerfStatus) {
    if let Some(transaction) = &mut self.maybe_transaction {
      transaction.add_span(PerfSpan {
        op,
        description,
        start_ms,
        end_ms: now_ms(),
        status,
      });
    }
  }

  /// A step with both ends known (from `performance` marks).
  pub fn span_between(&mut self, op: &'static str, description: &'static str, start_ms: f64, end_ms: f64) {
    if let Some(transaction) = &mut self.maybe_transaction {
      transaction.add_span(PerfSpan {
        op,
        description,
        start_ms,
        end_ms,
        status: PerfStatus::Ok,
      });
    }
  }

  pub fn tag(&mut self, key: &'static str, value: &'static str) {
    if let Some(transaction) = &mut self.maybe_transaction {
      transaction.set_tag(key, value);
    }
  }

  pub fn measure(&mut self, name: &'static str, value: f64, unit: PerfUnit) {
    if let Some(transaction) = &mut self.maybe_transaction {
      transaction.set_measurement(name, value, unit);
    }
  }

  /// Ends it now and queues it with the crash reports.
  pub fn finish(self, status: PerfStatus) {
    if let Some(transaction) = self.maybe_transaction {
      platform::capture(transaction.finish(now_ms(), status));
    }
  }
}

#[cfg(target_arch = "wasm32")]
mod platform {
  use teamcraft_client_api::crash_reporting::Transaction;

  pub fn should_trace() -> bool {
    crate::crash_reports::should_trace()
  }

  pub fn capture(transaction: Transaction) {
    crate::crash_reports::capture_transaction(&transaction);
  }

  pub fn random40() -> [u8; 40] {
    let mut bytes = [0; 40];
    // Without the browser's random source the IDs are zero: transactions then merge in the
    // backend, which is harmless.
    let _ = getrandom::getrandom(&mut bytes);
    bytes
  }

  fn performance() -> Option<web_sys::Performance> {
    web_sys::window()?.performance()
  }

  pub fn time_origin_ms() -> f64 {
    performance().map_or_else(js_sys::Date::now, |performance| performance.time_origin())
  }

  pub fn now_ms() -> f64 {
    performance().map_or_else(js_sys::Date::now, |performance| {
      performance.time_origin() + performance.now()
    })
  }
}

#[cfg(not(target_arch = "wasm32"))]
mod platform {
  use teamcraft_client_api::crash_reporting::Transaction;

  pub fn should_trace() -> bool {
    false
  }

  pub fn capture(_transaction: Transaction) {}

  pub fn random40() -> [u8; 40] {
    [0; 40]
  }

  pub fn time_origin_ms() -> f64 {
    0.0
  }

  pub fn now_ms() -> f64 {
    0.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn natively_nothing_is_measured_or_kept() {
    let mut measure = begin(PerfOperation::ChannelSwitch);
    assert!(!measure.is_on());
    measure.tag("source", "cache");
    measure.finish(PerfStatus::Ok);
    begin_pending(PerfOperation::MessageSend, "1");
    assert!(!is_pending(PerfOperation::MessageSend));
  }
}
