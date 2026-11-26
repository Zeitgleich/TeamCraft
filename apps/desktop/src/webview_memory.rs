//! WebKitGTK defaults for an app that stays open all day (docs/performance.md, "Desktop on Linux:
//! memory"). WebKitGTK starts as a web browser: it keeps pages for back and forward and up to 64 MiB
//! of resources no page shows any more (decoded pictures among them) in case they come back. The
//! app is one page that never navigates, and the pictures it shows are `blob:` URLs that are revoked
//! when they go, so neither cache is ever hit; they only hold memory.
//!
//! WebKit also only starts giving memory back when its web process passes a third of a limit that
//! defaults to the machine's RAM, at most 3 GB: about 1 GB on any current machine, far above the
//! app's budget of 400 MiB. [`set_memory_pressure`] lowers that first threshold (issue #1708).
//!
//! Only the first one, the conservative threshold: on WebKitGTK 2.54 the release at the strict
//! threshold deadlocks the web process (#2450). It frees the GPU resources of the compositor
//! thread under a lock that the flush it starts takes again on the same thread
//! (`SkiaGLContext::releaseUnusedResources` → `GrDirectContext::freeGpuResources`), and the main
//! thread waits for that thread for good. The conservative release only purges scratch resources,
//! which flushes nothing. So the strict threshold sits at the top of WebKit's own limit, further
//! away than WebKit's own default puts it, and the conservative one where #1708 measured it.

use std::time::Duration;

use webkit2gtk::{CacheModel, MemoryPressureSettings, SettingsExt, WebContextExt, WebViewExt};

/// The web process's memory limit WebKit's thresholds are fractions of, in MiB. `0` keeps WebKit's
/// own (the machine's RAM, at most 3 GB). Changing it moves both thresholds.
const LIMIT_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_LIMIT_MIB";
/// Fraction of the limit above which WebKit releases what it can do without (caches it can
/// rebuild).
const CONSERVATIVE_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_CONSERVATIVE";
/// Fraction of the limit above which WebKit also releases what costs time to rebuild (JIT code,
/// a full garbage collection, the compositor's GPU resources). Reaching it can deadlock WebKitGTK
/// 2.54's web process (#2450).
const STRICT_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_STRICT";
/// How often WebKit measures the web process, in seconds.
const POLL_ENV: &str = "TEAMCRAFT_WEBVIEW_MEMORY_POLL_SECS";
/// WebKit's own strict threshold, which a new `MemoryPressureSettings` starts with (its
/// conservative one is 0.33). Each setter refuses a value that would break `conservative < strict`
/// against the other's current value (a GLib assertion, the setting stays as it was), so the
/// thresholds are set in an order that keeps it.
const WEBKIT_STRICT_DEFAULT: f64 = 0.5;

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
  let settings = pressure.settings(limit_mib);
  tracing::info!(
    limit_mib,
    conservative = pressure.conservative_fraction,
    strict = pressure.strict_fraction,
    poll_secs = pressure.poll_interval.as_secs_f64(),
    "webview memory pressure settings"
  );
  wry::set_memory_pressure_settings(Some(settings));
}

/// One of WebKit's two memory pressure thresholds, as a fraction of the limit.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Threshold {
  Conservative(f64),
  Strict(f64),
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
  /// The defaults (docs/performance.md, "Desktop on Linux: memory pressure and the allocator").
  /// Conservative at 3072 MiB × 0.055 = 169 MiB, where #1708 measured 512 MiB × 0.33 on the large
  /// workspace of `tests/e2e` `desktop_perf.rs`. Strict at 3041 MiB, out of the page's reach:
  /// its release deadlocks WebKitGTK 2.54 (#2450, see the module's comment). The limit is
  /// WebKit's own largest, and WebKit's own strict default would be half of it.
  const DEFAULT: Self = Self {
    maybe_limit_mib: Some(3072),
    conservative_fraction: 0.055,
    strict_fraction: 0.99,
    poll_interval: Duration::from_secs(10),
  };

  fn from_env() -> Self {
    Self::parse(|name| std::env::var(name).ok())
  }

  /// WebKit's settings with these values and a limit of `limit_mib`.
  fn settings(&self, limit_mib: u32) -> MemoryPressureSettings {
    let mut settings = MemoryPressureSettings::new();
    settings.set_memory_limit(limit_mib);
    for threshold in self.thresholds_in_order() {
      match threshold {
        Threshold::Conservative(fraction) => settings.set_conservative_threshold(fraction),
        Threshold::Strict(fraction) => settings.set_strict_threshold(fraction),
      }
    }
    // Never killed: WebKit would end the page and the app reload it (webview_recovery.rs).
    settings.set_kill_threshold(0.0);
    settings.set_poll_interval(self.poll_interval.as_secs_f64());
    settings
  }

  /// The thresholds in the order WebKit takes both: a conservative threshold at or above WebKit's
  /// strict default only fits once the strict one is raised, a strict one at or below its
  /// conservative default only once that one is lowered. `parse` keeps `conservative < strict`, so
  /// one of the two orders always works.
  fn thresholds_in_order(&self) -> [Threshold; 2] {
    let conservative = Threshold::Conservative(self.conservative_fraction);
    let strict = Threshold::Strict(self.strict_fraction);
    if self.conservative_fraction >= WEBKIT_STRICT_DEFAULT {
      [strict, conservative]
    } else {
      [conservative, strict]
    }
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

  /// WebKit's own conservative threshold, which a new `MemoryPressureSettings` starts with.
  const WEBKIT_CONSERVATIVE_DEFAULT: f64 = 0.33;

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

  /// #2362: WebKit refuses a threshold that breaks `conservative < strict` against the other's
  /// value at that moment, and kept its own conservative third instead of 0.5. Replays the order
  /// against WebKit's rule, from its defaults (the settings object itself needs GTK).
  #[test]
  fn webkit_takes_both_thresholds_in_their_order() {
    let pairs = [
      (0.055, 0.99),
      (0.33, 0.7),
      (0.5, 0.7),
      (0.6, 0.9),
      (0.2, 0.3),
      (0.1, 0.45),
      (0.4, 0.45),
    ];
    for (conservative_fraction, strict_fraction) in pairs {
      let pressure = MemoryPressure {
        conservative_fraction,
        strict_fraction,
        ..MemoryPressure::DEFAULT
      };
      let (mut conservative, mut strict) = (WEBKIT_CONSERVATIVE_DEFAULT, WEBKIT_STRICT_DEFAULT);
      for threshold in pressure.thresholds_in_order() {
        match threshold {
          Threshold::Conservative(value) if value < strict => conservative = value,
          Threshold::Strict(value) if value > conservative => strict = value,
          refused => panic!("WebKit refuses {refused:?} for {conservative_fraction} / {strict_fraction}"),
        }
      }
      assert_eq!((conservative, strict), (conservative_fraction, strict_fraction));
    }
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
