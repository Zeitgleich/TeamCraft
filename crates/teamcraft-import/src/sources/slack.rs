//! A Slack workspace export, unpacked (docs/import-slack.md): `users.json`, `channels.json`,
//! `groups.json`, `dms.json`, `mpims.json` and a folder per conversation with a JSON file per day.
//! Slack's export has no files, only links that need a token; the conversion never downloads
//! them. Files downloaded beforehand into `__uploads/<file ID>/<name>` are taken from there.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::text::{SlackNames, slack_text};
use super::{
  BotRow, ChannelRow, ConvertError, ConvertSummary, ExportWriter, FileRow, MAX_JSON_FILE_BYTES, PostRow,
  ReactionRow, Skipped, SourceKind, UserRow, contained_file, join_inside, safe_file_name, stable_id,
};
use crate::export::{CHANNEL_DEFAULT, CHANNEL_DM, CHANNEL_GROUP, POST_FILE, POST_TEXT, SourceId};

/// Where downloaded files are looked for: `__uploads/<file ID>/<name>`.
pub const UPLOADS_DIR: &str = "__uploads";

/// Message subtypes that are what somebody wrote; every other subtype is Slack's own notice.
const WRITTEN_SUBTYPES: &[&str] = &[
  "bot_message",
  "me_message",
  "thread_broadcast",
  "reply_broadcast",
  "file_share",
];

/// One message of a day file. Unknown fields are ignored; missing ones are empty.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Message {
  #[serde(rename = "type")]
  pub kind: Option<String>,
  pub subtype: Option<String>,
  pub user: Option<String>,
  pub bot_id: Option<String>,
  pub username: Option<String>,
  pub bot_profile: Option<BotProfile>,
  pub text: Option<String>,
  pub ts: Option<String>,
  pub thread_ts: Option<String>,
  pub edited: Option<Edited>,
  pub reactions: Option<Vec<Reaction>>,
  pub files: Option<Vec<SlackFile>>,
  pub attachments: Option<Vec<Attachment>>,
  pub hidden: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BotProfile {
  pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Edited {
  pub ts: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Reaction {
  pub name: String,
  pub users: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct SlackFile {
  pub id: Option<String>,
  pub name: Option<String>,
  pub mimetype: Option<String>,
  pub filetype: Option<String>,
  pub size: Option<u64>,
  /// `hosted`, `snippet`, …; `tombstone`, `hidden_by_limit` and `external` aren't in an export.
  pub mode: Option<String>,
  pub is_external: bool,
}

/// An app's attachment: its text stands in for an empty message.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Attachment {
  pub fallback: Option<String>,
  pub pretext: Option<String>,
  pub text: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct User {
  id: String,
  name: Option<String>,
  real_name: Option<String>,
  deleted: bool,
  is_bot: bool,
  profile: Option<Profile>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Profile {
  email: Option<String>,
  real_name: Option<String>,
  display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Conversation {
  id: String,
  name: Option<String>,
  /// Seconds.
  created: Option<i64>,
  creator: Option<String>,
  is_archived: bool,
  members: Option<Vec<String>>,
  purpose: Option<Topic>,
  topic: Option<Topic>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct Topic {
  value: String,
}

/// Which list of the export a conversation is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConversationKind {
  Public,
  Private,
  GroupDm,
  Dm,
}

/// Parses one day file.
pub fn parse_day(bytes: &[u8]) -> Result<Vec<Message>, serde_json::Error> {
  serde_json::from_slice(bytes)
}

/// Slack's `ts` (`"1503435956.000247"`, seconds with microseconds) in milliseconds.
pub fn parse_ts(ts: &str) -> Option<i64> {
  let (seconds, fraction) = ts.split_once('.').unwrap_or((ts, ""));
  if seconds.is_empty() || !seconds.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  let seconds: i64 = seconds.parse().ok()?;
  let millis: i64 = format!("{:0<3}", &fraction[..fraction.len().min(3)])
    .parse()
    .ok()?;
  seconds.checked_mul(1000)?.checked_add(millis)
}

/// Converts the unpacked export in `source` into `out`.
pub fn convert(source: &Path, out: &Path) -> Result<ConvertSummary, ConvertError> {
  let users: Vec<User> = read_json(&source.join("users.json"), true)?;
  let mut people = People::new(&users);
  let mut conversations = Vec::new();
  for (file, kind, is_required) in [
    ("channels.json", ConversationKind::Public, true),
    ("groups.json", ConversationKind::Private, false),
    ("mpims.json", ConversationKind::GroupDm, false),
    ("dms.json", ConversationKind::Dm, false),
  ] {
    for conversation in read_json::<Conversation>(&source.join(file), is_required)? {
      conversations.push((kind, conversation));
    }
  }
  let channel_names: HashMap<String, String> = conversations
    .iter()
    .filter_map(|(_, conversation)| {
      conversation
        .name
        .clone()
        .map(|name| (conversation.id.clone(), name))
    })
    .collect();
  let known_users = people.ids.clone();
  let names = SlackNames {
    users: &known_users,
    channels: &channel_names,
  };
  let mut writer = ExportWriter::create(out)?;
  let mut channels = Vec::with_capacity(conversations.len());
  for (kind, conversation) in &conversations {
    let row = channel_row(*kind, conversation, &mut people);
    let maybe_folder = match kind {
      ConversationKind::Dm => Some(conversation.id.as_str()),
      _ => conversation.name.as_deref(),
    }
    .and_then(safe_file_name)
    .and_then(|name| join_inside(source, name))
    .and_then(|folder| contained_file(source, folder));
    match maybe_folder {
      Some(folder) => {
        let mut context = Context {
          source,
          conversation: &conversation.id,
          channel_id: row.id,
          names: &names,
          people: &mut people,
          writer: &mut writer,
        };
        context.convert_folder(&folder)?;
      },
      None => writer.skip(Skipped::NoFolder),
    }
    channels.push(row);
  }
  let (users, bots) = people.into_rows();
  writer.finish(channels, users, bots, &BTreeSet::new())
}

/// The export's people and bots, with a converted ID each.
struct People {
  ids: HashMap<String, SourceId>,
  users: Vec<UserRow>,
  bots: HashMap<String, BotRow>,
  display: HashMap<String, String>,
}

impl People {
  fn new(users: &[User]) -> Self {
    let mut people = Self {
      ids: HashMap::new(),
      users: Vec::new(),
      bots: HashMap::new(),
      display: HashMap::new(),
    };
    for user in users.iter().filter(|user| !user.id.is_empty()) {
      let id = stable_id(SourceKind::Slack, "user", &user.id);
      let profile = user.profile.clone().unwrap_or_default();
      let display_name = [
        &profile.display_name,
        &profile.real_name,
        &user.real_name,
        &user.name,
      ]
      .into_iter()
      .flatten()
      .find(|name| !name.trim().is_empty())
      .cloned()
      .unwrap_or_else(|| user.id.clone());
      people.ids.insert(user.id.clone(), id);
      people.display.insert(user.id.clone(), display_name.clone());
      if user.is_bot {
        people.bots.insert(
          user.id.clone(),
          BotRow {
            id,
            name: display_name,
          },
        );
      } else {
        people.users.push(UserRow {
          id,
          username: user.name.clone(),
          deleted: user.deleted,
          source_key: user.id.clone(),
          display_name,
          maybe_email: profile.email.filter(|email| !email.trim().is_empty()),
        });
      }
    }
    people
  }

  /// The person with this Slack ID; somebody `users.json` doesn't have is added as deleted.
  fn user(&mut self, slack_id: &str) -> SourceId {
    if let Some(id) = self.ids.get(slack_id) {
      return *id;
    }
    let id = stable_id(SourceKind::Slack, "user", slack_id);
    self.ids.insert(slack_id.to_owned(), id);
    self.users.push(UserRow {
      id,
      username: None,
      deleted: true,
      source_key: slack_id.to_owned(),
      display_name: slack_id.to_owned(),
      maybe_email: None,
    });
    id
  }

  /// The bot that wrote a message without a person.
  fn bot(&mut self, message: &Message) -> SourceId {
    let maybe_name = [
      message.username.clone(),
      message
        .bot_profile
        .as_ref()
        .and_then(|profile| profile.name.clone()),
    ]
    .into_iter()
    .flatten()
    .find(|name| !name.trim().is_empty());
    let key = message
      .bot_id
      .clone()
      .or_else(|| maybe_name.clone())
      .unwrap_or_else(|| "bot".to_owned());
    let bot_key = format!("bot:{key}");
    if let Some(bot) = self.bots.get(&bot_key) {
      return bot.id;
    }
    let id = stable_id(SourceKind::Slack, "bot", &key);
    self.bots.insert(
      bot_key,
      BotRow {
        id,
        name: maybe_name.unwrap_or_else(|| "Bot".to_owned()),
      },
    );
    id
  }

  fn display_name(&self, slack_id: &str) -> String {
    self
      .display
      .get(slack_id)
      .cloned()
      .unwrap_or_else(|| slack_id.to_owned())
  }

  fn into_rows(self) -> (Vec<UserRow>, Vec<BotRow>) {
    (self.users, self.bots.into_values().collect())
  }
}

fn channel_row(kind: ConversationKind, conversation: &Conversation, people: &mut People) -> ChannelRow {
  let member_keys = conversation.members.clone().unwrap_or_default();
  let members: Vec<SourceId> = member_keys.iter().map(|member| people.user(member)).collect();
  let created_ms = conversation.created.and_then(|seconds| seconds.checked_mul(1000));
  let name = match kind {
    ConversationKind::GroupDm => member_keys
      .iter()
      .map(|member| people.display_name(member))
      .collect::<Vec<_>>()
      .join(", "),
    ConversationKind::Dm => String::new(),
    ConversationKind::Public | ConversationKind::Private => conversation.name.clone().unwrap_or_default(),
  };
  let purpose = [&conversation.purpose, &conversation.topic]
    .into_iter()
    .flatten()
    .map(|topic| topic.value.clone())
    .find(|value| !value.trim().is_empty());
  ChannelRow {
    id: stable_id(SourceKind::Slack, "channel", &conversation.id),
    name,
    display_name: None,
    purpose,
    kind: match kind {
      ConversationKind::Public => CHANNEL_DEFAULT,
      ConversationKind::Private | ConversationKind::GroupDm => CHANNEL_GROUP,
      ConversationKind::Dm => CHANNEL_DM,
    },
    create_at: created_ms,
    close_at: conversation.is_archived.then(|| created_ms.unwrap_or(1).max(1)),
    creator_id: conversation
      .creator
      .as_deref()
      .map(|creator| people.user(creator)),
    members,
    source_key: conversation.id.clone(),
    is_public: kind == ConversationKind::Public,
    maybe_space: None,
  }
}

/// One conversation being converted.
struct Context<'a> {
  source: &'a Path,
  conversation: &'a str,
  channel_id: SourceId,
  names: &'a SlackNames<'a>,
  people: &'a mut People,
  writer: &'a mut ExportWriter,
}

impl Context<'_> {
  /// Every day file of the folder, oldest first.
  fn convert_folder(&mut self, folder: &Path) -> Result<(), ConvertError> {
    let io = |source| ConvertError::Io {
      path: folder.to_owned(),
      source,
    };
    if !folder.is_dir() {
      return Ok(());
    }
    let mut days: Vec<PathBuf> = std::fs::read_dir(folder)
      .map_err(io)?
      .filter_map(Result::ok)
      .map(|entry| entry.path())
      .filter(|path| {
        path
          .file_name()
          .and_then(|name| name.to_str())
          .is_some_and(is_day_file)
      })
      .collect();
    days.sort();
    for day in days {
      let bytes = read_limited(&day)?;
      let messages = parse_day(&bytes).map_err(|source| ConvertError::Json {
        path: day.clone(),
        maybe_line: None,
        source,
      })?;
      for message in &messages {
        self.convert_message(message)?;
      }
    }
    Ok(())
  }

  fn convert_message(&mut self, message: &Message) -> Result<(), ConvertError> {
    if message.kind.as_deref().is_some_and(|kind| kind != "message") || message.hidden {
      self.writer.skip(Skipped::SystemMessage);
      return Ok(());
    }
    match message.subtype.as_deref() {
      None => {},
      Some("tombstone") => {
        self.writer.skip(Skipped::Deleted);
        return Ok(());
      },
      Some(subtype) if WRITTEN_SUBTYPES.contains(&subtype) => {},
      Some(_) => {
        self.writer.skip(Skipped::SystemMessage);
        return Ok(());
      },
    }
    let Some((ts, sent_at)) = message
      .ts
      .as_deref()
      .and_then(|ts| parse_ts(ts).map(|at| (ts, at)))
    else {
      self.writer.skip(Skipped::BadTime);
      return Ok(());
    };
    let key = format!("{}/{ts}", self.conversation);
    let id = stable_id(SourceKind::Slack, "post", &key);
    // A message can be in the export twice (a reply sent to the channel too): the first counts.
    if self.writer.has_post(id) {
      return Ok(());
    }
    let author = match (&message.user, message.subtype.as_deref()) {
      (Some(user), maybe_subtype) if maybe_subtype != Some("bot_message") => self.people.user(user),
      _ => self.people.bot(message),
    };
    let maybe_root = message
      .thread_ts
      .as_deref()
      .filter(|thread| *thread != ts)
      .map(|thread| {
        stable_id(
          SourceKind::Slack,
          "post",
          &format!("{}/{thread}", self.conversation),
        )
      });
    let files = self.files_of(message);
    let mut post = PostRow {
      id,
      channel_id: self.channel_id,
      user_id: author,
      kind: POST_TEXT,
      message: slack_text(&text_of(message), self.names),
      create_at: sent_at,
      update_at: message
        .edited
        .as_ref()
        .and_then(|edited| edited.ts.as_deref())
        .and_then(parse_ts),
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
    for reaction in message.reactions.iter().flatten() {
      let name = reaction.name.split("::").next().unwrap_or_default().to_owned();
      if name.is_empty() {
        continue;
      }
      for user in &reaction.users {
        let user_id = self.people.user(user);
        self.writer.reaction(&ReactionRow {
          post_id: id,
          user_id,
          name: name.clone(),
        })?;
      }
    }
    // Every further file is a message of its own, a millisecond apart, in the same thread.
    for (offset, (props, path)) in (1_i64..).zip(files) {
      let file_key = format!("{key}/file/{offset}");
      let extra = PostRow {
        id: stable_id(SourceKind::Slack, "post", &file_key),
        message: String::new(),
        create_at: sent_at.saturating_add(offset),
        update_at: None,
        file_props: Some(props),
        source_path: Some(path),
        ..post.clone()
      };
      self.writer.post(&file_key, &extra)?;
    }
    Ok(())
  }

  /// The message's files that the export can have, with where a download of them would be.
  fn files_of(&mut self, message: &Message) -> Vec<(FileRow, PathBuf)> {
    let mut files = Vec::new();
    for file in message.files.iter().flatten() {
      let is_hosted = !file.is_external
        && !matches!(
          file.mode.as_deref(),
          Some("tombstone" | "hidden_by_limit" | "external")
        );
      if !is_hosted {
        self.writer.skip(Skipped::ExternalFile);
        continue;
      }
      let maybe_path = file
        .id
        .as_deref()
        .and_then(safe_file_name)
        .zip(file.name.as_deref().and_then(safe_file_name))
        .and_then(|(id, name)| join_inside(self.source, &format!("{UPLOADS_DIR}/{id}/{name}")))
        .and_then(|path| contained_file(self.source, path));
      let (Some(path), Some(name)) = (maybe_path, file.name.as_deref().and_then(safe_file_name)) else {
        self.writer.skip(Skipped::UnsafePath);
        continue;
      };
      files.push((
        FileRow {
          name: name.to_owned(),
          size: file.size,
          is_image: file
            .mimetype
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/")),
          extension: file.filetype.clone().filter(|kind| !kind.is_empty()),
        },
        path,
      ));
    }
    files
  }
}

/// The message's text, or its attachments' when it has none (apps often post only those).
fn text_of(message: &Message) -> String {
  let text = message.text.clone().unwrap_or_default();
  if !text.trim().is_empty() {
    return text;
  }
  message
    .attachments
    .iter()
    .flatten()
    .filter_map(|attachment| {
      [&attachment.pretext, &attachment.text, &attachment.fallback]
        .into_iter()
        .flatten()
        .find(|text| !text.trim().is_empty())
        .cloned()
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// `YYYY-MM-DD.json`.
fn is_day_file(name: &str) -> bool {
  let bytes = name.as_bytes();
  bytes.len() == 15
    && name.ends_with(".json")
    && bytes[..10].iter().enumerate().all(|(index, byte)| match index {
      4 | 7 => *byte == b'-',
      _ => byte.is_ascii_digit(),
    })
}

fn read_json<T: DeserializeOwned>(path: &Path, is_required: bool) -> Result<Vec<T>, ConvertError> {
  if !is_required && !path.exists() {
    return Ok(Vec::new());
  }
  let bytes = read_limited(path)?;
  serde_json::from_slice(&bytes).map_err(|source| ConvertError::Json {
    path: path.to_owned(),
    maybe_line: None,
    source,
  })
}

fn read_limited(path: &Path) -> Result<Vec<u8>, ConvertError> {
  let io = |source| ConvertError::Io {
    path: path.to_owned(),
    source,
  };
  let size = std::fs::metadata(path).map_err(io)?.len();
  if size > MAX_JSON_FILE_BYTES {
    return Err(ConvertError::TooLarge {
      path: path.to_owned(),
      limit_bytes: MAX_JSON_FILE_BYTES,
    });
  }
  std::fs::read(path).map_err(io)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn timestamps_are_read_exactly() {
    assert_eq!(parse_ts("1503435956.000247"), Some(1_503_435_956_000));
    assert_eq!(parse_ts("1503435956.123456"), Some(1_503_435_956_123));
    assert_eq!(parse_ts("1503435956.5"), Some(1_503_435_956_500));
    assert_eq!(parse_ts("1503435956"), Some(1_503_435_956_000));
    assert_eq!(parse_ts(""), None);
    assert_eq!(parse_ts("-5.1"), None);
    assert_eq!(parse_ts("1e9"), None);
    assert_eq!(parse_ts("1.2.3"), None);
    assert_eq!(parse_ts("99999999999999999999"), None);
    assert_eq!(parse_ts("9223372036854775807.0"), None);
  }

  #[test]
  fn day_files_are_dated_json() {
    assert!(is_day_file("2024-01-31.json"));
    assert!(!is_day_file("2024-1-31.json"));
    assert!(!is_day_file("users.json"));
    assert!(!is_day_file("2024-01-31.json.bak"));
  }

  #[test]
  fn malformed_day_files_are_errors_not_panics() {
    for input in [
      &b""[..],
      b"{",
      b"[{\"ts\": 5}]",
      b"[{\"reactions\": \"x\"}]",
      b"[1, 2]",
      b"null",
    ] {
      assert!(parse_day(input).is_err(), "{input:?}");
    }
    let messages = parse_day(br#"[{"type":"message","ts":"1.0","unknown":{"deep":[1]}}, {}]"#).unwrap();
    assert_eq!(messages.len(), 2);
  }

  #[test]
  fn attachments_stand_in_for_an_empty_text() {
    let message: Message = serde_json::from_str(
      r#"{"text":"","attachments":[{"fallback":"Build 7 failed"},{"pretext":"Deploy","text":"done"}]}"#,
    )
    .unwrap();
    assert_eq!(text_of(&message), "Build 7 failed\nDeploy");
  }
}
