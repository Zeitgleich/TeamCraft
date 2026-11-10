//! A SQLite VFS whose disk syncs are slow, as on a phone's flash (issue #1680): files are real
//! files, every `xSync` waits a set delay and is counted. Locks and the write-ahead log's shared
//! memory live in this process, which is all a test needs: every connection to the database is
//! one of its own.
//!
//! Written on `sqlite-plugin`'s safe `Vfs` trait: the workspace forbids `unsafe` in our crates.

use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use sqlite_plugin::flags::{AccessFlags, CreateMode, LockLevel, OpenKind, OpenMode, OpenOpts, ShmLockMode};
use sqlite_plugin::vars;
use sqlite_plugin::vfs::{RegisterOpts, Vfs, VfsHandle, VfsResult, register_static};

/// SQLite's number of locks in the shared memory of the write-ahead log (`SQLITE_SHM_NLOCK`).
const SHM_LOCKS: usize = 8;

/// A registered slow disk. Clones share the counters.
#[derive(Clone)]
pub struct SlowDisk {
  name: String,
  state: Arc<DiskState>,
}

/// What the disk counted so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DiskTally {
  /// Syncs of the database file.
  pub database_syncs: u64,
  /// Syncs of the write-ahead log.
  pub log_syncs: u64,
}

struct DiskState {
  sync_delay: Duration,
  database_syncs: AtomicU64,
  log_syncs: AtomicU64,
  /// Locks and shared memory per database file, shared by every connection to it.
  databases: Mutex<HashMap<PathBuf, Arc<Database>>>,
  next_handle: AtomicU64,
}

/// The VFS itself; SQLite owns it once it is registered.
struct SlowVfs {
  state: Arc<DiskState>,
}

/// One open file.
struct SlowFile {
  maybe_file: Option<File>,
  path: PathBuf,
  kind: OpenKind,
  is_readonly: bool,
  is_deleted_on_close: bool,
  /// Set for the main database file: its locks and shared memory.
  maybe_database: Option<Arc<Database>>,
  id: u64,
  lock: LockLevel,
  is_mapping_shm: bool,
}

#[derive(Default)]
struct Database {
  locks: Mutex<FileLocks>,
  shm: Mutex<Shm>,
}

/// SQLite's file locks (https://www.sqlite.org/lockingv3.html) between the handles of one file.
#[derive(Default)]
struct FileLocks {
  shared: HashSet<u64>,
  maybe_reserved: Option<u64>,
  maybe_pending: Option<u64>,
  maybe_exclusive: Option<u64>,
}

/// The write-ahead log's index: regions SQLite maps and writes through raw pointers, and its locks.
#[derive(Default)]
struct Shm {
  /// Cells: SQLite changes the bytes through the pointers it got from `shm_map`.
  regions: Vec<Box<[UnsafeCell<u8>]>>,
  mappers: HashSet<u64>,
  shared: [HashSet<u64>; SHM_LOCKS],
  exclusive: [Option<u64>; SHM_LOCKS],
}

impl SlowDisk {
  /// Registers a VFS (under a name of its own, not as the default) whose syncs take `sync_delay`.
  pub fn install(sync_delay: Duration) -> Self {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = format!("teamcraft-slow-disk-{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let state = Arc::new(DiskState {
      sync_delay,
      database_syncs: AtomicU64::new(0),
      log_syncs: AtomicU64::new(0),
      databases: Mutex::default(),
      next_handle: AtomicU64::new(0),
    });
    let vfs = SlowVfs {
      state: Arc::clone(&state),
    };
    register_static(
      CString::new(name.clone()).unwrap(),
      vfs,
      RegisterOpts { make_default: false },
    )
    .unwrap();
    Self { name, state }
  }

  /// A URI that opens the database at `path` on this disk.
  pub fn uri(&self, path: &Path) -> PathBuf {
    PathBuf::from(format!("file:{}?vfs={}", path.display(), self.name))
  }

  pub fn tally(&self) -> DiskTally {
    DiskTally {
      database_syncs: self.state.database_syncs.load(Ordering::Relaxed),
      log_syncs: self.state.log_syncs.load(Ordering::Relaxed),
    }
  }
}

impl DiskTally {
  pub fn syncs(&self) -> u64 {
    self.database_syncs + self.log_syncs
  }

  /// What was counted since `before`.
  pub fn since(&self, before: DiskTally) -> DiskTally {
    DiskTally {
      database_syncs: self.database_syncs - before.database_syncs,
      log_syncs: self.log_syncs - before.log_syncs,
    }
  }
}

impl Vfs for SlowVfs {
  type Handle = SlowFile;

  fn open(&self, maybe_path: Option<&str>, opts: OpenOpts) -> VfsResult<SlowFile> {
    let id = self.state.next_handle.fetch_add(1, Ordering::Relaxed);
    let path = match maybe_path {
      Some(path) => PathBuf::from(path),
      // A temporary file SQLite names none for (a sorter, a statement journal).
      None => std::env::temp_dir().join(format!("teamcraft-slow-disk-temp-{}-{id}", std::process::id())),
    };
    let mode = opts.mode();
    let mut options = OpenOptions::new();
    options.read(true);
    if let OpenMode::ReadWrite { create } = &mode {
      options.write(true);
      match create {
        CreateMode::None => {},
        CreateMode::Create => {
          options.create(true);
        },
        CreateMode::MustCreate => {
          options.create_new(true);
        },
      }
    }
    if maybe_path.is_none() {
      options.write(true).create(true);
    }
    let file = options.open(&path).map_err(|_| vars::SQLITE_CANTOPEN)?;
    let kind = opts.kind();
    let maybe_database = (kind == OpenKind::MainDb).then(|| {
      let mut databases = lock(&self.state.databases);
      Arc::clone(databases.entry(path.clone()).or_default())
    });
    Ok(SlowFile {
      maybe_file: Some(file),
      path,
      kind,
      is_readonly: mode.is_readonly(),
      is_deleted_on_close: opts.delete_on_close() || maybe_path.is_none(),
      maybe_database,
      id,
      lock: LockLevel::Unlocked,
      is_mapping_shm: false,
    })
  }

  fn delete(&self, path: &str) -> VfsResult<()> {
    match std::fs::remove_file(path) {
      Ok(()) => Ok(()),
      Err(error) if error.kind() == ErrorKind::NotFound => Err(vars::SQLITE_IOERR_DELETE_NOENT),
      Err(_) => Err(vars::SQLITE_IOERR_DELETE),
    }
  }

  fn access(&self, path: &str, flags: AccessFlags) -> VfsResult<bool> {
    let Ok(metadata) = std::fs::metadata(path) else {
      return Ok(false);
    };
    Ok(match flags {
      AccessFlags::Exists | AccessFlags::Read => true,
      AccessFlags::ReadWrite => !metadata.permissions().readonly(),
    })
  }

  fn file_size(&self, handle: &mut SlowFile) -> VfsResult<usize> {
    let length = handle
      .file()?
      .metadata()
      .map_err(|_| vars::SQLITE_IOERR_FSTAT)?
      .len();
    usize::try_from(length).map_err(|_| vars::SQLITE_IOERR_FSTAT)
  }

  fn truncate(&self, handle: &mut SlowFile, size: usize) -> VfsResult<()> {
    let size = u64::try_from(size).map_err(|_| vars::SQLITE_IOERR_TRUNCATE)?;
    handle
      .file()?
      .set_len(size)
      .map_err(|_| vars::SQLITE_IOERR_TRUNCATE)
  }

  fn write(&self, handle: &mut SlowFile, offset: usize, data: &[u8]) -> VfsResult<usize> {
    let file = handle.file()?;
    let offset = u64::try_from(offset).map_err(|_| vars::SQLITE_IOERR_WRITE)?;
    file
      .seek(SeekFrom::Start(offset))
      .and_then(|_| file.write_all(data))
      .map_err(|_| vars::SQLITE_IOERR_WRITE)?;
    Ok(data.len())
  }

  fn read(&self, handle: &mut SlowFile, offset: usize, data: &mut [u8]) -> VfsResult<usize> {
    let file = handle.file()?;
    let offset = u64::try_from(offset).map_err(|_| vars::SQLITE_IOERR_READ)?;
    file
      .seek(SeekFrom::Start(offset))
      .map_err(|_| vars::SQLITE_IOERR_READ)?;
    let mut read = 0;
    while read < data.len() {
      match file.read(&mut data[read..]) {
        Ok(0) => break,
        Ok(count) => read += count,
        Err(error) if error.kind() == ErrorKind::Interrupted => {},
        Err(_) => return Err(vars::SQLITE_IOERR_READ),
      }
    }
    Ok(read)
  }

  fn lock(&self, handle: &mut SlowFile, level: LockLevel) -> VfsResult<()> {
    if handle.lock >= level {
      return Ok(());
    }
    let Some(database) = handle.maybe_database.clone() else {
      handle.lock = level;
      return Ok(());
    };
    let mut locks = lock(&database.locks);
    let id = handle.id;
    let is_other = |maybe_holder: Option<u64>| maybe_holder.is_some_and(|holder| holder != id);
    match level {
      LockLevel::Unlocked => {},
      LockLevel::Shared => {
        if is_other(locks.maybe_pending) || is_other(locks.maybe_exclusive) {
          return Err(vars::SQLITE_BUSY);
        }
        locks.shared.insert(id);
      },
      LockLevel::Reserved => {
        if is_other(locks.maybe_reserved) || is_other(locks.maybe_exclusive) {
          return Err(vars::SQLITE_BUSY);
        }
        locks.maybe_reserved = Some(id);
      },
      LockLevel::Pending | LockLevel::Exclusive => {
        if is_other(locks.maybe_pending) || is_other(locks.maybe_exclusive) {
          return Err(vars::SQLITE_BUSY);
        }
        // Pending keeps new readers out while the ones there finish.
        locks.maybe_pending = Some(id);
        handle.lock = LockLevel::Pending;
        if level == LockLevel::Pending {
          return Ok(());
        }
        if locks.shared.iter().any(|holder| *holder != id) {
          return Err(vars::SQLITE_BUSY);
        }
        locks.maybe_exclusive = Some(id);
      },
    }
    handle.lock = level;
    Ok(())
  }

  fn unlock(&self, handle: &mut SlowFile, level: LockLevel) -> VfsResult<()> {
    if handle.lock <= level {
      return Ok(());
    }
    if let Some(database) = handle.maybe_database.clone() {
      let mut guard = lock(&database.locks);
      let locks = &mut *guard;
      let id = handle.id;
      for holder in [
        &mut locks.maybe_reserved,
        &mut locks.maybe_pending,
        &mut locks.maybe_exclusive,
      ] {
        if *holder == Some(id) {
          *holder = None;
        }
      }
      if level == LockLevel::Unlocked {
        locks.shared.remove(&id);
      }
    }
    handle.lock = level;
    Ok(())
  }

  fn check_reserved_lock(&self, handle: &mut SlowFile) -> VfsResult<bool> {
    let Some(database) = &handle.maybe_database else {
      return Ok(false);
    };
    let locks = lock(&database.locks);
    Ok(locks.maybe_reserved.is_some() || locks.maybe_pending.is_some() || locks.maybe_exclusive.is_some())
  }

  /// The slow part: the data is in the page cache already; the wait is what a phone's flash costs.
  fn sync(&self, handle: &mut SlowFile) -> VfsResult<()> {
    let counter = match handle.kind {
      OpenKind::Wal => &self.state.log_syncs,
      _ => &self.state.database_syncs,
    };
    counter.fetch_add(1, Ordering::Relaxed);
    std::thread::sleep(self.state.sync_delay);
    Ok(())
  }

  fn close(&self, mut handle: SlowFile) -> VfsResult<()> {
    self.unlock(&mut handle, LockLevel::Unlocked)?;
    if handle.is_mapping_shm {
      self.shm_unmap(&mut handle, false)?;
    }
    drop(handle.maybe_file.take());
    if handle.is_deleted_on_close {
      // Deliberately ignored: a temporary file that is already gone is what closing wanted.
      let _ = std::fs::remove_file(&handle.path);
    }
    Ok(())
  }

  /// As SQLite's own unix VFS: a page write never changes bytes outside the page. Nothing that
  /// would let SQLite skip a sync (atomic writes, safe appends).
  fn device_characteristics(&self, _handle: &mut SlowFile) -> VfsResult<i32> {
    Ok(vars::SQLITE_IOCAP_POWERSAFE_OVERWRITE)
  }

  fn shm_map(
    &self,
    handle: &mut SlowFile,
    region_index: usize,
    region_size: usize,
    is_extending: bool,
  ) -> VfsResult<Option<NonNull<u8>>> {
    let database = handle.database()?;
    let mut shm = lock(&database.shm);
    shm.mappers.insert(handle.id);
    handle.is_mapping_shm = true;
    while shm.regions.len() <= region_index {
      if !is_extending {
        return Ok(None);
      }
      shm
        .regions
        .push((0..region_size).map(|_| UnsafeCell::new(0)).collect());
    }
    // The cells' heap allocation doesn't move when the vector grows; it is freed only once no
    // connection maps it anymore.
    Ok(NonNull::new(
      shm.regions[region_index].as_ptr().cast::<u8>().cast_mut(),
    ))
  }

  fn shm_lock(&self, handle: &mut SlowFile, offset: u32, count: u32, mode: ShmLockMode) -> VfsResult<()> {
    let database = handle.database()?;
    let mut shm = lock(&database.shm);
    let id = handle.id;
    let start = usize::try_from(offset).map_err(|_| vars::SQLITE_IOERR)?;
    let end = start + usize::try_from(count).map_err(|_| vars::SQLITE_IOERR)?;
    if end > SHM_LOCKS {
      return Err(vars::SQLITE_IOERR);
    }
    let slots = start..end;
    match mode {
      ShmLockMode::LockShared => {
        if slots
          .clone()
          .any(|slot| shm.exclusive[slot].is_some_and(|holder| holder != id))
        {
          return Err(vars::SQLITE_BUSY);
        }
        for slot in slots {
          shm.shared[slot].insert(id);
        }
      },
      ShmLockMode::LockExclusive => {
        let is_taken = |shm: &Shm, slot: usize| {
          shm.exclusive[slot].is_some_and(|holder| holder != id)
            || shm.shared[slot].iter().any(|holder| *holder != id)
        };
        if slots.clone().any(|slot| is_taken(&shm, slot)) {
          return Err(vars::SQLITE_BUSY);
        }
        for slot in slots {
          shm.exclusive[slot] = Some(id);
        }
      },
      ShmLockMode::UnlockShared => {
        for slot in slots {
          shm.shared[slot].remove(&id);
        }
      },
      ShmLockMode::UnlockExclusive => {
        for slot in slots {
          if shm.exclusive[slot] == Some(id) {
            shm.exclusive[slot] = None;
          }
        }
      },
    }
    Ok(())
  }

  fn shm_barrier(&self, _handle: &mut SlowFile) {
    std::sync::atomic::fence(Ordering::SeqCst);
  }

  fn shm_unmap(&self, handle: &mut SlowFile, _is_deleting: bool) -> VfsResult<()> {
    let database = handle.database()?;
    let mut shm = lock(&database.shm);
    let id = handle.id;
    shm.mappers.remove(&id);
    for slot in 0..SHM_LOCKS {
      shm.shared[slot].remove(&id);
      if shm.exclusive[slot] == Some(id) {
        shm.exclusive[slot] = None;
      }
    }
    if shm.mappers.is_empty() {
      // The next connection builds the index again from the log, as after a reboot.
      shm.regions.clear();
    }
    handle.is_mapping_shm = false;
    Ok(())
  }
}

impl SlowFile {
  fn file(&mut self) -> VfsResult<&mut File> {
    self.maybe_file.as_mut().ok_or(vars::SQLITE_IOERR)
  }

  fn database(&self) -> VfsResult<Arc<Database>> {
    self.maybe_database.clone().ok_or(vars::SQLITE_IOERR)
  }
}

impl VfsHandle for SlowFile {
  fn readonly(&self) -> bool {
    self.is_readonly
  }

  fn in_memory(&self) -> bool {
    false
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
