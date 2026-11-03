//! The version of WebKitGTK at run time: the app runs on the distribution's library, and only that
//! tells whether a known crash of it may be fixed (#824). webkit2gtk-rs has no binding for it.

/// The WebKitGTK this process runs on, as `2.54.2`.
#[allow(unsafe_code)]
pub fn webkit_version() -> String {
  // SAFETY: these functions take nothing, touch no state and return constants compiled into the
  // library; they may be called before GTK or WebKit are set up.
  let (major, minor, micro) = unsafe {
    (
      webkit2gtk_sys::webkit_get_major_version(),
      webkit2gtk_sys::webkit_get_minor_version(),
      webkit2gtk_sys::webkit_get_micro_version(),
    )
  };
  format!("{major}.{minor}.{micro}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_version_has_three_numbers() {
    let version = webkit_version();
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "{version}");
    assert!(parts.iter().all(|part| part.parse::<u32>().is_ok()), "{version}");
  }
}
