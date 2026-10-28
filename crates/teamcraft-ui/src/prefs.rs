//! User preferences beyond the basics in `settings.rs`: notifications, privacy, calls, display and
//! sidebar options. One serializable tree, stored as JSON; every field has a default, so older stored
//! data keeps loading when fields are added (PLAN.md §4 and §5).
//!
//! Notification and privacy values are stored and shown here; enforcing them belongs to the
//! notification engine of `teamcraft-core`, which will read the same structure.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_client_api::message_sound::{MessageSound, OwnSoundLimits};
use teamcraft_proto::{DndWindow, Weekday};

use crate::attention::DEFAULT_ATTENTION_INPUT_SECS;
use crate::model::UserId;

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

/// Monday first, as the schedule editor lists them.
pub const EVERY_DAY: [Weekday; 7] = [
  Weekday::Monday,
  Weekday::Tuesday,
  Weekday::Wednesday,
  Weekday::Thursday,
  Weekday::Friday,
  Weekday::Saturday,
  Weekday::Sunday,
];

/// "Do not disturb" on a weekly schedule, plus the people and messages that still get through (also
/// during a manual DND). Mentions are still collected. Stored on the server (`notify_sync`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DndSchedule {
  pub is_enabled: bool,
  /// Weekly ranges in local time; kept while the schedule is off so turning it on again restores them.
  pub windows: Vec<DndWindow>,
  /// People whose messages notify anyway.
  pub allow_user_ids: Vec<UserId>,
  /// Messages marked urgent notify anyway.
  pub allows_urgent: bool,
}

impl Default for DndSchedule {
  fn default() -> Self {
    Self {
      is_enabled: false,
      windows: vec![default_dnd_window()],
      allow_user_ids: Vec::new(),
      allows_urgent: false,
    }
  }
}

/// The range a new schedule starts with: evenings and nights, every day.
pub fn default_dnd_window() -> DndWindow {
  DndWindow {
    weekdays: EVERY_DAY.to_vec(),
    start_minute: 18 * 60,
    end_minute: 8 * 60,
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
  /// This session counts as attentive (holding phone pushes back) only with input in the app within
  /// this many seconds (notifications.md, "Attention").
  pub attention_input_secs: u32,
  /// Report whether the user looks at this session at all (the user's choice, synced through the
  /// server's notification settings; off also where the instance turned it off).
  pub uses_desktop_attention: bool,
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
  /// When the quick do-not-disturb chosen on this device ends (the server ends it then too).
  pub maybe_dnd_until: Option<crate::model::TimestampMs>,
  /// Per conversation, keyed by the conversation ID as text.
  pub conversations: BTreeMap<String, ConversationNotify>,
  /// Per space, keyed by the space ID as text. Its channels follow it unless they have an entry of
  /// their own; "mute space" is an entry with the level `Nothing` or a mute time.
  pub spaces: BTreeMap<String, ConversationNotify>,
  /// A ringtone plays on the default output while a call rings (notifications.md, "Calls").
  pub plays_ringtone: bool,
  /// Desktop: a ringing call shows a small window above all other apps while TeamCraft is in the
  /// background, instead of the notification (#650).
  pub shows_call_window: bool,
  /// Remind this many minutes before a meeting starts; `None`: no reminder before.
  pub maybe_meeting_reminder_minutes: Option<u16>,
  /// Remind when a meeting starts.
  pub notifies_meeting_start: bool,
  /// What plays on this device when a message notifies (notifications.md, "Message sounds"): the
  /// system's sound (the default), none, one of the app's own sounds or a file picked here. One
  /// for every kind of message for now; a field per kind can follow, each of this type.
  pub message_sound: MessageSound,
  /// How large an own sound file may be and how long it plays.
  pub own_sound_limits: OwnSoundLimits,
}

/// Default of "remind before a meeting" (notifications.md, "Meeting reminders").
pub const DEFAULT_MEETING_REMINDER_MINUTES: u16 = 5;

impl Default for NotificationPrefs {
  fn default() -> Self {
    Self {
      desktop: NotifyLevel::Mentions,
      push: NotifyLevel::Mentions,
      push_when: PushWhen::AwayOrOffline,
      push_delay_minutes: 2,
      attention_input_secs: DEFAULT_ATTENTION_INPUT_SECS,
      uses_desktop_attention: true,
      first_name_is_mention: true,
      channel_mentions_notify: true,
      show_preview: true,
      confirm_channel_over: 10,
      keywords: Vec::new(),
      highlight_keywords: Vec::new(),
      thread_replies: ThreadReplies::Participating,
      auto_follow_on_channel_mention: true,
      dnd: DndSchedule::default(),
      maybe_dnd_until: None,
      conversations: BTreeMap::new(),
      spaces: BTreeMap::new(),
      plays_ringtone: true,
      shows_call_window: true,
      maybe_meeting_reminder_minutes: Some(DEFAULT_MEETING_REMINDER_MINUTES),
      notifies_meeting_start: true,
      message_sound: MessageSound::System,
      own_sound_limits: OwnSoundLimits::default(),
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

/// "Away after" of a new install: a real buffer before the user shows as away.
pub const DEFAULT_AWAY_AFTER_MINUTES: u32 = 15;

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
      away_after_minutes: Some(DEFAULT_AWAY_AFTER_MINUTES),
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
  /// What a screen share keeps under pressure (detail: resolution, motion: frame rate).
  pub screen_mode: teamcraft_client_api::calls::ScreenContentMode,
  /// Tones while the connection is lost and when it is back.
  pub status_sounds: teamcraft_client_api::calls::CallStatusSounds,
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
      screen_mode: teamcraft_client_api::calls::ScreenContentMode::Detail,
      status_sounds: teamcraft_client_api::calls::CallStatusSounds::All,
    }
  }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
  /// Chat-app style: own messages right in accent bubbles, others left in neutral bubbles.
  Bubbles,
  /// A roomy list with larger text. Stored as `comfortable` since before the layouts were
  /// renamed, so saved preferences keep their name.
  Comfortable,
  /// The dense list (the look `Comfortable` had before the roomier layout, issue #576).
  Compact,
}

impl Density {
  pub const ALL: [Density; 3] = [Self::Bubbles, Self::Comfortable, Self::Compact];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Bubbles => "bubbles",
      Self::Comfortable => "comfortable",
      Self::Compact => "compact",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|value| value.slug() == slug)
  }
}

/// How large pictures show in the conversation (issue #316).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageSize {
  Standard,
  Large,
}

impl ImageSize {
  pub const ALL: [ImageSize; 2] = [Self::Standard, Self::Large];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Standard => "standard",
      Self::Large => "large",
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
  /// Size of pictures in the conversation.
  pub image_size: ImageSize,
  /// The custom status emoji of the author next to the name in messages.
  pub show_status_in_messages: bool,
  /// Size of the whole interface, Ctrl/Cmd + `+` / `-` / `0`.
  pub zoom: Zoom,
  /// Size of the text only (issue #370); spacing next to text grows with it, the layout does not.
  pub text_size: TextSize,
  /// Seconds at the bottom of the focused conversation before the "New messages" divider goes;
  /// `None` keeps it until the user sends, presses Esc or leaves (#321).
  pub maybe_divider_dwell_secs: Option<u16>,
}

impl Default for DisplayPrefs {
  fn default() -> Self {
    Self {
      density: Density::Comfortable,
      image_size: ImageSize::Large,
      show_status_in_messages: true,
      zoom: Zoom::DEFAULT,
      text_size: TextSize::DEFAULT,
      maybe_divider_dwell_secs: Some(DEFAULT_DIVIDER_DWELL_SECS),
    }
  }
}

/// Default of [`DisplayPrefs::maybe_divider_dwell_secs`].
pub const DEFAULT_DIVIDER_DWELL_SECS: u16 = 10;

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

/// Text size in percent of the browser's (or system's) default, one of [`TextSize::STEPS`]. The
/// stylesheet's type scale is in rem, so it follows `--text-scale` on the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "u16", into = "u16")]
pub struct TextSize(u16);

impl From<u16> for TextSize {
  fn from(percent: u16) -> Self {
    Self::from_percent(percent)
  }
}

impl From<TextSize> for u16 {
  fn from(size: TextSize) -> Self {
    size.0
  }
}

impl TextSize {
  pub const STEPS: [u16; 5] = [90, 100, 110, 125, 150];
  pub const DEFAULT: TextSize = TextSize(100);

  /// The nearest step to a stored or chosen value.
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

  /// The value of the `--text-scale` CSS variable: `1.25` for 125 %.
  pub fn css_scale(self) -> String {
    format!("{}", f64::from(self.0) / 100.0)
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

/// How Home orders its conversations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DmSort {
  /// The newest message first.
  #[default]
  Recent,
  Alphabetical,
}

impl DmSort {
  pub const ALL: [DmSort; 2] = [Self::Recent, Self::Alphabetical];

  pub fn slug(self) -> &'static str {
    match self {
      Self::Recent => "recent",
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
  /// Order of the channels in a space.
  pub sort: SidebarSort,
  /// Order of Home.
  pub dm_sort: DmSort,
  /// Direct messages shown before "show more"; 0 shows all.
  pub dm_limit: u32,
  /// One sidebar with the channels of every space grouped by space (Slack style), instead of the
  /// space switcher with one space's channels at a time.
  pub is_grouped_by_space: bool,
  /// Switching to Home or to a space reopens the conversation that was open there. Off: Home
  /// shows its overview and a space opens its first channel (#498).
  pub reopen_last_conversation: bool,
  /// Conversations with unread messages in their own section at the top. Off by default: they stay
  /// in their normal place and only the bold name and the count mark them, so the list doesn't
  /// move under the pointer while the user works through them (#890).
  pub shows_unreads_section: bool,
}

impl Default for SidebarPrefs {
  fn default() -> Self {
    Self {
      sort: SidebarSort::Default,
      dm_sort: DmSort::Recent,
      dm_limit: 0,
      is_grouped_by_space: false,
      reopen_last_conversation: true,
      shows_unreads_section: false,
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

/// A step of the "Set up your workspace" checklist on Chats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupStep {
  ProfilePicture,
  InstallApp,
  BrowseChannels,
  InvitePeople,
  /// A step a newer version knows; ignored here.
  #[serde(other)]
  Unknown,
}

/// Progress through the setup checklist, remembered with the other preferences of this account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnboardingPrefs {
  pub done: Vec<SetupStep>,
  pub is_dismissed: bool,
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
  pub onboarding: OnboardingPrefs,
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
  fn text_size_snaps_to_a_step_and_has_a_css_scale() {
    assert_eq!(TextSize::from_percent(130).percent(), 125);
    assert_eq!(TextSize::from_percent(10).percent(), 90);
    assert_eq!(TextSize::from_percent(400).percent(), 150);
    assert_eq!(TextSize::DEFAULT.css_scale(), "1");
    assert_eq!(TextSize::from_percent(125).css_scale(), "1.25");
    assert_eq!(TextSize::from_percent(90).css_scale(), "0.9");
  }

  #[test]
  fn text_size_is_stored_as_a_number_and_defaults_for_old_prefs() {
    let prefs = Preferences::from_json(r#"{"display":{"text_size":110}}"#);
    assert_eq!(prefs.display.text_size.percent(), 110);
    let old = Preferences::from_json(r#"{"display":{"zoom":125}}"#);
    assert_eq!(old.display.text_size, TextSize::DEFAULT);
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
  fn call_and_meeting_alerts_are_on_by_default_and_can_be_turned_off() {
    let older = Preferences::from_json(r#"{"notifications":{"desktop":"all"}}"#).notifications;
    assert!(older.plays_ringtone);
    assert!(older.shows_call_window);
    assert_eq!(
      older.maybe_meeting_reminder_minutes,
      Some(DEFAULT_MEETING_REMINDER_MINUTES)
    );
    assert!(older.notifies_meeting_start);
    let off = Preferences::from_json(
      r#"{"notifications":{"plays_ringtone":false,"shows_call_window":false,"maybe_meeting_reminder_minutes":null,"notifies_meeting_start":false}}"#,
    )
    .notifications;
    assert!(!off.plays_ringtone);
    assert!(!off.shows_call_window);
    assert_eq!(off.maybe_meeting_reminder_minutes, None);
    assert!(!off.notifies_meeting_start);
  }

  #[test]
  fn the_message_sound_stays_the_system_sound_until_chosen() {
    let older = Preferences::from_json(r#"{"notifications":{"desktop":"all"}}"#).notifications;
    assert_eq!(older.message_sound, MessageSound::System);
    assert_eq!(older.own_sound_limits, OwnSoundLimits::default());
    let chosen = Preferences::from_json(
      r#"{"notifications":{"message_sound":"uh_oh","own_sound_limits":{"max_play_seconds":2}}}"#,
    )
    .notifications;
    assert_eq!(
      chosen.message_sound,
      MessageSound::BuiltIn(teamcraft_client_api::message_sound::BuiltInSound::UhOh)
    );
    assert_eq!(chosen.own_sound_limits.max_play_seconds, 2);
    // A sound of a newer version keeps the other preferences (and plays the default sound).
    let newer = Preferences::from_json(r#"{"notifications":{"desktop":"all","message_sound":"trumpet"}}"#);
    assert_eq!(newer.notifications.desktop, NotifyLevel::All);
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
