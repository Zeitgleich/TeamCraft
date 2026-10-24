//! Linux: whether libayatana-appindicator / libappindicator can be loaded. The tray crate loads it
//! with `dlopen` on first use and panics when it is missing (and release builds abort on panic), so
//! the tray is only created when the library is found. The search mirrors the dynamic loader's:
//! `LD_LIBRARY_PATH`, the executable's `$ORIGIN/../lib` (AppImage RUNPATH), the loader cache and
//! the default directories. A library the loader would find elsewhere only costs the tray icon.

use std::path::{Path, PathBuf};

/// The names libappindicator-sys tries, in its order (the unversioned ones with its default
/// `backcompat` feature).
const LIBRARY_NAMES: [&str; 4] = [
  "libayatana-appindicator3.so.1",
  "libappindicator3.so.1",
  "libayatana-appindicator3.so",
  "libappindicator3.so",
];
const LOADER_CACHE: &str = "/etc/ld.so.cache";
/// glibc's built-in search directories (multiarch directories are in the loader cache).
const DEFAULT_DIRECTORIES: [&str; 4] = ["/lib", "/usr/lib", "/lib64", "/usr/lib64"];

/// Whether one of the appindicator libraries is installed where the dynamic loader looks.
pub fn is_available() -> bool {
  let maybe_cache = std::fs::read(LOADER_CACHE).ok();
  is_found(maybe_cache.as_deref(), &search_directories(), |path| {
    path.is_file()
  })
}

fn search_directories() -> Vec<PathBuf> {
  let mut directories: Vec<PathBuf> = std::env::var_os("LD_LIBRARY_PATH")
    .map(|paths| std::env::split_paths(&paths).collect())
    .unwrap_or_default();
  if let Ok(exe) = std::env::current_exe()
    && let Some(bin) = exe.parent()
  {
    directories.push(bin.join("../lib"));
  }
  directories.extend(DEFAULT_DIRECTORIES.iter().map(PathBuf::from));
  directories
}

fn is_found(maybe_cache: Option<&[u8]>, directories: &[PathBuf], exists: impl Fn(&Path) -> bool) -> bool {
  LIBRARY_NAMES.iter().any(|name| {
    maybe_cache.is_some_and(|cache| cache_lists(cache, name))
      || directories.iter().any(|directory| exists(&directory.join(name)))
  })
}

/// The loader cache stores names and paths as NUL-terminated strings; a match must be a whole
/// file name (preceded by NUL or `/`), so `libappindicator3.so` doesn't match `…so.1`.
fn cache_lists(cache: &[u8], name: &str) -> bool {
  let mut needle = name.as_bytes().to_vec();
  needle.push(0);
  cache.windows(needle.len()).enumerate().any(|(start, window)| {
    window == needle.as_slice() && (start == 0 || matches!(cache[start - 1], 0 | b'/'))
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn missing_everywhere_is_not_found() {
    let cache = b"\0libgtk-3.so.0\0/usr/lib/x86_64-linux-gnu/libgtk-3.so.0\0";
    assert!(!is_found(Some(cache), &[PathBuf::from("/usr/lib")], |_| false));
    assert!(!is_found(None, &[], |_| true));
  }

  #[test]
  fn listed_in_the_loader_cache_is_found() {
    let cache = b"\0libayatana-appindicator3.so.1\0/usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1\0";
    assert!(is_found(Some(cache), &[], |_| false));
  }

  #[test]
  fn cache_match_needs_the_whole_file_name() {
    assert!(!cache_lists(b"\0libappindicator3.so.1\0", "libappindicator3.so"));
    assert!(!cache_lists(
      b"\0xlibappindicator3.so.1\0",
      "libappindicator3.so.1"
    ));
    assert!(cache_lists(
      b"/usr/lib/libappindicator3.so.1\0",
      "libappindicator3.so.1"
    ));
  }

  #[test]
  fn file_in_a_search_directory_is_found() {
    let bundled = PathBuf::from("/tmp/.mount_TeamCraft/usr/bin/../lib");
    let expected = bundled.join("libayatana-appindicator3.so");
    assert!(is_found(None, &[bundled], |path| path == expected));
  }
}
