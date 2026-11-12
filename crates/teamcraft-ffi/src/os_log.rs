//! The system log layer of the iOS app: one os_log line per `tracing` event.
//!
//! It replaces `tracing-oslog`, which made an os_activity for every span it saw and, on an event,
//! looked up the activity of the thread's current span with `expect`. Behind a per-layer filter
//! that drops `info` spans (ours keeps `warn` and the lifecycle targets) the current span often has
//! none, so a warning inside such a span panicked the engine thread and the app went down
//! (seen on iOS 1.0.31). This layer leaves spans alone: it only writes events.

use std::fmt::{self, Write as _};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};

/// Writes each event it gets as one line to `sink`. Spans are not looked at.
pub(crate) struct EventLines<F> {
  sink: F,
}

impl<F> EventLines<F>
where
  F: Fn(Level, &str) + Send + Sync + 'static,
{
  pub(crate) fn new(sink: F) -> Self {
    Self { sink }
  }
}

impl<S, F> Layer<S> for EventLines<F>
where
  S: Subscriber,
  F: Fn(Level, &str) + Send + Sync + 'static,
{
  fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
    (self.sink)(*event.metadata().level(), &line(event));
  }
}

/// The message first, then the other fields as `name=value`, as `tracing-oslog` wrote them.
fn line(event: &Event<'_>) -> String {
  let mut fields = Fields::default();
  event.record(&mut fields);
  let mut line = fields.message;
  for (name, value) in fields.rest {
    if !line.is_empty() {
      line.push(' ');
    }
    let _ = write!(line, "{name}={value}");
  }
  // os_log takes a C string.
  line.retain(|c| c != '\0');
  line
}

#[derive(Default)]
struct Fields {
  message: String,
  rest: Vec<(&'static str, String)>,
}

impl Visit for Fields {
  fn record_str(&mut self, field: &Field, value: &str) {
    if field.name() == "message" {
      self.message = value.to_owned();
    } else {
      self.rest.push((field.name(), format!("{value:?}")));
    }
  }

  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    if field.name() == "message" {
      self.message = format!("{value:?}");
    } else {
      self.rest.push((field.name(), format!("{value:?}")));
    }
  }
}

#[cfg(target_os = "ios")]
pub(crate) use ios::layer;

#[cfg(target_os = "ios")]
mod ios {
  use oslog::{Level as OsLevel, OsLog};
  use tracing::Level;

  use super::EventLines;

  /// Same mapping as `tracing-oslog` had: warnings orange, errors red in Console.app.
  fn kind(level: Level) -> OsLevel {
    match level {
      Level::TRACE => OsLevel::Debug,
      Level::DEBUG => OsLevel::Info,
      Level::INFO => OsLevel::Default,
      Level::WARN => OsLevel::Error,
      Level::ERROR => OsLevel::Fault,
    }
  }

  /// The layer for `subsystem` / `category`.
  pub(crate) fn layer(
    subsystem: &str,
    category: &str,
  ) -> EventLines<impl Fn(Level, &str) + Send + Sync + 'static> {
    let log = OsLog::new(subsystem, category);
    EventLines::new(move |level, line| log.with_level(kind(level), line))
  }
}

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use tracing_subscriber::layer::SubscriberExt as _;

  use super::*;
  use crate::logging::system_log_filter;

  type Lines = Arc<Mutex<Vec<(Level, String)>>>;

  fn capture() -> (Lines, EventLines<impl Fn(Level, &str) + Send + Sync + 'static>) {
    let lines = Lines::default();
    let sink = lines.clone();
    let layer = EventLines::new(move |level, line: &str| {
      sink.lock().unwrap().push((level, line.to_owned()));
    });
    (lines, layer)
  }

  #[test]
  fn a_warning_inside_a_span_the_filter_drops_is_written() {
    // The crash of 1.0.31: an `info` span the system log filter drops, a warning inside it.
    let (lines, layer) = capture();
    let subscriber = tracing_subscriber::registry().with(layer.with_filter(system_log_filter()));
    tracing::subscriber::with_default(subscriber, || {
      let span = tracing::info_span!(target: "teamcraft_core::client::gateway", "apply_event");
      let _entered = span.enter();
      tracing::warn!(target: "teamcraft_core::client::gateway", conversation = 7, "dropped");
      tracing::info!(target: "teamcraft_core::client::gateway", "not for the system log");
    });
    let lines = lines.lock().unwrap();
    assert_eq!(
      lines.as_slice(),
      &[(Level::WARN, "dropped conversation=7".to_owned())]
    );
  }

  #[test]
  fn strings_are_quoted_and_nul_bytes_dropped() {
    let (lines, layer) = capture();
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
      tracing::error!(step = "open", cause = %"a\0b", "failed");
    });
    let lines = lines.lock().unwrap();
    assert_eq!(
      lines[0],
      (Level::ERROR, "failed step=\"open\" cause=ab".to_owned())
    );
  }
}
