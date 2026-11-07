//! Panics reach `tracing`, and so the system log (issue #1142), with where they happened but not
//! their message, which can carry data (docs/bug-reports.md). A file of its own: it needs the
//! process's only subscriber and panic hook.
// Test helpers outside `#[test]` functions panic on failure like the tests themselves.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use teamcraft_ffi::{AppConfig, EventListener, TeamCraft, TokenProvider};
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt as _};

#[test]
fn a_panic_on_any_thread_reaches_the_log_with_its_location_but_not_its_message() {
  let lines = Lines::default();
  tracing::subscriber::set_global_default(tracing_subscriber::registry().with(lines.clone())).unwrap();
  let directory = std::env::temp_dir().join(format!("teamcraft-ffi-panic-{}", std::process::id()));
  let teamcraft = TeamCraft::new(
    AppConfig {
      server_url: "http://127.0.0.1:9".into(),
      data_dir: directory.to_string_lossy().into_owned(),
      account: "account".into(),
      database_key: vec![7; 32],
      push_gateway_url: None,
      is_memory_constrained: false,
    },
    Arc::new(NoToken),
    Arc::new(NoListener),
  )
  .unwrap();

  let thread = std::thread::spawn(|| panic!("a bug in the sync loop"));
  assert!(thread.join().is_err());

  // Cloned before asserting: a failed assertion panics, and the hook logs into the same lock.
  let logged = lines.0.lock().unwrap().clone();
  assert!(!logged.contains("a bug in the sync loop"), "{logged}");
  assert!(logged.contains("panic_log.rs"), "{logged}");
  assert!(logged.contains("message_chars=22"), "{logged}");
  drop(teamcraft);
  std::fs::remove_dir_all(&directory).unwrap();
}

/// Everything logged, as text.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<String>>);

impl<S: tracing::Subscriber> Layer<S> for Lines {
  fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
    event.record(&mut Text(&mut self.0.lock().unwrap()));
  }
}

struct Text<'a>(&'a mut String);

impl Visit for Text<'_> {
  fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
    let _ = writeln!(self.0, "{}={value:?}", field.name());
  }
}

struct NoToken;

impl TokenProvider for NoToken {
  fn access_token(&self) -> Option<String> {
    None
  }
}

struct NoListener;

impl EventListener for NoListener {
  fn on_change(&self) {}
}
