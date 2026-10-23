//! User preferences beyond the basics in `settings.rs`: notifications, privacy, calls, display and
//! sidebar options. One serializable tree, stored as JSON; every field has a default, so older stored
//! data keeps loading when fields are added (PLAN.md §4 and §5).
//!
//! Notification and privacy values are stored and shown here; enforcing them belongs to the
//! notification engine of `teamcraft-core`, which will read the same structure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Which messages notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
  All,
  /// Mentions, keywords and direct messages.
  Mentions,
  Nothing,
}

impl NotifyLevel {
  pub const ALL: [NotifyLevel; 3] = [Self::All, Self::Mentions, Self::Nothing];

  pub fn slug(self) -> &'static str {
    match self {
      Self::All => "all",
      Self::Mentions => "mentions",
      Self::Nothing => "nothing",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|level| level.slug() == slug)
  }
}

/// When push notifications reach the phone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushWhen {
  Always,
  AwayOrOffline,
}

impl PushWhen {
  pub const ALL: [PushWhen; 2] = [Self::Always, Self::AwayOrOffline];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Always => "always",
      Self::AwayOrOffline => "away-or-offline",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// Which thread replies notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadReplies {
  /// Threads the user started or replied in.
  Participating,
  /// Only threads the user started.
  Started,
  MentionsOnly,
}

impl ThreadReplies {
  pub const ALL: [ThreadReplies; 3] = [Self::Participating, Self::Started, Self::MentionsOnly];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Participating => "participating",
      Self::Started => "started",
      Self::MentionsOnly => "mentions",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// How long a conversation stays muted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuteFor {
  Off,
  OneHour,
  UntilTomorrow,
  Forever,
}

impl MuteFor {
  pub const ALL: [MuteFor; 4] = [Self::Off, Self::OneHour, Self::UntilTomorrow, Self::Forever];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Off => "off",
      Self::OneHour => "hour",
      Self::UntilTomorrow => "tomorrow",
      Self::Forever => "forever",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// Override of the global level for one conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationNotify {
  /// `None` follows the global level.
  #[serde(default)]
  pub maybe_level: Option<NotifyLevel>,
  #[serde(default = "default_mute")]
  pub mute: MuteFor,
}

fn default_mute() -> MuteFor {
  MuteFor::Off
}

impl Default for ConversationNotify {
  fn default() -> Self {
    Self {
      maybe_level: None,
      mute: MuteFor::Off,
    }
  }
}

/// "Do not disturb" on a schedule, e.g. 18:00 to 08:00. Mentions are still collected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DndSchedule {
  pub is_enabled: bool,
  /// `HH:MM`, local time.
  pub from: String,
  pub to: String,
  pub weekdays_only: bool,
}

impl Default for DndSchedule {
  fn default() -> Self {
    Self {
      is_enabled: false,
      from: "18:00".to_owned(),
      to: "08:00".to_owned(),
      weekdays_only: false,
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPrefs {
  pub desktop: NotifyLevel,
  pub push: NotifyLevel,
  pub push_when: PushWhen,
  /// Don't push to the phone while the desktop app is active, for this many minutes after activity.
  pub push_delay_minutes: u32,
  pub first_name_is_mention: bool,
  pub channel_mentions_notify: bool,
  /// Show the message text in desktop notifications (off: only who wrote where).
  pub show_preview: bool,
  /// Ask before @channel in conversations with more members than this (0 = never ask).
  pub confirm_channel_over: u32,
  pub keywords: Vec<String>,
  /// Highlighted in messages without notifying.
  pub highlight_keywords: Vec<String>,
  pub thread_replies: ThreadReplies,
  pub auto_follow_on_channel_mention: bool,
  pub dnd: DndSchedule,
  /// Per conversation, keyed by the conversation ID as text.
  pub conversations: BTreeMap<String, ConversationNotify>,
  /// Per space, keyed by the space ID as text. Its channels follow it unless they have an entry of
  /// their own; "mute space" is an entry with the level `Nothing` or a mute time.
  pub spaces: BTreeMap<String, ConversationNotify>,
}

impl Default for NotificationPrefs {
  fn default() -> Self {
    Self {
      desktop: NotifyLevel::Mentions,
      push: NotifyLevel::Mentions,
      push_when: PushWhen::AwayOrOffline,
      push_delay_minutes: 2,
      first_name_is_mention: true,
      channel_mentions_notify: true,
      show_preview: true,
      confirm_channel_over: 10,
      keywords: Vec::new(),
      highlight_keywords: Vec::new(),
      thread_replies: ThreadReplies::Participating,
      auto_follow_on_channel_mention: true,
      dnd: DndSchedule::default(),
      conversations: BTreeMap::new(),
      spaces: BTreeMap::new(),
    }
  }
}

impl NotificationPrefs {
  /// Adds a keyword to a list unless it is empty or already there (case-insensitive).
  pub fn add_keyword(list: &mut Vec<String>, keyword: &str) {
    let keyword = keyword.trim();
    if !keyword.is_empty() && !list.iter().any(|existing| existing.eq_ignore_ascii_case(keyword)) {
      list.push(keyword.to_owned());
    }
  }

  pub fn conversation(&self, id: &str) -> ConversationNotify {
    self.conversations.get(id).copied().unwrap_or_default()
  }

  /// Stores an override; a default-valued override is removed so the map stays small.
  pub fn set_conversation(&mut self, id: &str, value: ConversationNotify) {
    if value == ConversationNotify::default() {
      self.conversations.remove(id);
    } else {
      self.conversations.insert(id.to_owned(), value);
    }
  }

  pub fn space(&self, id: &str) -> ConversationNotify {
    self.spaces.get(id).copied().unwrap_or_default()
  }

  /// Stores a space's setting; a default-valued one is removed.
  pub fn set_space(&mut self, id: &str, value: ConversationNotify) {
    if value == ConversationNotify::default() {
      self.spaces.remove(id);
    } else {
      self.spaces.insert(id.to_owned(), value);
    }
  }

  /// What applies to a conversation: its own setting if it has one, else its space's. A
  /// conversation with its own setting ignores the space entirely.
  pub fn resolved(&self, id: &str, maybe_space: Option<&str>) -> ConversationNotify {
    let own = self.conversation(id);
    if own != ConversationNotify::default() {
      return own;
    }
    maybe_space.map(|space| self.space(space)).unwrap_or_default()
  }

  /// [`Self::effective_level`] with the space's setting taken into account.
  pub fn effective_level_in(&self, id: &str, maybe_space: Option<&str>) -> NotifyLevel {
    self.resolved(id, maybe_space).maybe_level.unwrap_or(self.desktop)
  }

  /// [`Self::is_muted`] with the space's setting taken into account.
  pub fn is_muted_in(&self, id: &str, maybe_space: Option<&str>) -> bool {
    let entry = self.resolved(id, maybe_space);
    entry.mute != MuteFor::Off || entry.maybe_level == Some(NotifyLevel::Nothing)
  }

  /// Whether the whole space is muted.
  pub fn is_space_muted(&self, id: &str) -> bool {
    let entry = self.space(id);
    entry.mute != MuteFor::Off || entry.maybe_level == Some(NotifyLevel::Nothing)
  }

  /// The level that applies to a conversation: its override, else the desktop level.
  pub fn effective_level(&self, id: &str) -> NotifyLevel {
    self.conversation(id).maybe_level.unwrap_or(self.desktop)
  }

  /// Whether the conversation looks muted in the sidebar.
  pub fn is_muted(&self, id: &str) -> bool {
    let entry = self.conversation(id);
    entry.mute != MuteFor::Off || entry.maybe_level == Some(NotifyLevel::Nothing)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyPrefs {
  pub share_read_receipts: bool,
  pub share_typing: bool,
  /// Minutes of inactivity before status turns to away; `None` never.
  pub away_after_minutes: Option<u32>,
}

impl Default for PrivacyPrefs {
  fn default() -> Self {
    Self {
      share_read_receipts: true,
      share_typing: true,
      away_after_minutes: Some(10),
    }
  }
}

/// Call defaults; the pre-join screen starts from these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CallPrefs {
  /// Device IDs; empty means the first one the system reports.
  pub camera_id: String,
  pub microphone_id: String,
  pub speaker_id: String,
  pub start_muted: bool,
  pub start_camera_off: bool,
  /// Slug of the default background effect (`none`, `blur-light`, `office`, …).
  pub background: String,
  pub mirror_self_view: bool,
  pub noise_suppression: bool,
  pub echo_cancellation: bool,
  pub push_to_talk: bool,
  /// Screen shares include the computer's sound (off until the user turns it on).
  pub share_sound: bool,
}

impl Default for CallPrefs {
  fn default() -> Self {
    Self {
      camera_id: String::new(),
      microphone_id: String::new(),
      speaker_id: String::new(),
      start_muted: false,
      start_camera_off: false,
      background: "none".to_owned(),
      mirror_self_view: true,
      noise_suppression: true,
      echo_cancellation: true,
      push_to_talk: false,
      share_sound: false,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
  Comfortable,
  Compact,
}

impl Density {
  pub const ALL: [Density; 2] = [Self::Comfortable, Self::Compact];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Comfortable => "comfortable",
      Self::Compact => "compact",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayPrefs {
  pub density: Density,
  /// The custom status emoji of the author next to the name in messages.
  pub show_status_in_messages: bool,
  /// Size of the whole interface, Ctrl/Cmd + `+` / `-` / `0`.
  pub zoom: Zoom,
}

impl Default for DisplayPrefs {
  fn default() -> Self {
    Self {
      density: Density::Comfortable,
      show_status_in_messages: true,
      zoom: Zoom::DEFAULT,
    }
  }
}

/// Interface zoom in percent, one of [`Zoom::STEPS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub struct Zoom(u16);

impl From<u16> for Zoom {
  fn from(percent: u16) -> Self {
    Self::from_percent(percent)
  }
}

impl From<Zoom> for u16 {
  fn from(zoom: Zoom) -> Self {
    zoom.0
  }
}

impl Zoom {
  pub const STEPS: [u16; 8] = [67, 80, 90, 100, 110, 125, 150, 175];
  pub const DEFAULT: Zoom = Zoom(100);

  /// The nearest step to a stored or typed value.
  pub fn from_percent(percent: u16) -> Self {
    let nearest = Self::STEPS
      .into_iter()
      .min_by_key(|step| step.abs_diff(percent))
      .unwrap_or(100);
    Self(nearest)
  }

  pub fn percent(self) -> u16 {
    self.0
  }

  pub fn larger(self) -> Self {
    Self(
      Self::STEPS
        .into_iter()
        .find(|step| *step > self.0)
        .unwrap_or(self.0),
    )
  }

  pub fn smaller(self) -> Self {
    Self(
      Self::STEPS
        .into_iter()
        .rev()
        .find(|step| *step < self.0)
        .unwrap_or(self.0),
    )
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarSort {
  /// As the server lists them.
  Default,
  Alphabetical,
}

impl SidebarSort {
  pub const ALL: [SidebarSort; 2] = [Self::Default, Self::Alphabetical];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Default => "default",
      Self::Alphabetical => "alphabetical",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SidebarPrefs {
  pub sort: SidebarSort,
  /// Direct messages shown before "show more"; 0 shows all.
  pub dm_limit: u32,
  /// One sidebar with the channels of every space grouped by space (Slack style), instead of the
  /// space switcher with one space's channels at a time.
  pub is_grouped_by_space: bool,
}

impl Default for SidebarPrefs {
  fn default() -> Self {
    Self {
      sort: SidebarSort::Default,
      dm_limit: 0,
      is_grouped_by_space: false,
    }
  }
}

/// Writing messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComposerPrefs {
  /// `:)` and `:tada:` become emoji when a message is sent.
  pub convert_emoticons: bool,
  /// The formatting toolbar and the preview under the input.
  pub show_formatting: bool,
}

impl Default for ComposerPrefs {
  fn default() -> Self {
    Self {
      convert_emoticons: true,
      show_formatting: false,
    }
  }
}

/// Everything in one place, stored as a single JSON document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
  pub notifications: NotificationPrefs,
  pub privacy: PrivacyPrefs,
  pub calls: CallPrefs,
  pub display: DisplayPrefs,
  pub sidebar: SidebarPrefs,
  pub composer: ComposerPrefs,
}

impl Preferences {
  /// Parses stored JSON; anything unreadable falls back to the defaults.
  pub fn from_json(text: &str) -> Self {
    serde_json::from_str(text).unwrap_or_default()
  }

  pub fn to_json(&self) -> String {
    serde_json::to_string(self).unwrap_or_default()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn zoom_steps_and_clamps() {
    assert_eq!(Zoom::DEFAULT.larger().percent(), 110);
    assert_eq!(Zoom::DEFAULT.smaller().percent(), 90);
    assert_eq!(Zoom::from_percent(175).larger().percent(), 175);
    assert_eq!(Zoom::from_percent(10).percent(), 67);
    assert_eq!(Zoom::from_percent(67).smaller().percent(), 67);
    assert_eq!(Zoom::from_percent(104).percent(), 100);
  }

  #[test]
  fn zoom_is_stored_as_a_number() {
    let prefs = Preferences::from_json(r#"{"display":{"zoom":125}}"#);
    assert_eq!(prefs.display.zoom.percent(), 125);
    // Values that are not a step snap to the nearest one.
    assert_eq!(
      Preferences::from_json(r#"{"display":{"zoom":3}}"#)
        .display
        .zoom
        .percent(),
      67
    );
    assert!(Preferences::default().to_json().contains(r#""zoom":100"#));
  }

  #[test]
  fn defaults_round_trip_through_json() {
    let prefs = Preferences::default();
    assert_eq!(Preferences::from_json(&prefs.to_json()), prefs);
  }

  #[test]
  fn older_or_partial_data_keeps_loading() {
    let prefs =
      Preferences::from_json(r#"{"notifications":{"desktop":"all","keywords":["deploy"]},"future_field":1}"#);
    assert_eq!(prefs.notifications.desktop, NotifyLevel::All);
    assert_eq!(prefs.notifications.keywords, ["deploy"]);
    assert_eq!(
      prefs.notifications.push_delay_minutes, 2,
      "missing fields use their defaults"
    );
    assert_eq!(Preferences::from_json("not json"), Preferences::default());
  }

  #[test]
  fn keywords_are_trimmed_and_deduplicated() {
    let mut list = vec!["Deploy".to_owned()];
    NotificationPrefs::add_keyword(&mut list, "  deploy ");
    NotificationPrefs::add_keyword(&mut list, "");
    NotificationPrefs::add_keyword(&mut list, "release");
    assert_eq!(list, ["Deploy", "release"]);
  }

  #[test]
  fn conversation_overrides_apply_and_clean_up() {
    let mut prefs = NotificationPrefs::default();
    assert_eq!(prefs.effective_level("7"), NotifyLevel::Mentions);
    prefs.set_conversation(
      "7",
      ConversationNotify {
        maybe_level: Some(NotifyLevel::All),
        mute: MuteFor::Off,
      },
    );
    assert_eq!(prefs.effective_level("7"), NotifyLevel::All);
    assert!(!prefs.is_muted("7"));
    prefs.set_conversation(
      "7",
      ConversationNotify {
        maybe_level: Some(NotifyLevel::Nothing),
        mute: MuteFor::Off,
      },
    );
    assert!(prefs.is_muted("7"));
    prefs.set_conversation("7", ConversationNotify::default());
    assert!(prefs.conversations.is_empty());
  }

  #[test]
  fn slugs_round_trip() {
    for level in NotifyLevel::ALL {
      assert_eq!(NotifyLevel::from_slug(level.slug()), Some(level));
    }
    for value in MuteFor::ALL {
      assert_eq!(MuteFor::from_slug(value.slug()), Some(value));
    }
    for value in ThreadReplies::ALL {
      assert_eq!(ThreadReplies::from_slug(value.slug()), Some(value));
    }
    assert_eq!(PushWhen::from_slug("nope"), None);
  }
}
