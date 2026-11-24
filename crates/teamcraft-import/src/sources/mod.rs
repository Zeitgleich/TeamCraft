//! Exports of other chat systems (Slack, Mattermost), converted into the export the import reads
//! (docs/import-slack.md, docs/import-mattermost.md).
//!
//! `teamcraft-import convert` reads the other system's export and writes the NDJSON files and the
//! mapping templates that `plan`, `run` and `serve` take, so everything after the conversion
//! (mapping, encryption, resuming, waiting for people) is the same for every source. The
//! conversion reads only local files: it downloads nothing and never follows a path out of the
//! export.

pub mod mattermost;
pub mod slack;
pub mod text;
mod writer;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

pub use writer::{BotRow, ChannelRow, ExportWriter, FileRow, PostRow, ReactionRow, UserRow};

use crate::export::SourceId;

/// The largest JSON file (a Slack day, `users.json`) the conversion reads.
pub const MAX_JSON_FILE_BYTES: u64 = 512 * 1024 * 1024;
/// The longest line of a Mattermost bulk export it reads.
pub const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Which system the export comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
  Slack,
  Mattermost,
}

impl FromStr for SourceKind {
  type Err = String;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    match value {
      "slack" => Ok(Self::Slack),
      "mattermost" => Ok(Self::Mattermost),
      other => Err(format!("unknown source {other:?} (slack or mattermost)")),
    }
  }
}

impl SourceKind {
  fn namespace(self) -> &'static str {
    match self {
      Self::Slack => "slack",
      Self::Mattermost => "mattermost",
    }
  }
}

/// What the conversion did, for the operator (no content, only counts and names of channels).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConvertSummary {
  pub channels: u64,
  pub direct: u64,
  pub users: u64,
  pub bots: u64,
  pub posts: u64,
  pub files: u64,
  pub missing_files: u64,
  pub reactions: u64,
  /// Left out, by reason.
  pub skipped: BTreeMap<Skipped, u64>,
}

/// Why something of the export is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skipped {
  /// Joins, leaves, topic changes and other messages the system wrote itself.
  SystemMessage,
  /// A deleted message.
  Deleted,
  /// A message whose time can't be read.
  BadTime,
  /// A file the export has only as a link (external, or removed by a plan limit).
  ExternalFile,
  /// A file name or path that would leave the export.
  UnsafePath,
  /// A conversation whose folder or name the export doesn't have.
  NoFolder,
  /// A line of a type the conversion doesn't know.
  UnknownLine,
  /// A message in a channel the export doesn't list.
  UnknownChannel,
}

impl Skipped {
  pub fn label(self) -> &'static str {
    match self {
      Self::SystemMessage => "system messages (joins, leaves, topic changes)",
      Self::Deleted => "deleted messages",
      Self::BadTime => "messages without a readable time",
      Self::ExternalFile => "files only linked, not in the export",
      Self::UnsafePath => "file names or paths outside the export",
      Self::NoFolder => "conversations without their folder",
      Self::UnknownLine => "lines of an unknown type",
      Self::UnknownChannel => "messages in a channel the export doesn't list",
    }
  }
}

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
  #[error("{path}: {source}")]
  Io { path: PathBuf, source: std::io::Error },
  #[error("{path}{}: {source}", line_suffix(*.maybe_line))]
  Json {
    path: PathBuf,
    maybe_line: Option<usize>,
    source: serde_json::Error,
  },
  #[error("{path}: larger than {limit_bytes} bytes; the conversion reads no more")]
  TooLarge { path: PathBuf, limit_bytes: u64 },
  #[error("{0}: the output directory holds other files; give an empty or new directory")]
  OutputNotEmpty(PathBuf),
  #[error("{path}: {reason}")]
  Invalid { path: PathBuf, reason: String },
  #[error("two {what} of the export get the same ID ({key}); report this as a bug")]
  IdCollision { what: &'static str, key: String },
}

fn line_suffix(maybe_line: Option<usize>) -> String {
  maybe_line.map_or_else(String::new, |line| format!(", line {line}"))
}

/// Converts the export at `source` into `out`.
pub fn convert(kind: SourceKind, source: &Path, out: &Path) -> Result<ConvertSummary, ConvertError> {
  match kind {
    SourceKind::Slack => slack::convert(source, out),
    SourceKind::Mattermost => mattermost::convert(source, out),
  }
}

/// A stable ID for something the other system names by a string: the same export converts to
/// the same IDs, so a second conversion and the import's progress agree (FNV-1a, 62 bits, never
/// 0). Not a security property: the writer refuses collisions.
pub fn stable_id(kind: SourceKind, what: &str, key: &str) -> SourceId {
  const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
  const PRIME: u64 = 0x0000_0100_0000_01b3;
  let mut hash = OFFSET;
  for byte in kind
    .namespace()
    .bytes()
    .chain([0])
    .chain(what.bytes())
    .chain([0])
    .chain(key.bytes())
  {
    hash ^= u64::from(byte);
    hash = hash.wrapping_mul(PRIME);
  }
  // Invariant: masked to 62 bits, so it fits an i64 and stays positive.
  let id = SourceId::try_from(hash & 0x3fff_ffff_ffff_ffff).unwrap_or(1);
  id.max(1)
}

/// `relative` inside `root`, or `None` when it would leave it (absolute, `..`, a prefix) or is
/// empty. Symbolic links out of the export are caught when the file is opened: see
/// [`contained_file`].
pub fn join_inside(root: &Path, relative: &str) -> Option<PathBuf> {
  let relative = Path::new(relative);
  let mut path = root.to_owned();
  let mut has_part = false;
  for component in relative.components() {
    match component {
      Component::Normal(part) => {
        path.push(part);
        has_part = true;
      },
      Component::CurDir => {},
      Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
    }
  }
  has_part.then_some(path)
}

/// The file at `path` if it is inside `root` once links are resolved; a missing file is kept as
/// it is (the import names it as missing), one that leads out of the export is `None`.
pub fn contained_file(root: &Path, path: PathBuf) -> Option<PathBuf> {
  let Ok(resolved) = path.canonicalize() else {
    return Some(path);
  };
  let root = root.canonicalize().ok()?;
  resolved.starts_with(&root).then_some(resolved)
}

/// A single file name from the export: no separators, not `.` or `..`, no control characters.
pub fn safe_file_name(name: &str) -> Option<&str> {
  let name = name.trim();
  let is_safe = !name.is_empty()
    && name != "."
    && name != ".."
    && !name.contains(['/', '\\'])
    && !name.chars().any(char::is_control);
  is_safe.then_some(name)
}

impl fmt::Display for ConvertSummary {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    writeln!(f, "# Conversion")?;
    writeln!(f)?;
    writeln!(f, "| What                  | Count |")?;
    writeln!(f, "|-----------------------|-------|")?;
    for (what, count) in [
      ("Channels", self.channels),
      ("Direct messages", self.direct),
      ("People", self.users),
      ("Bots and webhooks", self.bots),
      ("Messages", self.posts),
      ("Files", self.files),
      ("Files not in the export", self.missing_files),
      ("Reactions", self.reactions),
    ] {
      writeln!(f, "| {what:<21} | {count:>5} |")?;
    }
    if !self.skipped.is_empty() {
      writeln!(f)?;
      writeln!(f, "Left out:")?;
      writeln!(f)?;
      for (reason, count) in &self.skipped {
        writeln!(f, "- {count} {}", reason.label())?;
      }
    }
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stable_ids_are_stable_positive_and_namespaced() {
    let id = stable_id(SourceKind::Slack, "user", "U012AB3CD");
    assert_eq!(id, stable_id(SourceKind::Slack, "user", "U012AB3CD"));
    assert!(id > 0);
    assert_ne!(id, stable_id(SourceKind::Mattermost, "user", "U012AB3CD"));
    assert_ne!(id, stable_id(SourceKind::Slack, "channel", "U012AB3CD"));
    assert_ne!(
      stable_id(SourceKind::Slack, "a", "bc"),
      stable_id(SourceKind::Slack, "ab", "c")
    );
  }

  #[test]
  fn paths_stay_inside_the_export() {
    let root = Path::new("/export");
    assert_eq!(
      join_inside(root, "data/a/b.png"),
      Some(PathBuf::from("/export/data/a/b.png"))
    );
    assert_eq!(
      join_inside(root, "./data/b.png"),
      Some(PathBuf::from("/export/data/b.png"))
    );
    assert_eq!(join_inside(root, "../etc/passwd"), None);
    assert_eq!(join_inside(root, "data/../../x"), None);
    assert_eq!(join_inside(root, "/etc/passwd"), None);
    assert_eq!(join_inside(root, ""), None);
    assert_eq!(join_inside(root, "."), None);
  }

  #[test]
  fn file_names_are_single_names() {
    assert_eq!(safe_file_name(" report.pdf "), Some("report.pdf"));
    assert_eq!(safe_file_name("a/b"), None);
    assert_eq!(safe_file_name("a\\b"), None);
    assert_eq!(safe_file_name(".."), None);
    assert_eq!(safe_file_name("a\nb"), None);
    assert_eq!(safe_file_name(""), None);
  }

  #[cfg(unix)]
  #[test]
  fn a_link_out_of_the_export_is_refused() {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-contained-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(dir.join("export")).unwrap();
    std::fs::write(dir.join("secret"), "x").unwrap();
    std::fs::write(dir.join("export").join("fine"), "x").unwrap();
    std::os::unix::fs::symlink(dir.join("secret"), dir.join("export").join("link")).unwrap();
    let root = dir.join("export");
    assert_eq!(contained_file(&root, root.join("link")), None);
    assert!(contained_file(&root, root.join("fine")).is_some());
    assert_eq!(
      contained_file(&root, root.join("missing")),
      Some(root.join("missing"))
    );
    drop(std::fs::remove_dir_all(&dir));
  }
}
