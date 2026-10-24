//! Linux: whether libayatana-appindicator / libappindicator can be loaded. The tray crate loads it
//! with `dlopen` on first use and panics when it is missing (and release builds abort on panic), so
//! the tray is only created when the library is found. The search mirrors the dynamic loader's:
//! `LD_LIBRARY_PATH`, the executable's `$ORIGIN/../lib` (AppImage RUNPATH), the loader cache and
//! the default directories. Like the loader, it skips libraries built for another architecture (a
//! 32-bit library in `/usr/lib` must not count). A library the loader would find elsewhere only
//! costs the tray icon.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
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
/// The ELF identification and `e_type`/`e_machine`: all that is compared.
const ELF_HEADER_LEN: usize = 20;
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const ELF_CLASS_32: u8 = 1;
const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE: u8 = 1;
const ELF_DATA_BIG: u8 = 2;

/// The ELF header of a library file (its first bytes).
type ElfHeader = [u8; ELF_HEADER_LEN];

/// What a library must be built for to load into this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ElfTarget {
  class: u8,
  data: u8,
  /// `e_machine`; `None` on architectures not listed here, where class and byte order decide.
  maybe_machine: Option<u16>,
}

/// Whether one of the appindicator libraries for this architecture is installed where the dynamic
/// loader looks.
pub fn is_available() -> bool {
  let maybe_cache = std::fs::read(LOADER_CACHE).ok();
  is_found(
    maybe_cache.as_deref(),
    &search_directories(),
    ElfTarget::current(),
    read_elf_header,
  )
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

fn is_found(
  maybe_cache: Option<&[u8]>,
  directories: &[PathBuf],
  target: ElfTarget,
  read_header: impl Fn(&Path) -> Option<ElfHeader>,
) -> bool {
  LIBRARY_NAMES.iter().any(|name| {
    cache_paths(maybe_cache.unwrap_or_default(), name)
      .chain(directories.iter().map(|directory| directory.join(name)))
      .any(|path| read_header(&path).is_some_and(|header| target.matches(&header)))
  })
}

/// The paths the loader cache lists for `name`. The cache stores names and paths as NUL-terminated
/// strings; a path must be absolute and end in the whole file name, so `libappindicator3.so`
/// doesn't match `…so.1`. Paths of every architecture are listed; the caller checks the file.
fn cache_paths<'a>(cache: &'a [u8], name: &'a str) -> impl Iterator<Item = PathBuf> + 'a {
  cache.split(|byte| *byte == 0).filter_map(move |entry| {
    let path = Path::new(OsStr::from_bytes(entry));
    let is_match = entry.starts_with(b"/") && path.file_name() == Some(OsStr::new(name));
    is_match.then(|| path.to_path_buf())
  })
}

/// The first bytes of a file (following symlinks); `None` when it is missing or shorter.
fn read_elf_header(path: &Path) -> Option<ElfHeader> {
  let mut header = [0; ELF_HEADER_LEN];
  std::fs::File::open(path).ok()?.read_exact(&mut header).ok()?;
  Some(header)
}

impl ElfTarget {
  fn current() -> Self {
    let class = if cfg!(target_pointer_width = "64") {
      ELF_CLASS_64
    } else {
      ELF_CLASS_32
    };
    let data = if cfg!(target_endian = "little") {
      ELF_DATA_LITTLE
    } else {
      ELF_DATA_BIG
    };
    Self {
      class,
      data,
      maybe_machine: machine(std::env::consts::ARCH),
    }
  }

  fn matches(&self, header: &ElfHeader) -> bool {
    let machine_bytes = [header[18], header[19]];
    let machine = if header[5] == ELF_DATA_BIG {
      u16::from_be_bytes(machine_bytes)
    } else {
      u16::from_le_bytes(machine_bytes)
    };
    header[..4] == ELF_MAGIC
      && header[4] == self.class
      && header[5] == self.data
      && self.maybe_machine.is_none_or(|expected| machine == expected)
  }
}

/// `e_machine` of the architectures Linux desktops run on (the ELF spec's `EM_*` values).
fn machine(arch: &str) -> Option<u16> {
  match arch {
    "x86" => Some(3),
    "powerpc64" => Some(21),
    "s390x" => Some(22),
    "arm" => Some(40),
    "x86_64" => Some(62),
    "aarch64" => Some(183),
    "riscv64" => Some(243),
    "loongarch64" => Some(258),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const X86_64: ElfTarget = ElfTarget {
    class: ELF_CLASS_64,
    data: ELF_DATA_LITTLE,
    maybe_machine: Some(62),
  };
  const CACHED_PATH: &str = "/usr/lib/x86_64-linux-gnu/libayatana-appindicator3.so.1";

  #[test]
  fn missing_everywhere_is_not_found() {
    let cache = b"\0libgtk-3.so.0\0/usr/lib/x86_64-linux-gnu/libgtk-3.so.0\0";
    assert!(!is_found(
      Some(cache),
      &[PathBuf::from("/usr/lib")],
      X86_64,
      |_| None
    ));
    assert!(!is_found(None, &[], X86_64, |_| Some(elf(ELF_CLASS_64, 62))));
  }

  #[test]
  fn listed_in_the_loader_cache_is_found() {
    let cache = cache_listing(CACHED_PATH);
    let found = is_found(Some(&cache), &[], X86_64, |path| {
      (path == Path::new(CACHED_PATH)).then(|| elf(ELF_CLASS_64, 62))
    });
    assert!(found);
  }

  #[test]
  fn file_in_a_search_directory_is_found() {
    let bundled = PathBuf::from("/tmp/.mount_TeamCraft/usr/bin/../lib");
    let expected = bundled.join("libayatana-appindicator3.so");
    let found = is_found(None, &[bundled], X86_64, |path| {
      (path == expected).then(|| elf(ELF_CLASS_64, 62))
    });
    assert!(found);
  }

  #[test]
  fn cache_match_needs_the_whole_file_name() {
    let paths = |cache: &[u8], name| cache_paths(cache, name).collect::<Vec<_>>();
    assert!(paths(b"\0/usr/lib/libappindicator3.so.1\0", "libappindicator3.so").is_empty());
    assert!(paths(b"\0/usr/lib/xlibappindicator3.so.1\0", "libappindicator3.so.1").is_empty());
    // A bare name (the cache's key) is no path.
    assert!(paths(b"\0libappindicator3.so.1\0", "libappindicator3.so.1").is_empty());
    assert_eq!(
      paths(b"/usr/lib/libappindicator3.so.1\0", "libappindicator3.so.1"),
      [PathBuf::from("/usr/lib/libappindicator3.so.1")]
    );
  }

  mod architecture {
    use super::*;

    #[test]
    fn a_32_bit_library_does_not_count_for_a_64_bit_process() {
      let found = is_found(None, &[PathBuf::from("/usr/lib")], X86_64, |_| {
        Some(elf(ELF_CLASS_32, 3))
      });
      assert!(!found);
    }

    #[test]
    fn a_library_of_another_machine_does_not_count() {
      let cache = cache_listing("/usr/lib/aarch64-linux-gnu/libayatana-appindicator3.so.1");
      assert!(!is_found(Some(&cache), &[], X86_64, |_| Some(elf(
        ELF_CLASS_64,
        183
      ))));
    }

    #[test]
    fn the_matching_library_counts_next_to_a_foreign_one() {
      let cache = cache_listing(CACHED_PATH);
      let found = is_found(Some(&cache), &[PathBuf::from("/usr/lib")], X86_64, |path| {
        if path.starts_with("/usr/lib/x86_64-linux-gnu") {
          Some(elf(ELF_CLASS_64, 62))
        } else {
          Some(elf(ELF_CLASS_32, 3))
        }
      });
      assert!(found);
    }

    #[test]
    fn a_linker_script_does_not_count() {
      // Unversioned `.so` names are often linker scripts, which dlopen can't load.
      assert!(!X86_64.matches(b"/* GNU ld script\n   "));
    }

    #[test]
    fn an_unlisted_machine_is_decided_by_class_and_byte_order() {
      let target = ElfTarget {
        maybe_machine: None,
        ..X86_64
      };
      assert!(target.matches(&elf(ELF_CLASS_64, 999)));
      assert!(!target.matches(&elf(ELF_CLASS_32, 999)));
    }

    #[test]
    fn this_process_matches_its_own_architecture() {
      let target = ElfTarget::current();
      let maybe_header = std::env::current_exe().ok().and_then(|exe| read_elf_header(&exe));
      assert!(maybe_header.is_some_and(|header| target.matches(&header)));
    }
  }

  /// A little-endian ELF header of a shared object.
  fn elf(class: u8, machine: u16) -> ElfHeader {
    let mut header = [0; ELF_HEADER_LEN];
    header[..4].copy_from_slice(&ELF_MAGIC);
    header[4] = class;
    header[5] = ELF_DATA_LITTLE;
    header[16] = 3;
    header[18..20].copy_from_slice(&machine.to_le_bytes());
    header
  }

  /// A loader cache excerpt listing `path` under its file name.
  fn cache_listing(path: &str) -> Vec<u8> {
    let name = Path::new(path)
      .file_name()
      .map(OsStr::as_bytes)
      .unwrap_or_default();
    [b"\0", name, b"\0", path.as_bytes(), b"\0"].concat()
  }
}
