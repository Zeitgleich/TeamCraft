//! The app's places as URL paths, so reload, back/forward, bookmarks and "open in new tab" work.
//!
//! [`Route`] is the pure part: parsing a path and formatting one, with no browser access, so it is
//! the same in the web client and the desktop app (which uses the same paths inside its webview).
//! `history.rs` connects it to the address bar and `AppState`.
//!
//! Only public object IDs go into paths (security.md): never names, message text, search queries
//! or tokens. The message paths match the permalinks (`teamcraft:/c/{c}/m/{m}[/t/{t}]`).

use teamcraft_proto::MeetingId;

use crate::admin::AdminSection;
use crate::model::{ConversationId, MessageId, SpaceId, ThreadId};
use crate::state::{SettingsPage, SettingsTab};

/// More segments than the longest route (`/c/{c}/m/{m}/t/{t}`) are never a route.
const MAX_SEGMENTS: usize = 6;
/// Longer paths are not parsed at all.
const MAX_PATH_BYTES: usize = 256;

/// A place in the app that has its own URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
  /// `/`: wherever the app starts (the first channel, or the getting-started page).
  Start,
  /// `/home`: Home with no conversation open.
  Home,
  /// `/s/{space}`: a space; it lands in its first channel.
  Space(SpaceId),
  /// `/s/{space}/c/{channel}`: a channel of a space.
  Channel {
    space: SpaceId,
    channel: ConversationId,
  },
  /// `/c/{conversation}`: a conversation whose kind isn't known yet (it hasn't loaded).
  Conversation(ConversationId),
  /// `/dm/{conversation}`: a DM or group DM.
  Direct(ConversationId),
  /// `/c/{conversation}/t/{thread}`: a conversation with a thread open beside it.
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  /// `/c/{conversation}/m/{message}[/t/{thread}]`: a message, in its thread if it has one.
  Message {
    conversation: ConversationId,
    message: MessageId,
    maybe_thread: Option<ThreadId>,
  },
  Threads,
  Activity,
  Calls,
  Meetings,
  /// `/meeting/{meeting}`: the link of a meeting (docs/meetings.md). It opens the meeting, or asks
  /// the hosts to let the member in; the address bar then shows `/meetings`. A guest link is the
  /// same path with `#invite=` and never gets here (`app.rs` shows the guest page first).
  Meeting(MeetingId),
  /// `/people`: people and user groups.
  People,
  Saved,
  /// `/files`: every file the user can see.
  Files,
  /// `/search`: the search page. The query never goes into the URL.
  Search,
  /// `/settings`: the settings dialog's list of sections (phones); `/settings/{tab}`: a section.
  Settings(SettingsPage),
  /// `/admin/{section}`: the administration console.
  Admin(AdminSection),
}

impl Route {
  /// The route of a path (`location.pathname`), or `None` for anything that isn't one: unknown
  /// words, IDs that are not canonical numbers, empty segments, overlong paths.
  pub fn parse(path: &str) -> Option<Self> {
    if path.len() > MAX_PATH_BYTES {
      return None;
    }
    let rest = path.strip_prefix('/')?;
    if rest.starts_with('/') {
      return None;
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
      return Some(Self::Start);
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.len() > MAX_SEGMENTS {
      return None;
    }
    match segments.as_slice() {
      ["home"] => Some(Self::Home),
      ["threads"] => Some(Self::Threads),
      ["activity"] => Some(Self::Activity),
      ["calls"] => Some(Self::Calls),
      ["meetings"] => Some(Self::Meetings),
      ["meeting", meeting] => id(meeting).map(Self::Meeting),
      ["people"] => Some(Self::People),
      ["saved"] => Some(Self::Saved),
      ["files"] => Some(Self::Files),
      ["search"] => Some(Self::Search),
      ["settings"] => Some(Self::Settings(SettingsPage::Sections)),
      ["settings", tab] => SettingsTab::from_slug(tab).map(|tab| Self::Settings(SettingsPage::Section(tab))),
      ["admin"] => Some(Self::Admin(AdminSection::ALL[0])),
      ["admin", section] => admin_section(section).map(Self::Admin),
      ["s", space] => id(space).map(Self::Space),
      ["s", space, "c", channel] => Some(Self::Channel {
        space: id(space)?,
        channel: id(channel)?,
      }),
      ["dm", conversation] => id(conversation).map(Self::Direct),
      ["c", conversation] => id(conversation).map(Self::Conversation),
      ["c", conversation, "t", thread] => Some(Self::Thread {
        conversation: id(conversation)?,
        thread: id(thread)?,
      }),
      ["c", conversation, "m", message] => Some(Self::Message {
        conversation: id(conversation)?,
        message: id(message)?,
        maybe_thread: None,
      }),
      ["c", conversation, "m", message, "t", thread] => Some(Self::Message {
        conversation: id(conversation)?,
        message: id(message)?,
        maybe_thread: Some(id(thread)?),
      }),
      _ => None,
    }
  }

  /// The path of this route; [`Route::parse`] reads it back unchanged.
  pub fn path(&self) -> String {
    match self {
      Self::Start => "/".to_owned(),
      Self::Home => "/home".to_owned(),
      Self::Space(space) => format!("/s/{space}"),
      Self::Channel { space, channel } => format!("/s/{space}/c/{channel}"),
      Self::Conversation(conversation) => format!("/c/{conversation}"),
      Self::Direct(conversation) => format!("/dm/{conversation}"),
      Self::Thread { conversation, thread } => format!("/c/{conversation}/t/{thread}"),
      Self::Message {
        conversation,
        message,
        maybe_thread: None,
      } => format!("/c/{conversation}/m/{message}"),
      Self::Message {
        conversation,
        message,
        maybe_thread: Some(thread),
      } => format!("/c/{conversation}/m/{message}/t/{thread}"),
      Self::Threads => "/threads".to_owned(),
      Self::Activity => "/activity".to_owned(),
      Self::Calls => "/calls".to_owned(),
      Self::Meetings => "/meetings".to_owned(),
      Self::Meeting(meeting) => format!("/meeting/{meeting}"),
      Self::People => "/people".to_owned(),
      Self::Saved => "/saved".to_owned(),
      Self::Files => "/files".to_owned(),
      Self::Search => "/search".to_owned(),
      Self::Settings(SettingsPage::Sections) => "/settings".to_owned(),
      Self::Settings(SettingsPage::Section(tab)) => format!("/settings/{}", tab.slug()),
      Self::Admin(section) => format!("/admin/{}", section.slug()),
    }
  }

  /// The route of a permalink (`teamcraft:/c/{c}/m/{m}[/t/{t}]`, also with `//`), so message links
  /// handed to the desktop app open the same place as in the browser.
  pub fn from_permalink(link: &str) -> Option<Self> {
    let rest = link.trim().strip_prefix("teamcraft:")?;
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let path = if rest.starts_with('/') {
      rest.to_owned()
    } else {
      format!("/{rest}")
    };
    Self::parse(&path).filter(|route| matches!(route, Self::Message { .. }))
  }

  /// The conversation this route shows, if any.
  pub fn conversation(&self) -> Option<ConversationId> {
    match self {
      Self::Channel { channel, .. } => Some(*channel),
      Self::Conversation(conversation)
      | Self::Direct(conversation)
      | Self::Thread { conversation, .. }
      | Self::Message { conversation, .. } => Some(*conversation),
      Self::Start
      | Self::Home
      | Self::Space(_)
      | Self::Threads
      | Self::Activity
      | Self::Calls
      | Self::Meetings
      | Self::Meeting(_)
      | Self::People
      | Self::Saved
      | Self::Files
      | Self::Search
      | Self::Settings(_)
      | Self::Admin(_) => None,
    }
  }

  /// Whether `other` is the same page in another spelling or another tab of it: the same
  /// conversation once its kind has loaded (`/c/1` and `/s/2/c/1`), or another settings section.
  /// Moving between those replaces the history entry instead of adding one. The phones' list of
  /// settings sections is a page of its own, so Back returns to it from a section.
  pub fn is_same_page(&self, other: &Self) -> bool {
    match (self, other) {
      (Self::Settings(SettingsPage::Section(_)), Self::Settings(SettingsPage::Section(_))) => true,
      (
        Self::Channel { .. } | Self::Conversation(_) | Self::Direct(_),
        Self::Channel { .. } | Self::Conversation(_) | Self::Direct(_),
      ) => self.conversation() == other.conversation(),
      _ => self == other,
    }
  }
}

/// What the address bar should do to show a new route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryStep {
  /// The URL already shows it.
  Keep,
  /// A new entry: the user went somewhere (Back returns).
  Push,
  /// The same entry: the place was spelled differently, a tab changed, or the app redirected.
  Replace,
}

impl HistoryStep {
  /// How to get from the route in the address bar to `next`. `is_redirect` is set while the app
  /// is still following a URL it was given (an unknown ID falling back to Home, `/` landing in the
  /// first channel), which must not leave the given URL behind as a dead entry.
  pub fn between(shown: &Route, next: &Route, is_redirect: bool) -> Self {
    if shown == next {
      Self::Keep
    } else if is_redirect || shown.is_same_page(next) {
      Self::Replace
    } else {
      Self::Push
    }
  }
}

/// How far Back and Forward can go from the current history entry, for the top bar's buttons
/// (#874). The History API doesn't tell, so the app counts: `depth` is the entry's depth in its own
/// stack (0 for the entry the page was opened on, the same number kept in `history.state`) and
/// `furthest` the deepest entry seen since, which Forward can still reach.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HistoryReach {
  depth: u32,
  furthest: u32,
}

impl HistoryReach {
  /// The page is open on an entry at `depth` (after a reload the ones ahead are unknown).
  pub fn at(depth: u32) -> Self {
    Self {
      depth,
      furthest: depth,
    }
  }

  /// The app pushed an entry: the ones ahead are gone.
  pub fn pushed(self) -> Self {
    Self::at(self.depth + 1)
  }

  /// The user moved to the entry at `depth` (`popstate`).
  pub fn moved_to(self, depth: u32) -> Self {
    Self {
      depth,
      furthest: self.furthest.max(depth),
    }
  }

  pub fn can_go_back(self) -> bool {
    self.depth > 0
  }

  pub fn can_go_forward(self) -> bool {
    self.depth < self.furthest
  }
}

/// A canonical ID: digits only, no sign or leading zero, so every ID has exactly one path.
fn id<T: std::str::FromStr + std::fmt::Display>(text: &str) -> Option<T> {
  if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
    return None;
  }
  let id: T = text.parse().ok()?;
  (id.to_string() == text).then_some(id)
}

fn admin_section(slug: &str) -> Option<AdminSection> {
  AdminSection::ALL
    .into_iter()
    .find(|section| section.slug() == slug)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  const SPACE: u64 = 1_000_001;
  const CHANNEL: u64 = 2_000_002;
  const MESSAGE: u64 = 3_000_003;
  const THREAD: u64 = 4_000_004;
  const MEETING: u64 = 5_000_005;

  mod reach {
    use super::*;

    #[test]
    fn back_and_forward_follow_the_entries_the_app_pushed() {
      let start = HistoryReach::at(0);
      assert!(!start.can_go_back() && !start.can_go_forward());

      let two_deep = start.pushed().pushed();
      assert!(two_deep.can_go_back() && !two_deep.can_go_forward());

      let back_once = two_deep.moved_to(1);
      assert!(back_once.can_go_back() && back_once.can_go_forward());
      let back_twice = back_once.moved_to(0);
      assert!(!back_twice.can_go_back() && back_twice.can_go_forward());

      // Going somewhere new from the middle drops the entries ahead.
      let branched = back_once.pushed();
      assert!(branched.can_go_back() && !branched.can_go_forward());
      // A reload keeps the entry's depth but forgets what is ahead.
      assert!(HistoryReach::at(2).can_go_back() && !HistoryReach::at(2).can_go_forward());
    }
  }

  mod round_trip {
    use super::*;

    #[test]
    fn every_route_reads_back_from_its_path() {
      for route in all_routes() {
        let path = route.path();
        assert_eq!(Route::parse(&path), Some(route), "{path}");
      }
    }

    #[test]
    fn every_settings_tab_and_admin_section_has_a_path() {
      for tab in SettingsTab::ALL {
        let route = Route::Settings(SettingsPage::Section(tab));
        assert_eq!(Route::parse(&route.path()), Some(route));
      }
      let sections = Route::Settings(SettingsPage::Sections);
      assert_eq!(Route::parse(&sections.path()), Some(sections));
      for section in AdminSection::ALL {
        let route = Route::Admin(section);
        assert_eq!(Route::parse(&route.path()), Some(route));
      }
    }

    #[test]
    fn paths_are_the_documented_ones() {
      assert_eq!(
        Route::Channel {
          space: id_from_raw(SPACE),
          channel: id_from_raw(CHANNEL)
        }
        .path(),
        "/s/1000001/c/2000002"
      );
      assert_eq!(
        Route::Message {
          conversation: id_from_raw(CHANNEL),
          message: id_from_raw(MESSAGE),
          maybe_thread: Some(id_from_raw(THREAD)),
        }
        .path(),
        "/c/2000002/m/3000003/t/4000004"
      );
      assert_eq!(Route::Direct(id_from_raw(CHANNEL)).path(), "/dm/2000002");
      assert_eq!(Route::People.path(), "/people");
      assert_eq!(Route::Start.path(), "/");
    }
  }

  mod parsing {
    use super::*;

    #[test]
    fn a_trailing_slash_is_the_same_route() {
      assert_eq!(Route::parse("/home/"), Some(Route::Home));
      assert_eq!(
        Route::parse("/dm/2000002/"),
        Some(Route::Direct(id_from_raw(CHANNEL)))
      );
      assert_eq!(Route::parse("/"), Some(Route::Start));
    }

    #[test]
    fn bare_settings_and_admin_open_their_first_page() {
      assert_eq!(
        Route::parse("/settings"),
        Some(Route::Settings(SettingsPage::Sections))
      );
      assert_eq!(Route::parse("/admin"), Some(Route::Admin(AdminSection::ALL[0])));
    }

    #[test]
    fn malformed_paths_are_no_route() {
      for path in [
        "",
        "home",
        "//",
        "//home",
        "/home//",
        "/Home",
        "/unknown",
        "/s",
        "/s/",
        "/s/abc",
        "/s/1/c",
        "/s/1/x/2",
        "/c/1/m",
        "/c/1/m/2/t",
        "/c/1/m/2/t/3/x",
        "/c/1/x/2",
        "/dm/1/2",
        "/settings/unknown",
        "/settings/display/more",
        "/admin/unknown",
        "/admin/../settings",
        "/c/1/../2",
      ] {
        assert_eq!(Route::parse(path), None, "{path:?}");
      }
    }

    #[test]
    fn ids_must_be_canonical_numbers() {
      for path in [
        "/dm/+5",
        "/dm/-5",
        "/dm/007",
        "/dm/5a",
        "/dm/5%20",
        "/dm/1e3",
        "/dm/ 5",
        "/dm/99999999999999999999999",
        "/dm/%2F",
      ] {
        assert_eq!(Route::parse(path), None, "{path:?}");
      }
    }

    #[test]
    fn overlong_and_deep_paths_are_rejected() {
      let long = format!("/dm/{}", "1".repeat(MAX_PATH_BYTES));
      assert_eq!(Route::parse(&long), None);
      assert_eq!(Route::parse("/a/b/c/d/e/f/g"), None);
    }

    #[test]
    fn permalinks_open_the_message() {
      let expected = Some(Route::Message {
        conversation: id_from_raw(CHANNEL),
        message: id_from_raw(MESSAGE),
        maybe_thread: None,
      });
      assert_eq!(Route::from_permalink("teamcraft:/c/2000002/m/3000003"), expected);
      assert_eq!(
        Route::from_permalink(" teamcraft://c/2000002/m/3000003 "),
        expected
      );
      assert_eq!(
        Route::from_permalink("teamcraft:/c/2000002/m/3000003/t/4000004"),
        Some(Route::Message {
          conversation: id_from_raw(CHANNEL),
          message: id_from_raw(MESSAGE),
          maybe_thread: Some(id_from_raw(THREAD)),
        })
      );
    }

    #[test]
    fn other_links_are_no_permalink() {
      for link in [
        "teamcraft://link?v=1&rv=abc",
        "teamcraft:/settings/display",
        "teamcraft:/c/2000002",
        "https://example.com/c/2000002/m/3000003",
        "",
      ] {
        assert_eq!(Route::from_permalink(link), None, "{link:?}");
      }
    }
  }

  mod history_steps {
    use super::*;

    #[test]
    fn going_somewhere_else_pushes() {
      let channel = channel_route();
      assert_eq!(
        HistoryStep::between(&channel, &Route::Threads, false),
        HistoryStep::Push
      );
      let thread = Route::Thread {
        conversation: id_from_raw(CHANNEL),
        thread: id_from_raw(THREAD),
      };
      assert_eq!(HistoryStep::between(&channel, &thread, false), HistoryStep::Push);
      assert_eq!(
        HistoryStep::between(&channel, &Route::Settings(SettingsPage::Sections), false),
        HistoryStep::Push
      );
    }

    #[test]
    fn the_same_page_spelled_differently_replaces() {
      let loaded = channel_route();
      let unloaded = Route::Conversation(id_from_raw(CHANNEL));
      assert_eq!(
        HistoryStep::between(&unloaded, &loaded, false),
        HistoryStep::Replace
      );
      assert_eq!(
        HistoryStep::between(
          &Route::Settings(SettingsPage::Section(SettingsTab::DEFAULT)),
          &Route::Settings(SettingsPage::Section(SettingsTab::AudioVideo)),
          false
        ),
        HistoryStep::Replace
      );
    }

    #[test]
    fn a_section_from_the_phones_section_list_is_a_new_entry() {
      assert_eq!(
        HistoryStep::between(
          &Route::Settings(SettingsPage::Sections),
          &Route::Settings(SettingsPage::Section(SettingsTab::Devices)),
          false
        ),
        HistoryStep::Push
      );
    }

    #[test]
    fn the_settings_tabs_before_the_rework_still_open() {
      for (old, tab) in [
        ("display", SettingsTab::Appearance),
        ("sidebar", SettingsTab::Preferences),
        ("language", SettingsTab::Preferences),
        ("privacy", SettingsTab::Security),
        ("calls", SettingsTab::AudioVideo),
        ("advanced", SettingsTab::About),
      ] {
        assert_eq!(
          Route::parse(&format!("/settings/{old}")),
          Some(Route::Settings(SettingsPage::Section(tab))),
          "{old}"
        );
      }
    }

    #[test]
    fn redirects_replace_and_unchanged_routes_keep() {
      assert_eq!(
        HistoryStep::between(&Route::Start, &channel_route(), true),
        HistoryStep::Replace
      );
      assert_eq!(
        HistoryStep::between(&Route::Home, &Route::Home, true),
        HistoryStep::Keep
      );
      assert_eq!(
        HistoryStep::between(&Route::Home, &Route::Home, false),
        HistoryStep::Keep
      );
    }

    #[test]
    fn other_conversations_are_other_pages() {
      let other = Route::Direct(id_from_raw(CHANNEL + 1));
      assert!(!channel_route().is_same_page(&other));
      assert!(!Route::Admin(AdminSection::Users).is_same_page(&Route::Admin(AdminSection::Audit)));
    }
  }

  fn channel_route() -> Route {
    Route::Channel {
      space: id_from_raw(SPACE),
      channel: id_from_raw(CHANNEL),
    }
  }

  fn all_routes() -> Vec<Route> {
    let conversation = id_from_raw(CHANNEL);
    let message = id_from_raw(MESSAGE);
    let thread = id_from_raw(THREAD);
    vec![
      Route::Start,
      Route::Home,
      Route::Space(id_from_raw(SPACE)),
      channel_route(),
      Route::Conversation(conversation),
      Route::Direct(conversation),
      Route::Thread { conversation, thread },
      Route::Message {
        conversation,
        message,
        maybe_thread: None,
      },
      Route::Message {
        conversation,
        message,
        maybe_thread: Some(thread),
      },
      Route::Threads,
      Route::Activity,
      Route::Calls,
      Route::Meetings,
      Route::Meeting(id_from_raw(MEETING)),
      Route::People,
      Route::Saved,
      Route::Files,
      Route::Search,
      Route::Settings(SettingsPage::Sections),
      Route::Settings(SettingsPage::Section(SettingsTab::Security)),
      Route::Admin(AdminSection::Audit),
    ]
  }
}
