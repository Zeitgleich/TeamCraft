//! The Synology Chat export as `tools/synology-export/chat-export.sh` writes it (docs/import-synology.md):
//! NDJSON files, one JSON object per line, and the attachments under `raw/uploads/{channel}/{post}`.
//!
//! Everything but the posts is small and read whole. The posts (millions of lines, hundreds of MB)
//! are indexed once by byte range per channel, since the export writes them in channel and time
//! order, and read one channel at a time.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;

/// A Synology channel or user ID.
pub type SourceId = i64;

/// Channel types of Synology Chat.
pub const CHANNEL_DEFAULT: i64 = 0;
pub const CHANNEL_GROUP: i64 = 1;
pub const CHANNEL_DM: i64 = 2;
pub const CHANNEL_MY_SPACE: i64 = 3;
pub const CHANNEL_CHATBOT: i64 = 5;

/// Post types of Synology Chat.
pub const POST_TEXT: i64 = 1;
pub const POST_FILE: i64 = 2;
pub const POST_STICKER: i64 = 3;

/// The export, read from its directory.
pub struct Export {
  pub root: PathBuf,
  pub uploads: PathBuf,
  pub channels: BTreeMap<SourceId, Channel>,
  pub users: BTreeMap<SourceId, ExportUser>,
  /// Names of bots, chatbots and webhooks that wrote posts.
  pub bots: BTreeMap<SourceId, String>,
  pub reactions: HashMap<SourceId, Vec<Reaction>>,
  pub reads: HashMap<(SourceId, SourceId), i64>,
  pub favorites: Vec<(SourceId, SourceId)>,
  pub mutes: Vec<(SourceId, SourceId)>,
  pub webhooks: Vec<Webhook>,
  posts_path: PathBuf,
  /// Byte ranges of each channel's posts in `posts.ndjson`, and how many there are.
  post_ranges: HashMap<SourceId, PostRange>,
  /// Posts per channel and author.
  author_counts: HashMap<SourceId, HashMap<SourceId, u64>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PostRange {
  start: u64,
  end: u64,
  count: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
  pub id: SourceId,
  #[serde(default)]
  pub name: Option<String>,
  #[serde(default)]
  pub display_name: Option<String>,
  #[serde(default)]
  pub purpose: Option<String>,
  #[serde(rename = "type")]
  pub kind: i64,
  #[serde(default)]
  pub create_at: Option<i64>,
  #[serde(default)]
  pub close_at: Option<i64>,
  #[serde(default)]
  pub creator_id: Option<SourceId>,
  #[serde(default)]
  pub members: Option<Vec<SourceId>>,
}

impl Channel {
  pub fn members(&self) -> &[SourceId] {
    self.members.as_deref().unwrap_or_default()
  }

  pub fn is_archived(&self) -> bool {
    self.close_at.is_some_and(|at| at > 0)
  }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportUser {
  pub id: SourceId,
  #[serde(default)]
  pub username: Option<String>,
  #[serde(default)]
  pub deleted: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct BotRow {
  id: SourceId,
  #[serde(default)]
  name: Option<String>,
}

/// One post of the export.
#[derive(Debug, Clone, Deserialize)]
pub struct Post {
  pub id: SourceId,
  pub channel_id: SourceId,
  pub user_id: SourceId,
  #[serde(rename = "type")]
  pub kind: i64,
  #[serde(default)]
  pub message: Option<String>,
  pub create_at: i64,
  #[serde(default)]
  pub update_at: Option<i64>,
  #[serde(default)]
  pub thread_id: Option<SourceId>,
  #[serde(default)]
  pub mentions: Option<Vec<SourceId>>,
  #[serde(default)]
  pub file_props: Option<FileProps>,
}

impl Post {
  /// The thread root this post answers, if it is a reply (a root names itself).
  pub fn maybe_root(&self) -> Option<SourceId> {
    self.thread_id.filter(|root| *root != self.id)
  }

  /// Edited a while after it was written (Synology sets `update_at` on every write).
  pub fn maybe_edited_at(&self) -> Option<i64> {
    self.update_at.filter(|at| *at > self.create_at + EDIT_SLACK_MS)
  }
}

/// `update_at` within this of `create_at` is the write itself, not an edit.
const EDIT_SLACK_MS: i64 = 1_000;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileProps {
  #[serde(default)]
  pub name: Option<String>,
  #[serde(default)]
  pub size: Option<u64>,
  #[serde(default)]
  pub is_image: bool,
  /// The extension Synology recorded (`pdf`, `png`).
  #[serde(rename = "type", default)]
  pub extension: Option<String>,
  #[serde(default)]
  pub image: Option<ImageSize>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ImageSize {
  #[serde(default)]
  pub width: u32,
  #[serde(default)]
  pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reaction {
  pub post_id: SourceId,
  pub user_id: SourceId,
  /// The gemoji short code.
  pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ReadRow {
  channel_id: SourceId,
  user_id: SourceId,
  last_view_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
struct MemberRow {
  channel_id: SourceId,
  user_id: SourceId,
  #[serde(default)]
  mute: Option<bool>,
}

/// A webhook to recreate as a bridge source (its token is not in the export).
#[derive(Debug, Clone, Deserialize)]
pub struct Webhook {
  #[serde(default)]
  pub name: Option<String>,
  /// Absent for a webhook whose channel is gone.
  #[serde(default)]
  pub channel_id: Option<SourceId>,
  #[serde(default)]
  pub url: Option<String>,
  #[serde(default)]
  pub disabled: bool,
  #[serde(skip)]
  pub is_outgoing: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
  #[error("{path}: {source}")]
  Io { path: PathBuf, source: std::io::Error },
  #[error("{path}, line {line}: {source}")]
  Json {
    path: PathBuf,
    line: usize,
    source: serde_json::Error,
  },
  #[error("posts.ndjson is not in channel order: channel {0} appears twice")]
  PostsOutOfOrder(SourceId),
}

impl Export {
  /// Reads the export in `root`; attachments are looked up in `uploads` (default
  /// `root/raw/uploads`). Optional files that are missing count as empty.
  pub fn open(root: &Path, maybe_uploads: Option<&Path>) -> Result<Self, ExportError> {
    let channels = read_all::<Channel>(&root.join("channels.ndjson"), true)?
      .into_iter()
      .map(|channel| (channel.id, channel))
      .collect();
    let users = read_all::<ExportUser>(&root.join("users.ndjson"), true)?
      .into_iter()
      .map(|user| (user.id, user))
      .collect();
    let bots = read_all::<BotRow>(&root.join("bots.ndjson"), false)?
      .into_iter()
      .filter_map(|bot| {
        bot
          .name
          .filter(|name| !name.trim().is_empty())
          .map(|name| (bot.id, name))
      })
      .collect();
    let mut reactions: HashMap<SourceId, Vec<Reaction>> = HashMap::new();
    for reaction in read_all::<Reaction>(&root.join("reactions.ndjson"), false)? {
      reactions.entry(reaction.post_id).or_default().push(reaction);
    }
    let reads = read_all::<ReadRow>(&root.join("reads.ndjson"), false)?
      .into_iter()
      .map(|row| ((row.channel_id, row.user_id), row.last_view_at))
      .collect();
    let favorites = read_all::<MemberRow>(&root.join("favorites.ndjson"), false)?
      .into_iter()
      .map(|row| (row.channel_id, row.user_id))
      .collect();
    let mutes = read_all::<MemberRow>(&root.join("notifications.ndjson"), false)?
      .into_iter()
      .filter(|row| row.mute == Some(true))
      .map(|row| (row.channel_id, row.user_id))
      .collect();
    let mut webhooks = read_all::<Webhook>(&root.join("webhooks_incoming.ndjson"), false)?;
    let outgoing = read_all::<Webhook>(&root.join("webhooks_outgoing.ndjson"), false)?;
    webhooks.extend(outgoing.into_iter().map(|webhook| Webhook {
      is_outgoing: true,
      ..webhook
    }));
    let posts_path = root.join("posts.ndjson");
    let (post_ranges, author_counts) = index_posts(&posts_path)?;
    Ok(Self {
      root: root.to_owned(),
      uploads: maybe_uploads.map_or_else(|| root.join("raw").join("uploads"), Path::to_owned),
      channels,
      users,
      bots,
      reactions,
      reads,
      favorites,
      mutes,
      webhooks,
      posts_path,
      post_ranges,
      author_counts,
    })
  }

  /// How many posts the channel has.
  pub fn post_count(&self, channel: SourceId) -> u64 {
    self.post_ranges.get(&channel).map_or(0, |range| range.count)
  }

  /// How many posts each author wrote in the channel.
  pub fn authors(&self, channel: SourceId) -> HashMap<SourceId, u64> {
    self.author_counts.get(&channel).cloned().unwrap_or_default()
  }

  /// The channel's posts in the export's order (time), read from the file.
  pub fn posts(&self, channel: SourceId) -> Result<Vec<Post>, ExportError> {
    let Some(range) = self.post_ranges.get(&channel).copied() else {
      return Ok(Vec::new());
    };
    let io = |source| ExportError::Io {
      path: self.posts_path.clone(),
      source,
    };
    let mut file = File::open(&self.posts_path).map_err(io)?;
    file.seek(SeekFrom::Start(range.start)).map_err(io)?;
    let reader = BufReader::new(file.take(range.end - range.start));
    let mut posts = Vec::with_capacity(usize::try_from(range.count).unwrap_or(0));
    for (index, line) in reader.lines().enumerate() {
      let line = line.map_err(io)?;
      if line.trim().is_empty() {
        continue;
      }
      let post: Post = serde_json::from_str(&line).map_err(|source| ExportError::Json {
        path: self.posts_path.clone(),
        line: index + 1,
        source,
      })?;
      posts.push(post);
    }
    posts.sort_by_key(|post| (post.create_at, post.id));
    Ok(posts)
  }

  /// Where a post's attachment is.
  pub fn upload_path(&self, post: &Post) -> PathBuf {
    self
      .uploads
      .join(post.channel_id.to_string())
      .join(post.id.to_string())
  }

  /// Where a sticker's picture is.
  pub fn sticker_path(&self, name: &str) -> PathBuf {
    self.root.join("stickers").join(format!("{name}.png"))
  }
}

/// Reads every line of an NDJSON file. A missing file is an error only when `is_required`.
fn read_all<T: DeserializeOwned>(path: &Path, is_required: bool) -> Result<Vec<T>, ExportError> {
  let file = match File::open(path) {
    Ok(file) => file,
    Err(error) if error.kind() == std::io::ErrorKind::NotFound && !is_required => return Ok(Vec::new()),
    Err(source) => {
      return Err(ExportError::Io {
        path: path.to_owned(),
        source,
      });
    },
  };
  let mut rows = Vec::new();
  for (index, line) in BufReader::new(file).lines().enumerate() {
    let line = line.map_err(|source| ExportError::Io {
      path: path.to_owned(),
      source,
    })?;
    if line.trim().is_empty() {
      continue;
    }
    rows.push(serde_json::from_str(&line).map_err(|source| ExportError::Json {
      path: path.to_owned(),
      line: index + 1,
      source,
    })?);
  }
  Ok(rows)
}

type PostIndex = (
  HashMap<SourceId, PostRange>,
  HashMap<SourceId, HashMap<SourceId, u64>>,
);

/// The byte range of each channel's posts, and the posts per author. Reads only the `channel_id`
/// and `user_id` of every line.
fn index_posts(path: &Path) -> Result<PostIndex, ExportError> {
  #[derive(Deserialize)]
  struct ChannelOnly {
    channel_id: SourceId,
    user_id: SourceId,
  }
  let io = |source| ExportError::Io {
    path: path.to_owned(),
    source,
  };
  let mut reader = BufReader::new(File::open(path).map_err(io)?);
  let mut ranges: HashMap<SourceId, PostRange> = HashMap::new();
  let mut authors: HashMap<SourceId, HashMap<SourceId, u64>> = HashMap::new();
  let mut offset = 0_u64;
  let mut maybe_current: Option<SourceId> = None;
  let mut line = String::new();
  let mut number = 0;
  loop {
    line.clear();
    let read = reader.read_line(&mut line).map_err(io)?;
    if read == 0 {
      return Ok((ranges, authors));
    }
    number += 1;
    let start = offset;
    offset += read as u64;
    if line.trim().is_empty() {
      continue;
    }
    let row: ChannelOnly = serde_json::from_str(&line).map_err(|source| ExportError::Json {
      path: path.to_owned(),
      line: number,
      source,
    })?;
    if maybe_current != Some(row.channel_id) {
      if ranges.contains_key(&row.channel_id) {
        return Err(ExportError::PostsOutOfOrder(row.channel_id));
      }
      ranges.insert(
        row.channel_id,
        PostRange {
          start,
          end: start,
          count: 0,
        },
      );
      maybe_current = Some(row.channel_id);
    }
    if let Some(range) = ranges.get_mut(&row.channel_id) {
      range.end = offset;
      range.count += 1;
    }
    *authors
      .entry(row.channel_id)
      .or_default()
      .entry(row.user_id)
      .or_default() += 1;
  }
}

#[cfg(test)]
mod tests {
  use std::io::Write;

  use super::*;

  #[test]
  fn posts_are_read_per_channel_in_time_order() {
    let dir = scratch("posts");
    write(
      &dir,
      "posts.ndjson",
      &[
        r#"{"id":2,"channel_id":1,"user_id":7,"type":1,"message":"b","create_at":20}"#,
        r#"{"id":1,"channel_id":1,"user_id":7,"type":1,"message":"a","create_at":10}"#,
        r#"{"id":5,"channel_id":3,"user_id":7,"type":2,"message":"","create_at":5,"file_props":{"name":"x.pdf","size":3}}"#,
      ],
    );
    minimal(&dir);
    let export = Export::open(&dir, None).unwrap();
    assert_eq!(export.post_count(1), 2);
    assert_eq!(export.post_count(3), 1);
    assert_eq!(export.post_count(9), 0);
    let ids: Vec<_> = export.posts(1).unwrap().iter().map(|post| post.id).collect();
    assert_eq!(ids, [1, 2]);
    assert_eq!(
      export.posts(3).unwrap()[0]
        .file_props
        .as_ref()
        .unwrap()
        .name
        .as_deref(),
      Some("x.pdf")
    );
  }

  #[test]
  fn interleaved_channels_are_refused() {
    let dir = scratch("interleaved");
    write(
      &dir,
      "posts.ndjson",
      &[
        r#"{"id":1,"channel_id":1,"user_id":7,"type":1,"create_at":1}"#,
        r#"{"id":2,"channel_id":2,"user_id":7,"type":1,"create_at":1}"#,
        r#"{"id":3,"channel_id":1,"user_id":7,"type":1,"create_at":2}"#,
      ],
    );
    minimal(&dir);
    assert!(matches!(
      Export::open(&dir, None),
      Err(ExportError::PostsOutOfOrder(1))
    ));
  }

  #[test]
  fn a_broken_line_names_its_file_and_line() {
    let dir = scratch("broken");
    write(&dir, "posts.ndjson", &[]);
    write(&dir, "channels.ndjson", &[r#"{"id":1,"type":1}"#, "{not json"]);
    write(&dir, "users.ndjson", &[]);
    let Err(ExportError::Json { line, .. }) = Export::open(&dir, None) else {
      panic!("expected a JSON error");
    };
    assert_eq!(line, 2);
  }

  #[test]
  fn a_reply_names_its_root_and_an_edit_needs_time() {
    let post: Post = serde_json::from_str(
      r#"{"id":5,"channel_id":1,"user_id":2,"type":1,"create_at":1000,"update_at":1500,"thread_id":4}"#,
    )
    .unwrap();
    assert_eq!(post.maybe_root(), Some(4));
    assert_eq!(post.maybe_edited_at(), None);
    let root: Post = serde_json::from_str(
      r#"{"id":4,"channel_id":1,"user_id":2,"type":1,"create_at":1,"update_at":90000,"thread_id":4}"#,
    )
    .unwrap();
    assert_eq!(root.maybe_root(), None);
    assert_eq!(root.maybe_edited_at(), Some(90_000));
  }

  fn minimal(dir: &Path) {
    write(dir, "channels.ndjson", &[]);
    write(dir, "users.ndjson", &[]);
  }

  fn write(dir: &Path, name: &str, lines: &[&str]) {
    let mut file = File::create(dir.join(name)).unwrap();
    for line in lines {
      writeln!(file, "{line}").unwrap();
    }
  }

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-import-export-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }
}
