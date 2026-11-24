//! A Mattermost bulk export (`mmctl export create --attachments`, unpacked; docs/import-mattermost.md):
//! one JSON object per line (`version`, `team`, `channel`, `user`, `post`, `direct_channel`,
//! `direct_post`, …) and the attachments under `data/`, named by the posts relative to the
//! export's folder. The export names everything by name, not ID: a channel is `team/name`, a
//! person their user name.
//!
//! The file is read twice, line by line: first everything but the posts (people, channels), then
//! the posts, so mentions find every person whatever the order of the lines.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::text::mattermost_text;
use super::{
  BotRow, ChannelRow, ConvertError, ConvertSummary, ExportWriter, FileRow, MAX_LINE_BYTES, PostRow,
  ReactionRow, Skipped, SourceKind, UserRow, contained_file, join_inside, stable_id,
};
use crate::export::{CHANNEL_DEFAULT, CHANNEL_DM, CHANNEL_GROUP, POST_FILE, POST_TEXT, SourceId};

/// One line of the export. Unknown types (schemes, roles, emoji) are left out.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Line {
  Version {},
  Team {
    team: Team,
  },
  Channel {
    channel: Channel,
  },
  User {
    user: User,
  },
  Post {
    post: Post,
  },
  DirectChannel {
    direct_channel: DirectChannel,
  },
  DirectPost {
    direct_post: DirectPost,
  },
  #[serde(other)]
  Unknown,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Team {
  pub name: String,
  pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Channel {
  pub team: String,
  pub name: String,
  pub display_name: Option<String>,
  /// `O`: open, `P`: private.
  #[serde(rename = "type")]
  pub kind: Option<String>,
  pub header: Option<String>,
  pub purpose: Option<String>,
  pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct User {
  pub username: String,
  pub email: Option<String>,
  pub nickname: Option<String>,
  pub first_name: Option<String>,
  pub last_name: Option<String>,
  pub delete_at: Option<i64>,
  pub teams: Option<Vec<UserTeam>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserTeam {
  pub name: String,
  pub channels: Option<Vec<UserChannel>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct UserChannel {
  pub name: String,
  pub favorite: Option<bool>,
}

/// What a post, a reply and a direct post have in common.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PostBody {
  pub user: String,
  pub message: Option<String>,
  pub create_at: Option<i64>,
  pub edit_at: Option<i64>,
  pub delete_at: Option<i64>,
  /// Webhook and bot markers and app attachments; read leniently, its shape varies.
  pub props: Option<Value>,
  pub reactions: Option<Vec<Reaction>>,
  pub attachments: Option<Vec<Attachment>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Post {
  pub team: String,
  pub channel: String,
  #[serde(flatten)]
  pub body: PostBody,
  pub replies: Option<Vec<PostBody>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DirectChannel {
  pub members: Option<Vec<String>>,
  pub participants: Option<Vec<Participant>>,
  pub favorited_by: Option<Vec<String>>,
  pub header: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Participant {
  pub username: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct DirectPost {
  pub channel_members: Option<Vec<String>>,
  #[serde(flatten)]
  pub body: PostBody,
  pub replies: Option<Vec<PostBody>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Reaction {
  pub user: String,
  pub emoji_name: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Attachment {
  pub path: String,
}

/// Parses one line of the export.
pub fn parse_line(bytes: &[u8]) -> Result<Line, serde_json::Error> {
  serde_json::from_slice(bytes)
}

/// Converts the export file `source` (`import.jsonl` or the like; attachments relative to its
/// folder) into `out`.
pub fn convert(source: &Path, out: &Path) -> Result<ConvertSummary, ConvertError> {
  let root = source.parent().map_or_else(|| PathBuf::from("."), Path::to_owned);
  let mut state = State::default();
  let mut writer = ExportWriter::create(out)?;
  for_each_line(source, |line| {
    match line {
      Line::Team { team } => state.team(team),
      Line::Channel { channel } => state.channel(&channel),
      Line::User { user } => state.user(&user),
      Line::DirectChannel { direct_channel } => state.direct_channel(&direct_channel),
      Line::Unknown => writer.skip(Skipped::UnknownLine),
      Line::Version {} | Line::Post { .. } | Line::DirectPost { .. } => {},
    }
    Ok(())
  })?;
  state.attach_memberships();
  let usernames: HashMap<String, SourceId> = state
    .users
    .iter()
    .map(|(name, row)| (name.clone(), row.id))
    .collect();
  for_each_line(source, |line| {
    let mut context = Context {
      root: &root,
      usernames: &usernames,
      state: &mut state,
      writer: &mut writer,
    };
    match line {
      Line::Post { post } => context.post(&post),
      Line::DirectPost { direct_post } => context.direct_post(&direct_post),
      _ => Ok(()),
    }
  })?;
  let favorites = state.favorites.clone();
  let (channels, users, bots) = state.into_rows();
  writer.finish(channels, users, bots, &favorites)
}

/// Calls `each` with every line; blank lines are skipped.
fn for_each_line(
  path: &Path,
  mut each: impl FnMut(Line) -> Result<(), ConvertError>,
) -> Result<(), ConvertError> {
  let io = |source| ConvertError::Io {
    path: path.to_owned(),
    source,
  };
  let mut reader = BufReader::new(File::open(path).map_err(io)?);
  let mut buffer = Vec::new();
  let mut number = 0;
  loop {
    buffer.clear();
    let limit = u64::try_from(MAX_LINE_BYTES)
      .unwrap_or(u64::MAX)
      .saturating_add(1);
    let read = (&mut reader)
      .take(limit)
      .read_until(b'\n', &mut buffer)
      .map_err(io)?;
    if read == 0 {
      return Ok(());
    }
    number += 1;
    if buffer.len() > MAX_LINE_BYTES {
      return Err(ConvertError::TooLarge {
        path: path.to_owned(),
        limit_bytes: limit - 1,
      });
    }
    if buffer.iter().all(u8::is_ascii_whitespace) {
      continue;
    }
    let line = parse_line(&buffer).map_err(|source| ConvertError::Json {
      path: path.to_owned(),
      maybe_line: Some(number),
      source,
    })?;
    each(line)?;
  }
}

/// What the export's lines say about teams, people and conversations.
#[derive(Default)]
struct State {
  teams: HashMap<String, String>,
  /// `team/name` or `dm:a,b` → the conversation.
  channels: BTreeMap<String, ChannelRow>,
  /// Lower-case user name → the person.
  users: BTreeMap<String, UserRow>,
  bots: BTreeMap<String, BotRow>,
  /// `team/channel` → the user names that are members, from the people's lines.
  members: BTreeMap<String, BTreeSet<String>>,
  /// User names that marked `team/channel` a favourite.
  favorite_names: Vec<(String, String)>,
  favorites: BTreeSet<(SourceId, SourceId)>,
}

impl State {
  fn team(&mut self, team: Team) {
    let display = team
      .display_name
      .filter(|name| !name.trim().is_empty())
      .unwrap_or_else(|| team.name.clone());
    self.teams.insert(team.name, display);
  }

  fn channel(&mut self, channel: &Channel) {
    let key = format!("{}/{}", channel.team, channel.name);
    let is_public = channel.kind.as_deref() != Some("P");
    let purpose = [&channel.purpose, &channel.header]
      .into_iter()
      .flatten()
      .find(|text| !text.trim().is_empty())
      .cloned();
    self.channels.insert(
      key.clone(),
      ChannelRow {
        id: stable_id(SourceKind::Mattermost, "channel", &key),
        name: channel.name.clone(),
        display_name: channel
          .display_name
          .clone()
          .filter(|name| !name.trim().is_empty()),
        purpose,
        kind: if is_public { CHANNEL_DEFAULT } else { CHANNEL_GROUP },
        create_at: None,
        close_at: channel.deleted_at.filter(|at| *at > 0),
        creator_id: None,
        members: Vec::new(),
        source_key: key,
        is_public,
        maybe_space: Some(channel.team.clone()),
      },
    );
  }

  fn user(&mut self, user: &User) {
    if user.username.trim().is_empty() {
      return;
    }
    self.user_id(&user.username);
    let full_name = [&user.first_name, &user.last_name]
      .into_iter()
      .flatten()
      .filter(|part| !part.trim().is_empty())
      .cloned()
      .collect::<Vec<_>>()
      .join(" ");
    let display_name = [Some(full_name), user.nickname.clone()]
      .into_iter()
      .flatten()
      .find(|name| !name.trim().is_empty())
      .unwrap_or_else(|| user.username.clone());
    if let Some(row) = self.users.get_mut(&user.username.to_lowercase()) {
      row.deleted = user.delete_at.is_some_and(|at| at > 0);
      row.display_name = display_name;
      row.maybe_email = user.email.clone().filter(|email| !email.trim().is_empty());
    }
    for team in user.teams.iter().flatten() {
      for channel in team.channels.iter().flatten() {
        let key = format!("{}/{}", team.name, channel.name);
        self
          .members
          .entry(key.clone())
          .or_default()
          .insert(user.username.to_lowercase());
        if channel.favorite == Some(true) {
          self.favorite_names.push((key, user.username.to_lowercase()));
        }
      }
    }
  }

  fn direct_channel(&mut self, direct: &DirectChannel) {
    let members: Vec<String> = match (&direct.members, &direct.participants) {
      (Some(members), _) => members.clone(),
      (None, Some(participants)) => participants
        .iter()
        .map(|participant| participant.username.clone())
        .collect(),
      (None, None) => Vec::new(),
    };
    let key = self.direct_key(&members);
    for name in direct.favorited_by.iter().flatten() {
      self.favorite_names.push((key.clone(), name.to_lowercase()));
    }
  }

  /// The person with this user name; one without a line of their own is added as deleted.
  fn user_id(&mut self, username: &str) -> SourceId {
    let key = username.to_lowercase();
    if let Some(row) = self.users.get(&key) {
      return row.id;
    }
    let id = stable_id(SourceKind::Mattermost, "user", &key);
    self.users.insert(
      key.clone(),
      UserRow {
        id,
        username: Some(username.to_owned()),
        deleted: true,
        source_key: username.to_owned(),
        display_name: username.to_owned(),
        maybe_email: None,
      },
    );
    id
  }

  fn bot(&mut self, name: &str) -> SourceId {
    let key = name.to_lowercase();
    if let Some(bot) = self.bots.get(&key) {
      return bot.id;
    }
    let id = stable_id(SourceKind::Mattermost, "bot", &key);
    self.bots.insert(
      key,
      BotRow {
        id,
        name: name.to_owned(),
      },
    );
    id
  }

  /// The DM or group conversation of these people, added if the export didn't list it.
  fn direct_key(&mut self, members: &[String]) -> String {
    let names: BTreeSet<String> = members.iter().map(|name| name.to_lowercase()).collect();
    let key = format!("dm:{}", names.iter().cloned().collect::<Vec<_>>().join(","));
    if self.channels.contains_key(&key) {
      return key;
    }
    let ids: Vec<SourceId> = members.iter().map(|name| self.user_id(name)).collect();
    let is_group = names.len() > 2;
    let name = if is_group {
      names.iter().cloned().collect::<Vec<_>>().join(", ")
    } else {
      String::new()
    };
    self.channels.insert(
      key.clone(),
      ChannelRow {
        id: stable_id(SourceKind::Mattermost, "direct", &key),
        name,
        display_name: None,
        purpose: None,
        kind: if is_group { CHANNEL_GROUP } else { CHANNEL_DM },
        create_at: None,
        close_at: None,
        creator_id: None,
        members: ids,
        source_key: key.clone(),
        is_public: false,
        maybe_space: None,
      },
    );
    key
  }

  /// Channel members and favourites, once every person is known.
  fn attach_memberships(&mut self) {
    let members = std::mem::take(&mut self.members);
    for (key, names) in members {
      let ids: Vec<SourceId> = names.iter().map(|name| self.user_id(name)).collect();
      if let Some(row) = self.channels.get_mut(&key) {
        row.members = ids;
      }
    }
    for (key, name) in std::mem::take(&mut self.favorite_names) {
      let user = self.user_id(&name);
      if let Some(row) = self.channels.get(&key) {
        self.favorites.insert((row.id, user));
      }
    }
    for row in self.channels.values_mut() {
      if let Some(team) = row.maybe_space.as_ref().and_then(|team| self.teams.get(team)) {
        row.maybe_space = Some(team.clone());
      }
    }
  }

  fn into_rows(self) -> (Vec<ChannelRow>, Vec<UserRow>, Vec<BotRow>) {
    (
      self.channels.into_values().collect(),
      self.users.into_values().collect(),
      self.bots.into_values().collect(),
    )
  }
}

/// The posts being converted.
struct Context<'a> {
  root: &'a Path,
  usernames: &'a HashMap<String, SourceId>,
  state: &'a mut State,
  writer: &'a mut ExportWriter,
}

impl Context<'_> {
  fn post(&mut self, post: &Post) -> Result<(), ConvertError> {
    let key = format!("{}/{}", post.team, post.channel);
    let Some(channel_id) = self.state.channels.get(&key).map(|row| row.id) else {
      self.writer.skip(Skipped::UnknownChannel);
      return Ok(());
    };
    self.thread(&key, channel_id, &post.body, post.replies.as_deref())
  }

  fn direct_post(&mut self, post: &DirectPost) -> Result<(), ConvertError> {
    let members = post.channel_members.clone().unwrap_or_default();
    if members.is_empty() {
      self.writer.skip(Skipped::UnknownChannel);
      return Ok(());
    }
    let key = self.state.direct_key(&members);
    let Some(channel_id) = self.state.channels.get(&key).map(|row| row.id) else {
      self.writer.skip(Skipped::UnknownChannel);
      return Ok(());
    };
    self.thread(&key, channel_id, &post.body, post.replies.as_deref())
  }

  /// A post and its replies.
  fn thread(
    &mut self,
    channel_key: &str,
    channel_id: SourceId,
    root: &PostBody,
    maybe_replies: Option<&[PostBody]>,
  ) -> Result<(), ConvertError> {
    let Some(root_id) = self.one(channel_key, channel_id, root, None)? else {
      return Ok(());
    };
    for reply in maybe_replies.unwrap_or_default() {
      self.one(channel_key, channel_id, reply, Some(root_id))?;
    }
    Ok(())
  }

  /// One post; its ID, or `None` when it is left out.
  fn one(
    &mut self,
    channel_key: &str,
    channel_id: SourceId,
    body: &PostBody,
    maybe_root: Option<SourceId>,
  ) -> Result<Option<SourceId>, ConvertError> {
    if body.delete_at.is_some_and(|at| at > 0) {
      self.writer.skip(Skipped::Deleted);
      return Ok(None);
    }
    let Some(create_at) = body.create_at.filter(|at| *at > 0) else {
      self.writer.skip(Skipped::BadTime);
      return Ok(None);
    };
    let base = match maybe_root {
      Some(root) => format!("{channel_key}/{root}/{}/{create_at}", body.user.to_lowercase()),
      None => format!("{channel_key}/{}/{create_at}", body.user.to_lowercase()),
    };
    // Two posts of one person in one millisecond: the later one gets a number.
    let (key, id) = (1_u32..)
      .map(|ordinal| match ordinal {
        1 => base.clone(),
        _ => format!("{base}#{ordinal}"),
      })
      .map(|key| {
        let id = stable_id(SourceKind::Mattermost, "post", &key);
        (key, id)
      })
      .find(|(_, id)| !self.writer.has_post(*id))
      .unwrap_or_default();
    let author = self.author(body);
    let files = self.files_of(body);
    let mut post = PostRow {
      id,
      channel_id,
      user_id: author,
      kind: POST_TEXT,
      message: mattermost_text(&text_of(body), self.usernames),
      create_at,
      update_at: body.edit_at.filter(|at| *at > create_at),
      thread_id: maybe_root,
      file_props: None,
      source_path: None,
    };
    let mut files = files.into_iter();
    if let Some((props, path)) = files.next() {
      post.kind = POST_FILE;
      post.file_props = Some(props);
      post.source_path = Some(path);
    }
    self.writer.post(&key, &post)?;
    for reaction in body.reactions.iter().flatten() {
      if reaction.emoji_name.is_empty() || reaction.user.is_empty() {
        continue;
      }
      let user_id = self.state.user_id(&reaction.user);
      self.writer.reaction(&ReactionRow {
        post_id: id,
        user_id,
        name: reaction.emoji_name.clone(),
      })?;
    }
    for (offset, (props, path)) in (1_i64..).zip(files) {
      let file_key = format!("{key}/file/{offset}");
      let extra = PostRow {
        id: stable_id(SourceKind::Mattermost, "post", &file_key),
        message: String::new(),
        create_at: create_at.saturating_add(offset),
        update_at: None,
        file_props: Some(props),
        source_path: Some(path),
        ..post.clone()
      };
      self.writer.post(&file_key, &extra)?;
    }
    Ok(Some(id))
  }

  /// The person, bot or webhook that wrote it.
  fn author(&mut self, body: &PostBody) -> SourceId {
    let props = body.props.as_ref();
    if is_true(props.and_then(|props| props.get("from_webhook"))) {
      let name = props
        .and_then(|props| props.get("override_username"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Webhook");
      return self.state.bot(name);
    }
    if is_true(props.and_then(|props| props.get("from_bot"))) && !body.user.is_empty() {
      return self.state.bot(&body.user);
    }
    self.state.user_id(&body.user)
  }

  fn files_of(&mut self, body: &PostBody) -> Vec<(FileRow, PathBuf)> {
    let mut files = Vec::new();
    for attachment in body.attachments.iter().flatten() {
      let maybe_path =
        join_inside(self.root, &attachment.path).and_then(|path| contained_file(self.root, path));
      let Some(path) = maybe_path else {
        self.writer.skip(Skipped::UnsafePath);
        continue;
      };
      let Some(name) = path.file_name().and_then(|name| name.to_str()).map(str::to_owned) else {
        self.writer.skip(Skipped::UnsafePath);
        continue;
      };
      let extension = Path::new(&name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
      let is_image = extension.as_deref().is_some_and(|extension| {
        matches!(
          extension,
          "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "avif" | "bmp"
        )
      });
      files.push((
        FileRow {
          size: std::fs::metadata(&path).ok().map(|metadata| metadata.len()),
          name,
          is_image,
          extension,
        },
        path,
      ));
    }
    files
  }
}

/// `true` or `"true"`, as Mattermost writes its markers.
fn is_true(maybe_value: Option<&Value>) -> bool {
  match maybe_value {
    Some(Value::Bool(value)) => *value,
    Some(Value::String(value)) => value.eq_ignore_ascii_case("true"),
    _ => false,
  }
}

/// The text, or its app attachments' when it has none.
fn text_of(body: &PostBody) -> String {
  let text = body.message.clone().unwrap_or_default();
  if !text.trim().is_empty() {
    return text;
  }
  let Some(Value::Array(attachments)) = body.props.as_ref().and_then(|props| props.get("attachments")) else {
    return text;
  };
  attachments
    .iter()
    .filter_map(|attachment| {
      ["pretext", "text", "fallback"]
        .into_iter()
        .filter_map(|field| attachment.get(field).and_then(Value::as_str))
        .find(|text| !text.trim().is_empty())
        .map(str::to_owned)
    })
    .collect::<Vec<_>>()
    .join("\n")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn lines_of_every_known_type_parse() {
    let lines = [
      r#"{"type":"version","version":1,"info":{"generator":"x"}}"#,
      r#"{"type":"team","team":{"name":"team-a","display_name":"Team A"}}"#,
      r#"{"type":"channel","channel":{"team":"team-a","name":"town-square","type":"O"}}"#,
      r#"{"type":"user","user":{"username":"anna","teams":[{"name":"team-a","channels":[{"name":"town-square","favorite":true}]}]}}"#,
      r#"{"type":"post","post":{"team":"team-a","channel":"town-square","user":"anna","message":"hi","create_at":1,"replies":[{"user":"ben","message":"yo","create_at":2}]}}"#,
      r#"{"type":"direct_channel","direct_channel":{"members":["anna","ben"]}}"#,
      r#"{"type":"direct_post","direct_post":{"channel_members":["anna","ben"],"user":"anna","message":"hi","create_at":3}}"#,
      r#"{"type":"scheme","scheme":{"name":"x"}}"#,
    ];
    let parsed: Vec<Line> = lines
      .iter()
      .map(|line| parse_line(line.as_bytes()).unwrap())
      .collect();
    assert!(matches!(parsed[4], Line::Post { ref post } if post.replies.as_ref().unwrap().len() == 1));
    assert!(matches!(parsed[7], Line::Unknown));
  }

  #[test]
  fn malformed_lines_are_errors_not_panics() {
    for input in [
      &b""[..],
      b"{",
      b"{}",
      b"{\"type\":5}",
      b"{\"type\":\"post\"}",
      b"{\"type\":\"post\",\"post\":{\"create_at\":\"soon\"}}",
      b"{\"type\":\"user\",\"user\":{\"teams\":{}}}",
      b"[]",
    ] {
      assert!(parse_line(input).is_err(), "{:?}", String::from_utf8_lossy(input));
    }
  }

  #[test]
  fn markers_are_read_leniently() {
    assert!(is_true(Some(&Value::String("true".into()))));
    assert!(is_true(Some(&Value::Bool(true))));
    assert!(!is_true(Some(&Value::from(1))));
    assert!(!is_true(None));
  }
}
