//! WebKitGTK defaults for an app that stays open all day (docs/performance.md, "Desktop on Linux:
//! memory"). WebKitGTK starts as a web browser: it keeps pages for back and forward and up to 64 MiB
//! of resources no page shows any more (decoded pictures among them) in case they come back. The
//! app is one page that never navigates, and the pictures it shows are `blob:` URLs that are revoked
//! when they go, so neither cache is ever hit; they only hold memory.
//!
//! WebKit also only starts giving memory back when its web process passes a third of a limit that
//! defaults to the machine's RAM, at most 3 GB: about 1 GB on any current machine, far above the
//! app's budget of 400 MiB. [`set_memory_pressure`] lowers that limit (issue #1708).

use std::time::Duration;

use webkit2gtk::{CacheModel, MemoryPressureSettings, SettingsExt, WebContextExt, WebViewExt};

/// The web process's memory limit WebKit's thresholds are fractions of, in MiB. `0` keeps WebKit's
/// own (the machine's RAM, at most 3 GB).
const LIMIT_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_LIMIT_MIB";
/// Fraction of the limit above which WebKit releases what it can do without (caches it can
/// rebuild).
const CONSERVATIVE_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_CONSERVATIVE";
/// Fraction of the limit above which WebKit also releases what costs time to rebuild (JIT code,
/// a full garbage collection).
const STRICT_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_STRICT";
/// How often WebKit measures the web process, in seconds.
const POLL_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_POLL_SECS";

/// Sets the memory caches of the webview's process to what a single-page app uses.
pub fn configure(webview: &webkit2gtk::WebView) {
  // Keeps no dead resources and no back/forward pages; live resources are capped as before.
  if let Some(context) = webview.context() {
    context.set_cache_model(CacheModel::DocumentViewer);
  }
  // wry turns it on; it keeps the whole page when navigating away, which the app never does.
  if let Some(settings) = WebViewExt::settings(webview) {
    settings.set_enable_page_cache(false);
  }
}

/// Sets WebKit's memory pressure settings for the web contexts created after this call (the
/// vendored wry applies them, crates/vendor/wry/VENDORED.md). Call it on the main thread before the
/// first webview is built: WebKit reads them only when a web context is created.
pub fn set_memory_pressure() {
  let pressure = MemoryPressure::from_env();
  let Some(limit_mib) = pressure.maybe_limit_mib else {
    wry::set_memory_pressure_settings(None);
    return;
  };
  let mut settings = MemoryPressureSettings::new();
  settings.set_memory_limit(limit_mib);
  settings.set_conservative_threshold(pressure.conservative_fraction);
  settings.set_strict_threshold(pressure.strict_fraction);
  // Never killed: WebKit would end the page and the app reload it (webview_recovery.rs).
  settings.set_kill_threshold(0.0);
  settings.set_poll_interval(pressure.poll_interval.as_secs_f64());
  tracing::info!(
    limit_mib,
    conservative = pressure.conservative_fraction,
    strict = pressure.strict_fraction,
    poll_secs = pressure.poll_interval.as_secs_f64(),
    "webview memory pressure settings"
  );
  wry::set_memory_pressure_settings(Some(settings));
}

/// When WebKit starts releasing memory in the app's web process.
#[derive(Clone, Copy, Debug, PartialEq)]
struct MemoryPressure {
  /// `None`: WebKit's own settings.
  maybe_limit_mib: Option<u32>,
  conservative_fraction: f64,
  strict_fraction: f64,
  poll_interval: Duration,
}

impl MemoryPressure {
  /// The defaults, measured on the large workspace of `tests/e2e` `desktop_perf.rs`
  /// (docs/performance.md, "Desktop on Linux: memory pressure and the allocator").
  const DEFAULT: Self = Self {
    maybe_limit_mib: Some(512),
    conservative_fraction: 0.5,
    strict_fraction: 0.7,
    poll_interval: Duration::from_secs(10),
  };

  fn from_env() -> Self {
    Self::parse(|name| std::env::var(name).ok())
  }

  /// Each variable that is set and valid replaces its default; thresholds that don't keep
  /// `0 < conservative < strict < 1` fall back to both defaults.
  fn parse(var: impl Fn(&str) -> Option<String>) -> Self {
    let default = Self::DEFAULT;
    let maybe_limit_mib = match parse_var::<u32>(&var, LIMIT_ENV) {
      Some(0) => None,
      Some(limit_mib) => Some(limit_mib),
      None => default.maybe_limit_mib,
    };
    let conservative_fraction = parse_var(&var, CONSERVATIVE_ENV).unwrap_or(default.conservative_fraction);
    let strict_fraction = parse_var(&var, STRICT_ENV).unwrap_or(default.strict_fraction);
    let are_thresholds_valid =
      0.0 < conservative_fraction && conservative_fraction < strict_fraction && strict_fraction < 1.0;
    let (conservative_fraction, strict_fraction) = match are_thresholds_valid {
      true => (conservative_fraction, strict_fraction),
      false => {
        tracing::warn!(
          conservative_fraction,
          strict_fraction,
          "webview memory thresholds must keep 0 < conservative < strict < 1; using the defaults"
        );
        (default.conservative_fraction, default.strict_fraction)
      },
    };
    let poll_interval = parse_var::<f64>(&var, POLL_ENV)
      .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
      .map_or(default.poll_interval, Duration::from_secs_f64);
    Self {
      maybe_limit_mib,
      conservative_fraction,
      strict_fraction,
      poll_interval,
    }
  }
}

/// The variable's value, or `None` when it is unset or not a `T` (logged).
fn parse_var<T: std::str::FromStr>(var: &impl Fn(&str) -> Option<String>, name: &str) -> Option<T> {
  let value = var(name)?;
  let parsed = value.trim().parse().ok();
  if parsed.is_none() {
    tracing::warn!(
      variable = name,
      value,
      "ignoring an invalid webview memory setting"
    );
  }
  parsed
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;

  use super::*;

  #[test]
  fn nothing_set_gives_the_defaults() {
    assert_eq!(parse(&[]), MemoryPressure::DEFAULT);
  }

  #[test]
  fn each_variable_replaces_its_default() {
    let pressure = parse(&[
      (LIMIT_ENV, "768"),
      (CONSERVATIVE_ENV, "0.3"),
      (STRICT_ENV, "0.6"),
      (POLL_ENV, "2.5"),
    ]);
    assert_eq!(
      pressure,
      MemoryPressure {
        maybe_limit_mib: Some(768),
        conservative_fraction: 0.3,
        strict_fraction: 0.6,
        poll_interval: Duration::from_millis(2500),
      }
    );
  }

  #[test]
  fn a_limit_of_zero_keeps_webkits_own_settings() {
    assert_eq!(parse(&[(LIMIT_ENV, "0")]).maybe_limit_mib, None);
  }

  #[test]
  fn invalid_values_fall_back_to_the_defaults() {
    let pressure = parse(&[
      (LIMIT_ENV, "a lot"),
      (CONSERVATIVE_ENV, "0.4"),
      (STRICT_ENV, "0.3"),
      (POLL_ENV, "0"),
    ]);
    assert_eq!(pressure, MemoryPressure::DEFAULT);
    let pressure = parse(&[(STRICT_ENV, "1.5")]);
    assert_eq!(pressure.strict_fraction, MemoryPressure::DEFAULT.strict_fraction);
    assert_eq!(
      parse(&[(POLL_ENV, "-3")]).poll_interval,
      MemoryPressure::DEFAULT.poll_interval
    );
  }

  fn parse(vars: &[(&str, &str)]) -> MemoryPressure {
    let vars: HashMap<String, String> = vars
      .iter()
      .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
      .collect();
    MemoryPressure::parse(|name| vars.get(name).cloned())
  }
}
