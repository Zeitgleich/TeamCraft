//! Windows 11 shows its snap layouts flyout over a window's maximize button when the window
//! answers `WM_NCHITTEST` with `HTMAXBUTTON` there (#173). The custom title bar draws the button
//! in the page, so the shell tells this module where it is and the module answers for it.
//!
//! WebView2's child windows cover the whole client area and take the hit test first. They answer
//! `HTTRANSPARENT` over the button, which hands the test to the top-level window, and that answers
//! `HTMAXBUTTON`. The page no longer sees the pointer over the button, so the module reports it
//! entering and leaving (`on_hover`) for the hover look, and carries out the click itself.

/// The maximize button's rectangle in the window's client area, in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonRect {
  pub left: i32,
  pub top: i32,
  pub right: i32,
  pub bottom: i32,
}

impl ButtonRect {
  /// Whether a point (client coordinates) is on the button. The right and bottom edges are outside.
  pub fn contains(&self, x: i32, y: i32) -> bool {
    x >= self.left && x < self.right && y >= self.top && y < self.bottom
  }
}

#[cfg(windows)]
pub use imp::{SnapLayoutsError, install_snap_layouts, set_maximize_button};

#[cfg(windows)]
mod imp {
  use std::ffi::c_void;
  use std::panic::{AssertUnwindSafe, catch_unwind};
  use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
  use std::sync::{Mutex, OnceLock, PoisonError};

  use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
  use windows::Win32::Graphics::Gdi::ClientToScreen;
  use windows::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
  };
  use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
  use windows::Win32::UI::WindowsAndMessaging::{
    EnumChildWindows, HTMAXBUTTON, IsZoomed, PostMessageW, SC_MAXIMIZE, SC_RESTORE, WM_NCDESTROY,
    WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SYSCOMMAND,
  };
  use windows::core::BOOL;

  use super::ButtonRect;

  /// Subclass identifiers (any value, unique per procedure in this process).
  const TOP_LEVEL_SUBCLASS_ID: usize = 0x7443_0173;
  const CHILD_SUBCLASS_ID: usize = 0x7443_0174;
  /// `HTTRANSPARENT` as a window procedure's result: the test goes to the window below.
  const HIT_TRANSPARENT: isize = -1;

  /// Why the snap layouts could not be set up.
  #[derive(Debug, thiserror::Error)]
  pub enum SnapLayoutsError {
    #[error("the window procedure could not be subclassed")]
    Subclass,
    #[error("the snap layouts are already set up for a window")]
    AlreadyInstalled,
  }

  /// What the window procedures share; there is one main window per process.
  struct State {
    rect: Mutex<Option<ButtonRect>>,
    top_level: AtomicIsize,
    is_hovered: AtomicBool,
    on_hover: Box<dyn Fn(bool) + Send + Sync>,
  }

  static STATE: OnceLock<State> = OnceLock::new();

  /// Starts answering the hit test of the window `hwnd` (a raw handle, as Tauri's `hwnd()` gives it).
  /// `on_hover(true)` is called when the pointer enters the maximize button and `on_hover(false)`
  /// when it leaves, on the window's UI thread. Call on that thread. The button's place comes with
  /// [`set_maximize_button`].
  pub fn install_snap_layouts(
    hwnd: isize,
    on_hover: impl Fn(bool) + Send + Sync + 'static,
  ) -> Result<(), SnapLayoutsError> {
    let state = State {
      rect: Mutex::new(None),
      top_level: AtomicIsize::new(hwnd),
      is_hovered: AtomicBool::new(false),
      on_hover: Box::new(on_hover),
    };
    STATE.set(state).map_err(|_| SnapLayoutsError::AlreadyInstalled)?;
    // SAFETY: `hwnd` is the main window of this process and this runs on its thread, as
    // `SetWindowSubclass` requires; `top_level_proc` has the SUBCLASSPROC signature and needs no data.
    #[allow(unsafe_code)]
    let is_subclassed =
      unsafe { SetWindowSubclass(as_hwnd(hwnd), Some(top_level_proc), TOP_LEVEL_SUBCLASS_ID, 0).as_bool() };
    if is_subclassed {
      Ok(())
    } else {
      Err(SnapLayoutsError::Subclass)
    }
  }

  /// Tells where the maximize button is (`None`: there is none, as with the system title bar), and
  /// subclasses the windows WebView2 has created inside the window by now (it creates some late, so
  /// the page repeats this when its layout changes). Call on the window's UI thread.
  pub fn set_maximize_button(maybe_rect: Option<ButtonRect>) {
    let Some(state) = STATE.get() else {
      return;
    };
    *state.rect.lock().unwrap_or_else(PoisonError::into_inner) = maybe_rect;
    if maybe_rect.is_none() {
      set_hovered(state, false);
    }
    let top_level = as_hwnd(state.top_level.load(Ordering::Relaxed));
    // SAFETY: `top_level` is the main window; `subclass_child` has the WNDENUMPROC signature and
    // runs during the call, on this thread, as `SetWindowSubclass` requires. Subclassing a window
    // that is already subclassed with the same procedure and id only replaces its data.
    #[allow(unsafe_code)]
    unsafe {
      let _ = EnumChildWindows(Some(top_level), Some(subclass_child), LPARAM(0));
    }
  }

  /// WebView2's windows: over the button they let the top-level window answer.
  #[allow(unsafe_code)]
  unsafe extern "system" fn subclass_child(child: HWND, _data: LPARAM) -> BOOL {
    // SAFETY: `child` is a live window handed out by `EnumChildWindows` on the thread that owns it.
    unsafe {
      let _ = SetWindowSubclass(child, Some(child_proc), CHILD_SUBCLASS_ID, 0);
    }
    BOOL(1)
  }

  #[allow(unsafe_code)]
  unsafe extern "system" fn child_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
  ) -> LRESULT {
    if message == WM_NCHITTEST && is_over_button(lparam) {
      return LRESULT(HIT_TRANSPARENT);
    }
    if message == WM_NCDESTROY {
      // SAFETY: `hwnd` is the window this procedure is subclassing.
      unsafe {
        let _ = RemoveWindowSubclass(hwnd, Some(child_proc), CHILD_SUBCLASS_ID);
      }
    }
    // SAFETY: passes the message on to the next procedure of the subclass chain, as required.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
  }

  #[allow(unsafe_code)]
  unsafe extern "system" fn top_level_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
  ) -> LRESULT {
    match message {
      WM_NCHITTEST if is_over_button(lparam) => return LRESULT(HTMAXBUTTON as isize),
      WM_NCMOUSEMOVE if wparam.0 == HTMAXBUTTON as usize => {
        if let Some(state) = STATE.get() {
          set_hovered(state, true);
          let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE | TME_NONCLIENT,
            hwndTrack: hwnd,
            dwHoverTime: 0,
          };
          // SAFETY: `track` is a valid, initialised struct of the size it states.
          let _ = unsafe { TrackMouseEvent(&mut track) };
        }
      },
      WM_NCMOUSEMOVE | WM_NCMOUSELEAVE => {
        if let Some(state) = STATE.get() {
          set_hovered(state, false);
        }
      },
      // The press is ours, so Windows does not start its own caption button handling; the click
      // is the release over the button.
      WM_NCLBUTTONDOWN if wparam.0 == HTMAXBUTTON as usize => return LRESULT(0),
      WM_NCLBUTTONUP if wparam.0 == HTMAXBUTTON as usize => {
        // SAFETY: plain calls on the window this procedure belongs to. An error from the post
        // means the window is gone.
        unsafe {
          let command = if IsZoomed(hwnd).as_bool() {
            SC_RESTORE
          } else {
            SC_MAXIMIZE
          };
          let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(command as usize), LPARAM(0));
        }
        return LRESULT(0);
      },
      WM_NCDESTROY => {
        // SAFETY: `hwnd` is the window this procedure is subclassing.
        unsafe {
          let _ = RemoveWindowSubclass(hwnd, Some(top_level_proc), TOP_LEVEL_SUBCLASS_ID);
        }
      },
      _ => {},
    }
    // SAFETY: passes the message on to the next procedure of the subclass chain, as required.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
  }

  /// Whether the screen point of a hit test message (`lparam`) is on the maximize button.
  fn is_over_button(lparam: LPARAM) -> bool {
    let Some(state) = STATE.get() else {
      return false;
    };
    let Some(rect) = *state.rect.lock().unwrap_or_else(PoisonError::into_inner) else {
      return false;
    };
    let mut origin = POINT::default();
    // SAFETY: `origin` is a valid out slot; the handle is the main window, which a failing call
    // (a destroyed window) leaves at the zero point.
    #[allow(unsafe_code)]
    let is_ok =
      unsafe { ClientToScreen(as_hwnd(state.top_level.load(Ordering::Relaxed)), &mut origin).as_bool() };
    if !is_ok {
      return false;
    }
    // The point is two signed 16-bit words (negative on monitors left of or above the primary one).
    let x = i32::from((lparam.0 & 0xFFFF) as u16 as i16);
    let y = i32::from(((lparam.0 >> 16) & 0xFFFF) as u16 as i16);
    rect.contains(x - origin.x, y - origin.y)
  }

  fn set_hovered(state: &State, is_hovered: bool) {
    if state.is_hovered.swap(is_hovered, Ordering::Relaxed) != is_hovered {
      // A panic must not unwind into the window procedure's caller.
      if catch_unwind(AssertUnwindSafe(|| (state.on_hover)(is_hovered))).is_err() {
        tracing::error!("the maximize button hover callback panicked");
      }
    }
  }

  fn as_hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
  }
}

#[cfg(test)]
mod tests {
  use super::ButtonRect;

  #[test]
  fn the_button_contains_its_inside_but_not_its_far_edges() {
    let rect = ButtonRect {
      left: 100,
      top: 0,
      right: 146,
      bottom: 44,
    };
    assert!(rect.contains(100, 0));
    assert!(rect.contains(145, 43));
    assert!(!rect.contains(146, 10));
    assert!(!rect.contains(120, 44));
    assert!(!rect.contains(99, 10));
  }
}
