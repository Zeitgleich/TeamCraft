//! Calls back when the computer resumes from sleep, so the shell reconnects at once instead of
//! waiting for the connection supervisor's next check (robustness-audit.md, "Desktop sleep/resume").
//! Linux has its own hook in the shell (logind); here are macOS and Windows.

/// Why a resume hook could not be installed.
#[derive(Debug, thiserror::Error)]
pub enum ResumeWatchError {
  #[error("resume notifications are not available on this platform")]
  Unsupported,
  #[cfg(windows)]
  #[error("the system refused the resume notification registration (error {code})")]
  Register { code: u32 },
}

/// Calls `on_resume` (from an OS thread, so keep it short and non-blocking) every time the
/// computer wakes up, until the process ends.
pub fn watch_resume(on_resume: impl Fn() + Send + Sync + 'static) -> Result<(), ResumeWatchError> {
  imp::watch_resume(Box::new(on_resume))
}

type OnResume = Box<dyn Fn() + Send + Sync + 'static>;

#[cfg(target_os = "macos")]
mod imp {
  use std::ptr::NonNull;

  use block2::RcBlock;
  use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification};
  use objc2_foundation::NSNotification;

  use super::{OnResume, ResumeWatchError};

  pub fn watch_resume(on_resume: OnResume) -> Result<(), ResumeWatchError> {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let block = RcBlock::new(move |_notification: NonNull<NSNotification>| on_resume());
    // SAFETY: the name is the system's static notification name; no sender object and no operation
    // queue are given (the block then runs on the posting thread); the block is `Send + Sync`
    // because `on_resume` is, which is what "block must be sendable" asks for.
    #[allow(unsafe_code)]
    let observer = unsafe {
      center.addObserverForName_object_queue_usingBlock(
        Some(NSWorkspaceDidWakeNotification),
        None,
        None,
        &block,
      )
    };
    // The center retains the observer and the copied block for as long as it exists, which is the
    // process lifetime; dropping our references does not unregister it.
    drop(observer);
    Ok(())
  }
}

#[cfg(windows)]
mod imp {
  use std::ffi::c_void;
  use std::panic::{AssertUnwindSafe, catch_unwind};

  use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
  use windows::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification,
  };
  use windows::Win32::UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC};

  use super::{OnResume, ResumeWatchError};

  pub fn watch_resume(on_resume: OnResume) -> Result<(), ResumeWatchError> {
    // Lives for the rest of the process: the system calls back at any time and nothing unregisters.
    let context = Box::into_raw(Box::new(on_resume));
    let mut parameters = DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
      Callback: Some(on_power_event),
      Context: context.cast::<c_void>(),
    };
    let mut registration: *mut c_void = std::ptr::null_mut();
    // SAFETY: with `DEVICE_NOTIFY_CALLBACK` the recipient is a pointer to a
    // `DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS`, valid for the duration of the call (the system copies it);
    // `registration` is a valid out slot; `context` stays allocated because it is never freed, and
    // `on_power_event` only reads it.
    #[allow(unsafe_code)]
    let status = unsafe {
      PowerRegisterSuspendResumeNotification(
        DEVICE_NOTIFY_CALLBACK,
        HANDLE(std::ptr::from_mut(&mut parameters).cast::<c_void>()),
        &mut registration,
      )
    };
    if status == ERROR_SUCCESS {
      Ok(())
    } else {
      Err(ResumeWatchError::Register { code: status.0 })
    }
  }

  /// The system's power callback, on a thread of its own.
  #[allow(unsafe_code)]
  unsafe extern "system" fn on_power_event(
    context: *const c_void,
    kind: u32,
    _setting: *const c_void,
  ) -> u32 {
    if kind == PBT_APMRESUMEAUTOMATIC {
      // SAFETY: `context` is the `OnResume` leaked by `watch_resume`, never freed and only shared.
      let on_resume = unsafe { &*context.cast::<OnResume>() };
      // A panic must not unwind into the system's thread; the next resume tries again.
      if catch_unwind(AssertUnwindSafe(on_resume)).is_err() {
        tracing::error!("resume callback panicked");
      }
    }
    ERROR_SUCCESS.0
  }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
  use super::{OnResume, ResumeWatchError};

  pub fn watch_resume(_on_resume: OnResume) -> Result<(), ResumeWatchError> {
    Err(ResumeWatchError::Unsupported)
  }
}

#[cfg(test)]
mod tests {
  #[cfg(not(any(target_os = "macos", windows)))]
  #[test]
  fn other_platforms_report_unsupported() {
    assert!(matches!(
      super::watch_resume(|| {}),
      Err(super::ResumeWatchError::Unsupported)
    ));
  }
}
