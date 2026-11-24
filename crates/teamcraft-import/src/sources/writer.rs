//! Writes a converted export: the NDJSON files `Export::open` reads, the two mapping templates and
//! a report. Messages arrive in any order and are spooled per channel, so memory stays bounded by
//! the open files, not by the export; `posts.ndjson` then holds each channel's posts together, as
//! the import's index needs.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{ConvertError, ConvertSummary, Skipped};
use crate::export::{CHANNEL_DEFAULT, CHANNEL_DM, CHANNEL_GROUP, SourceId};

/// Written first: a directory with it is ours to write again.
const MARKER: &str = "CONVERTED";
const SPOOL_DIR: &str = ".spool";
/// Spool files kept open at once.
const MAX_OPEN_SPOOLS: usize = 64;

/// The converted export being written.
pub struct ExportWriter {
  out: PathBuf,
  spool_dir: PathBuf,
  open_spools: HashMap<SourceId, BufWriter<File>>,
  spooled: BTreeSet<SourceId>,
  reactions: BufWriter<File>,
  post_ids: HashSet<SourceId>,
  posts_per_channel: HashMap<SourceId, u64>,
  posts_per_author: HashMap<SourceId, u64>,
  summary: ConvertSummary,
}

/// A channel, DM or group conversation, as `channels.ndjson` has it.
#[derive(Debug, Clone, Serialize)]
pub struct ChannelRow {
  pub id: SourceId,
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub display_name: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub purpose: Option<String>,
  #[serde(rename = "type")]
  pub kind: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub create_at: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub close_at: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub creator_id: Option<SourceId>,
  pub members: Vec<SourceId>,
  /// The other system's name for it (`C012AB3CD`, `team-a/town-square`), for the template.
  #[serde(skip)]
  pub source_key: String,
  #[serde(skip)]
  pub is_public: bool,
  /// The space the template suggests (a Mattermost team).
  #[serde(skip)]
  pub maybe_space: Option<String>,
}

/// A person of the export, as `users.ndjson` has it.
#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
  pub id: SourceId,
  pub username: Option<String>,
  pub deleted: bool,
  #[serde(skip)]
  pub source_key: String,
  #[serde(skip)]
  pub display_name: String,
  #[serde(skip)]
  pub maybe_email: Option<String>,
}

/// A bot or webhook that wrote messages.
#[derive(Debug, Clone, Serialize)]
pub struct BotRow {
  pub id: SourceId,
  pub name: String,
}

/// One message, as `posts.ndjson` has it (at most one file; more become messages of their own).
#[derive(Debug, Clone, Serialize)]
pub struct PostRow {
  pub id: SourceId,
  pub channel_id: SourceId,
  pub user_id: SourceId,
  #[serde(rename = "type")]
  pub kind: i64,
  pub message: String,
  pub create_at: i64,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub update_at: Option<i64>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub thread_id: Option<SourceId>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub file_props: Option<FileRow>,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRow {
  pub name: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  pub size: Option<u64>,
  pub is_image: bool,
  /// The extension (`pdf`).
  #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
  pub extension: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReactionRow {
  pub post_id: SourceId,
  pub user_id: SourceId,
  /// The emoji's short code (`thumbsup`).
  pub name: String,
}

#[derive(Serialize)]
struct FavoriteRow {
  channel_id: SourceId,
  user_id: SourceId,
}

impl ExportWriter {
  /// Starts writing into `out`, which must be new, empty, or an earlier conversion's output.
  pub fn create(out: &Path) -> Result<Self, ConvertError> {
    let io = |source| ConvertError::Io {
      path: out.to_owned(),
      source,
    };
    if out.exists() {
      let mut entries = std::fs::read_dir(out).map_err(io)?;
      if entries.next().is_some() && !out.join(MARKER).is_file() {
        return Err(ConvertError::OutputNotEmpty(out.to_owned()));
      }
    }
    crate::files::make_state_dir(out).map_err(io)?;
    write_whole(&out.join(MARKER), b"teamcraft-import convert\n")?;
    let spool_dir = out.join(SPOOL_DIR);
    if spool_dir.exists() {
      std::fs::remove_dir_all(&spool_dir).map_err(|source| ConvertError::Io {
        path: spool_dir.clone(),
        source,
      })?;
    }
    crate::files::make_state_dir(&spool_dir).map_err(|source| ConvertError::Io {
      path: spool_dir.clone(),
      source,
    })?;
    let reactions = BufWriter::new(create_private(&out.join("reactions.ndjson"), false)?);
    Ok(Self {
      out: out.to_owned(),
      spool_dir,
      open_spools: HashMap::new(),
      spooled: BTreeSet::new(),
      reactions,
      post_ids: HashSet::new(),
      posts_per_channel: HashMap::new(),
      posts_per_author: HashMap::new(),
      summary: ConvertSummary::default(),
    })
  }

  /// Counts something left out.
  pub fn skip(&mut self, reason: Skipped) {
    *self.summary.skipped.entry(reason).or_default() += 1;
  }

  /// Whether a message with this ID was written already.
  pub fn has_post(&self, id: SourceId) -> bool {
    self.post_ids.contains(&id)
  }

  /// Adds a message; `key` names it in the other system, for the error on a collision.
  pub fn post(&mut self, key: &str, post: &PostRow) -> Result<(), ConvertError> {
    if !self.post_ids.insert(post.id) {
      return Err(ConvertError::IdCollision {
        what: "messages",
        key: key.to_owned(),
      });
    }
    if post.file_props.is_some() {
      self.summary.files += 1;
      if post.source_path.as_ref().is_none_or(|path| !path.is_file()) {
        self.summary.missing_files += 1;
      }
    }
    self.summary.posts += 1;
    *self.posts_per_channel.entry(post.channel_id).or_default() += 1;
    *self.posts_per_author.entry(post.user_id).or_default() += 1;
    let line = to_line(post);
    let path = self.spool_dir.join(format!("{}.ndjson", post.channel_id));
    let spool = self.spool(post.channel_id)?;
    spool
      .write_all(line.as_bytes())
      .map_err(|source| ConvertError::Io { path, source })
  }

  pub fn reaction(&mut self, reaction: &ReactionRow) -> Result<(), ConvertError> {
    self.summary.reactions += 1;
    let path = self.out.join("reactions.ndjson");
    self
      .reactions
      .write_all(to_line(reaction).as_bytes())
      .map_err(|source| ConvertError::Io { path, source })
  }

  /// Writes the rest and the templates; returns what was converted.
  pub fn finish(
    mut self,
    mut channels: Vec<ChannelRow>,
    mut users: Vec<UserRow>,
    mut bots: Vec<BotRow>,
    favorites: &BTreeSet<(SourceId, SourceId)>,
  ) -> Result<ConvertSummary, ConvertError> {
    check_unique(
      "conversations",
      channels.iter().map(|row| (row.id, &row.source_key)),
    )?;
    check_unique(
      "people or bots",
      users
        .iter()
        .map(|row| (row.id, &row.source_key))
        .chain(bots.iter().map(|row| (row.id, &row.name))),
    )?;
    channels.sort_by_key(|row| row.id);
    users.sort_by_key(|row| row.id);
    bots.sort_by_key(|row| row.id);
    self.summary.channels = count(&channels, |kind| kind == CHANNEL_DEFAULT || kind == CHANNEL_GROUP);
    self.summary.direct = count(&channels, |kind| kind == CHANNEL_DM);
    self.summary.users = users.len() as u64;
    self.summary.bots = bots.len() as u64;
    write_lines(&self.out.join("channels.ndjson"), &channels)?;
    write_lines(&self.out.join("users.ndjson"), &users)?;
    write_lines(&self.out.join("bots.ndjson"), &bots)?;
    let favorites: Vec<FavoriteRow> = favorites
      .iter()
      .map(|(channel_id, user_id)| FavoriteRow {
        channel_id: *channel_id,
        user_id: *user_id,
      })
      .collect();
    write_lines(&self.out.join("favorites.ndjson"), &favorites)?;
    self.write_posts()?;
    let reactions_path = self.out.join("reactions.ndjson");
    self.reactions.flush().map_err(|source| ConvertError::Io {
      path: reactions_path,
      source,
    })?;
    write_whole(
      &self.out.join("channels.csv"),
      channel_template(&channels, &self.posts_per_channel).as_bytes(),
    )?;
    write_whole(
      &self.out.join("users.csv"),
      user_template(&users, &self.posts_per_author).as_bytes(),
    )?;
    write_whole(
      &self.out.join("convert-report.md"),
      self.summary.to_string().as_bytes(),
    )?;
    std::fs::remove_dir_all(&self.spool_dir).map_err(|source| ConvertError::Io {
      path: self.spool_dir.clone(),
      source,
    })?;
    Ok(self.summary)
  }

  /// The channel's spool file, opened (for appending) if it isn't.
  fn spool(&mut self, channel: SourceId) -> Result<&mut BufWriter<File>, ConvertError> {
    if !self.open_spools.contains_key(&channel) {
      if self.open_spools.len() >= MAX_OPEN_SPOOLS {
        self.close_spools()?;
      }
      let path = self.spool_dir.join(format!("{channel}.ndjson"));
      let is_new = self.spooled.insert(channel);
      let file = create_private(&path, !is_new)?;
      self.open_spools.insert(channel, BufWriter::new(file));
    }
    self
      .open_spools
      .get_mut(&channel)
      .ok_or_else(|| ConvertError::Invalid {
        path: self.spool_dir.clone(),
        reason: "a spool file was not open".into(),
      })
  }

  fn close_spools(&mut self) -> Result<(), ConvertError> {
    for (channel, mut spool) in self.open_spools.drain() {
      spool.flush().map_err(|source| ConvertError::Io {
        path: self.spool_dir.join(format!("{channel}.ndjson")),
        source,
      })?;
    }
    Ok(())
  }

  /// `posts.ndjson`: the spool files one after the other.
  fn write_posts(&mut self) -> Result<(), ConvertError> {
    self.close_spools()?;
    let path = self.out.join("posts.ndjson");
    let io = |source| ConvertError::Io {
      path: path.clone(),
      source,
    };
    let mut posts = BufWriter::new(create_private(&path, false)?);
    for channel in &self.spooled {
      let spool_path = self.spool_dir.join(format!("{channel}.ndjson"));
      let mut spool = File::open(&spool_path).map_err(|source| ConvertError::Io {
        path: spool_path.clone(),
        source,
      })?;
      std::io::copy(&mut spool, &mut posts).map_err(io)?;
    }
    posts.flush().map_err(io)
  }
}

fn count(channels: &[ChannelRow], is_kind: impl Fn(i64) -> bool) -> u64 {
  channels.iter().filter(|row| is_kind(row.kind)).count() as u64
}

fn check_unique<'a>(
  what: &'static str,
  rows: impl Iterator<Item = (SourceId, &'a String)>,
) -> Result<(), ConvertError> {
  let mut seen = HashSet::new();
  for (id, key) in rows {
    if !seen.insert(id) {
      return Err(ConvertError::IdCollision {
        what,
        key: key.clone(),
      });
    }
  }
  Ok(())
}

fn channel_template(channels: &[ChannelRow], posts: &HashMap<SourceId, u64>) -> String {
  let mut text = String::from(
    "channel_id,source_channel,name,visibility,space,members,posts,archived,teamcraft_channel,teamcraft_name,import_history,import\n",
  );
  for channel in channels
    .iter()
    .filter(|row| row.kind == CHANNEL_DEFAULT || row.kind == CHANNEL_GROUP)
  {
    let fields = [
      channel.id.to_string(),
      channel.source_key.clone(),
      channel.name.clone(),
      if channel.is_public { "public" } else { "private" }.to_owned(),
      channel.maybe_space.clone().unwrap_or_default(),
      channel.members.len().to_string(),
      posts.get(&channel.id).copied().unwrap_or(0).to_string(),
      if channel.close_at.is_some() { "yes" } else { "no" }.to_owned(),
      String::new(),
      String::new(),
      "yes".to_owned(),
      "yes".to_owned(),
    ];
    push_record(&mut text, &fields);
  }
  text
}

fn user_template(users: &[UserRow], posts: &HashMap<SourceId, u64>) -> String {
  let mut text = String::from("source_id,source_user,name,email,deleted,posts,teamcraft_user\n");
  for user in users {
    let fields = [
      user.id.to_string(),
      user.source_key.clone(),
      user.display_name.clone(),
      user.maybe_email.clone().unwrap_or_default(),
      if user.deleted { "yes" } else { "no" }.to_owned(),
      posts.get(&user.id).copied().unwrap_or(0).to_string(),
      String::new(),
    ];
    push_record(&mut text, &fields);
  }
  text
}

/// One CSV record (RFC 4180), as the mapping reads it back. A leading `=`, `+`, `-` or `@` is
/// quoted with an apostrophe so a spreadsheet doesn't run a name as a formula.
fn push_record(text: &mut String, fields: &[String]) {
  for (index, field) in fields.iter().enumerate() {
    if index > 0 {
      text.push(',');
    }
    let field = if field.starts_with(['=', '+', '-', '@']) {
      format!("'{field}")
    } else {
      field.clone()
    };
    if field.contains([',', '"', '\n', '\r']) {
      text.push('"');
      text.push_str(&field.replace('"', "\"\""));
      text.push('"');
    } else {
      text.push_str(&field);
    }
  }
  text.push('\n');
}

fn to_line(row: &impl Serialize) -> String {
  // Invariant: the rows are plain structs of strings and numbers, which always serialize.
  let mut line = serde_json::to_string(row).unwrap_or_default();
  line.push('\n');
  line
}

fn write_lines<T: Serialize>(path: &Path, rows: &[T]) -> Result<(), ConvertError> {
  let mut text = String::new();
  for row in rows {
    text.push_str(&to_line(row));
  }
  write_whole(path, text.as_bytes())
}

fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), ConvertError> {
  crate::files::write_private(path, bytes).map_err(|source| ConvertError::Io {
    path: path.to_owned(),
    source,
  })
}

/// Opens a file readable by this user only: new and empty, or for appending.
fn create_private(path: &Path, is_append: bool) -> Result<File, ConvertError> {
  let mut options = std::fs::OpenOptions::new();
  options.write(true).create(true);
  if is_append {
    options.append(true);
  } else {
    options.truncate(true);
  }
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  options.open(path).map_err(|source| ConvertError::Io {
    path: path.to_owned(),
    source,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mapping::{parse_channels, parse_users};

  #[test]
  fn templates_read_back_as_mappings() {
    let channels = vec![ChannelRow {
      id: 5,
      name: "=cmd, \"quoted\"".into(),
      display_name: None,
      purpose: None,
      kind: CHANNEL_DEFAULT,
      create_at: None,
      close_at: None,
      creator_id: None,
      members: vec![1, 2],
      source_key: "C1".into(),
      is_public: true,
      maybe_space: Some("Team A".into()),
    }];
    let text = channel_template(&channels, &HashMap::from([(5, 3)]));
    let rows = parse_channels(&text, "channels.csv").unwrap();
    let row = &rows[&5];
    assert_eq!(row.name, "=cmd, \"quoted\"");
    assert!(row.is_public);
    assert_eq!(row.space, "Team A");
    let users = vec![UserRow {
      id: 9,
      username: Some("anna".into()),
      deleted: false,
      source_key: "U9".into(),
      display_name: "Anna A".into(),
      maybe_email: Some("anna@example.com".into()),
    }];
    let mut text = user_template(&users, &HashMap::new());
    assert!(parse_users(&text, "users.csv").unwrap().is_empty());
    text = text.replace(",0,\n", ",0,anna\n");
    assert_eq!(parse_users(&text, "users.csv").unwrap()[&9], "anna");
  }

  #[test]
  fn an_output_directory_with_other_files_is_refused() {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-writer-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mine.txt"), "x").unwrap();
    assert!(matches!(
      ExportWriter::create(&dir),
      Err(ConvertError::OutputNotEmpty(_))
    ));
    std::fs::remove_file(dir.join("mine.txt")).unwrap();
    let writer = ExportWriter::create(&dir).unwrap();
    writer
      .finish(Vec::new(), Vec::new(), Vec::new(), &BTreeSet::new())
      .unwrap();
    // A second conversion into the same directory is fine.
    assert!(ExportWriter::create(&dir).is_ok());
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn posts_of_one_channel_stay_together_whatever_the_order() {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-spool-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    let mut writer = ExportWriter::create(&dir).unwrap();
    for (id, channel) in [(1, 10), (2, 20), (3, 10), (4, 20)] {
      writer.post(&id.to_string(), &post(id, channel)).unwrap();
    }
    assert!(matches!(
      writer.post("again", &post(3, 10)),
      Err(ConvertError::IdCollision { .. })
    ));
    let channels = vec![channel(10), channel(20)];
    writer
      .finish(channels, Vec::new(), Vec::new(), &BTreeSet::new())
      .unwrap();
    let export = crate::export::Export::open(&dir, None).unwrap();
    assert_eq!(export.post_count(10), 2);
    assert_eq!(export.post_count(20), 2);
    assert!(!dir.join(SPOOL_DIR).exists());
    drop(std::fs::remove_dir_all(&dir));
  }

  fn post(id: SourceId, channel_id: SourceId) -> PostRow {
    PostRow {
      id,
      channel_id,
      user_id: 1,
      kind: 1,
      message: "hi".into(),
      create_at: id,
      update_at: None,
      thread_id: None,
      file_props: None,
      source_path: None,
    }
  }

  fn channel(id: SourceId) -> ChannelRow {
    ChannelRow {
      id,
      name: format!("c{id}"),
      display_name: None,
      purpose: None,
      kind: CHANNEL_GROUP,
      create_at: None,
      close_at: None,
      creator_id: None,
      members: vec![1],
      source_key: format!("C{id}"),
      is_public: false,
      maybe_space: None,
    }
  }
}
