//! Background work: writing usage counters to the store and forgetting expired limiter entries.

use std::time::Duration;

use tracing::warn;

use crate::app::{AppState, now_millis};

/// Runs until the task is dropped.
pub async fn run(state: AppState) {
  let mut flush = tokio::time::interval(Duration::from_secs(
    state.config.database.flush_interval_secs.max(1),
  ));
  let mut sweep = tokio::time::interval(Duration::from_secs(60));
  loop {
    tokio::select! {
      _ = flush.tick() => flush_usage(&state).await,
      _ = sweep.tick() => state.limiter.sweep(now_millis()),
    }
  }
}

/// Writes the counters since the last flush. Also called on shutdown.
pub async fn flush_usage(state: &AppState) {
  let drained = state.limiter.drain_pending();
  if drained.is_empty() {
    return;
  }
  let now_ms = now_millis();
  let mut by_day: std::collections::BTreeMap<i64, Vec<_>> = std::collections::BTreeMap::new();
  for (server_id, day, counts) in drained {
    by_day.entry(day).or_default().push((server_id, counts));
  }
  for (day, entries) in by_day {
    if let Err(error) = state
      .store
      .add_usage(entries, day, i64::try_from(now_ms / 1000).unwrap_or(i64::MAX))
      .await
    {
      warn!(%error, "writing usage counters failed");
    }
  }
}
