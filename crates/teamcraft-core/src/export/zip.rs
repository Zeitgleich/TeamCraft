//! A ZIP archive written front to back, for the data export (issue #211): every entry is stored
//! (not compressed) and followed by a data descriptor, so nothing is ever written twice and the
//! archive streams into any sink, a file on disk or the pieces of a browser download. Large
//! archives use ZIP64 (more than 65,535 entries, entries or offsets beyond 4 GiB).
//!
//! What is written so far is described by [`ZipState`], which only changes when an entry ends:
//! saved with the export's checkpoint, it lets a later run cut the file back to the end of the last
//! whole entry and go on from there.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::checksum::FileChecksum;

use super::ExportError;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const DATA_DESCRIPTOR: u32 = 0x0807_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
/// 2.0: stored entries with data descriptors; 4.5: ZIP64.
const VERSION_PLAIN: u16 = 20;
const VERSION_ZIP64: u16 = 45;
/// Sizes in a data descriptor follow (bit 3); the name is UTF-8 (bit 11).
const FLAGS: u16 = 0x0008 | 0x0800;
const STORED: u16 = 0;
const ZIP64_EXTRA: u16 = 0x0001;
/// Where a 32-bit field can't hold the value and the ZIP64 field has it.
const MAX_32: u64 = 0xFFFF_FFFF;
const MAX_16: u64 = 0xFFFF;
/// The longest entry name: ZIP keeps the length in 16 bits.
const MAX_NAME_BYTES: usize = 0xFFFF;

/// Where the archive goes: appended to, piece by piece, and never read back.
pub trait ExportSink {
  /// Appends `bytes`.
  fn write_all(&mut self, bytes: &[u8]) -> Result<(), ExportError>;
  /// Makes what was written so far durable: the export's checkpoint is saved right after, and
  /// must never claim more than the sink holds.
  fn flush(&mut self) -> Result<(), ExportError>;
}

/// One whole entry of the archive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipEntry {
  pub name: String,
  /// Where its local header starts.
  pub offset: u64,
  pub crc32: u32,
  pub size: u64,
  /// SHA-256 of its content, as `sha256sum` prints it (also listed in the manifest).
  pub sha256: String,
}

/// The whole entries written so far and where the next one starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipState {
  pub entries: Vec<ZipEntry>,
  pub offset: u64,
}

/// Writes the archive into a sink; see the module docs.
#[derive(Default)]
pub struct ZipWriter {
  state: ZipState,
  /// DOS date and time stamped on every entry: when the export began.
  dos_time: (u16, u16),
  maybe_open: Option<OpenEntry>,
}

struct OpenEntry {
  name: String,
  offset: u64,
  crc: crc32fast::Hasher,
  checksum: FileChecksum,
  size: u64,
}

impl ZipWriter {
  /// A new archive whose entries carry `created_at_ms` (Unix milliseconds, UTC) as their time.
  pub fn new(created_at_ms: i64) -> Self {
    Self::resume(ZipState::default(), created_at_ms)
  }

  /// Goes on after the entries of `state`; the sink must hold exactly `state.offset` bytes.
  pub fn resume(state: ZipState, created_at_ms: i64) -> Self {
    Self {
      state,
      dos_time: dos_date_time(created_at_ms),
      maybe_open: None,
    }
  }

  /// The whole entries so far; an entry being written is not part of it until it ends.
  pub fn state(&self) -> &ZipState {
    &self.state
  }

  /// Bytes written so far, the open entry's included.
  pub fn len(&self) -> u64 {
    match &self.maybe_open {
      Some(open) => open
        .offset
        .saturating_add(local_header_len(&open.name))
        .saturating_add(open.size),
      None => self.state.offset,
    }
  }

  pub fn is_empty(&self) -> bool {
    self.len() == 0
  }

  pub fn is_open(&self) -> bool {
    self.maybe_open.is_some()
  }

  /// Starts the entry `name` (a path with `/`, no leading slash).
  pub fn begin(&mut self, sink: &mut dyn ExportSink, name: &str) -> Result<(), ExportError> {
    if self.maybe_open.is_some() {
      return Err(ExportError::Invalid("an entry is already open"));
    }
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.starts_with('/') {
      return Err(ExportError::Invalid("an entry name must be a relative path"));
    }
    let mut header = Vec::with_capacity(30 + name.len());
    put_u32(&mut header, LOCAL_HEADER);
    put_u16(&mut header, VERSION_PLAIN);
    put_u16(&mut header, FLAGS);
    put_u16(&mut header, STORED);
    put_u16(&mut header, self.dos_time.1);
    put_u16(&mut header, self.dos_time.0);
    // CRC and sizes follow in the data descriptor.
    put_u32(&mut header, 0);
    put_u32(&mut header, 0);
    put_u32(&mut header, 0);
    put_u16(&mut header, name.len() as u16);
    put_u16(&mut header, 0);
    header.extend_from_slice(name.as_bytes());
    sink.write_all(&header)?;
    self.maybe_open = Some(OpenEntry {
      name: name.to_owned(),
      offset: self.state.offset,
      crc: crc32fast::Hasher::new(),
      checksum: FileChecksum::new(),
      size: 0,
    });
    Ok(())
  }

  /// Appends to the open entry.
  pub fn write(&mut self, sink: &mut dyn ExportSink, bytes: &[u8]) -> Result<(), ExportError> {
    let Some(open) = self.maybe_open.as_mut() else {
      return Err(ExportError::Invalid("no entry is open"));
    };
    sink.write_all(bytes)?;
    open.crc.update(bytes);
    open.checksum.update(bytes);
    open.size = open.size.saturating_add(bytes.len() as u64);
    Ok(())
  }

  /// Ends the open entry with its data descriptor; it is part of [`Self::state`] from now on.
  pub fn end(&mut self, sink: &mut dyn ExportSink) -> Result<&ZipEntry, ExportError> {
    let Some(open) = self.maybe_open.take() else {
      return Err(ExportError::Invalid("no entry is open"));
    };
    let crc32 = open.crc.finalize();
    let mut descriptor = Vec::with_capacity(24);
    put_u32(&mut descriptor, DATA_DESCRIPTOR);
    put_u32(&mut descriptor, crc32);
    // Readers take the descriptor's size fields as 64-bit when the central directory has a ZIP64
    // field for the entry, which it has exactly when the size doesn't fit 32 bits.
    if open.size >= MAX_32 {
      put_u64(&mut descriptor, open.size);
      put_u64(&mut descriptor, open.size);
    } else {
      put_u32(&mut descriptor, open.size as u32);
      put_u32(&mut descriptor, open.size as u32);
    }
    sink.write_all(&descriptor)?;
    self.state.offset = open
      .offset
      .saturating_add(local_header_len(&open.name))
      .saturating_add(open.size)
      .saturating_add(descriptor.len() as u64);
    self.state.entries.push(ZipEntry {
      name: open.name,
      offset: open.offset,
      crc32,
      size: open.size,
      sha256: open.checksum.finish_hex(),
    });
    self.state.entries.last().ok_or(ExportError::Invalid("no entry"))
  }

  /// A whole entry at once.
  pub fn add(&mut self, sink: &mut dyn ExportSink, name: &str, bytes: &[u8]) -> Result<(), ExportError> {
    self.begin(sink, name)?;
    self.write(sink, bytes)?;
    self.end(sink)?;
    Ok(())
  }

  /// Writes the central directory; the archive is complete after it. Answers its whole length.
  pub fn finish(self, sink: &mut dyn ExportSink) -> Result<u64, ExportError> {
    if self.maybe_open.is_some() {
      return Err(ExportError::Invalid("an entry is still open"));
    }
    let directory_offset = self.state.offset;
    let mut directory = Vec::new();
    for entry in &self.state.entries {
      self.central_header(&mut directory, entry);
    }
    let directory_len = directory.len() as u64;
    let count = self.state.entries.len() as u64;
    let directory_end = directory_offset.saturating_add(directory_len);
    let needs_zip64 = count >= MAX_16 || directory_offset >= MAX_32 || directory_len >= MAX_32;
    if needs_zip64 {
      put_u32(&mut directory, ZIP64_END);
      // The record's size after this field.
      put_u64(&mut directory, 44);
      put_u16(&mut directory, VERSION_ZIP64);
      put_u16(&mut directory, VERSION_ZIP64);
      put_u32(&mut directory, 0);
      put_u32(&mut directory, 0);
      put_u64(&mut directory, count);
      put_u64(&mut directory, count);
      put_u64(&mut directory, directory_len);
      put_u64(&mut directory, directory_offset);
      put_u32(&mut directory, ZIP64_LOCATOR);
      put_u32(&mut directory, 0);
      put_u64(&mut directory, directory_end);
      put_u32(&mut directory, 1);
    }
    put_u32(&mut directory, END_OF_CENTRAL);
    put_u16(&mut directory, 0);
    put_u16(&mut directory, 0);
    put_u16(&mut directory, count.min(MAX_16) as u16);
    put_u16(&mut directory, count.min(MAX_16) as u16);
    put_u32(&mut directory, directory_len.min(MAX_32) as u32);
    put_u32(&mut directory, directory_offset.min(MAX_32) as u32);
    put_u16(&mut directory, 0);
    sink.write_all(&directory)?;
    sink.flush()?;
    Ok(directory_offset.saturating_add(directory.len() as u64))
  }

  fn central_header(&self, out: &mut Vec<u8>, entry: &ZipEntry) {
    let mut extra = Vec::new();
    if entry.size >= MAX_32 {
      put_u64(&mut extra, entry.size);
      put_u64(&mut extra, entry.size);
    }
    if entry.offset >= MAX_32 {
      put_u64(&mut extra, entry.offset);
    }
    let is_zip64 = !extra.is_empty();
    let version = if is_zip64 { VERSION_ZIP64 } else { VERSION_PLAIN };
    put_u32(out, CENTRAL_HEADER);
    put_u16(out, version);
    put_u16(out, version);
    put_u16(out, FLAGS);
    put_u16(out, STORED);
    put_u16(out, self.dos_time.1);
    put_u16(out, self.dos_time.0);
    put_u32(out, entry.crc32);
    put_u32(out, entry.size.min(MAX_32) as u32);
    put_u32(out, entry.size.min(MAX_32) as u32);
    put_u16(out, entry.name.len() as u16);
    put_u16(out, if is_zip64 { extra.len() as u16 + 4 } else { 0 });
    // No comment, disk 0, no attributes.
    put_u16(out, 0);
    put_u16(out, 0);
    put_u16(out, 0);
    put_u32(out, 0);
    put_u32(out, entry.offset.min(MAX_32) as u32);
    out.extend_from_slice(entry.name.as_bytes());
    if is_zip64 {
      put_u16(out, ZIP64_EXTRA);
      put_u16(out, extra.len() as u16);
      out.extend_from_slice(&extra);
    }
  }
}

fn local_header_len(name: &str) -> u64 {
  30 + name.len() as u64
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
  out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
  out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
  out.extend_from_slice(&value.to_le_bytes());
}

/// The DOS date and time of `unix_ms` (UTC; DOS has no zone), clamped to what DOS can hold
/// (1980 to 2107).
fn dos_date_time(unix_ms: i64) -> (u16, u16) {
  let seconds = unix_ms.div_euclid(1000);
  let days = seconds.div_euclid(86_400);
  let of_day = seconds.rem_euclid(86_400);
  let (year, month, day) = civil_from_days(days);
  if year < 1980 {
    return ((1 << 5) | 1, 0);
  }
  let year = year.min(2107);
  let date = (((year - 1980) as u16) << 9) | ((month as u16) << 5) | day as u16;
  let time =
    (((of_day / 3600) as u16) << 11) | ((((of_day % 3600) / 60) as u16) << 5) | ((of_day % 60) / 2) as u16;
  (date, time)
}

/// Year, month (1-12) and day (1-31) of a day count since 1970-01-01 (Howard Hinnant's algorithm).
pub(super) fn civil_from_days(days: i64) -> (i64, u32, u32) {
  let shifted = days + 719_468;
  let era = shifted.div_euclid(146_097);
  let day_of_era = shifted.rem_euclid(146_097);
  let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
  let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
  let month_index = (5 * day_of_year + 2) / 153;
  let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
  let month = if month_index < 10 {
    month_index + 3
  } else {
    month_index - 9
  } as u32;
  let year = year_of_era + era * 400 + i64::from(month <= 2);
  (year, month, day)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::export::MemorySink;

  #[test]
  fn days_become_calendar_dates() {
    assert_eq!(civil_from_days(0), (1970, 1, 1));
    assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    assert_eq!(civil_from_days(19_782), (2024, 2, 29));
  }

  #[test]
  fn the_dos_time_packs_date_and_time() {
    // 2024-02-29 13:45:30 UTC.
    let (date, time) = dos_date_time(1_709_214_330_000);
    assert_eq!(date, (44 << 9) | (2 << 5) | 29);
    assert_eq!(time, (13 << 11) | (45 << 5) | 15);
    assert_eq!(
      dos_date_time(0),
      ((1 << 5) | 1, 0),
      "before 1980: the earliest DOS date"
    );
  }

  #[test]
  fn an_entry_is_only_part_of_the_state_once_it_ends() {
    let mut sink = MemorySink::default();
    let mut zip = ZipWriter::new(0);
    zip.begin(&mut sink, "a.txt").unwrap();
    zip.write(&mut sink, b"hello").unwrap();
    assert!(zip.state().entries.is_empty());
    assert_eq!(zip.len(), sink.bytes.len() as u64);
    zip.end(&mut sink).unwrap();
    assert_eq!(zip.state().entries.len(), 1);
    assert_eq!(zip.state().offset, sink.bytes.len() as u64);
    assert_eq!(zip.state().entries[0].size, 5);
    assert_eq!(
      zip.state().entries[0].sha256,
      "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    );
  }

  #[test]
  fn names_must_be_relative_and_entries_cannot_nest() {
    let mut sink = MemorySink::default();
    let mut zip = ZipWriter::new(0);
    assert!(zip.begin(&mut sink, "/etc/passwd").is_err());
    assert!(zip.begin(&mut sink, "").is_err());
    zip.begin(&mut sink, "a").unwrap();
    assert!(zip.begin(&mut sink, "b").is_err());
    assert!(zip.end(&mut sink).is_ok());
    assert!(zip.end(&mut sink).is_err());
  }
}
