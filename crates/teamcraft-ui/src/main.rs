//! Browser entry point built by `trunk`.

use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Highest level the page writes to the browser console. Dependencies log key material and message
/// metadata at DEBUG/TRACE (DAVE does, see #443), and the console is readable by anyone at the
/// screen and by extensions, so release builds stop at INFO (security.md: never log keys).
const PAGE_LEVEL: LevelFilter = if cfg!(debug_assertions) {
  LevelFilter::DEBUG
} else {
  LevelFilter::INFO
};

fn main() {
  let console_layer = tracing_web::MakeWebConsoleWriter::new();
  let fmt_layer = tracing_subscriber::fmt::layer()
    .with_ansi(false)
    .without_time()
    .with_writer(console_layer)
    .with_filter(PAGE_LEVEL);
  tracing_subscriber::registry()
    .with(fmt_layer)
    // The recent log for "Report a problem" (docs/bug-reports.md), redacted, in memory only.
    .with(teamcraft_client_api::debug_log::layer())
    // Spans of the core and the page for performance data and crash breadcrumbs (docs/tracing.md);
    // off until the crash report settings turn it on.
    .with(teamcraft_client_api::crash_reporting::trace::layer())
    .init();
  teamcraft_ui::mount();
}
