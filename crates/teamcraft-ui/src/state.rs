//! Shared application state, provided through Leptos context.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;

use crate::admin::AdminSection;
use crate::backend::Backend;
use crate::calls::CallState;
use crate::lightbox::MediaKey;
use crate::model::{
  Conversation, ConversationId, MessageId, SpaceId, ThreadId, TimestampMs, User, UserId, id_from_raw,
};
use crate::navigation::{Step, divider_index, next_unread};
use crate::prefs::SidebarSort;
use crate::reactive::{SetIfAlive, get_if_observed};
use crate::route::Route;
use crate::sections::{HomeFilter, apply_favorite_order, build_direct, build_sections};
use crate::security::DeviceApproval;
use crate::settings::{LastOpenPlace, Settings, last_open, remember_last_open};
use crate::toast_timer::{Hold, ToastClock};
use crate::unread_divider::{DividerEvent, next_divider};
use crate::viewport::{CompactDepth, CompactPane, Layout, current_layout};
use teamcraft_client_api::actions::{NewChannel, NewSpace, PermalinkTarget};
use teamcraft_proto::{MeetingId, Role};

/// How many of the newest messages are searched for the first unread one.
const UNREAD_SCAN_LIMIT: usize = 500;

/// How long a message opened from search or a link stays highlighted.
const HIGHLIGHT_DURATION: Duration = Duration::from_millis(2_500);
/// Toasts shown at once; a new one closes the oldest beyond this.
const MAX_TOASTS: usize = 4;
/// How long a deleted message can be restored before it is deleted for everyone.
const UNDO_WINDOW: Duration = Duration::from_secs(6);
/// How long a confirmation toast ("Link copied") stays.
const BRIEF_TOAST: Duration = Duration::from_secs(4);

/// A short message over the app, optionally with an Undo button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Toast {
  pub id: u64,
  pub text: String,
  pub maybe_undo: Option<Undo>,
  pub tone: ToastTone,
}

/// How a toast looks and is announced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToastTone {
  Info,
  /// A failed action (issue #322), with the technical detail behind a "Details" toggle.
  Error {
    detail: String,
  },
}

/// What the Undo button of a toast reverts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Undo {
  /// Keep a message whose deletion is still waiting.
  RestoreMessage(MessageId),
  /// Stay in a channel whose leaving is still waiting.
  StayInChannel(ConversationId),
}

/// A message the user is replying to with a quote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyTarget {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub author_name: String,
  pub excerpt: String,
}

/// A space or channel the user just created; it opens as soon as it shows up (the server answers
/// asynchronously and hands out the ID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingCreation {
  Space {
    name: String,
    known: Vec<SpaceId>,
  },
  Channel {
    space: SpaceId,
    name: String,
    known: Vec<ConversationId>,
  },
}

/// Where the unread messages began when a conversation was opened: the feed shows a "New messages"
/// divider above this message index and starts there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnreadMarker {
  pub conversation: ConversationId,
  pub index: usize,
}

/// The entry of the space rail that is selected, and so which sidebar shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RailEntry {
  /// Chats: the DMs and group DMs, which live outside spaces (Element's and Discord's home).
  Chats,
  /// The space in [`AppState::space`] (or all spaces, in the grouped sidebar).
  Space,
}

/// What fills the main column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainView {
  Conversation(ConversationId),
  /// The threads inbox, optionally with one thread open in the side panel.
  Threads,
  /// The administration console (admins only).
  Admin(AdminSection),
  /// Message search; the query lives in [`AppState::search_query`].
  Search,
  /// Messages the user saved for later.
  Saved,
  /// Mentions, reactions to the user's messages and replies in their threads.
  Activity,
  /// The user's calls: missed, incoming and outgoing.
  Calls,
  /// Scheduled and instant meetings (signed-in builds).
  Meetings,
  /// People and user groups.
  Directory,
  /// First steps for an empty workspace or space: create a space or channel, invite colleagues.
  GettingStarted,
  /// Home with no conversation open.
  Home,
  /// The space in [`AppState::space`] with nothing open: its channel list in the compact layout
  /// (the wider layouts open its first channel instead).
  Space,
  /// A public channel of the space the user hasn't joined: what it is about, and "Join".
  ChannelPreview(SpaceId, ConversationId),
}

/// What the right-hand panel shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidePanel {
  Closed,
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  Members,
  /// The pinned messages of the open conversation.
  Pins,
}

/// Modal and popover visibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overlay {
  None,
  QuickSwitcher,
  StatusMenu,
  CustomStatus,
  Settings,
  Shortcuts,
  /// Another device of the user asks for approval.
  DeviceApproval,
  /// QR code and code of this (new) device while it waits to be linked.
  LinkDevice,
  /// Compare the old and new identity key of someone in a conversation.
  VerifyKey(ConversationId),
  /// Compare the safety number with a contact (Settings > Devices and security).
  VerifyContact(UserId),
  /// The dialog that schedules a meeting.
  ScheduleMeeting,
  /// The link of the meeting just scheduled or started (`MeetingsHub::maybe_created`), to copy.
  MeetingCreated,
  /// Pick a conversation to forward a message to.
  Forward(ConversationId, MessageId),
  NewSpace,
  /// Pick one or several people to write to (a DM or a group DM).
  NewMessage,
  /// Open spaces to join and the user's own spaces, and the way to create a new one; a shared
  /// space link puts its space first.
  BrowseSpaces(Option<SpaceId>),
  /// Invite people to a space: from the directory, by e-mail, or with the link of an open space.
  InvitePeople(SpaceId),
  /// Add people to a channel (through its encryption group).
  AddToChannel(ConversationId),
  /// Add people to the DM or group DM call of this conversation: the call moves into a meeting.
  AddToCall(ConversationId),
  NewChannel(SpaceId),
  BrowseChannels(SpaceId),
  ChannelSettings(ConversationId),
  SpaceSettings(SpaceId),
  /// The image lightbox over a conversation, opened on this image.
  Lightbox(ConversationId, MediaKey),
  /// The preview of a Markdown, HTML or PDF file of a conversation.
  FilePreview(ConversationId, MediaKey),
  /// The versions of an edited message.
  EditHistory(ConversationId, MessageId),
  /// Edit the bookmarks bar of a conversation.
  Bookmarks(ConversationId),
  /// Another person's profile card.
  UserProfile(UserId),
  /// Calls aren't available in this client (the browser, for now): where the apps are.
  CallsUnavailable,
}

/// The sections of the settings dialog (PLAN.md §4 and §5, issue #327).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
  Account,
  Appearance,
  Notifications,
  Preferences,
  Keyboard,
  AudioVideo,
  Devices,
  Security,
  About,
}

/// What the settings dialog shows: on phones the list of sections is a page of its own
/// (`/settings`); wider layouts always show a section beside the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsPage {
  Sections,
  Section(SettingsTab),
}

/// What became of a route the app was asked to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteOutcome {
  Shown,
  /// It names something that hasn't loaded yet; it opens once it does.
  Waiting,
  /// It names something that doesn't exist or the user can't open; Home shows instead.
  Unavailable,
}

#[derive(Clone, Copy)]
pub struct AppState {
  backend: StoredValue<Rc<dyn Backend>, LocalStorage>,
  /// Bumped after every backend mutation; reading data through [`AppState::read`] subscribes to it.
  revision: RwSignal<u64>,
  pub space: RwSignal<SpaceId>,
  pub rail: RwSignal<RailEntry>,
  pub view: RwSignal<MainView>,
  pub panel: RwSignal<SidePanel>,
  pub overlay: RwSignal<Overlay>,
  /// The layout the window's size calls for (`viewport.rs`).
  pub layout: RwSignal<Layout>,
  /// Text of the search box, kept when leaving and re-entering the search page.
  pub search_query: RwSignal<String>,
  /// What the top bar's search starts with when it opens (Ctrl/Cmd+F: the open conversation).
  pub switcher_query: RwSignal<String>,
  pub call: CallState,
  /// The login waiting for the user's approval, shown while the overlay is `DeviceApproval`.
  pub device_prompt: RwSignal<Option<DeviceApproval>>,
  /// Bumped when the composer sent a message, so the feed scrolls to it.
  /// The message the composer quotes in its next message.
  pub reply_target: RwSignal<Option<ReplyTarget>>,
  /// The people of a DM or group DM that was asked for and is not there yet.
  pub pending_dm: RwSignal<Option<Vec<UserId>>>,
  /// Short messages shown over the app until dismissed (reminders, undo).
  pub toasts: RwSignal<Vec<Toast>>,
  next_toast: StoredValue<u64>,
  /// Countdowns of the toasts that close by themselves; they pause on hover and focus (#367).
  toast_clock: StoredValue<ToastClock, LocalStorage>,
  /// Messages deleted with an undo window: hidden now, deleted when the window ends.
  pub hidden_messages: RwSignal<HashSet<MessageId>>,
  /// Channels left with an undo window: gone from the sidebar now, left when the window ends.
  pub hidden_conversations: RwSignal<HashSet<ConversationId>>,
  /// A conversation to open as soon as it appears in the lists (after joining it).
  pub pending_open: RwSignal<Option<ConversationId>>,
  /// A space or channel to open once it was created.
  pub pending_creation: RwSignal<Option<PendingCreation>>,
  /// Started on the getting-started page because nothing had loaded yet: the first conversation
  /// opens once the data arrives.
  awaits_first_conversation: RwSignal<bool>,
  pub jump_to_latest: RwSignal<u64>,
  /// A request to scroll a conversation's feed to a message index (jump to date).
  pub jump_to_index: RwSignal<Option<(ConversationId, usize)>>,
  /// The "New messages" position of the open conversation, taken before it was marked as read.
  pub unread_marker: RwSignal<Option<UnreadMarker>>,
  /// The first visible message of conversations the user left while scrolled up, to return there.
  pub scroll_anchors: StoredValue<HashMap<ConversationId, usize>>,
  /// A message just opened from search, a link or the activity list; its row lights up briefly.
  pub highlighted: RwSignal<Option<MessageId>>,
  /// A message the composer asked to edit (↑ in the empty composer); its row opens the editor.
  pub edit_request: RwSignal<Option<MessageId>>,
  /// The page the settings dialog shows.
  pub settings_page: RwSignal<SettingsPage>,
  /// The message a link or jump opened in the current conversation: the URL names it until the
  /// user goes somewhere else.
  linked_message: RwSignal<Option<PermalinkTarget>>,
  /// A route that names something which hasn't loaded yet.
  pub pending_route: RwSignal<Option<Route>>,
  /// A meeting link (`/meeting/{id}`) the meetings screen has yet to open; it takes it.
  pub maybe_meeting_link: RwSignal<Option<MeetingId>>,
  /// Bumped when a route named something that doesn't exist or the user can't open.
  pub unavailable_routes: RwSignal<u64>,
  /// Set while the app follows a URL it was given or redirects on its own (the first conversation
  /// once startup data arrived), so the address bar replaces its entry instead of adding one
  /// (`history.rs` takes it).
  follows_url: StoredValue<bool>,
  pub settings: Settings,
}

/// Where the app starts: the first conversation of a space, or Home with the welcome page when
/// there is none yet (there is one Home: the rail's Home entry and its sidebar).
fn start_position(start: Option<(SpaceId, ConversationId)>) -> (SpaceId, RailEntry, MainView) {
  match start {
    Some((space, conversation)) => (space, RailEntry::Space, MainView::Conversation(conversation)),
    None => (id_from_raw(0), RailEntry::Chats, MainView::GettingStarted),
  }
}

/// Home's page without any DM: the welcome page while the workspace is empty (create a space,
/// find people), else Home's own start page.
/// The remembered conversation if it is still among `candidates` (it may have been left or
/// archived since); `None` otherwise, so the caller picks its own default.
fn remembered_conversation(
  candidates: impl IntoIterator<Item = ConversationId>,
  remembered: Option<ConversationId>,
) -> Option<ConversationId> {
  let remembered = remembered?;
  candidates
    .into_iter()
    .any(|id| id == remembered)
    .then_some(remembered)
}

fn home_view_without_dms(has_spaces: bool) -> MainView {
  if has_spaces {
    MainView::Home
  } else {
    MainView::GettingStarted
  }
}

/// Which pane a view fills in the compact layout: the lists of Home and of a space, a side panel
/// over its conversation, and the main column for everything else.
fn compact_depth_for(view: MainView, panel: SidePanel) -> CompactDepth {
  match compact_pane_for(view, panel) {
    CompactPane::Panel => CompactDepth::Panel,
    CompactPane::List => CompactDepth::TopLevel,
    CompactPane::Main if matches!(view, MainView::Activity | MainView::GettingStarted) => {
      CompactDepth::TopLevel
    },
    CompactPane::Main => CompactDepth::Page,
  }
}

fn compact_pane_for(view: MainView, panel: SidePanel) -> CompactPane {
  match view {
    MainView::Home | MainView::Space => CompactPane::List,
    MainView::Conversation(_) if panel != SidePanel::Closed => CompactPane::Panel,
    _ => CompactPane::Main,
  }
}

impl AppState {
  /// `start` is the first space and conversation; without one Home opens with the welcome page.
  pub fn new(backend: Rc<dyn Backend>, settings: Settings, start: Option<(SpaceId, ConversationId)>) -> Self {
    let (space, rail, view) = start_position(start);
    Self {
      backend: StoredValue::new_local(backend),
      revision: RwSignal::new(0),
      space: RwSignal::new(space),
      rail: RwSignal::new(rail),
      view: RwSignal::new(view),
      panel: RwSignal::new(SidePanel::Closed),
      overlay: RwSignal::new(Overlay::None),
      layout: RwSignal::new(current_layout()),
      search_query: RwSignal::new(String::new()),
      switcher_query: RwSignal::new(String::new()),
      call: CallState::new(),
      device_prompt: RwSignal::new(None),
      reply_target: RwSignal::new(None),
      pending_dm: RwSignal::new(None),
      toasts: RwSignal::new(Vec::new()),
      next_toast: StoredValue::new(0),
      toast_clock: StoredValue::new_local(ToastClock::default()),
      hidden_messages: RwSignal::new(HashSet::new()),
      hidden_conversations: RwSignal::new(HashSet::new()),
      pending_open: RwSignal::new(None),
      pending_creation: RwSignal::new(None),
      awaits_first_conversation: RwSignal::new(start.is_none()),
      jump_to_latest: RwSignal::new(0),
      jump_to_index: RwSignal::new(None),
      unread_marker: RwSignal::new(None),
      edit_request: RwSignal::new(None),
      highlighted: RwSignal::new(None),
      scroll_anchors: StoredValue::new(HashMap::new()),
      settings_page: RwSignal::new(SettingsPage::Section(SettingsTab::DEFAULT)),
      linked_message: RwSignal::new(None),
      pending_route: RwSignal::new(None),
      maybe_meeting_link: RwSignal::new(None),
      unavailable_routes: RwSignal::new(0),
      follows_url: StoredValue::new(false),
      settings,
    }
  }

  /// Tells every reader that the backend changed on its own (data arrived over the network or from
  /// the desktop shell), so views re-read.
  pub fn backend_changed(&self) {
    // Called by the backend from outside the reactive tree, possibly after this shell is gone
    // (sign-out, a new session): then there is nothing to update (issue #322).
    if self.revision.try_update(|revision| *revision += 1).is_none() {
      return;
    }
    self.open_pending_creation();
    if let Some(route) = self.pending_route.get_untracked() {
      self.follow_route(route);
    }
    self.open_first_loaded_conversation();
    if let Some(conversation) = self.pending_open.get_untracked()
      && self.read(|backend| backend.conversation(conversation)).is_some()
    {
      self.pending_open.set(None);
      self.open_conversation(conversation);
    }
    if let Some(users) = self.pending_dm.get_untracked()
      && let Some(conversation) = self.direct_with(&users)
    {
      self.pending_dm.set(None);
      self.open_conversation(conversation);
    }
  }

  /// Forgets that `conversation` opens once it shows up, after its join was refused: a later join
  /// from elsewhere must not jump into it (issue #328). Called when the core answered, possibly
  /// after the shell is gone.
  pub fn forget_pending_open(&self, conversation: ConversationId) {
    let _ = self.pending_open.try_update(|maybe_open| {
      if *maybe_open == Some(conversation) {
        *maybe_open = None;
      }
    });
  }

  /// Creates a space and switches to it once it exists.
  pub fn create_space(&self, space: NewSpace) {
    let known = self.read(|backend| backend.spaces().into_iter().map(|space| space.id).collect());
    self.pending_creation.set(Some(PendingCreation::Space {
      name: space.name.clone(),
      known,
    }));
    self.write(|backend| backend.create_space(space));
    self.open_pending_creation();
  }

  /// Joins an open space and switches to it once the server confirmed.
  pub fn join_space(&self, space: SpaceId, name: String) {
    let known = self.read(|backend| backend.spaces().into_iter().map(|space| space.id).collect());
    self
      .pending_creation
      .set(Some(PendingCreation::Space { name, known }));
    self.write(|backend| backend.join_space(space));
    self.open_pending_creation();
  }

  /// Creates a channel and opens it once it exists.
  pub fn create_channel(&self, channel: NewChannel) {
    let known = self.read(|backend| {
      backend
        .space_conversations(channel.space)
        .into_iter()
        .map(|conversation| conversation.id)
        .collect()
    });
    self.pending_creation.set(Some(PendingCreation::Channel {
      space: channel.space,
      name: channel.name.clone(),
      known,
    }));
    self.write(|backend| backend.create_channel_with(channel));
    self.open_pending_creation();
  }

  fn open_first_loaded_conversation(&self) {
    if !self.awaits_first_conversation.get_untracked()
      || self.view.get_untracked() != MainView::GettingStarted
      || self.pending_route.get_untracked().is_some()
    {
      return;
    }
    let first = self.backend.with_value(|backend| {
      backend
        .spaces()
        .into_iter()
        .find_map(|space| backend.space_conversations(space.id).into_iter().next())
        .or_else(|| backend.direct_conversations().into_iter().next())
    });
    if let Some(conversation) = first {
      // Phones start on Home's list, like the mobile apps.
      if self.is_compact() {
        self.awaits_first_conversation.set(false);
        self.open_chats();
      } else {
        self.open_conversation(conversation.id);
      }
      // It stands in for the start page the URL already shows, so Back doesn't return to a `/`
      // that would just open it again.
      self.follows_url.set_value(true);
    }
  }

  fn open_pending_creation(&self) {
    let Some(pending) = self.pending_creation.get_untracked() else {
      return;
    };
    match pending {
      PendingCreation::Space { name, known } => {
        let found = self.backend.with_value(|backend| {
          backend
            .spaces()
            .into_iter()
            .find(|space| space.name == name && !known.contains(&space.id))
        });
        if let Some(space) = found {
          self.pending_creation.set(None);
          self.space.set(space.id);
          self.open_first_conversation_of(space.id);
        }
      },
      PendingCreation::Channel { space, name, known } => {
        let found = self.backend.with_value(|backend| {
          backend
            .space_conversations(space)
            .into_iter()
            .find(|conversation| conversation.name == name && !known.contains(&conversation.id))
        });
        if let Some(conversation) = found {
          self.pending_creation.set(None);
          self.open_conversation(conversation.id);
        }
      },
    }
  }

  /// Opens the first channel of a space, or shows its getting-started page when it has none.
  /// Opens the channel the space had open last (#498), else its first channel.
  pub fn open_first_conversation_of(&self, space: SpaceId) {
    let remembered = self
      .reopen_setting()
      .then(|| last_open(self.me().id).spaces.get(&space).copied())
      .flatten();
    let first = self.backend.with_value(|backend| {
      let channels = backend.space_conversations(space);
      remembered_conversation(channels.iter().map(|channel| channel.id), remembered)
        .or_else(|| channels.first().map(|channel| channel.id))
    });
    match first {
      Some(id) => self.open_conversation(id),
      None => {
        self.view.set(MainView::GettingStarted);
        self.panel.set(SidePanel::Closed);
      },
    }
  }

  /// Opens the direct conversation with a person, creating it first if there is none yet.
  pub fn open_direct(&self, user: UserId) {
    self.open_direct_with(vec![user]);
  }

  /// Opens the DM (one person) or group DM (several) with exactly these people, creating it first
  /// if there is none yet. The user themself may be in `users`.
  pub fn open_direct_with(&self, mut users: Vec<UserId>) {
    let me = self.me().id;
    users.retain(|user| *user != me);
    users.sort();
    users.dedup();
    if users.is_empty() {
      return;
    }
    if let Some(conversation) = self.direct_with(&users) {
      self.open_conversation(conversation);
      return;
    }
    self.pending_dm.set(Some(users.clone()));
    match users.as_slice() {
      [user] => {
        let user = *user;
        self.write(|backend| backend.open_dm(user));
      },
      _ => self.write(|backend| backend.open_group_dm(users)),
    }
  }

  pub(crate) fn direct_with(&self, users: &[UserId]) -> Option<ConversationId> {
    let me = self.me().id;
    let kind = if users.len() == 1 {
      crate::model::ConversationKind::Direct
    } else {
      crate::model::ConversationKind::Group
    };
    self.read(|backend| {
      backend
        .direct_conversations()
        .into_iter()
        .find(|conversation| conversation.kind == kind && has_exactly(&conversation.member_ids, users, me))
        .map(|conversation| conversation.id)
    })
  }

  /// Switches the sidebar to a space (all spaces in the grouped sidebar). Lands in its first channel
  /// unless the open page isn't tied to a space (threads, search, …) or already belongs to it.
  pub fn open_space(&self, space: SpaceId) {
    self.space.set(space);
    self.rail.set(RailEntry::Space);
    let is_grouped = self
      .settings
      .prefs
      .with_untracked(|prefs| prefs.sidebar.is_grouped_by_space);
    let belongs = match self.view.get_untracked() {
      MainView::Conversation(current) => {
        self
          .read(|backend| backend.conversation(current))
          .is_some_and(|conversation| {
            conversation.maybe_space.is_some() && (is_grouped || conversation.maybe_space == Some(space))
          })
      },
      MainView::Threads
      | MainView::Search
      | MainView::Meetings
      | MainView::Saved
      | MainView::Activity
      | MainView::Calls
      | MainView::Directory => true,
      MainView::ChannelPreview(previewed, _) => is_grouped || previewed == space,
      MainView::Admin(_) | MainView::GettingStarted | MainView::Home | MainView::Space => false,
    };
    if self.is_compact() {
      self.show_space_list();
    } else if !belongs {
      self.open_first_conversation_of(space);
    }
  }

  /// Shows Chats: the DM it had open last (#498), or the page's start when there is none.
  pub fn open_chats(&self) {
    self.rail.set(RailEntry::Chats);
    // Phones show the chat list (the welcome page while the workspace is empty); a conversation
    // opens from there.
    if self.is_compact() {
      let is_empty =
        self.read(|backend| backend.spaces().is_empty() && backend.direct_conversations().is_empty());
      self.view.set(if is_empty {
        MainView::GettingStarted
      } else {
        MainView::Home
      });
      self.panel.set(SidePanel::Closed);
      return;
    }
    let is_direct_open = self
      .current_conversation_id()
      .and_then(|id| self.read(|backend| backend.conversation(id)))
      .is_some_and(|conversation| conversation.kind.is_direct_like());
    if is_direct_open {
      return;
    }
    let direct = self.direct_list();
    let remembered = self
      .reopen_setting()
      .then(|| last_open(self.me().id).home)
      .flatten();
    match remembered_conversation(direct.iter().map(|conversation| conversation.id), remembered) {
      Some(id) => self.open_conversation(id),
      None => {
        let has_spaces = self.read(|backend| !backend.spaces().is_empty());
        self.view.set(home_view_without_dms(has_spaces));
        self.panel.set(SidePanel::Closed);
      },
    }
  }

  /// Whether switching to Home or a space reopens what was open there (the sidebar setting).
  fn reopen_setting(&self) -> bool {
    self
      .settings
      .prefs
      .with_untracked(|prefs| prefs.sidebar.reopen_last_conversation)
  }

  /// Keeps the conversation of Home or a space for the next visit to it.
  fn remember_place(&self, conversation: &Conversation, id: ConversationId) {
    let place = match conversation.maybe_space {
      Some(space) => LastOpenPlace::Space(space, id),
      None if conversation.kind.is_direct_like() => LastOpenPlace::Home(id),
      None => return,
    };
    remember_last_open(self.me().id, place);
  }

  /// DMs and group DMs as Home lists them (favorites first, then by recency
  /// or name), without the display limit.
  pub fn direct_list(&self) -> Vec<Conversation> {
    let sort = self.settings.prefs.with(|prefs| prefs.sidebar.dm_sort);
    build_direct(self.direct_entries(), sort, 0, "", HomeFilter::All)
      .sections
      .into_iter()
      .flat_map(|section| section.conversations)
      .collect()
  }

  /// DMs and group DMs (without the ones being left, mutes applied), each with the time of its
  /// newest message.
  pub fn direct_entries(&self) -> Vec<(Conversation, Option<TimestampMs>)> {
    let list = self.with_mutes(self.sidebar_list(self.read(|backend| backend.direct_conversations())));
    self.read(|backend| {
      list
        .into_iter()
        .map(|conversation| {
          let at = backend.last_activity(conversation.id);
          (conversation, at)
        })
        .collect()
    })
  }

  /// Reads from the backend and re-runs the calling reactive scope after any mutation.
  pub fn read<R>(&self, read: impl FnOnce(&dyn Backend) -> R) -> R {
    get_if_observed(self.revision);
    self.backend.with_value(|backend| read(backend.as_ref()))
  }

  /// Applies a mutation and notifies every reader.
  pub fn write<R>(&self, write: impl FnOnce(&dyn Backend) -> R) -> R {
    let result = self.backend.with_value(|backend| write(backend.as_ref()));
    self.revision.update(|revision| *revision += 1);
    result
  }

  /// Whether the shell that owns this state is still mounted. Timers and tasks that call several
  /// methods check it once instead of guarding each (issue #322).
  pub fn is_alive(&self) -> bool {
    !self.revision.is_disposed()
  }

  /// [`Self::write`] for async tasks: nothing happens once the shell is gone (issue #322).
  pub fn try_write<R>(&self, write: impl FnOnce(&dyn Backend) -> R) -> Option<R> {
    let result = self.backend.try_with_value(|backend| write(backend.as_ref()))?;
    let _ = self.revision.try_update(|revision| *revision += 1);
    Some(result)
  }

  pub fn me(&self) -> User {
    self.read(|backend| backend.me())
  }

  pub fn current_conversation_id(&self) -> Option<ConversationId> {
    match self.view.get() {
      MainView::Conversation(id) => Some(id),
      MainView::Threads
      | MainView::Admin(_)
      | MainView::Search
      | MainView::Meetings
      | MainView::Saved
      | MainView::Activity
      | MainView::Calls
      | MainView::Directory
      | MainView::GettingStarted
      | MainView::Home
      | MainView::Space
      | MainView::ChannelPreview(..) => None,
    }
  }

  /// Follows an announced move of the call of `from` into the meeting room `to`: the core checks it
  /// (announced in `from`'s group, nobody unexpected in `to`'s), then this device joins the room's
  /// call with its microphone and camera as they were. A failed check stays in this call and says
  /// why (a toast and `pending`).
  pub fn follow_call_move(
    &self,
    from: ConversationId,
    to: ConversationId,
    pending: crate::feedback::PendingAction,
  ) {
    let state = *self;
    pending.run_and_then(
      || state.write(|backend| backend.accept_call_move(from, to)),
      move || {
        let call = state.call;
        let is_still_in_it = call
          .maybe_request
          .try_get_untracked()
          .flatten()
          .is_some_and(|request| request.conversation == from);
        if !is_still_in_it {
          return;
        }
        let _ = call.move_offer.try_set(None);
        if state.view.try_get_untracked() == Some(MainView::Conversation(from)) {
          state.open_conversation(to);
        }
        // Joining leaves the DM's call first.
        call.join(crate::calls::CallRequest {
          conversation: to,
          kind: crate::calls::CallKind::Group,
        });
      },
    );
  }

  /// Answers a ringing call on this device: opens its conversation and joins. The in-app card and
  /// the system notification's Accept share this.
  pub fn accept_call(&self, incoming: crate::calls::IncomingCall, with_video: bool) {
    let kind = match self
      .read(|backend| backend.conversation(incoming.conversation))
      .map(|conversation| conversation.kind)
    {
      Some(crate::model::ConversationKind::Direct) | None => crate::calls::CallKind::Direct,
      Some(_) => crate::calls::CallKind::Group,
    };
    self.call.incoming.set(None);
    self.call.is_camera_on.set(with_video);
    self.open_conversation(incoming.conversation);
    self.call.join(crate::calls::CallRequest {
      conversation: incoming.conversation,
      kind,
    });
  }

  pub fn open_conversation(&self, id: ConversationId) {
    self.awaits_first_conversation.set(false);
    let Some(conversation) = self.read(|backend| backend.conversation(id)) else {
      return;
    };
    if let Some(space) = conversation.maybe_space {
      self.space.set(space);
      self.rail.set(RailEntry::Space);
    } else if conversation.kind.is_direct_like() {
      self.rail.set(RailEntry::Chats);
    }
    if let SidePanel::Thread {
      conversation: open, ..
    } = self.panel.get_untracked()
      && open != id
    {
      self.panel.set(SidePanel::Closed);
    }
    if self.view.get_untracked() != MainView::Conversation(id) {
      // Leaving a conversation removes its divider; the new one is placed from its read marker.
      if let MainView::Conversation(previous) = self.view.get_untracked() {
        self.divider_event(previous, DividerEvent::Left);
      }
      self.unread_marker.set(self.unread_marker_for(&conversation));
      self.settings.remember_conversation(id);
      self.remember_place(&conversation, id);
      self.linked_message.set(None);
    }
    self.view.set(MainView::Conversation(id));
    self.overlay.set(Overlay::None);
    if conversation.is_catching_up {
      // Only a catch-up has a use for it; settled switches cost no extra request.
      self.read(|backend| backend.focus_conversation(id));
    }
    if conversation.has_unread() {
      self.write(|backend| backend.mark_read(id));
    }
  }

  /// Sending in a conversation reads everything before the sent message: the user answers what they
  /// saw. The read marker goes to the newest message already in the store, before the send is
  /// confirmed, so the sent message itself plays no part. Nothing to send when nothing is unread.
  pub fn mark_read_on_send(&self, conversation: ConversationId) {
    let has_unread = self
      .read(|backend| backend.conversation(conversation))
      .is_some_and(|conversation| conversation.has_unread());
    if has_unread {
      self.write(|backend| backend.mark_read(conversation));
    }
  }

  /// Moves or removes the "New messages" divider after something happened in `conversation`.
  pub fn divider_event(&self, conversation: ConversationId, event: DividerEvent) {
    let Some(current) = self.unread_marker.try_get_untracked() else {
      return;
    };
    let next = next_divider(current, conversation, event);
    if next != current {
      self.unread_marker.try_set(next);
    }
  }

  /// Esc in a conversation: everything is read and the divider goes (#321). The read marker only
  /// moves forward (the core's queue, #159).
  pub fn mark_conversation_read(&self, conversation: ConversationId) {
    self.write(|backend| backend.mark_read(conversation));
    self.divider_event(conversation, DividerEvent::MarkedRead);
  }

  /// Where the "New messages" divider of a conversation goes, before opening marks it as read.
  fn unread_marker_for(&self, conversation: &Conversation) -> Option<UnreadMarker> {
    let unread = conversation.unread_count as usize;
    if unread == 0 {
      return None;
    }
    let id = conversation.id;
    let (count, maybe_first, tail_start, tail_ids) = self.backend.with_value(|backend| {
      let count = backend.message_count(id);
      let tail_len = unread.saturating_mul(2).clamp(1, UNREAD_SCAN_LIMIT).min(count);
      let tail_start = count - tail_len;
      let tail_ids: Vec<MessageId> = backend
        .messages(id, tail_start, tail_len)
        .into_iter()
        .map(|message| message.id)
        .collect();
      (count, backend.first_unread(id), tail_start, tail_ids)
    });
    divider_index(count, unread, tail_start, &tail_ids, maybe_first).map(|index| UnreadMarker {
      conversation: id,
      index,
    })
  }

  /// Opens the previous or next conversation with unread messages in the sidebar (wrapping).
  pub fn step_unread(&self, step: Step) {
    let list: Vec<(ConversationId, bool)> = self
      .navigable_conversations()
      .into_iter()
      .map(|conversation| {
        let is_unread =
          conversation.mention_count > 0 || (conversation.unread_count > 0 && !conversation.is_muted);
        (conversation.id, is_unread)
      })
      .collect();
    if let Some(target) = next_unread(&list, self.current_conversation_id(), step) {
      self.open_conversation(target);
    }
  }

  /// Marks every conversation of the current list (the space's channels, or the DMs) as read.
  pub fn mark_all_read(&self) {
    let unread: Vec<ConversationId> = self
      .navigable_conversations()
      .into_iter()
      .filter(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0)
      .map(|conversation| conversation.id)
      .collect();
    if unread.is_empty() {
      return;
    }
    self.write(|backend| {
      for conversation in unread {
        backend.mark_read(conversation);
      }
    });
  }

  /// Shows a toast until the user closes it.
  pub fn show_toast(&self, text: String) {
    self.push_toast(text, None);
  }

  /// Shows a failed action until the user closes it. Called from outside the reactive tree (the
  /// core answered), so it does nothing once the shell is gone. A repeat of any error still shown
  /// adds no second toast; background failures are rate-limited before they get here (#344).
  pub fn show_error(&self, text: String, detail: String) {
    let Some(id) = self.next_toast.try_get_value() else {
      return;
    };
    let _ = self.toasts.try_update(|list| {
      let is_shown = list.iter().any(|shown| {
        shown.text == text && matches!(&shown.tone, ToastTone::Error { detail: shown } if *shown == detail)
      });
      if !is_shown {
        self.next_toast.set_value(id + 1);
        push_capped(
          list,
          Toast {
            id,
            text,
            maybe_undo: None,
            tone: ToastTone::Error { detail },
          },
        );
      }
    });
  }

  /// Shows a confirmation that goes away by itself ("Link copied").
  pub fn show_brief_toast(&self, text: String) {
    let toast = self.push_toast(text, None);
    self.close_toast_after(toast, BRIEF_TOAST, || {});
  }

  /// Closes a toast and runs what its end does (an Undo toast commits now). Also called by the
  /// toast timer, which may fire after the shell is gone.
  pub fn dismiss_toast(&self, id: u64) {
    let _ = self.toasts.try_update(|list| list.retain(|toast| toast.id != id));
    // A hold from a toast that went away must not stall the others: nothing is left to point at,
    // and a removed toast takes the focus with it without a `focusout`.
    if self.toasts.try_with_untracked(Vec::is_empty) == Some(true) {
      self.hold_toasts(Hold::Pointer, false);
    }
    if !is_focus_in_toasts() {
      self.hold_toasts(Hold::Focus, false);
    }
    let maybe_end = self.toast_clock.try_update_value(|clock| clock.end(id)).flatten();
    if let Some(end) = maybe_end {
      end();
    }
  }

  /// The page goes away (closed, navigated, frozen or discarded): what the Undo toasts hold back is
  /// done now rather than never.
  pub fn end_timed_toasts(&self) {
    let pending: Vec<u64> = self
      .toast_clock
      .try_with_value(|clock| clock.on_end.keys().copied().collect())
      .unwrap_or_default();
    for toast in pending {
      self.dismiss_toast(toast);
    }
  }

  /// The pointer or focus is on the toasts, or the page is hidden: countdowns stand still (#367).
  pub fn hold_toasts(&self, hold: Hold, is_on: bool) {
    let now = crate::clock::now_ms();
    if self
      .toast_clock
      .try_update_value(|clock| clock.timers.set_hold(hold, is_on, now))
      .is_some()
    {
      self.schedule_toast_wake();
    }
  }

  /// Hides an own message at once and deletes it after the undo window, unless Undo was pressed.
  pub fn delete_with_undo(&self, conversation: ConversationId, message: MessageId, text: String) {
    let state = *self;
    self.hidden_messages.update(|hidden| {
      hidden.insert(message);
    });
    let toast = self.push_toast(text, Some(Undo::RestoreMessage(message)));
    self.close_toast_after(toast, UNDO_WINDOW, move || {
      let is_still_deleted = state
        .hidden_messages
        .try_update(|hidden| hidden.remove(&message))
        .unwrap_or(false);
      if is_still_deleted {
        state.try_write(|backend| backend.delete_message(conversation, message));
      }
    });
  }

  /// Runs the Undo of a toast and closes it.
  pub fn undo(&self, toast: u64, undo: Undo) {
    match undo {
      Undo::RestoreMessage(message) => self.hidden_messages.update(|hidden| {
        hidden.remove(&message);
      }),
      Undo::StayInChannel(conversation) => {
        self.hidden_conversations.update(|hidden| {
          hidden.remove(&conversation);
        });
        self.open_conversation(conversation);
      },
    }
    self.dismiss_toast(toast);
  }

  /// Leaves a channel after the undo window; it disappears from the sidebar at once.
  pub fn leave_with_undo(&self, conversation: ConversationId, text: String) {
    let state = *self;
    if self.current_conversation_id() == Some(conversation) {
      let next = self
        .navigable_conversations()
        .into_iter()
        .find(|candidate| candidate.id != conversation);
      match next {
        Some(next) => self.open_conversation(next.id),
        None => self.open_threads(),
      }
    }
    self.hidden_conversations.update(|hidden| {
      hidden.insert(conversation);
    });
    let toast = self.push_toast(text, Some(Undo::StayInChannel(conversation)));
    self.close_toast_after(toast, UNDO_WINDOW, move || {
      let is_still_leaving = state
        .hidden_conversations
        .try_update(|hidden| hidden.remove(&conversation))
        .unwrap_or(false);
      if is_still_leaving {
        state.try_write(|backend| backend.leave_channel(conversation));
      }
    });
  }

  /// Conversations as the sidebar shows them: without the ones being left, favorites in the user's
  /// order.
  pub fn sidebar_list(&self, mut list: Vec<Conversation>) -> Vec<Conversation> {
    self
      .hidden_conversations
      .with(|hidden| list.retain(|conversation| !hidden.contains(&conversation.id)));
    if self.settings.prefs.with(|prefs| prefs.sidebar.sort) == SidebarSort::Default {
      self
        .settings
        .favorite_order
        .with(|order| apply_favorite_order(&mut list, order));
    }
    list
  }

  /// Conversations with the notification settings applied: one muted there (or through its space)
  /// looks muted in the lists too.
  pub fn with_mutes(&self, mut list: Vec<Conversation>) -> Vec<Conversation> {
    self.settings.prefs.with(|prefs| {
      for conversation in &mut list {
        let maybe_space = conversation.maybe_space.map(|space| space.to_string());
        conversation.is_muted |= prefs
          .notifications
          .is_muted_in(&conversation.id.to_string(), maybe_space.as_deref());
      }
    });
    list
  }

  /// Closes `toast` after `duration` of countdown, which pauses while held; `on_end` runs when it
  /// closes, by the countdown or earlier.
  fn close_toast_after(&self, toast: u64, duration: Duration, on_end: impl FnOnce() + 'static) {
    let now = crate::clock::now_ms();
    let is_alive = self
      .toast_clock
      .try_update_value(|clock| {
        clock.timers.start(toast, duration, now);
        clock.on_end.insert(toast, Box::new(on_end));
      })
      .is_some();
    if is_alive {
      self.schedule_toast_wake();
    }
  }

  /// One browser timer for the nearest deadline; none while the countdowns are held. Native builds
  /// (tests) have no browser timer and drive the countdowns themselves.
  fn schedule_toast_wake(&self) {
    let state = *self;
    let now = crate::clock::now_ms();
    let _ = self.toast_clock.try_update_value(|clock| {
      if let Some(wake) = clock.maybe_wake.take() {
        wake.clear();
      }
      let Some(deadline) = clock.timers.next_deadline() else {
        return;
      };
      let wait = Duration::from_millis(u64::try_from(deadline - now).unwrap_or(0));
      #[cfg(target_arch = "wasm32")]
      match leptos::leptos_dom::helpers::set_timeout_with_handle(move || state.wake_toasts(), wait) {
        Ok(handle) => clock.maybe_wake = Some(handle),
        // Without a timer the toast stays until closed: worse, but nothing is lost.
        Err(_) => tracing::debug!("could not start the toast timer"),
      }
      #[cfg(not(target_arch = "wasm32"))]
      let _ = (state, wait);
    });
  }

  /// The toast timer fired: closes the toasts whose time ran out and waits for the next one.
  #[cfg(target_arch = "wasm32")]
  fn wake_toasts(&self) {
    self.wake_toasts_at(crate::clock::now_ms());
  }

  /// Native builds have no toast timer; tests call this with their own clock.
  #[cfg(any(target_arch = "wasm32", test))]
  fn wake_toasts_at(&self, now: TimestampMs) {
    let expired = self
      .toast_clock
      .try_update_value(|clock| {
        clock.maybe_wake = None;
        clock.timers.take_expired(now)
      })
      .unwrap_or_default();
    for toast in expired {
      self.dismiss_toast(toast);
    }
    self.schedule_toast_wake();
  }

  fn push_toast(&self, text: String, maybe_undo: Option<Undo>) -> u64 {
    let id = self.next_toast.get_value();
    self.next_toast.set_value(id + 1);
    self.toasts.update(|list| {
      push_capped(
        list,
        Toast {
          id,
          text,
          maybe_undo,
          tone: ToastTone::Info,
        },
      )
    });
    id
  }

  pub fn open_admin(&self, section: AdminSection) {
    self.view.set(MainView::Admin(section));
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  /// Leaves the administration console and returns to the first conversation of the current space.
  pub fn exit_admin(&self) {
    let space = self.space.get_untracked();
    let first = self.read(|backend| {
      backend
        .space_conversations(space)
        .into_iter()
        .next()
        .map(|conv| conv.id)
    });
    match first {
      Some(id) => self.open_conversation(id),
      None => self.open_threads(),
    }
  }

  /// Opens the search page, optionally with a query already filled in.
  /// Opens the top bar's search, the one search for people, threads, channels, spaces and
  /// messages, starting with `query`.
  pub fn open_switcher(&self, query: &str) {
    self.switcher_query.set(query.to_owned());
    self.overlay.set(Overlay::QuickSwitcher);
  }

  /// The `in:` token of the open conversation (`in:#channel`, `in:@person`); `None` when no
  /// conversation is open. The search is scoped to it by default (issue #579).
  pub fn conversation_scope(&self) -> Option<String> {
    let id = self.current_conversation_id()?;
    let conversation = self.read(|backend| backend.conversation(id))?;
    Some(crate::search_scope::scope_token(
      conversation.kind,
      &conversation.name,
    ))
  }

  /// Opens the search limited to the open conversation (a removable chip), or the plain search
  /// when no conversation is open.
  pub fn open_scoped_switcher(&self) {
    let query = self
      .conversation_scope()
      .map(|token| format!("{token} "))
      .unwrap_or_default();
    self.open_switcher(&query);
  }

  pub fn open_search(&self, maybe_query: Option<&str>) {
    if let Some(query) = maybe_query {
      self.search_query.set(query.to_owned());
    }
    self.view.set(MainView::Search);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  /// Shows a public channel the user hasn't joined: its name and topic, and the way to join.
  pub fn open_channel_preview(&self, space: SpaceId, channel: ConversationId) {
    self.space.set(space);
    self.rail.set(RailEntry::Space);
    self.view.set(MainView::ChannelPreview(space, channel));
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_directory(&self) {
    self.view.set(MainView::Directory);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_saved(&self) {
    self.view.set(MainView::Saved);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_activity(&self) {
    self.view.set(MainView::Activity);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_calls(&self) {
    self.view.set(MainView::Calls);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
    self.write(|backend| backend.refresh_call_history());
  }

  /// Shows a message: in its thread if it is a reply, else in the conversation, scrolled to it.
  pub fn open_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
    maybe_thread: Option<ThreadId>,
  ) {
    self.open_conversation(conversation);
    self.highlight(message);
    if let Some(thread) = maybe_thread {
      self.open_thread(conversation, thread);
    }
    self.linked_message.set(Some(PermalinkTarget {
      conversation,
      message,
      maybe_thread,
    }));
    if maybe_thread.is_some() {
      return;
    }
    let index = self.read(|backend| {
      let count = backend.message_count(conversation);
      backend
        .messages(conversation, 0, count)
        .iter()
        .position(|candidate| candidate.id == message)
    });
    if let Some(index) = index {
      self.jump_to_index.set(Some((conversation, index)));
    }
  }

  /// Lights up a message for a moment so the eye finds it after a jump.
  fn highlight(&self, message: MessageId) {
    let state = *self;
    self.highlighted.set(Some(message));
    set_timeout(
      move || {
        if state.highlighted.try_get_untracked() == Some(Some(message)) {
          state.highlighted.set_if_alive(None);
        }
      },
      HIGHLIGHT_DURATION,
    );
  }

  pub fn open_meetings(&self) {
    self.view.set(MainView::Meetings);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_threads(&self) {
    self.view.set(MainView::Threads);
    self.panel.set(SidePanel::Closed);
    self.overlay.set(Overlay::None);
  }

  pub fn open_thread(&self, conversation: ConversationId, thread: ThreadId) {
    if self
      .linked_message
      .with_untracked(|linked| linked.is_some_and(|linked| linked.maybe_thread != Some(thread)))
    {
      self.linked_message.set(None);
    }
    self.panel.set(SidePanel::Thread { conversation, thread });
  }

  /// Whether one pane shows at a time (phones).
  pub fn is_compact(&self) -> bool {
    get_if_observed(self.layout).is_compact()
  }

  /// How deep the page is in the compact layout (#368); the bottom navigation shows on top-level
  /// pages only.
  pub fn compact_depth(&self) -> CompactDepth {
    compact_depth_for(self.view.get(), self.panel.get())
  }

  /// The pane that fills the window in the compact layout.
  pub fn compact_pane(&self) -> CompactPane {
    compact_pane_for(self.view.get(), self.panel.get())
  }

  /// One level up in the compact layout, where there is no history entry to go back to: from a
  /// side panel to its conversation, from a conversation to the list it is in, from anything else
  /// to Home.
  pub fn go_up(&self) {
    if self.panel.get_untracked() != SidePanel::Closed {
      self.panel.set(SidePanel::Closed);
      return;
    }
    let maybe_space = match self.view.get_untracked() {
      MainView::Conversation(id) => self
        .read(|backend| backend.conversation(id))
        .and_then(|conversation| conversation.maybe_space),
      MainView::ChannelPreview(space, _) => Some(space),
      _ => None,
    };
    match maybe_space {
      Some(space) => self.open_space(space),
      None => self.open_chats(),
    }
  }

  /// Follows a layout change: a space's list has no page of its own in the wider layouts.
  pub fn layout_changed(&self, layout: Layout) {
    if !layout.is_compact() && self.view.get_untracked() == MainView::Space {
      self.open_first_conversation_of(self.space.get_untracked());
    }
  }

  fn show_space_list(&self) {
    self.view.set(MainView::Space);
    self.panel.set(SidePanel::Closed);
  }

  /// Opens the settings dialog: on phones its list of sections, otherwise the first section.
  pub fn open_settings(&self) {
    self.show_settings(SettingsPage::Sections);
  }

  /// Opens the settings dialog on one section.
  pub fn open_settings_at(&self, tab: SettingsTab) {
    self.show_settings(SettingsPage::Section(tab));
  }

  /// The list of sections is a page of its own only on phones; elsewhere it stands beside the
  /// first section.
  fn show_settings(&self, page: SettingsPage) {
    let page = match page {
      SettingsPage::Sections if !self.layout.get_untracked().is_compact() => {
        SettingsPage::Section(SettingsTab::DEFAULT)
      },
      other => other,
    };
    self.settings_page.set(page);
    self.overlay.set(Overlay::Settings);
  }

  pub fn close_overlay(&self) {
    self.overlay.set(Overlay::None);
  }

  /// All conversations of the current sidebar in its order: the DMs on Home,
  /// otherwise the channels of the current space (of every space in the grouped sidebar).
  pub fn navigable_conversations(&self) -> Vec<Conversation> {
    if self.rail.get() == RailEntry::Chats {
      return self.direct_list();
    }
    let space = self.space.get();
    let is_grouped = self
      .settings
      .prefs
      .with(|prefs| prefs.sidebar.is_grouped_by_space);
    let options = self.settings.sidebar_options();
    let all = self.sidebar_list(self.read(|backend| {
      if is_grouped {
        backend
          .spaces()
          .into_iter()
          .flat_map(|space| backend.space_conversations(space.id))
          .collect()
      } else {
        backend.space_conversations(space)
      }
    }));
    build_sections(&all, &options)
      .into_iter()
      .flat_map(|section| section.conversations)
      .collect()
  }

  /// Moves to the previous (-1) or next (+1) conversation in the sidebar.
  pub fn step_conversation(&self, direction: i32) {
    let list = self.navigable_conversations();
    let Some(current) = self.current_conversation_id() else {
      return;
    };
    let Some(position) = list.iter().position(|conversation| conversation.id == current) else {
      return;
    };
    let target = position as i32 + direction;
    if let Some(next) = usize::try_from(target).ok().and_then(|index| list.get(index)) {
      self.open_conversation(next.id);
    }
  }

  /// The route of what the app shows now; the address bar follows it (`history.rs`).
  pub fn route(&self) -> Route {
    if self.overlay.get() == Overlay::Settings {
      return Route::Settings(self.settings_page.get());
    }
    match self.view.get() {
      MainView::Conversation(id) => self.conversation_route(id),
      MainView::Threads => Route::Threads,
      MainView::Admin(section) => Route::Admin(section),
      MainView::Search => Route::Search,
      MainView::Saved => Route::Saved,
      MainView::Activity => Route::Activity,
      MainView::Calls => Route::Calls,
      MainView::Meetings => Route::Meetings,
      MainView::Directory => Route::People,
      MainView::Home => Route::Home,
      MainView::Space => Route::Space(self.space.get()),
      // Not joined, so not a channel route yet: a reload lands in the space.
      MainView::ChannelPreview(space, _) => Route::Space(space),
      MainView::GettingStarted => {
        let space = self.space.get();
        let is_known_space = self.rail.get() == RailEntry::Space
          && self.read(|backend| backend.spaces().iter().any(|candidate| candidate.id == space));
        if is_known_space {
          Route::Space(space)
        } else {
          Route::Start
        }
      },
    }
  }

  fn conversation_route(&self, id: ConversationId) -> Route {
    let panel = self.panel.get();
    if let Some(linked) = self.linked_message.get()
      && linked.conversation == id
      && linked.maybe_thread.is_none_or(|thread| {
        panel
          == SidePanel::Thread {
            conversation: id,
            thread,
          }
      })
    {
      return Route::Message {
        conversation: id,
        message: linked.message,
        maybe_thread: linked.maybe_thread,
      };
    }
    if let SidePanel::Thread { conversation, thread } = panel
      && conversation == id
    {
      return Route::Thread { conversation, thread };
    }
    match self.read(|backend| backend.conversation(id)) {
      Some(conversation) => match conversation.maybe_space {
        Some(space) => Route::Channel { space, channel: id },
        None if conversation.kind.is_direct_like() => Route::Direct(id),
        None => Route::Conversation(id),
      },
      None => Route::Conversation(id),
    }
  }

  /// Shows the place a URL names. Something that hasn't loaded yet is waited for while nothing has
  /// loaded at all; anything else that can't be opened falls back to Home with a notice.
  pub fn follow_route(&self, route: Route) -> RouteOutcome {
    // Called from outside reactive scopes (start, popstate, data arriving), like event handlers.
    untrack(|| self.follow_route_now(route))
  }

  fn follow_route_now(&self, route: Route) -> RouteOutcome {
    let before = self.route_untracked();
    let outcome = match self.open_route(route) {
      RouteOutcome::Shown => RouteOutcome::Shown,
      RouteOutcome::Waiting | RouteOutcome::Unavailable if self.has_loaded_data() => {
        self.show_unavailable_route();
        RouteOutcome::Unavailable
      },
      RouteOutcome::Waiting | RouteOutcome::Unavailable => RouteOutcome::Waiting,
    };
    let maybe_pending = (outcome == RouteOutcome::Waiting).then_some(route);
    if self.pending_route.get_untracked() != maybe_pending {
      self.pending_route.set(maybe_pending);
    }
    if self.route_untracked() != before {
      self.follows_url.set_value(true);
    }
    outcome
  }

  /// Stops waiting for `route` if it never loaded: Home with a notice.
  pub fn give_up_route(&self, route: Route) {
    // Called by a timer, which may fire after the shell is gone.
    if self.pending_route.try_get_untracked() != Some(Some(route)) {
      return;
    }
    self.pending_route.set(None);
    self.reject_route();
  }

  /// A URL that is no route at all, or one that can't be shown: Home with a notice.
  pub fn reject_route(&self) {
    self.show_unavailable_route();
    self.follows_url.set_value(true);
  }

  /// Forgets a route that was still waiting, because the user went somewhere else meanwhile.
  pub fn cancel_pending_route(&self) {
    if self.pending_route.get_untracked().is_some() {
      self.pending_route.set(None);
    }
  }

  /// Whether the last changes came from following a URL; reading it resets it.
  pub fn take_follows_url(&self) -> bool {
    let follows = self.follows_url.get_value();
    self.follows_url.set_value(false);
    follows
  }

  /// [`AppState::route`] without subscribing the caller.
  pub fn route_untracked(&self) -> Route {
    untrack(|| self.route())
  }

  /// Opens the place of a route. `Waiting` means it names something this device doesn't know (yet).
  fn open_route(&self, route: Route) -> RouteOutcome {
    if !matches!(route, Route::Settings(_)) && self.overlay.get_untracked() == Overlay::Settings {
      self.overlay.set(Overlay::None);
    }
    match route {
      Route::Start => self.open_start(),
      Route::Home => self.open_chats(),
      Route::Space(space) => {
        if !self.read(|backend| backend.spaces().iter().any(|candidate| candidate.id == space)) {
          // The link of an open space the user isn't in yet: offer to join it.
          if self.read(|backend| {
            backend
              .joinable_spaces()
              .iter()
              .any(|candidate| candidate.id == space)
          }) {
            self.open_start();
            self.overlay.set(Overlay::BrowseSpaces(Some(space)));
            return RouteOutcome::Shown;
          }
          return RouteOutcome::Waiting;
        }
        self.space.set(space);
        self.rail.set(RailEntry::Space);
        if self.is_compact() {
          self.show_space_list();
        } else {
          self.open_first_conversation_of(space);
        }
      },
      Route::Channel { channel: id, .. } | Route::Conversation(id) | Route::Direct(id) => {
        if !self.is_known_conversation(id) {
          return RouteOutcome::Waiting;
        }
        self.open_conversation(id);
        self.panel.set(SidePanel::Closed);
      },
      Route::Thread { conversation, thread } => {
        if !self.is_known_conversation(conversation) {
          return RouteOutcome::Waiting;
        }
        self.open_conversation(conversation);
        self.open_thread(conversation, thread);
      },
      Route::Message {
        conversation,
        message,
        maybe_thread,
      } => {
        if !self.is_known_conversation(conversation) {
          return RouteOutcome::Waiting;
        }
        if maybe_thread.is_none() {
          self.panel.set(SidePanel::Closed);
        }
        self.open_message(conversation, message, maybe_thread);
      },
      Route::Threads => self.open_threads(),
      Route::Activity => self.open_activity(),
      Route::Calls => self.open_calls(),
      Route::Meetings => self.open_meetings(),
      Route::Meeting(meeting) => {
        self.open_meetings();
        self.maybe_meeting_link.set(Some(meeting));
      },
      Route::People => self.open_directory(),
      Route::Saved => self.open_saved(),
      Route::Search => self.open_search(None),
      Route::Settings(page) => self.show_settings(page),
      Route::Admin(section) => {
        if !self.read(|backend| backend.my_role()).includes(&Role::Admin) {
          return RouteOutcome::Unavailable;
        }
        self.open_admin(section);
      },
    }
    RouteOutcome::Shown
  }

  /// Where the app starts: the first channel, else Home with the welcome page (as
  /// [`start_position`] does on startup).
  fn open_start(&self) {
    if self.is_compact() {
      self.open_chats();
      return;
    }
    let first = self.read(|backend| {
      let space = backend.spaces().into_iter().next()?;
      let conversation = backend.space_conversations(space.id).into_iter().next()?;
      Some(conversation.id)
    });
    match first {
      Some(conversation) => self.open_conversation(conversation),
      None => {
        let (_, rail, view) = start_position(None);
        self.rail.set(rail);
        self.view.set(view);
        self.panel.set(SidePanel::Closed);
      },
    }
  }

  fn show_unavailable_route(&self) {
    self.open_chats();
    self.unavailable_routes.update(|count| *count += 1);
  }

  fn is_known_conversation(&self, id: ConversationId) -> bool {
    self.read(|backend| backend.conversation(id)).is_some()
  }

  /// Whether any spaces or DMs have arrived: from then on, a missing ID is not just late.
  fn has_loaded_data(&self) -> bool {
    self.read(|backend| !backend.spaces().is_empty() || !backend.direct_conversations().is_empty())
  }
}

pub fn use_app_state() -> AppState {
  expect_context::<AppState>()
}

impl SettingsTab {
  /// In the order of the section list.
  pub const ALL: [SettingsTab; 9] = [
    Self::Account,
    Self::Appearance,
    Self::Notifications,
    Self::Preferences,
    Self::Keyboard,
    Self::AudioVideo,
    Self::Devices,
    Self::Security,
    Self::About,
  ];
  /// The section the dialog opens with beside the list.
  pub const DEFAULT: SettingsTab = Self::Account;

  pub fn slug(self) -> &'static str {
    match self {
      Self::Account => "account",
      Self::Appearance => "appearance",
      Self::Notifications => "notifications",
      Self::Preferences => "preferences",
      Self::Keyboard => "keyboard",
      Self::AudioVideo => "audio-video",
      Self::Devices => "devices",
      Self::Security => "security",
      Self::About => "about",
    }
  }

  /// The section of a path segment, including the tabs before #327, so old links keep working.
  pub fn from_slug(slug: &str) -> Option<Self> {
    let legacy = match slug {
      "display" => Some(Self::Appearance),
      "sidebar" | "language" => Some(Self::Preferences),
      "privacy" => Some(Self::Security),
      "calls" => Some(Self::AudioVideo),
      "advanced" => Some(Self::About),
      _ => None,
    };
    legacy.or_else(|| Self::ALL.into_iter().find(|tab| tab.slug() == slug))
  }
}

/// Whether `members` are exactly `users` plus (optionally) `me`.
fn has_exactly(members: &[UserId], users: &[UserId], me: UserId) -> bool {
  users.iter().all(|user| members.contains(user))
    && members
      .iter()
      .all(|member| *member == me || users.contains(member))
}

/// Whether the keyboard focus is on a toast (native builds have no focus).
fn is_focus_in_toasts() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.active_element())
      .and_then(|element| element.closest(".toasts").ok().flatten())
      .is_some()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    false
  }
}

/// Adds a toast and closes the oldest beyond [`MAX_TOASTS`]. Toasts with an Undo stay: closing
/// one would take away the only way back.
fn push_capped(list: &mut Vec<Toast>, toast: Toast) {
  list.push(toast);
  while list.len() > MAX_TOASTS {
    let Some(oldest) = list.iter().position(|toast| toast.maybe_undo.is_none()) else {
      break;
    };
    list.remove(oldest);
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::mock::MockBackend;
  use crate::model::thread_id_of;
  use crate::route::HistoryStep;

  const NOW: TimestampMs = 1_792_584_000_000;

  #[test]
  fn without_a_channel_the_app_starts_on_home_with_the_welcome_page() {
    let (_, rail, view) = start_position(None);
    assert_eq!(rail, RailEntry::Chats);
    assert_eq!(view, MainView::GettingStarted);
  }

  #[test]
  fn with_a_channel_the_app_starts_in_its_space() {
    let space: SpaceId = id_from_raw(7);
    let conversation: ConversationId = id_from_raw(9);
    assert_eq!(
      start_position(Some((space, conversation))),
      (space, RailEntry::Space, MainView::Conversation(conversation))
    );
  }

  #[test]
  fn home_without_dms_welcomes_an_empty_workspace() {
    assert_eq!(home_view_without_dms(false), MainView::GettingStarted);
    assert_eq!(home_view_without_dms(true), MainView::Home);
  }

  #[test]
  fn the_remembered_conversation_reopens_only_while_it_exists() {
    let (kept, gone): (ConversationId, ConversationId) = (id_from_raw(1), id_from_raw(2));
    assert_eq!(
      remembered_conversation([kept, id_from_raw(3)], Some(kept)),
      Some(kept)
    );
    assert_eq!(remembered_conversation([kept], Some(gone)), None);
    assert_eq!(remembered_conversation([kept], None), None);
  }

  mod state_to_route {
    use super::*;

    #[test]
    fn a_channel_is_named_with_its_space() {
      let state = test_state();
      let (space, channel) = first_channel(&state);
      state.open_conversation(channel);
      assert_eq!(state.route(), Route::Channel { space, channel });
    }

    #[test]
    fn a_dm_has_its_own_path() {
      let state = test_state();
      let dm = first_dm(&state);
      state.open_conversation(dm);
      assert_eq!(state.route(), Route::Direct(dm));
    }

    #[test]
    fn an_open_thread_is_part_of_the_route() {
      let state = test_state();
      let (_, channel) = first_channel(&state);
      let thread = thread_id_of(id_from_raw(42));
      state.open_conversation(channel);
      state.open_thread(channel, thread);
      assert_eq!(
        state.route(),
        Route::Thread {
          conversation: channel,
          thread
        }
      );
      state.panel.set(SidePanel::Closed);
      assert!(matches!(state.route(), Route::Channel { .. }));
    }

    #[test]
    fn pages_and_the_settings_dialog_have_routes() {
      let state = test_state();
      state.open_directory();
      assert_eq!(state.route(), Route::People);
      state.open_admin(AdminSection::Audit);
      assert_eq!(state.route(), Route::Admin(AdminSection::Audit));
      state.open_settings();
      assert_eq!(
        state.route(),
        Route::Settings(SettingsPage::Section(SettingsTab::DEFAULT))
      );
      state
        .settings_page
        .set(SettingsPage::Section(SettingsTab::AudioVideo));
      assert_eq!(
        state.route(),
        Route::Settings(SettingsPage::Section(SettingsTab::AudioVideo))
      );
      state.close_overlay();
      assert_eq!(state.route(), Route::Admin(AdminSection::Audit));
    }

    #[test]
    fn the_search_route_never_carries_the_query() {
      let state = test_state();
      state.open_search(Some("in:#general secret plans"));
      assert_eq!(state.route(), Route::Search);
      assert_eq!(state.route().path(), "/search");
    }
  }

  mod route_to_state {
    use super::*;

    #[test]
    fn every_page_route_opens_its_page_and_reads_back() {
      let state = test_state();
      for route in [
        Route::Threads,
        Route::Activity,
        Route::Calls,
        Route::Meetings,
        Route::People,
        Route::Saved,
        Route::Search,
        Route::Settings(SettingsPage::Section(SettingsTab::Security)),
        Route::Admin(AdminSection::Users),
      ] {
        assert_eq!(state.follow_route(route), RouteOutcome::Shown, "{route:?}");
        assert_eq!(state.route(), route);
      }
    }

    #[test]
    fn a_meeting_link_opens_the_meetings_screen_and_hands_the_meeting_over() {
      let state = test_state();
      let meeting: MeetingId = crate::model::id_from_raw(5_000_005);
      assert_eq!(state.follow_route(Route::Meeting(meeting)), RouteOutcome::Shown);
      assert_eq!(state.route(), Route::Meetings);
      assert_eq!(state.maybe_meeting_link.get_untracked(), Some(meeting));
    }

    #[test]
    fn a_conversation_route_opens_it_in_its_canonical_spelling() {
      let state = test_state();
      let (space, channel) = first_channel(&state);
      state.open_threads();
      assert_eq!(
        state.follow_route(Route::Conversation(channel)),
        RouteOutcome::Shown
      );
      assert_eq!(state.route(), Route::Channel { space, channel });
      assert!(state.take_follows_url(), "the URL is replaced, not pushed");
      assert!(!state.take_follows_url(), "taking it resets it");
    }

    #[test]
    fn a_thread_route_opens_the_thread() {
      let state = test_state();
      let (_, channel) = first_channel(&state);
      let route = Route::Thread {
        conversation: channel,
        thread: thread_id_of(id_from_raw(42)),
      };
      assert_eq!(state.follow_route(route), RouteOutcome::Shown);
      assert_eq!(state.route(), route);
    }

    #[test]
    fn leaving_settings_by_route_closes_the_dialog() {
      let state = test_state();
      let dm = first_dm(&state);
      state.follow_route(Route::Settings(SettingsPage::Section(SettingsTab::Appearance)));
      assert_eq!(state.overlay.get(), Overlay::Settings);
      state.follow_route(Route::Direct(dm));
      assert_eq!(state.overlay.get(), Overlay::None);
      assert_eq!(state.route(), Route::Direct(dm));
    }

    #[test]
    fn the_link_of_an_open_space_offers_to_join_it() {
      let state = test_state();
      let open = state
        .read(|backend| backend.joinable_spaces())
        .first()
        .map(|space| space.id)
        .expect("demo data has an open space to join");
      assert_eq!(state.follow_route(Route::Space(open)), RouteOutcome::Shown);
      assert_eq!(state.overlay.get_untracked(), Overlay::BrowseSpaces(Some(open)));
    }

    #[test]
    fn a_space_route_lands_in_its_first_channel() {
      let state = test_state();
      let spaces = state.read(|backend| backend.spaces());
      let second = spaces
        .get(1)
        .map(|space| space.id)
        .expect("demo data has two spaces");
      let first_channel = state.read(|backend| backend.space_conversations(second))[0].id;
      state.open_threads();
      assert_eq!(state.follow_route(Route::Space(second)), RouteOutcome::Shown);
      assert_eq!(
        state.route(),
        Route::Channel {
          space: second,
          channel: first_channel
        }
      );
    }

    #[test]
    fn an_unknown_id_falls_back_to_home_with_a_notice() {
      let state = test_state();
      let unknown: ConversationId = id_from_raw(999_999_999);
      for route in [
        Route::Conversation(unknown),
        Route::Direct(unknown),
        Route::Space(id_from_raw(999_999_999)),
        Route::Thread {
          conversation: unknown,
          thread: thread_id_of(id_from_raw(1)),
        },
      ] {
        let before = state.unavailable_routes.get();
        state.open_threads();
        assert_eq!(state.follow_route(route), RouteOutcome::Unavailable, "{route:?}");
        assert_eq!(state.unavailable_routes.get(), before + 1, "{route:?}");
        assert_eq!(state.rail.get(), RailEntry::Chats, "{route:?}");
        assert_eq!(
          state.pending_route.get(),
          None,
          "loaded data: nothing to wait for"
        );
      }
    }

    #[test]
    fn a_rejected_url_shows_home_with_a_notice() {
      let state = test_state();
      state.reject_route();
      assert_eq!(state.unavailable_routes.get(), 1);
      assert_eq!(state.rail.get(), RailEntry::Chats);
      assert!(state.take_follows_url());
    }

    #[test]
    fn giving_up_only_drops_the_route_still_waited_for() {
      let state = test_state();
      let waited: Route = Route::Direct(id_from_raw(5));
      state.give_up_route(waited);
      assert_eq!(state.unavailable_routes.get(), 0, "nothing was waiting");
      state.pending_route.set(Some(waited));
      state.give_up_route(Route::Threads);
      assert_eq!(state.pending_route.get(), Some(waited));
      state.give_up_route(waited);
      assert_eq!(state.pending_route.get(), None);
      assert_eq!(state.unavailable_routes.get(), 1);
    }

    #[test]
    fn the_start_route_opens_the_first_channel() {
      let state = test_state();
      let (space, channel) = first_channel(&state);
      state.open_threads();
      assert_eq!(state.follow_route(Route::Start), RouteOutcome::Shown);
      assert_eq!(state.route(), Route::Channel { space, channel });
    }

    #[test]
    fn the_start_route_without_a_channel_opens_home_with_the_welcome_page() {
      let backend: Rc<dyn Backend> = Rc::new(MockBackend::empty_workspace(NOW));
      let state = AppState::new(backend, Settings::for_tests(), None);
      state.open_threads();
      state.rail.set(RailEntry::Space);
      assert_eq!(state.follow_route(Route::Start), RouteOutcome::Shown);
      assert_eq!(state.rail.get(), RailEntry::Chats);
      assert_eq!(state.view.get(), MainView::GettingStarted);
      assert_eq!(state.route(), Route::Start);
    }

    #[test]
    fn the_first_conversation_after_startup_data_replaces_the_start_entry() {
      let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(NOW));
      // Started before any data had arrived: the getting-started page under `/`.
      let state = AppState::new(backend, Settings::for_tests(), None);
      assert_eq!(state.route(), Route::Start);
      assert!(!state.take_follows_url());
      state.backend_changed();
      let (space, channel) = first_channel(&state);
      let next = state.route();
      assert_eq!(next, Route::Channel { space, channel });
      let is_redirect = state.take_follows_url();
      assert!(is_redirect, "the URL is replaced, not pushed");
      assert_eq!(
        HistoryStep::between(&Route::Start, &next, is_redirect),
        HistoryStep::Replace
      );
    }

    #[test]
    fn data_arriving_later_leaves_the_history_alone() {
      let state = test_state();
      state.open_threads();
      assert!(!state.take_follows_url());
      state.backend_changed();
      assert_eq!(state.route(), Route::Threads);
      assert!(!state.take_follows_url(), "nothing was redirected");
    }
  }

  /// Issue #322: the core answers, a timer fires or the backend reports a change after the shell
  /// was unmounted (sign-out, a new session). Nothing may panic then.
  mod after_the_shell_is_gone {
    use super::*;

    #[test]
    fn callbacks_from_outside_the_reactive_tree_do_nothing() {
      let owner = Owner::new();
      let state = owner.with(test_state);
      owner.cleanup();
      state.backend_changed();
      state.show_error("refused".into(), "detail".into());
      state.dismiss_toast(0);
      assert!(state.try_write(|backend| backend.spaces()).is_none());
    }

    #[test]
    fn while_the_shell_lives_try_write_writes() {
      let owner = Owner::new();
      owner.with(|| {
        let state = test_state();
        assert!(state.try_write(|backend| backend.spaces()).is_some());
      });
    }
  }

  mod error_toasts {
    use super::*;

    #[test]
    fn an_error_shows_with_its_detail_and_a_repeat_shows_once() {
      let state = test_state();
      state.show_error("refused".into(), "detail".into());
      state.show_error("refused".into(), "detail".into());
      let toasts = state.toasts.get_untracked();
      assert_eq!(toasts.len(), 1);
      assert_eq!(
        toasts[0].tone,
        ToastTone::Error {
          detail: "detail".into()
        }
      );
      state.show_error("refused".into(), "other detail".into());
      assert_eq!(state.toasts.get_untracked().len(), 2);
    }

    #[test]
    fn a_repeat_of_any_shown_error_shows_once() {
      let state = test_state();
      state.show_error("one".into(), "a".into());
      state.show_error("two".into(), "b".into());
      state.show_error("one".into(), "a".into());
      assert_eq!(state.toasts.get_untracked().len(), 2);
    }

    /// #367: an Undo toast closed before its countdown ran out commits at once (the message leaves
    /// the hidden set because its deletion went out).
    #[test]
    fn closing_an_undo_toast_commits_at_once() {
      let state = test_state();
      let (_, conversation) = first_channel(&state);
      let message = id_from_raw(1);
      state.delete_with_undo(conversation, message, "deleted".into());
      assert!(state.hidden_messages.get_untracked().contains(&message));
      state.dismiss_toast(state.toasts.get_untracked()[0].id);
      assert!(state.toasts.get_untracked().is_empty());
      assert!(!state.hidden_messages.get_untracked().contains(&message));
    }

    #[test]
    fn toasts_close_when_their_countdown_ran_out_but_not_while_held() {
      let state = test_state();
      let (_, conversation) = first_channel(&state);
      let message = id_from_raw(1);
      let now = crate::clock::now_ms();
      state.show_brief_toast("Link copied".into());
      state.delete_with_undo(conversation, message, "deleted".into());
      state.hold_toasts(Hold::Pointer, true);
      state.wake_toasts_at(now + 60_000);
      assert_eq!(state.toasts.get_untracked().len(), 2, "held under the pointer");
      state.hold_toasts(Hold::Pointer, false);
      state.wake_toasts_at(now + BRIEF_TOAST.as_millis() as i64);
      assert_eq!(state.toasts.get_untracked().len(), 1, "the brief toast closed");
      state.wake_toasts_at(now + UNDO_WINDOW.as_millis() as i64);
      assert!(state.toasts.get_untracked().is_empty());
      assert!(
        !state.hidden_messages.get_untracked().contains(&message),
        "the deletion went out"
      );
    }

    /// Review of #387: closing the focused toast moves the focus away without a `focusout`; the
    /// other toast's countdown must go on.
    #[test]
    fn closing_the_focused_toast_releases_the_focus_hold() {
      let state = test_state();
      let now = crate::clock::now_ms();
      state.show_brief_toast("one".into());
      state.show_brief_toast("two".into());
      let first = state.toasts.get_untracked()[0].id;
      state.hold_toasts(Hold::Focus, true);
      state.dismiss_toast(first);
      state.wake_toasts_at(now + BRIEF_TOAST.as_millis() as i64);
      assert!(
        state.toasts.get_untracked().is_empty(),
        "the second toast closed on time"
      );
    }

    #[test]
    fn leaving_the_page_commits_what_undo_toasts_hold_back() {
      let state = test_state();
      let (_, conversation) = first_channel(&state);
      let message = id_from_raw(1);
      state.delete_with_undo(conversation, message, "deleted".into());
      state.show_brief_toast("Link copied".into());
      state.end_timed_toasts();
      assert!(state.toasts.get_untracked().is_empty());
      assert!(
        !state.hidden_messages.get_untracked().contains(&message),
        "the deletion went out"
      );
    }

    #[test]
    fn the_oldest_toast_closes_beyond_the_cap_but_undo_toasts_stay() {
      let state = test_state();
      state.push_toast("deleted".into(), Some(Undo::RestoreMessage(id_from_raw(1))));
      for index in 0..10 {
        state.show_error(format!("error {index}"), String::new());
      }
      let toasts = state.toasts.get_untracked();
      assert_eq!(toasts.len(), MAX_TOASTS);
      assert!(toasts[0].maybe_undo.is_some(), "the undo toast stays");
      assert_eq!(toasts.last().map(|toast| toast.text.as_str()), Some("error 9"));
    }
  }

  mod compact {
    use super::*;

    /// #368: what slides which way depends on how deep the page is.
    #[test]
    fn lists_and_activity_are_top_level_a_conversation_a_page_a_thread_a_panel() {
      let state = compact_state();
      let (space, conversation) = first_channel(&state);
      state.open_space(space);
      assert_eq!(state.compact_depth(), CompactDepth::TopLevel);
      state.open_activity();
      assert_eq!(state.compact_depth(), CompactDepth::TopLevel);
      state.open_conversation(conversation);
      assert_eq!(state.compact_depth(), CompactDepth::Page);
      state.panel.set(SidePanel::Pins);
      assert_eq!(state.compact_depth(), CompactDepth::Panel);
    }

    #[test]
    fn home_and_a_space_show_their_list_instead_of_a_conversation() {
      let state = compact_state();
      let (space, _) = first_channel(&state);
      state.open_chats();
      assert_eq!(state.view.get(), MainView::Home);
      assert_eq!(state.compact_pane(), CompactPane::List);
      assert_eq!(state.route(), Route::Home);
      state.open_space(space);
      assert_eq!(state.view.get(), MainView::Space);
      assert_eq!(state.compact_pane(), CompactPane::List);
      assert_eq!(state.route(), Route::Space(space));
    }

    #[test]
    fn the_space_route_reads_back_as_the_spaces_list() {
      let state = compact_state();
      let (space, channel) = first_channel(&state);
      state.open_conversation(channel);
      assert_eq!(state.follow_route(Route::Space(space)), RouteOutcome::Shown);
      assert_eq!(state.view.get(), MainView::Space);
      assert_eq!(state.route(), Route::Space(space));
    }

    #[test]
    fn the_start_route_opens_homes_list() {
      let state = compact_state();
      assert_eq!(state.follow_route(Route::Start), RouteOutcome::Shown);
      assert_eq!(state.route(), Route::Home);
    }

    #[test]
    fn a_thread_fills_the_window_over_its_conversation() {
      let state = compact_state();
      let (_, channel) = first_channel(&state);
      state.open_conversation(channel);
      assert_eq!(state.compact_pane(), CompactPane::Main);
      state.open_thread(channel, thread_id_of(id_from_raw(42)));
      assert_eq!(state.compact_pane(), CompactPane::Panel);
    }

    #[test]
    fn up_goes_from_the_thread_to_the_conversation_to_its_list() {
      let state = compact_state();
      let (space, channel) = first_channel(&state);
      state.open_conversation(channel);
      state.open_thread(channel, thread_id_of(id_from_raw(42)));
      state.go_up();
      assert_eq!(state.view.get(), MainView::Conversation(channel));
      assert_eq!(state.panel.get(), SidePanel::Closed);
      state.go_up();
      assert_eq!(state.route(), Route::Space(space));
      let dm = first_dm(&state);
      state.open_conversation(dm);
      state.go_up();
      assert_eq!(state.route(), Route::Home);
    }

    #[test]
    fn a_wider_window_opens_the_first_channel_of_the_listed_space() {
      let state = compact_state();
      let (space, channel) = first_channel(&state);
      state.open_space(space);
      state.layout.set(Layout::Medium);
      state.layout_changed(Layout::Medium);
      assert_eq!(state.route(), Route::Channel { space, channel });
    }

    #[test]
    fn startup_data_lands_on_homes_list() {
      let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(NOW));
      let state = AppState::new(backend, Settings::for_tests(), None);
      state.layout.set(Layout::Compact);
      state.backend_changed();
      assert_eq!(state.route(), Route::Home);
    }

    #[test]
    fn settings_open_on_their_section_list_on_phones_only() {
      let state = compact_state();
      state.open_settings();
      assert_eq!(state.route(), Route::Settings(SettingsPage::Sections));
      state.close_overlay();
      state.layout.set(Layout::Medium);
      state.open_settings();
      assert_eq!(
        state.route(),
        Route::Settings(SettingsPage::Section(SettingsTab::DEFAULT))
      );
      state.follow_route(Route::Settings(SettingsPage::Sections));
      assert_eq!(
        state.settings_page.get(),
        SettingsPage::Section(SettingsTab::DEFAULT)
      );
    }

    fn compact_state() -> AppState {
      let state = test_state();
      state.layout.set(Layout::Compact);
      state
    }
  }

  mod unread_divider {
    use super::*;

    #[test]
    fn sending_removes_the_divider() {
      let state = test_state();
      let unread = unread_conversation(&state);
      state.open_conversation(unread);
      place_divider(&state, unread);
      state.divider_event(unread, DividerEvent::Sent);
      assert_eq!(state.unread_marker.get_untracked(), None);
    }

    #[test]
    fn sending_reads_what_came_before() {
      let state = test_state();
      let unread = unread_conversation(&state);
      // Unread although open: a message that arrived while the user read along.
      state.mark_read_on_send(unread);
      let after = state
        .read(|backend| backend.conversation(unread))
        .expect("still there");
      assert!(!after.has_unread());
    }

    #[test]
    fn coming_back_with_nothing_new_shows_no_divider() {
      let state = test_state();
      let unread = unread_conversation(&state);
      state.open_conversation(unread);
      place_divider(&state, unread);
      state.open_conversation(first_dm(&state));
      assert!(
        state
          .unread_marker
          .get_untracked()
          .is_none_or(|marker| marker.conversation != unread)
      );
      // Opening marked it read: nothing is new now.
      state.open_conversation(unread);
      assert_eq!(state.unread_marker.get_untracked(), None);
    }

    #[test]
    fn esc_marks_read_and_removes_the_divider() {
      let state = test_state();
      let unread = unread_conversation(&state);
      place_divider(&state, unread);
      state.mark_conversation_read(unread);
      assert_eq!(state.unread_marker.get_untracked(), None);
      let after = state
        .read(|backend| backend.conversation(unread))
        .expect("still there");
      assert_eq!(after.unread_count, 0);
    }

    #[test]
    fn only_a_real_conversation_change_counts_as_leaving() {
      let state = test_state();
      let unread = unread_conversation(&state);
      state.open_conversation(unread);
      place_divider(&state, unread);
      // Opening the same conversation again (a remount, a route written another way) keeps it.
      state.open_conversation(unread);
      assert_eq!(
        state
          .unread_marker
          .get_untracked()
          .map(|marker| marker.conversation),
        Some(unread)
      );
      state.open_conversation(first_dm(&state));
      assert!(
        state
          .unread_marker
          .get_untracked()
          .is_none_or(|marker| marker.conversation != unread)
      );
    }

    #[test]
    fn the_divider_never_moves_the_read_marker_back() {
      let state = test_state();
      let unread = unread_conversation(&state);
      state.mark_conversation_read(unread);
      // News while away places a divider; that is view state only, nothing becomes unread again.
      state.divider_event(
        unread,
        DividerEvent::ArrivedWhileAway {
          first_new: 2,
          is_own: false,
          has_seen_divider: true,
        },
      );
      assert_eq!(
        state.unread_marker.get_untracked().map(|marker| marker.index),
        Some(2)
      );
      let after = state
        .read(|backend| backend.conversation(unread))
        .expect("still there");
      assert_eq!(after.unread_count, 0);
      state.divider_event(unread, DividerEvent::DwelledAtBottom);
      assert_eq!(state.unread_marker.get_untracked(), None);
    }

    fn place_divider(state: &AppState, conversation: ConversationId) {
      state.unread_marker.set(Some(UnreadMarker {
        conversation,
        index: 3,
      }));
    }

    fn unread_conversation(state: &AppState) -> ConversationId {
      state
        .read(|backend| {
          let mut all: Vec<Conversation> = backend
            .spaces()
            .into_iter()
            .flat_map(|space| backend.space_conversations(space.id))
            .collect();
          all.extend(backend.direct_conversations());
          all
        })
        .into_iter()
        .find(|conversation| conversation.unread_count > 0)
        .map(|conversation| conversation.id)
        .expect("demo data has unread messages")
    }
  }

  fn test_state() -> AppState {
    let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(NOW));
    let start = first_of(backend.as_ref());
    AppState::new(backend, Settings::for_tests(), start)
  }

  fn first_of(backend: &dyn Backend) -> Option<(SpaceId, ConversationId)> {
    let space = backend.spaces().into_iter().next()?;
    let conversation = backend.space_conversations(space.id).into_iter().next()?;
    Some((space.id, conversation.id))
  }

  fn first_channel(state: &AppState) -> (SpaceId, ConversationId) {
    state
      .read(|backend| first_of(backend))
      .expect("demo data has a channel")
  }

  fn first_dm(state: &AppState) -> ConversationId {
    state
      .read(|backend| backend.direct_conversations())
      .into_iter()
      .find(|conversation| conversation.kind == crate::model::ConversationKind::Direct)
      .map(|conversation| conversation.id)
      .expect("demo data has a DM")
  }
}
