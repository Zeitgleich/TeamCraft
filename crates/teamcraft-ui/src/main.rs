//! Browser entry point built by `trunk`.

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn main() {
  let console_layer = tracing_web::MakeWebConsoleWriter::new();
  let fmt_layer = tracing_subscriber::fmt::layer()
    .with_ansi(false)
    .without_time()
    .with_writer(console_layer);
  tracing_subscriber::registry().with(fmt_layer).init();
  teamcraft_ui::mount();
}
