//! View models the UI renders. The mock backend produces them today; the real client core
//! (`teamcraft-core`) will map decrypted data into the same shapes.

use serde::{Deserialize, Serialize};
pub use teamcraft_proto::{ConversationId, DeviceId, Id, MessageId, Snowflake, SpaceId, ThreadId, UserId};

/// Builds a typed ID from a small integer. Mock data and tests use it; real IDs come from the server.
pub fn id_from_raw<T: Id>(raw: u64) -> T {
  let value = i64::try_from(raw).unwrap_or(i64::MAX);
  match Snowflake::new(value) {
    Ok(snowflake) => T::from_snowflake(snowflake),
    // Invariant: `value` is clamped to 0..=i64::MAX above, which `Snowflake::new` always accepts.
    Err(_) => unreachable!("clamped value is never negative"),
  }
}

/// The numeric value of an ID, for deriving other mock IDs and sorting.
pub fn raw_id(id: impl Id) -> u64 {
  u64::try_from(id.snowflake().get()).unwrap_or(0)
}

/// A thread is identified by its root message (proto: "ThreadId is the ID of its root message").
pub fn thread_id_of(root: MessageId) -> ThreadId {
  ThreadId::from_snowflake(root.snowflake())
}

pub fn root_message_of(thread: ThreadId) -> MessageId {
  MessageId::from_snowflake(thread.snowflake())
}

/// Milliseconds since the Unix epoch.
pub type TimestampMs = i64;

/// Presence state a user chose or the system derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PresenceStatus {
  Online,
  Away,
  DoNotDisturb,
  Offline,
}

impl PresenceStatus {
  pub const ALL: [PresenceStatus; 4] = [Self::Online, Self::Away, Self::DoNotDisturb, Self::Offline];

  /// CSS modifier and stable identifier.
  pub fn slug(self) -> &'static str {
    match self {
      Self::Online => "online",
      Self::Away => "away",
      Self::DoNotDisturb => "dnd",
      Self::Offline => "offline",
    }
  }
}

/// Emoji, text and optional expiry shown next to a user's name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomStatus {
  pub emoji: String,
  pub text: String,
  pub expiry: StatusExpiry,
}

/// When a custom status clears itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusExpiry {
  Never,
  ThirtyMinutes,
  OneHour,
  Today,
  ThisWeek,
}

impl StatusExpiry {
  pub const ALL: [StatusExpiry; 5] = [
    Self::Today,
    Self::ThirtyMinutes,
    Self::OneHour,
    Self::ThisWeek,
    Self::Never,
  ];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Never => "never",
      Self::ThirtyMinutes => "30m",
      Self::OneHour => "1h",
      Self::Today => "today",
      Self::ThisWeek => "week",
    }
  }

  pub fn from_slug(slug: &str) -> Self {
    Self::ALL
      .into_iter()
      .find(|expiry| expiry.slug() == slug)
      .unwrap_or(Self::Today)
  }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
  pub id: UserId,
  pub username: String,
  pub display_name: String,
  pub presence: PresenceStatus,
  pub maybe_custom_status: Option<CustomStatus>,
  /// Hue (0–359) used for the generated avatar colour.
  pub avatar_hue: u16,
  pub is_guest: bool,
  pub is_bot: bool,
}

impl User {
  /// First letters of the display name, used inside generated avatars.
  pub fn initials(&self) -> String {
    initials(&self.display_name)
  }
}

pub fn initials(name: &str) -> String {
  let mut letters: Vec<char> = name
    .split_whitespace()
    .filter_map(|word| word.chars().next())
    .flat_map(char::to_uppercase)
    .collect();
  letters.truncate(2);
  letters.into_iter().collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Space {
  pub id: SpaceId,
  pub name: String,
  pub hue: u16,
  pub unread_count: u32,
  pub mention_count: u32,
}

/// What kind of conversation a sidebar entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConversationKind {
  Public,
  Private,
  Direct,
  Group,
  /// Logs, bots and webhooks, shown in the compact log view.
  Integration,
}

impl ConversationKind {
  pub fn is_direct_like(self) -> bool {
    matches!(self, Self::Direct | Self::Group)
  }
}

/// Sidebar bucket a conversation is listed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SidebarCategory {
  Favorites,
  Channels,
  Integrations,
  DirectMessages,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
  pub id: ConversationId,
  /// `None` for DMs and group DMs, which live outside spaces.
  pub maybe_space: Option<SpaceId>,
  pub kind: ConversationKind,
  pub category: SidebarCategory,
  pub name: String,
  pub topic: String,
  pub member_ids: Vec<UserId>,
  pub unread_count: u32,
  pub mention_count: u32,
  pub is_favorite: bool,
  pub is_muted: bool,
  /// Whether the signed-in user may post (announcement and integration channels may forbid it).
  pub can_post: bool,
  pub bookmarks: Vec<Bookmark>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
  pub emoji: String,
  pub title: String,
  pub kind: BookmarkKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookmarkKind {
  Link,
  File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LogLevel {
  Debug,
  Info,
  Warn,
  Error,
}

impl LogLevel {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Debug => "debug",
      Self::Info => "info",
      Self::Warn => "warn",
      Self::Error => "error",
    }
  }
}

impl LogLevel {
  /// The level named by a log word (`warning`, `ERR`, `fatal`, …); `None` for anything else.
  pub fn from_word(word: &str) -> Option<Self> {
    match word.to_lowercase().as_str() {
      "trace" | "debug" => Some(Self::Debug),
      "info" | "notice" => Some(Self::Info),
      "warn" | "warning" => Some(Self::Warn),
      "error" | "err" | "fatal" | "critical" | "alert" | "emergency" => Some(Self::Error),
      _ => None,
    }
  }
}

/// Shows a message of an integration channel as one log line: the bridge writes `[LEVEL] text`
/// (or a bare `ERROR text`), which becomes a level and the text without the prefix. Lines without a
/// level are `info`.
pub fn as_log_line(mut message: Message) -> Message {
  if message.kind != MessageKind::User {
    return message;
  }
  let body = message.body.trim_start();
  let (level, text) = match split_level(body) {
    Some((level, text)) => (level, text.to_owned()),
    None => (LogLevel::Info, body.to_owned()),
  };
  message.kind = MessageKind::Log(level);
  message.body = text;
  message
}

fn split_level(body: &str) -> Option<(LogLevel, &str)> {
  let first_end = body.find(char::is_whitespace).unwrap_or(body.len());
  let (first, rest) = body.split_at(first_end);
  let word = first.trim_matches(['[', ']', ':']);
  let level = LogLevel::from_word(word)?;
  Some((level, rest.trim_start()))
}

/// Which system event a system message reports; the text is built from i18n.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemEvent {
  Joined,
  Left,
  TopicChanged { topic: String },
  Renamed { name: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MessageKind {
  User,
  System(SystemEvent),
  /// One line of an integration channel, rendered in the compact log view.
  Log(LogLevel),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
  pub emoji: String,
  pub count: u32,
  pub reacted_by_me: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
  pub id: MessageId,
  pub conversation: ConversationId,
  pub author: UserId,
  pub kind: MessageKind,
  pub sent_at: TimestampMs,
  pub body: String,
  pub is_edited: bool,
  pub reactions: Vec<Reaction>,
  pub reply_count: u32,
  /// Set on replies: the thread they belong to.
  pub maybe_thread: Option<ThreadId>,
}

/// A followed thread as listed in the threads inbox.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThreadSummary {
  pub conversation: ConversationId,
  pub root: Message,
  pub reply_count: u32,
  pub unread_replies: u32,
  pub last_reply_at: TimestampMs,
  pub participant_ids: Vec<UserId>,
  pub has_mention: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn bridge_lines_become_log_lines() {
    let line = |body: &str| {
      as_log_line(Message {
        id: id_from_raw(1),
        conversation: id_from_raw(2),
        author: id_from_raw(3),
        kind: MessageKind::User,
        sent_at: 0,
        body: body.to_owned(),
        is_edited: false,
        reactions: Vec::new(),
        reply_count: 0,
        maybe_thread: None,
      })
    };
    assert_eq!(line("[ERROR] disk full").kind, MessageKind::Log(LogLevel::Error));
    assert_eq!(line("[ERROR] disk full").body, "disk full");
    assert_eq!(line("warning: slow query").kind, MessageKind::Log(LogLevel::Warn));
    assert_eq!(line("warning: slow query").body, "slow query");
    assert_eq!(line("started").kind, MessageKind::Log(LogLevel::Info));
    assert_eq!(line("started").body, "started");
    assert_eq!(line("[DEBUG]").body, "");
  }

  #[test]
  fn initials_take_two_letters() {
    assert_eq!(initials("Ada Lovelace"), "AL");
    assert_eq!(initials("grace brewster murray hopper"), "GB");
    assert_eq!(initials("single"), "S");
    assert_eq!(initials(""), "");
  }

  #[test]
  fn expiry_slug_round_trips() {
    for expiry in StatusExpiry::ALL {
      assert_eq!(StatusExpiry::from_slug(expiry.slug()), expiry);
    }
  }
}
