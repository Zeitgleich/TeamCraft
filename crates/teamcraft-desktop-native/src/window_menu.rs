//! The system window menu (restore, move, size, minimize, maximize, close) on a right-click on the
//! custom title bar (#174). A window without a title bar gets no menu from Windows, so the shell
//! opens the one `GetSystemMenu` returns at the pointer and posts the chosen command back, as the
//! caption's own right-click does (the same menu Alt+Space opens).

#[cfg(windows)]
pub use imp::show_window_menu;

#[cfg(windows)]
mod imp {
  use std::ffi::c_void;

  use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
  use windows::Win32::UI::WindowsAndMessaging::{
    EnableMenuItem, GetCursorPos, GetSystemMenu, IsZoomed, MF_ENABLED, MF_GRAYED, PostMessageW, SC_MAXIMIZE,
    SC_MINIMIZE, SC_RESTORE, SC_SIZE, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu,
    WM_SYSCOMMAND,
  };

  /// Opens the system menu of the window `hwnd` (a raw handle, as Tauri's `hwnd()` gives it) at the
  /// pointer, and carries out the command picked. Call on the window's UI thread; it returns when
  /// the menu closes. Does nothing when the window has no system menu.
  pub fn show_window_menu(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: plain user32 calls on a window handle of this process; a stale handle makes them fail
    // (returning null or false), which is handled below. `point` is a valid out slot.
    #[allow(unsafe_code)]
    unsafe {
      let menu = GetSystemMenu(hwnd, false);
      if menu.is_invalid() {
        return;
      }
      let mut point = POINT::default();
      if GetCursorPos(&mut point).is_err() {
        return;
      }
      // Without a caption the menu does not know which entries apply: maximized windows restore,
      // others maximize; neither can be resized when maximized.
      let is_maximized = IsZoomed(hwnd).as_bool();
      let enabled = |is_enabled: bool| if is_enabled { MF_ENABLED } else { MF_GRAYED };
      let _ = EnableMenuItem(menu, SC_RESTORE, enabled(is_maximized));
      let _ = EnableMenuItem(menu, SC_MAXIMIZE, enabled(!is_maximized));
      let _ = EnableMenuItem(menu, SC_MINIMIZE, MF_ENABLED);
      let _ = EnableMenuItem(menu, SC_SIZE, enabled(!is_maximized));
      // The menu closes on a click elsewhere only when its window is in front.
      let _ = SetForegroundWindow(hwnd);
      let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        None,
        hwnd,
        None,
      );
      let id = command.0;
      if id > 0 {
        // An error means the window is gone; nothing to do then.
        let _ = PostMessageW(Some(hwnd), WM_SYSCOMMAND, WPARAM(id as usize), LPARAM(0));
      }
    }
  }
}
