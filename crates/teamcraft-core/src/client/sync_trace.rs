//! Where the time of the last gateway connection went, phase by phase (issue #714): socket and
//! handshake, the full sync's steps or the resume, the reads once per connection. Durations only,
//! from the client's clock, nothing about the content; the perf tests and docs/performance.md read
//! them, the log shows them at `info`.

use super::Client;
use crate::api::ServerApi;
use crate::store::LocalStore;

/// One step of a connection and how long it took.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPhase {
  pub name: &'static str,
  pub ms: i64,
}

/// The phases of the connection in progress, or of the last one.
#[derive(Clone, Debug, Default)]
pub(crate) struct SyncTrace {
  phases: Vec<SyncPhase>,
  started_ms: i64,
  lap_ms: i64,
}

impl SyncTrace {
  pub(crate) fn start(&mut self, now_ms: i64) {
    self.phases.clear();
    self.started_ms = now_ms;
    self.lap_ms = now_ms;
  }

  pub(crate) fn lap(&mut self, name: &'static str, now_ms: i64) {
    let ms = now_ms.saturating_sub(self.lap_ms);
    tracing::debug!(phase = name, ms, "connection phase done");
    self.phases.push(SyncPhase { name, ms });
    self.lap_ms = now_ms;
  }

  pub(crate) fn total_ms(&self) -> i64 {
    self.lap_ms.saturating_sub(self.started_ms)
  }

  pub(crate) fn phases(&self) -> &[SyncPhase] {
    &self.phases
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The phases of the last gateway connection (fresh or resumed) in their order; empty before
  /// the first one.
  pub fn last_connection_phases(&self) -> &[SyncPhase] {
    self.sync_trace.phases()
  }

  pub(crate) fn trace_lap(&mut self, name: &'static str) {
    let now = self.now_ms();
    self.sync_trace.lap(name, now);
  }

  pub(crate) fn trace_start(&mut self) {
    let now = self.now_ms();
    self.sync_trace.start(now);
  }

  /// Logs the phases at once, so one line in the log says where a slow start went.
  pub(crate) fn trace_finish(&mut self, outcome: &'static str) {
    let phases: Vec<String> = self
      .sync_trace
      .phases()
      .iter()
      .map(|phase| format!("{}={}", phase.name, phase.ms))
      .collect();
    tracing::info!(
      outcome,
      total_ms = self.sync_trace.total_ms(),
      phases = %phases.join(" "),
      "gateway connection caught up"
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn laps_measure_from_the_previous_lap() {
    let mut trace = SyncTrace::default();
    trace.start(1_000);
    trace.lap("socket", 1_040);
    trace.lap("sync", 1_100);
    assert_eq!(
      trace.phases(),
      &[
        SyncPhase {
          name: "socket",
          ms: 40
        },
        SyncPhase { name: "sync", ms: 60 },
      ]
    );
    assert_eq!(trace.total_ms(), 100);
    trace.start(2_000);
    assert!(trace.phases().is_empty());
  }
}
