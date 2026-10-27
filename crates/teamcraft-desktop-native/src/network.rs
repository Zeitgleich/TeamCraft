//! Calls back when the network comes back, so the shell reconnects at once instead of waiting for
//! the connection supervisor's backoff (issue #883: after a wake-up the network is often only up
//! seconds later). Linux has its own hook in the shell (NetworkManager); here are macOS
//! (Network.framework's path monitor) and Windows (the IP helper's connectivity hint).

use std::sync::Mutex;

/// Why the network watch could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum NetworkWatchError {
  #[error("network notifications are not available on this platform")]
  Unsupported,
  #[cfg(target_os = "macos")]
  #[error("the system could not create a network path monitor")]
  Create,
  #[cfg(windows)]
  #[error("the system refused the network notification registration (error {code})")]
  Register { code: u32 },
}

/// Calls `on_up` (from an OS thread, so keep it short and non-blocking) every time the system
/// reports the network usable again after it was not, until the process ends. The state at
/// installation does not count.
pub fn watch_network_up(on_up: impl Fn() + Send + Sync + 'static) -> Result<(), NetworkWatchError> {
  imp::watch_network_up(Watcher::new(Box::new(on_up)))
}

type OnUp = Box<dyn Fn() + Send + Sync + 'static>;

/// Turns the system's reports (up or not, repeated for every path change) into "it is back".
struct Watcher {
  on_up: OnUp,
  maybe_was_up: Mutex<Option<bool>>,
}

impl Watcher {
  fn new(on_up: OnUp) -> Self {
    Self {
      on_up,
      maybe_was_up: Mutex::new(None),
    }
  }

  /// One report from the system; calls `on_up` when it brings the network back.
  #[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
  fn report(&self, is_up: bool) {
    let has_come_back = match self.maybe_was_up.lock() {
      Ok(mut slot) => is_back(slot.replace(is_up), is_up),
      Err(_) => false,
    };
    if has_come_back {
      (self.on_up)();
    }
  }
}

/// The network is back: it is up now and was reported down before (not just at the first report).
fn is_back(maybe_was_up: Option<bool>, is_up: bool) -> bool {
  maybe_was_up == Some(false) && is_up
}

#[cfg(target_os = "macos")]
mod imp {
  use std::ffi::c_void;
  use std::panic::{AssertUnwindSafe, catch_unwind};

  use block2::{Block, RcBlock};

  use super::{NetworkWatchError, Watcher};

  /// `nw_path_status_satisfied`: the path can be used.
  const NW_PATH_STATUS_SATISFIED: i32 = 1;
  /// `QOS_CLASS_UTILITY`: path updates are not latency-critical for the UI.
  const QOS_CLASS_UTILITY: isize = 0x11;

  // Network.framework and libdispatch (libSystem); the objects are opaque OS objects.
  #[link(name = "Network", kind = "framework")]
  #[allow(unsafe_code)]
  unsafe extern "C" {
    fn nw_path_monitor_create() -> *mut c_void;
    fn nw_path_monitor_set_queue(monitor: *mut c_void, queue: *mut c_void);
    fn nw_path_monitor_set_update_handler(monitor: *mut c_void, handler: &Block<dyn Fn(*mut c_void)>);
    fn nw_path_monitor_start(monitor: *mut c_void);
    fn nw_path_get_status(path: *mut c_void) -> i32;
  }

  #[allow(unsafe_code)]
  unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
  }

  pub fn watch_network_up(watcher: Watcher) -> Result<(), NetworkWatchError> {
    // SAFETY: plain constructor without arguments; a null result is checked below.
    #[allow(unsafe_code)]
    let monitor = unsafe { nw_path_monitor_create() };
    if monitor.is_null() {
      return Err(NetworkWatchError::Create);
    }
    let handler = RcBlock::new(move |path: *mut c_void| {
      // SAFETY: the monitor hands a valid `nw_path_t` for the duration of the handler call.
      #[allow(unsafe_code)]
      let status = unsafe { nw_path_get_status(path) };
      // A panic must not unwind into libdispatch; the next path change tries again.
      if catch_unwind(AssertUnwindSafe(|| {
        watcher.report(status == NW_PATH_STATUS_SATISFIED)
      }))
      .is_err()
      {
        tracing::error!("network callback panicked");
      }
    });
    // SAFETY: `monitor` is a fresh, non-null path monitor; the global queue is a static system
    // object; the monitor copies the handler block before returning. The monitor is never
    // cancelled or released: it lives for the rest of the process, like the resume observer.
    #[allow(unsafe_code)]
    unsafe {
      nw_path_monitor_set_queue(monitor, dispatch_get_global_queue(QOS_CLASS_UTILITY, 0));
      nw_path_monitor_set_update_handler(monitor, &handler);
      nw_path_monitor_start(monitor);
    }
    Ok(())
  }
}

#[cfg(windows)]
mod imp {
  use std::ffi::c_void;
  use std::panic::{AssertUnwindSafe, catch_unwind};

  use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
  use windows::Win32::NetworkManagement::IpHelper::NotifyNetworkConnectivityHintChange;
  use windows::Win32::Networking::WinSock::{
    NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityLevelHintConstrainedInternetAccess,
    NetworkConnectivityLevelHintInternetAccess,
  };

  use super::{NetworkWatchError, Watcher};

  pub fn watch_network_up(watcher: Watcher) -> Result<(), NetworkWatchError> {
    // Lives for the rest of the process: the system calls back at any time and nothing unregisters.
    let context = Box::into_raw(Box::new(watcher));
    let mut handle = HANDLE::default();
    // SAFETY: `on_hint` matches the callback type; `context` stays allocated because it is never
    // freed, and `on_hint` only reads it; `handle` is a valid out slot. The initial notification
    // reports the current state, which `Watcher` does not count as "back".
    #[allow(unsafe_code)]
    let status = unsafe {
      NotifyNetworkConnectivityHintChange(
        Some(on_hint),
        Some(context.cast::<c_void>().cast_const()),
        true,
        &mut handle,
      )
    };
    if status == ERROR_SUCCESS {
      Ok(())
    } else {
      Err(NetworkWatchError::Register { code: status.0 })
    }
  }

  /// The system's connectivity callback, on a thread of its own.
  #[allow(unsafe_code)]
  unsafe extern "system" fn on_hint(context: *const c_void, hint: NL_NETWORK_CONNECTIVITY_HINT) {
    // SAFETY: `context` is the `Watcher` leaked by `watch_network_up`, never freed and only shared.
    let watcher = unsafe { &*context.cast::<Watcher>() };
    let is_up = hint.ConnectivityLevel == NetworkConnectivityLevelHintInternetAccess
      || hint.ConnectivityLevel == NetworkConnectivityLevelHintConstrainedInternetAccess;
    // A panic must not unwind into the system's thread; the next change tries again.
    if catch_unwind(AssertUnwindSafe(|| watcher.report(is_up))).is_err() {
      tracing::error!("network callback panicked");
    }
  }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
  use super::{NetworkWatchError, Watcher};

  pub fn watch_network_up(_watcher: Watcher) -> Result<(), NetworkWatchError> {
    Err(NetworkWatchError::Unsupported)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;

  #[test]
  fn only_a_report_after_a_down_one_counts_as_back() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&calls);
    let watcher = Watcher::new(Box::new(move || {
      counting.fetch_add(1, Ordering::SeqCst);
    }));
    watcher.report(true);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the state at installation");
    watcher.report(false);
    watcher.report(false);
    watcher.report(true);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    watcher.report(true);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "another path change while up");
  }

  #[cfg(not(any(target_os = "macos", windows)))]
  #[test]
  fn other_platforms_report_unsupported() {
    assert!(matches!(
      watch_network_up(|| {}),
      Err(NetworkWatchError::Unsupported)
    ));
  }
}
