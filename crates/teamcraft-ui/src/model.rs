//! View models the UI renders. The mock backend produces them today; the real client core
//! (`teamcraft-core`) will map decrypted data into the same shapes.

use std::fmt;

/// Declares a `Copy` newtype around a `u64` snowflake. Replace with the shared macro from
/// `teamcraft-proto` once the UI depends on it.
macro_rules! id_newtype {
  ($(#[$meta:meta])* $name:ident) => {
    $(#[$meta])*
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
    pub struct $name(pub u64);

    impl fmt::Display for $name {
      fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
      }
    }
  };
}

id_newtype!(
  /// Identifies a space (a project inside the workspace).
  SpaceId
);
id_newtype!(
  /// Identifies a channel, DM or group DM.
  ConversationId
);
id_newtype!(
  /// Identifies a message inside its conversation.
  MessageId
);
id_newtype!(
  /// Identifies a user.
  UserId
);

/// Milliseconds since the Unix epoch.
pub type TimestampMs = i64;

/// Presence state a user chose or the system derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomStatus {
  pub emoji: String,
  pub text: String,
  pub expiry: StatusExpiry,
}

/// When a custom status clears itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq)]
pub struct Space {
  pub id: SpaceId,
  pub name: String,
  pub hue: u16,
  pub unread_count: u32,
  pub mention_count: u32,
}

/// What kind of conversation a sidebar entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SidebarCategory {
  Favorites,
  Channels,
  Integrations,
  DirectMessages,
}

#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
  pub emoji: String,
  pub title: String,
  pub kind: BookmarkKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookmarkKind {
  Link,
  File,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// Which system event a system message reports; the text is built from i18n.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemEvent {
  Joined,
  Left,
  TopicChanged { topic: String },
  Renamed { name: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum MessageKind {
  User,
  System(SystemEvent),
  /// One line of an integration channel, rendered in the compact log view.
  Log(LogLevel),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reaction {
  pub emoji: String,
  pub count: u32,
  pub reacted_by_me: bool,
}

#[derive(Clone, Debug, PartialEq)]
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
  /// Set on replies: the root message of the thread.
  pub maybe_thread_root: Option<MessageId>,
}

/// A followed thread as listed in the threads inbox.
#[derive(Clone, Debug, PartialEq)]
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
