//! View models the UI renders. The client core (`teamcraft-core`) maps decrypted data into these
//! shapes.

use serde::{Deserialize, Serialize};
use teamcraft_proto::Permission;
pub use teamcraft_proto::{ChannelRole, InviteId, JoinPolicy, SpaceRole};
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

/// Bit set on the IDs of messages that are still in the client's outbox (not confirmed by the
/// server yet). Real snowflakes stay below it for decades (41 bits of milliseconds shifted by 22).
pub const PENDING_ID_BIT: i64 = 1 << 62;

/// Whether a message is still waiting in the outbox; the UI shows it as "sending".
pub fn is_pending(id: MessageId) -> bool {
  id.snowflake().get() & PENDING_ID_BIT != 0
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

/// Emoji, text and optional expiry shown next to a user's name (PLAN.md §4: server-visible).
/// Conversions to and from the wire form live in [`crate::custom_status`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CustomStatus {
  pub emoji: String,
  pub text: String,
  /// The user's choice when setting it.
  pub expiry: StatusExpiry,
  /// When it ends, by the server's clock: set on statuses the server reported, and by clients that
  /// know the user's time zone when they resolve `expiry` ("today" ends at local midnight).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<TimestampMs>,
}

/// When a custom status clears itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
  /// The organization from the identity provider, shown as a badge (no permission depends on it).
  #[serde(default)]
  pub maybe_organization: Option<String>,
  /// Whether the person has set up the app (has a device), as far as the backend knows.
  #[serde(default)]
  pub device_setup: DeviceSetup,
  /// The account is deactivated: shown as the author of old messages, never offered in people
  /// lists, pickers or mentions (issue #654).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_deactivated: bool,
  /// The account was deleted (issue #630): only a tombstone naming old messages is left. The UI
  /// shows its own "Deleted user" instead of the name; always deactivated too.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_deleted: bool,
  /// The e-mail address, only where the workspace's directory policy shows it to this viewer (the
  /// server leaves it out otherwise).
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_email: Option<String>,
}

/// Whether a person has a device to receive messages on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceSetup {
  HasDevices,
  /// Hasn't set up the app yet: a DM is made, they get it once their first device appears.
  NoDevices,
  /// The backend doesn't know (older server, or only seen in member lists).
  #[default]
  #[serde(other)]
  Unknown,
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
  /// Hue of the letter tile: from [`Self::maybe_color`] when it has one, else derived.
  pub hue: u16,
  /// The `#rrggbb` color the space's admins picked.
  #[serde(default)]
  pub maybe_color: Option<String>,
  /// Version of the space's picture (`GET /api/v1/spaces/{id}/image`); without one the space
  /// shows its letter tile.
  #[serde(default)]
  pub maybe_image_version: Option<String>,
  pub unread_count: u32,
  pub mention_count: u32,
  #[serde(default)]
  pub maybe_description: Option<String>,
  /// How many people belong to the space (0 when the server doesn't say).
  #[serde(default)]
  pub member_count: u32,
  /// Who may join: an open space can be shared as a link, anyone in the company can join it.
  #[serde(default = "invite_only")]
  pub join_policy: JoinPolicy,
  /// Whether the user may add people to the space (the server checks again).
  #[serde(default)]
  pub can_add_members: bool,
  /// Whether the user may invite guests to the space (the server checks again).
  #[serde(default)]
  pub can_invite_guests: bool,
  /// Whether the user may change the space's name, picture, join policy and archive it (the
  /// server checks again).
  #[serde(default)]
  pub can_manage_space: bool,
  /// Whether the user may create channels in the space (guests may not; the server checks
  /// again).
  #[serde(default)]
  pub can_create_channels: bool,
}

impl Space {
  /// Whether "Invite people" leads anywhere: adding members or guests, or sharing the link of an
  /// open space, which guests don't do (issue #691).
  pub fn can_invite(&self, is_guest: bool) -> bool {
    self.can_add_members || self.can_invite_guests || (!is_guest && self.join_policy == JoinPolicy::Open)
  }
}

/// A member of a channel with the role the member list holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMember {
  pub user: UserId,
  pub role: ChannelRole,
}

/// A source of the integration bridge that posts into a channel, announced in the app
/// (docs/bridge.md, "Sources"). Only sources in force are listed; the token is never known again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationSource {
  pub id: teamcraft_proto::SourceId,
  pub name: String,
  pub kind: teamcraft_proto::SourceKind,
  pub created_by: UserId,
  pub created_at_ms: i64,
}

/// A source to announce: the app generated the token and keeps only its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewIntegrationSource {
  pub id: teamcraft_proto::SourceId,
  pub name: String,
  pub kind: teamcraft_proto::SourceKind,
  pub token_sha256: teamcraft_proto::SourceTokenHash,
  pub options: teamcraft_proto::SourceOptions,
}

/// A member of a space as the space settings list them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceMember {
  pub user: UserId,
  pub display_name: String,
  pub is_guest: bool,
  /// An integration's bot account: listed as "Integration", without a role to change (issue
  /// #764); it is added and removed in the channels' integration settings.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_bot: bool,
  pub role: SpaceRole,
}

/// The member list of a space as far as the user may see it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "members", rename_all = "snake_case")]
pub enum SpaceRoster {
  /// Not loaded yet ([`crate::backend::ChatBackend::refresh_space_roster`]).
  #[default]
  Loading,
  /// The user may not see who is in the space (guests, people outside it).
  Hidden,
  Loaded(Vec<SpaceMember>),
}

/// An invitation by e-mail that nobody has signed in with yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvite {
  pub id: InviteId,
  pub email: String,
  pub role: SpaceRole,
}

/// What a space from an older peer is taken for: nothing to share as a link.
fn invite_only() -> JoinPolicy {
  JoinPolicy::InviteOnly
}

/// Hue of a `#rrggbb` color, or `None` for anything else (and for greys, which have no hue).
pub fn hue_from_hex(color: &str) -> Option<u16> {
  let hex = color.strip_prefix('#')?;
  if hex.len() != 6 || !hex.is_ascii() {
    return None;
  }
  let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok().map(f32::from);
  let (r, g, b) = (channel(0..2)?, channel(2..4)?, channel(4..6)?);
  let (max, min) = (r.max(g).max(b), r.min(g).min(b));
  if (max - min).abs() < f32::EPSILON {
    return None;
  }
  let delta = max - min;
  let hue = if (max - r).abs() < f32::EPSILON {
    ((g - b) / delta).rem_euclid(6.0)
  } else if (max - g).abs() < f32::EPSILON {
    (b - r) / delta + 2.0
  } else {
    (r - g) / delta + 4.0
  };
  Some((hue * 60.0).round() as u16 % 360)
}

/// An open space the user is not in yet and may join.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinableSpace {
  pub id: SpaceId,
  pub name: String,
  #[serde(default)]
  pub maybe_description: Option<String>,
  /// Hue of the letter tile, as in [`Space::hue`].
  #[serde(default)]
  pub hue: u16,
  #[serde(default)]
  pub maybe_image_version: Option<String>,
  #[serde(default)]
  pub member_count: u32,
}

impl JoinableSpace {
  /// The space as its tile shows it (picture or letters); not one the user is in.
  pub fn tile(&self) -> Space {
    Space {
      id: self.id,
      name: self.name.clone(),
      hue: self.hue,
      maybe_color: None,
      maybe_image_version: self.maybe_image_version.clone(),
      unread_count: 0,
      mention_count: 0,
      maybe_description: self.maybe_description.clone(),
      member_count: self.member_count,
      join_policy: JoinPolicy::Open,
      can_add_members: false,
      can_invite_guests: false,
      can_manage_space: false,
      can_create_channels: false,
    }
  }
}

/// Something about who is in a conversation's encryption group that the user should see or act
/// on (e2ee.md §4.6, removal order).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MembershipIssue {
  /// This device's removal of `user` (the user's own leave when it is them) is on its way.
  RemovalPending { user: UserId },
  /// That removal or leave was refused too often; "retry" ([`crate::backend::ChatBackend::retry_removal`]).
  RemovalFailed { user: UserId },
  /// An admin's removal from outside the group that can't be confirmed: the server lists `user`
  /// again, or somebody may have added them back since. "Remove" signs it afresh
  /// ([`crate::backend::ChatBackend::retry_removal`]), "keep" drops it
  /// ([`crate::backend::ChatBackend::discard_removal`]).
  RemovalUnconfirmed { user: UserId },
  /// The server lists the user but refuses this device the encryption group: new messages may be
  /// missing.
  GroupRefused,
  /// `user` hasn't set up the app yet; they are added to the encryption group (and get the
  /// messages) once their first device appears.
  WaitingForFirstDevice { user: UserId },
  /// `user` still had no device after the waiting time (`pending_addition_expiry`): they never set
  /// up the app. Their first device, should it appear, still gets them in.
  NeverSetUp { user: UserId },
  /// `user`'s devices have no key package left on the server; they are added once one of their
  /// devices comes online and uploads new ones.
  NoKeyPackage { user: UserId },
  /// None of `user`'s devices passed the checks (signed device list, credential), so they can't
  /// be added; a new device of theirs is tried again.
  DevicesUnverified { user: UserId },
  /// A DM this device opened waits for the welcome into the group the other side founded:
  /// "setting up encryption", with a retry ([`crate::backend::ChatBackend::retry_dm_setup`]).
  /// `remaining_ms` after it was read it becomes [`Self::EncryptionSetupFailed`]; the view reads
  /// the issues again then.
  EncryptionSetup {
    #[serde(default)]
    remaining_ms: u64,
  },
  /// That welcome didn't come in time; "retry" asks the members again.
  EncryptionSetupFailed,
  #[serde(other)]
  Unknown,
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
  /// The client is still catching up this conversation's history after being away (issue #247):
  /// what it shows may be incomplete.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_catching_up: bool,
  /// The version of the channel's picture (`GET /api/v1/conversations/{id}/image`), if it has one.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_image_version: Option<String>,
  /// The user's notes to themself (e2ee.md, "Notes to self"), from the DM's participants
  /// ([`teamcraft_proto::ConversationResponse::is_notes_to_self`]), not its current members.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_notes_to_self: bool,
  /// The channel permissions the server says the user holds here
  /// ([`teamcraft_proto::ConversationResponse::maybe_my_permissions`]); `None` when it didn't
  /// say (an older server), and the client evaluates the built-in scheme instead
  /// ([`crate::role_rules::RoleActor::maybe_granted`]). The UI offers an action only when it is
  /// in here (issue #691); the server checks every request again.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_permissions: Option<Vec<teamcraft_proto::Permission>>,
  /// A DM or group DM whose other people are all deactivated (issue #698): listed apart, after
  /// the active conversations; its history stays readable.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub has_only_deactivated_others: bool,
}

impl Conversation {
  /// Whether it is a channel of a space, the only kind with settings (topic, purpose, name): DMs,
  /// group DMs and meeting chats live outside spaces and the server refuses settings for them.
  pub fn has_channel_settings(&self) -> bool {
    !self.kind.is_direct_like() && self.maybe_space.is_some()
  }

  /// Whether anything in it waits to be read: the read marker has something to move past.
  pub fn has_unread(&self) -> bool {
    self.unread_count > 0 || self.mention_count > 0
  }

  /// Whether the user may react here (`add_reaction`, which an archived or a moderated channel
  /// takes away; issues #691, #758): the server's word, otherwise wherever the user may post.
  pub fn can_react(&self) -> bool {
    self
      .maybe_holds(&Permission::AddReaction)
      .unwrap_or(self.can_post)
  }

  /// Whether the user may pin messages and keep links and files as bookmarks (`manage_bookmarks`;
  /// a pin is sent as a message, so posting too): the server's word, otherwise everybody but
  /// guests, as in the built-in scheme.
  pub fn can_manage_bookmarks(&self, is_guest: bool) -> bool {
    self.can_post
      && self
        .maybe_holds(&Permission::ManageBookmarks)
        .unwrap_or(!is_guest)
  }

  /// Whether the user may change the channel itself, such as its name or archiving it
  /// (`manage_channel_properties`); never in DMs and group DMs. Without the server's word it is
  /// offered and the server decides.
  pub fn can_manage_channel(&self) -> bool {
    !self.kind.is_direct_like()
      && self
        .maybe_holds(&Permission::ManageChannelProperties)
        .unwrap_or(true)
  }

  /// Whether the server says the user holds `permission` here; `None` when it didn't say.
  fn maybe_holds(&self, permission: &Permission) -> Option<bool> {
    self
      .maybe_permissions
      .as_ref()
      .map(|granted| granted.contains(permission))
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
  pub emoji: String,
  pub title: String,
  pub kind: BookmarkKind,
  /// The link of a [`BookmarkKind::Link`]; empty for files.
  #[serde(default)]
  pub url: String,
  /// The message whose file a [`BookmarkKind::File`] opens.
  #[serde(default)]
  pub maybe_message: Option<MessageId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BookmarkKind {
  Link,
  File,
}

/// Ordered from least to most severe, so `level >= minimum` filters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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
///
/// A batch (several lines, usually in a code fence, see `docs/bridge.md`) keeps its lines as they
/// are with the fence removed; its level is the worst of its lines and [`crate::log::log_entries`]
/// splits it again for display.
pub fn as_log_line(mut message: Message) -> Message {
  if message.kind != MessageKind::User {
    return message;
  }
  if let Some(lines) = crate::log::batch_lines(&message.body).filter(|lines| lines.len() > 1) {
    message.kind = MessageKind::Log(crate::log::worst_level(&lines));
    message.body = lines.join("\n");
    return message;
  }
  let single_line =
    crate::log::batch_lines(&message.body).and_then(|mut lines| (lines.len() == 1).then(|| lines.remove(0)));
  let body = single_line.as_deref().unwrap_or(&message.body).trim_start();
  let (level, text) = match split_level(body) {
    Some((level, text)) => (level, text.to_owned()),
    None => (LogLevel::Info, body.to_owned()),
  };
  message.kind = MessageKind::Log(level);
  message.body = text;
  message
}

/// Turns a log line back into the chat message the bridge sent (the "chat" view of an integration
/// channel): a batch is a code block again, a single line gets its `[LEVEL]` prefix back.
pub fn as_chat_message(message: &mut Message) {
  let MessageKind::Log(level) = message.kind else {
    return;
  };
  message.body = if message.body.contains('\n') {
    format!("```\n{}\n```", message.body)
  } else {
    format!("[{}] {}", level.slug().to_uppercase(), message.body)
  };
  message.kind = MessageKind::User;
}

pub(crate) fn split_level(body: &str) -> Option<(LogLevel, &str)> {
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
  /// A change to the conversation that is no join or leave (archived, made private or public,
  /// moved, a workspace admin who joined): a line reading the workspace event of the message's
  /// extras, whatever the join/leave setting says. Known from the message alone, so the row is a
  /// line before its extras arrive.
  Change,
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
  fn hex_colors_have_a_hue_and_everything_else_none() {
    assert_eq!(hue_from_hex("#ff0000"), Some(0));
    assert_eq!(hue_from_hex("#00ff00"), Some(120));
    assert_eq!(hue_from_hex("#0000ff"), Some(240));
    assert_eq!(hue_from_hex("#808080"), None);
    assert_eq!(hue_from_hex("red"), None);
    assert_eq!(hue_from_hex("#ff00"), None);
    assert_eq!(hue_from_hex("#ffé0"), None);
  }

  #[test]
  fn spaces_from_older_snapshots_have_no_color_or_image() {
    let space: Space = serde_json::from_value(serde_json::json!({
      "id": "7", "name": "Project A", "hue": 10, "unread_count": 0, "mention_count": 0,
    }))
    .unwrap();
    assert_eq!(space.maybe_color, None);
    assert_eq!(space.maybe_image_version, None);
  }

  /// Issue #758: what a conversation offers the native apps, from the server's answer and without
  /// it (an older server).
  mod offered_actions {
    use super::*;

    #[test]
    fn the_servers_answer_decides() {
      let announcements = conversation(
        ConversationKind::Public,
        false,
        Some(vec![Permission::AddReaction]),
      );
      assert!(announcements.can_react());
      assert!(!announcements.can_manage_bookmarks(false));
      assert!(!announcements.can_manage_channel());
      let managed = conversation(
        ConversationKind::Private,
        true,
        Some(vec![
          Permission::CreatePost,
          Permission::ManageBookmarks,
          Permission::ManageChannelProperties,
        ]),
      );
      assert!(!managed.can_react(), "moderated: posting without reactions");
      assert!(
        managed.can_manage_bookmarks(true),
        "the answer counts, not the guess"
      );
      assert!(managed.can_manage_channel());
      let nothing = conversation(ConversationKind::Public, true, Some(Vec::new()));
      assert!(!nothing.can_react());
      assert!(!nothing.can_manage_bookmarks(false));
      assert!(!nothing.can_manage_channel());
    }

    #[test]
    fn without_an_answer_the_built_in_scheme_and_posting_decide() {
      let open = conversation(ConversationKind::Public, true, None);
      assert!(open.can_react());
      assert!(open.can_manage_bookmarks(false));
      assert!(!open.can_manage_bookmarks(true), "guests keep no bookmarks");
      assert!(open.can_manage_channel(), "offered; the server decides");
      let archived = conversation(ConversationKind::Public, false, None);
      assert!(!archived.can_react());
      assert!(!archived.can_manage_bookmarks(false));
    }

    #[test]
    fn direct_messages_have_no_channel_to_manage() {
      for kind in [ConversationKind::Direct, ConversationKind::Group] {
        let everything = conversation(kind, true, Some(vec![Permission::ManageChannelProperties]));
        assert!(!everything.can_manage_channel(), "{kind:?}");
        assert!(!conversation(kind, true, None).can_manage_channel(), "{kind:?}");
      }
    }

    fn conversation(
      kind: ConversationKind,
      can_post: bool,
      maybe_permissions: Option<Vec<Permission>>,
    ) -> Conversation {
      Conversation {
        id: id_from_raw(1),
        maybe_space: None,
        kind,
        category: SidebarCategory::Channels,
        name: "news".into(),
        topic: String::new(),
        member_ids: Vec::new(),
        unread_count: 0,
        mention_count: 0,
        is_favorite: false,
        is_muted: false,
        can_post,
        bookmarks: Vec::new(),
        is_catching_up: false,
        maybe_image_version: None,
        is_notes_to_self: false,
        maybe_permissions,
        has_only_deactivated_others: false,
      }
    }
  }

  #[test]
  fn membership_issues_keep_their_wire_form_and_tolerate_new_kinds() {
    let issue = MembershipIssue::RemovalFailed { user: id_from_raw(7) };
    let json = serde_json::to_value(&issue).unwrap_or_default();
    assert_eq!(json["type"], "removal_failed");
    // Dropped in #661: an older core's leftover issue is ignored.
    assert_eq!(
      serde_json::from_value::<MembershipIssue>(serde_json::json!({ "type": "left_in_group", "user": "7" }))
        .ok(),
      Some(MembershipIssue::Unknown)
    );
    assert_eq!(
      serde_json::from_value::<MembershipIssue>(serde_json::json!({ "type": "group_refused" })).ok(),
      Some(MembershipIssue::GroupRefused)
    );
    assert_eq!(
      serde_json::from_value::<MembershipIssue>(serde_json::json!({ "type": "from_a_newer_core" })).ok(),
      Some(MembershipIssue::Unknown)
    );
  }

  #[test]
  fn pending_ids_are_recognized() {
    assert!(!is_pending(id_from_raw(42)));
    assert!(is_pending(id_from_raw(
      u64::try_from(PENDING_ID_BIT | 7).unwrap_or(0)
    )));
  }

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
  fn chat_view_restores_the_message_text() {
    let mut message = Message {
      id: id_from_raw(1),
      conversation: id_from_raw(2),
      author: id_from_raw(3),
      kind: MessageKind::Log(LogLevel::Warn),
      sent_at: 0,
      body: "slow query".into(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
    };
    as_chat_message(&mut message);
    assert_eq!(
      (message.kind, message.body.as_str()),
      (MessageKind::User, "[WARN] slow query")
    );
    message.kind = MessageKind::Log(LogLevel::Error);
    message.body = "[INFO] a\n[ERROR] b".into();
    as_chat_message(&mut message);
    assert_eq!(message.body, "```\n[INFO] a\n[ERROR] b\n```");
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
