//! Areas of the app in wasm chunks of their own (#334): the code an area alone uses (the
//! administration console, …) is split off the app and loads the first time the area opens, so the
//! first visit downloads less (robustness.md, "Download size"; docs/performance.md, "Web bundle
//! size"). Code several areas share goes into shared chunks; what the first screen needs stays in
//! the app.
//!
//! The signed-in app itself is one (`session`): a guest who opens a meeting link loads only the
//! guest pages, and `init.js` fetches the session's chunks (one file, their pack, #1465) next to
//! the app for every other page, so signed-in users don't wait for them one after the other.
//!
//! Each split point is a `wasm_split` function that builds the area's view; [`lazy_area`] calls it,
//! shows a placeholder while the chunk loads and, when it can't load (offline, a deploy in between),
//! says so with a retry instead of an empty view. The split itself is done by
//! `teamcraft-web-bundle` after the build; on other targets (tests, desktop shell tools) the
//! functions are called directly.

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local_scoped_with_cancellation;
use leptos::wasm_split::{SplitLoaderError, wasm_split};

use crate::admin::AdminSection;
use crate::admin_live::{AdminLive, AdminTask};
use crate::calls::CallRequest;
use crate::components::activity_view::{ActivityHome, ActivitySidebar};
use crate::components::add_to_call::AddToCallDialog;
use crate::components::admin_console::AdminConsole;
use crate::components::bookmarks::BookmarksDialog;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::calls_unavailable::CallsUnavailableDialog;
use crate::components::calls_view::CallsView;
use crate::components::canvas_view::CanvasView;
use crate::components::composer_toolbar::{ActiveFormats, FormatAction, FormatToolbarPanel};
use crate::components::conversation_notify::QuietHoursDialog;
use crate::components::date_jump::{DateJumpMenu, DateJumpPanel};
use crate::components::directory_view::DirectoryView;
use crate::components::downloads_menu::DownloadsButton;
use crate::components::drafts_view::DraftsView;
use crate::components::emoji_picker::EmojiPickerPanel;
use crate::components::file_preview::FilePreview;
use crate::components::files_sidebar::FilesSidebar;
use crate::components::files_view::FilesView;
use crate::components::guest_meeting::{GuestMeeting, GuestSession};
use crate::components::home::NewMessageDialog;
use crate::components::huddle_bar::{CallBar, HuddleBarContent};
use crate::components::lightbox::Lightbox;
use crate::components::link_request_prompt::{LinkRequestCards, Step as LinkStep};
use crate::components::log_view::IntegrationFeed;
use crate::components::login::{EnrollScreen, SessionGate};
use crate::components::meetings_view::{
  CancelMeetingDialog, MeetingCreatedDialog, MeetingsView, ScheduleMeetingDialog,
};
use crate::components::members_panel::MembersPanel;
use crate::components::message_actions::{EditHistoryDialog, ForwardDialog, PinsPanel, SavedView};
use crate::components::profile_views::UserProfileDialog;
use crate::components::quick_switcher::QuickSwitcher;
use crate::components::report_problem::ReportProblemDialog;
use crate::components::scheduled_view::ScheduledView;
use crate::components::search_view::SearchView;
use crate::components::security_ui::{
  DeviceApprovalDialog, LinkDeviceDialog, VerifyContactDialog, VerifyKeyDialog,
};
use crate::components::settings_modal::{SettingsModal, ShortcutsModal};
#[cfg(target_arch = "wasm32")]
use crate::components::share_download::ShareDownloadPage;
use crate::components::share_link_dialog::ShareLinkDialog;
use crate::components::space_invites::{AddToChannelDialog, BrowseSpacesDialog, InvitePeopleDialog};
use crate::components::status_menu::{CustomStatusDialog, StatusDurationDialog, StatusMenu};
use crate::components::tab_elsewhere::TabElsewhere;
use crate::components::update_notice::UpdateNoticeBanner;
use crate::components::video_player::VideoPlayer;
use crate::components::workspace_dialogs::{
  BrowseChannelsDialog, ChannelSettingsDialog, NewChannelDialog, NewSpaceDialog, SpaceSettingsDialog,
};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::model::{ConversationId, MessageId, SpaceId, UserId};
use crate::reactive::SetIfAlive;
use crate::session::SessionControl;
use crate::settings::Settings;
use crate::share_into::ShareDialog;
use crate::state::AppState;
use crate::tab_handover::TabState;
use teamcraft_client_api::app_link::{AppLink, AppLinkStep, next_step};
use teamcraft_client_api::remote::LinkRequestView;
use teamcraft_client_api::server_entry::ServerOrigin;
use teamcraft_proto::{AttachmentId, MeetingId};

/// Where an area shows: how its placeholder and its error sit in the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
  /// The main pane: the placeholder fills it.
  Pane,
  /// A dialog or a screen above the app: the placeholder floats, the error is a dialog that closes
  /// the overlay.
  Overlay,
  /// The whole window (the signed-in app, the call screens).
  Screen,
  /// The sidebar's place: the placeholder takes its box.
  Sidebar,
  /// A conversation's side panel: the placeholder takes its box.
  Panel,
  /// A part of a view (a bar, a toolbar, a prompt): the placeholder is a line in its place.
  Inline,
  /// A popover (a picker, a menu): the placeholder floats; the error leaves the app usable, and
  /// what opened the popover closes it.
  Popover,
}

/// Where an area stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
  Loading,
  Ready,
  Failed,
}

/// An area's view on its way: its chunk loading, then the view built.
type AreaLoad = Pin<Box<dyn Future<Output = Result<AnyView, SplitLoaderError>>>>;
/// What an area needs before its view is built, when that is more than its chunk.
type AreaFirst = Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<(), String>>>>>;

/// Declares split points: `name(args) => module: view` builds `view` in chunk `module`.
macro_rules! areas {
  ($( $(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) => $module:ident: $view:expr; )*) => {
    $(
      $(#[$doc])*
      #[wasm_split($module, wasm_split_path = ::leptos::wasm_split, fallible)]
      pub fn $name($($arg: $ty),*) -> Result<AnyView, SplitLoaderError> {
        Ok($view.into_any())
      }
    )*
  };
}

areas! {
  /// Sign-in and the signed-in app: everything but the guest pages.
  session(settings: Settings) => session: view! { <SessionGate settings=settings /> };
  /// Setting up this device: saving the recovery kit at first sign-in, or unlocking the keys
  /// (e2ee.md §6.5). A device sees it once, so it loads when needed.
  enroll_screen(
    name: String,
    has_existing_identity: bool,
    was_data_lost: bool,
    can_restore_on_device: bool,
    control: SessionControl
  ) => setup: view! {
    <EnrollScreen
      name=name
      has_existing_identity=has_existing_identity
      was_data_lost=was_data_lost
      can_restore_on_device=can_restore_on_device
      control=control
    />
  };
  /// The administration console: the largest part of the app, for admins only.
  admin_console(section: AdminSection) => admin: view! { <AdminConsole section=section /> };
  settings(_unit: ()) => settings: view! { <SettingsModal /> };
  /// The keyboard shortcuts, next to their settings (#695).
  shortcuts(_unit: ()) => settings: view! { <ShortcutsModal /> };
  lightbox(conversation: ConversationId, start: MediaKey) => lightbox: view! { <Lightbox conversation=conversation start=start /> };
  /// The app's own video player in a message row, once play is pressed (#638): a first visit
  /// doesn't need it (#695). The lightbox's chunk uses it as well.
  inline_video_player(src: String, video: NodeRef<html::Video>, label: String, on_unplayable: Callback<()>) => video: view! {
    <VideoPlayer src=src video=video label=label class="media-video-player" is_autoplay=true on_unplayable=on_unplayable />
  };
  /// The preview of Markdown, HTML, PDF and text files (#557), with the text and log viewer (#821).
  file_preview(conversation: ConversationId, key: MediaKey) => preview: view! { <FilePreview conversation=conversation key=key /> };
  /// Creating and configuring spaces and channels, and inviting people.
  new_space(_unit: ()) => workspace: view! { <NewSpaceDialog /> };
  new_channel(space: SpaceId) => workspace: view! { <NewChannelDialog space=space /> };
  browse_channels(space: SpaceId) => workspace: view! { <BrowseChannelsDialog space=space /> };
  channel_settings(conversation: ConversationId) => workspace: view! { <ChannelSettingsDialog conversation=conversation /> };
  space_settings(space: SpaceId) => workspace: view! { <SpaceSettingsDialog space=space /> };
  browse_spaces(maybe_highlight: Option<SpaceId>) => workspace: view! { <BrowseSpacesDialog maybe_highlight=maybe_highlight /> };
  invite_people(space: SpaceId) => workspace: view! { <InvitePeopleDialog space=space /> };
  add_to_channel(conversation: ConversationId) => workspace: view! { <AddToChannelDialog conversation=conversation /> };
  /// The call screens (the incoming call card stays in the app, so a call rings at once).
  call_prejoin(request: CallRequest) => calls: view! { <PreJoinScreen request=request /> };
  call_screen(request: CallRequest) => calls: view! { <CallScreen request=request /> };
  /// "Add to call" is a member's: in `dialogs`, not in the call screens' module a guest loads (#1413).
  add_to_call(conversation: ConversationId) => dialogs: view! { <AddToCallDialog conversation=conversation /> };
  /// The bar of a call going on in a conversation, once there is one: it leads to the call screens.
  huddle_bar(conversation: ConversationId) => calls: view! { <HuddleBarContent conversation=conversation /> };
  /// The bar of a minimized call above the app (#1141).
  call_bar(_unit: ()) => calls: view! { <CallBar /> };
  meetings(_unit: ()) => meetings: view! { <MeetingsView /> };
  schedule_meeting(_unit: ()) => meetings: view! { <ScheduleMeetingDialog /> };
  edit_meeting(meeting: MeetingId) => meetings: view! { <ScheduleMeetingDialog maybe_edit=meeting /> };
  cancel_meeting(meeting: MeetingId, occurrence: i64) => meetings: view! { <CancelMeetingDialog meeting=meeting occurrence=occurrence /> };
  meeting_created(_unit: ()) => meetings: view! { <MeetingCreatedDialog /> };
  /// What a guest sees after a host let them in: the meeting chat and its call. A module of its
  /// own, not the signed-in app's: somebody from outside who joins once loads only what the
  /// meeting needs (docs/performance.md, "Web bundle size").
  guest_meeting(session: GuestSession) => guest: view! { <GuestMeeting session=session /> };
  directory(_unit: ()) => directory: view! { <DirectoryView /> };
  search(_unit: ()) => search: view! { <SearchView /> };
  /// The top bar's search (Ctrl/Cmd+K), in the chunk of the search results it leads to (#695).
  quick_switcher(_unit: ()) => search: view! { <QuickSwitcher /> };
  /// Where to send what another app shared (#208).
  share_into(_unit: ()) => dialogs: view! { <ShareDialog /> };
  /// Dialogs a first visit doesn't open: on a message, a file or a contact's keys (#695).
  forward(from: ConversationId, message: MessageId, maybe_attachment: Option<AttachmentId>) => dialogs: view! { <ForwardDialog from=from message=message maybe_attachment=maybe_attachment /> };
  edit_history(conversation: ConversationId, message: MessageId) => dialogs: view! { <EditHistoryDialog conversation=conversation message=message /> };
  bookmarks(conversation: ConversationId) => dialogs: view! { <BookmarksDialog conversation=conversation /> };
  quiet_hours(conversation: ConversationId) => dialogs: view! { <QuietHoursDialog conversation=conversation /> };
  share_file(conversation: ConversationId, message: MessageId, file: AttachmentId) => dialogs: view! { <ShareLinkDialog conversation=conversation message=message file=file /> };
  verify_key(conversation: ConversationId) => dialogs: view! { <VerifyKeyDialog conversation=conversation /> };
  verify_contact(user: UserId) => dialogs: view! { <VerifyContactDialog user=user /> };
  /// More dialogs a first visit doesn't open: a person's profile, a new direct message, linking or
  /// approving a device, why calls are unavailable.
  user_profile(user: UserId) => dialogs: view! { <UserProfileDialog user=user /> };
  new_message(_unit: ()) => dialogs: view! { <NewMessageDialog /> };
  link_device(_unit: ()) => dialogs: view! { <LinkDeviceDialog /> };
  /// "Report a problem" (docs/bug-reports.md).
  report_problem(_unit: ()) => dialogs: view! { <ReportProblemDialog /> };
  device_approval(_unit: ()) => dialogs: view! { <DeviceApprovalDialog /> };
  calls_unavailable(_unit: ()) => dialogs: view! { <CallsUnavailableDialog /> };
  /// Another device asking to be linked to this account, once one asks.
  link_requests(
    requests: RwSignal<Vec<LinkRequestView>>,
    active: RwSignal<Option<(LinkRequestView, LinkStep)>>
  ) => dialogs: view! {
    <LinkRequestCards requests=requests active=active />
  };
  /// The formatting toolbar above the message input: off until it is turned on.
  format_toolbar(active: Signal<ActiveFormats>, on_action: Callback<FormatAction>) => dialogs: view! {
    <FormatToolbarPanel active=active on_action=on_action />
  };
  /// The desktop app's update banner; the browser has no updates to show and never loads it.
  update_notice(_unit: ()) => dialogs: view! { <UpdateNoticeBanner /> };
  /// The desktop app's downloads button and list; the browser has its own and never loads them.
  downloads(_unit: ()) => dialogs: view! { <DownloadsButton /> };
  /// The emoji picker and the date pills' menu, the first time they open.
  emoji_picker(anchor: web_sys::Element, on_pick: Callback<String>, on_close: Callback<()>, returns_focus: bool) => dialogs: view! {
    <EmojiPickerPanel anchor=anchor on_pick=on_pick on_close=on_close returns_focus=returns_focus />
  };
  date_jump(conversation: ConversationId, menu: DateJumpMenu) => dialogs: view! { <DateJumpPanel conversation=conversation menu=menu /> };
  /// What a browser tab shows while another tab of the account runs the app (#744).
  tab_elsewhere(tab: Signal<TabState>) => dialogs: view! { <TabElsewhere tab=tab /> };
  /// The account menu behind the user's picture and the custom status it opens.
  status_menu(_unit: ()) => account: view! { <StatusMenu /> };
  custom_status(_unit: ()) => account: view! { <CustomStatusDialog /> };
  status_duration(_unit: ()) => account: view! { <StatusDurationDialog /> };
  /// Home pages a first visit doesn't open (Meetings and People have their own chunks), and a
  /// conversation's side panels except the thread.
  saved(_unit: ()) => places: view! { <SavedView /> };
  scheduled(_unit: ()) => places: view! { <ScheduledView /> };
  drafts(_unit: ()) => places: view! { <DraftsView /> };
  files(_unit: ()) => places: view! { <FilesView /> };
  /// The Files place's sidebar: its filters and scopes (#1510).
  files_sidebar(_unit: ()) => places: view! { <FilesSidebar /> };
  call_history(_unit: ()) => places: view! { <CallsView /> };
  pins(conversation: ConversationId) => places: view! { <PinsPanel conversation=conversation /> };
  members(conversation: ConversationId) => places: view! { <MembersPanel conversation=conversation /> };
  /// Activity (#752): its list in the sidebar's place and its main column.
  activity_list(_unit: ()) => activity: view! { <ActivitySidebar /> };
  activity_home(_unit: ()) => activity: view! { <ActivityHome /> };
  /// A channel's shared page (#1221) with the CRDT it is edited with: loads when its tab opens.
  canvas(conversation: ConversationId) => canvas: view! { <CanvasView conversation=conversation /> };
  /// The log view of an integration channel: its toolbar and the filtered log.
  integration_feed(conversation: ConversationId) => log: view! { <IntegrationFeed conversation=conversation /> };
  /// The public page of a share link (`/f/<id>`, e2ee.md §4.5.1), a page of its own: neither a
  /// member nor a guest needs it (#1413).
  #[cfg(target_arch = "wasm32")]
  share_download(id: String, fragment: String) => share: view! { <ShareDownloadPage id=id fragment=fragment /> };
}

/// The app's own message sounds (#800), synthesized by teamcraft-media in the page: a first visit
/// doesn't need them, so they load with the first sound (the page keeps each one once made).
#[cfg(target_arch = "wasm32")]
#[wasm_split(sounds, wasm_split_path = ::leptos::wasm_split, fallible)]
pub fn message_sound(
  sound: teamcraft_client_api::message_sound::BuiltInSound,
) -> Result<Vec<f32>, SplitLoaderError> {
  Ok(teamcraft_media::message_sounds::synthesize(sound))
}

/// What to do with a link from outside the app (`app_link::next_step`), which names who owns a
/// host from the Public Suffix List (`psl`: about 0.9 MB before wasm-opt, 0.26 MB gzip after).
/// Only the desktop app receives such links, so the list loads with the first one instead of with
/// every page (#1148). A module of its own: in `dialogs` every member's first emoji picker or
/// profile would load it too, and it shares almost nothing with the other modules.
#[wasm_split(links, wasm_split_path = ::leptos::wasm_split, fallible)]
pub fn app_link_step(
  link: &AppLink,
  maybe_active: Option<&ServerOrigin>,
  is_server_locked: bool,
) -> Result<AppLinkStep, SplitLoaderError> {
  Ok(next_step(link, maybe_active, is_server_locked))
}

/// Reading the administration console's data (`admin_live.rs`): the lists, their details and the
/// wire types they arrive in, which only an admin's console needs. They load with the console's
/// chunk.
#[wasm_split(admin, wasm_split_path = ::leptos::wasm_split, fallible)]
pub fn admin_refresh(live: AdminLive) -> Result<Pin<Box<dyn Future<Output = ()>>>, SplitLoaderError> {
  Ok(Box::pin(async move { live.refresh_once().await }))
}

/// The console's other requests (changes, on-demand data, connection tests), in its chunk as well.
#[wasm_split(admin, wasm_split_path = ::leptos::wasm_split, fallible)]
pub fn admin_task(
  live: AdminLive,
  task: AdminTask,
) -> Result<Pin<Box<dyn Future<Output = ()>>>, SplitLoaderError> {
  Ok(Box::pin(async move { live.perform(task).await }))
}

/// An engine request a split module runs, on its way (#1413).
#[cfg(target_arch = "wasm32")]
pub(crate) type EngineRun<'a> = Pin<Box<dyn Future<Output = Result<(), teamcraft_core::CoreError>> + 'a>>;

/// A chat request in JSON, between two tabs of an account (`tabs`) or to the desktop shell
/// (`shell`), on its way.
pub(crate) type ChatRun<T> = Pin<Box<dyn Future<Output = Result<T, String>>>>;

/// The engine's side of the workspace dialogs, a channel's settings and members, and the console's
/// changes to spaces and channels (`CommandPart::Workspace`, #1413): a guest's page and a member's
/// first screen never run it, so it loads with the dialogs that ask for it.
#[cfg(target_arch = "wasm32")]
#[wasm_split(workspace, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn workspace_command(
  client: &mut crate::web_core::WebClient,
  command: teamcraft_core::ui_backend::CoreCommand,
) -> Result<EngineRun<'_>, SplitLoaderError> {
  Ok(Box::pin(client.run_workspace_command(command)))
}

/// A new channel or member through the engine (`web_core::EnginePart::Workspace`), with the
/// workspace dialogs.
#[cfg(target_arch = "wasm32")]
#[wasm_split(workspace, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn workspace_action(
  client: &mut crate::web_core::WebClient,
  action: teamcraft_client_api::remote::ChatAction,
) -> Result<EngineRun<'_>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::workspace_action(client, action)))
}

/// A new recovery passphrase or key (`web_core::EnginePart::Settings`), with the security settings.
#[cfg(target_arch = "wasm32")]
#[wasm_split(settings, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn settings_action(
  client: &mut crate::web_core::WebClient,
  action: teamcraft_client_api::remote::ChatAction,
) -> Result<EngineRun<'_>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::settings_action(client, action)))
}

/// The console's engine operations (`web_core::EnginePart::Admin`): the workspace's policy and
/// roster, and its operations on users and their chats.
#[cfg(target_arch = "wasm32")]
#[wasm_split(admin, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn admin_action(
  client: &mut crate::web_core::WebClient,
  action: teamcraft_client_api::remote::ChatAction,
) -> Result<EngineRun<'_>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::admin_action(client, action)))
}

/// The engine's side of the console's own changes (`CommandPart::Admin`), in the console's chunk.
#[cfg(target_arch = "wasm32")]
#[wasm_split(admin, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn admin_command(
  client: &mut crate::web_core::WebClient,
  command: teamcraft_core::ui_backend::CoreCommand,
) -> Result<EngineRun<'_>, SplitLoaderError> {
  Ok(Box::pin(client.run_admin_command(command)))
}

/// The page's UI hands chat actions and queries to the core typed (`web_core::act`, `query`); in
/// JSON they only travel between two tabs of an account (#744). The JSON code of `ChatAction`,
/// `ChatQuery` and `ChatAnswer` (about 0.65 MB of the app before wasm-opt) is in this module of its
/// own (#1413), which loads when another tab of the account asks something of this one.
#[cfg(target_arch = "wasm32")]
#[wasm_split(tabs, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn chat_request_from_json(
  command: String,
  args: serde_json::Value,
) -> Result<ChatRun<serde_json::Value>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::serve_chat_json(command, args)))
}

/// A follower tab's chat action, as JSON for the tab that runs the core.
#[cfg(target_arch = "wasm32")]
#[wasm_split(tabs, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn act_in_owner_tab(
  follower: Rc<crate::web_tabs::Follower>,
  action: teamcraft_client_api::remote::ChatAction,
) -> Result<ChatRun<()>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::act_in_owner_tab(follower, action)))
}

/// A follower tab's chat query, as JSON for the tab that runs the core.
#[cfg(target_arch = "wasm32")]
#[wasm_split(tabs, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn query_owner_tab(
  follower: Rc<crate::web_tabs::Follower>,
  query: teamcraft_client_api::remote::ChatQuery,
) -> Result<ChatRun<teamcraft_client_api::remote::ChatAnswer>, SplitLoaderError> {
  Ok(Box::pin(crate::web_core::query_owner_tab(follower, query)))
}

/// The desktop shell takes chat actions and queries in JSON (`core_action`, `core_query`); a browser
/// hands them to the page's core typed. So the JSON code of `ChatAction` and `ChatAnswer` (about
/// 0.65 MB before wasm-opt, shared with `tabs`) is in a module only the desktop app loads (#1413).
#[wasm_split(shell, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn shell_action(
  action: teamcraft_client_api::remote::ChatAction,
) -> Result<ChatRun<()>, SplitLoaderError> {
  Ok(Box::pin(crate::remote_backend::shell_action(action)))
}

/// A chat query for the desktop shell, as JSON.
#[wasm_split(shell, wasm_split_path = ::leptos::wasm_split, fallible)]
pub(crate) fn shell_query(
  query: teamcraft_client_api::remote::ChatQuery,
) -> Result<ChatRun<teamcraft_client_api::remote::ChatAnswer>, SplitLoaderError> {
  Ok(Box::pin(crate::remote_backend::shell_query(query)))
}

/// Shows the view `load` builds once its chunk is there: a placeholder until then, and a message
/// with a retry when the chunk can't be loaded.
pub fn lazy_area<F, Fut>(place: Place, load: F) -> AnyView
where
  F: Fn() -> Fut + 'static,
  Fut: Future<Output = Result<AnyView, SplitLoaderError>> + 'static,
{
  // Only this boxing is per caller: the views below are built once for every area. Generic, they
  // were copied for each caller's closure types, a few kB of the first visit each.
  area_view(place, None, Rc::new(move || Box::pin(load())))
}

/// The administration console. Its texts are a file of their own on the web, fetched here before
/// its view is built (`I18n::load_admin_texts`, #1462): a text read before they arrive would show
/// its key, and nothing would render it again when they came.
pub fn admin_console_area(section: AdminSection) -> AnyView {
  let i18n = use_i18n();
  area_view(
    Place::Pane,
    Some(Rc::new(move || Box::pin(i18n.load_admin_texts()))),
    Rc::new(move || Box::pin(admin_console(section))),
  )
}

/// [`lazy_area`] for any loader.
fn area_view(place: Place, maybe_first: Option<AreaFirst>, load: Rc<dyn Fn() -> AreaLoad>) -> AnyView {
  let i18n = use_i18n();
  let phase = RwSignal::new(Phase::Loading);
  let ready: StoredValue<Option<AnyView>, LocalStorage> = StoredValue::new_local(None);
  // The area's owner, also for a retry from the error view (whose own owner goes as soon as the
  // phase changes): the view is built under it, and loading stops when the area goes, so a view
  // never comes up under a disposed owner (whose cleanups, like a call screen releasing the
  // camera, would never run).
  let owner = Owner::current();
  let start: Rc<dyn Fn()> = Rc::new(move || {
    let run = || {
      phase.set(Phase::Loading);
      let maybe_first = maybe_first.as_ref().map(|first| first());
      let loading = load();
      spawn_local_scoped_with_cancellation(async move {
        if let Some(first) = maybe_first
          && let Err(error) = first.await
        {
          tracing::warn!(%error, "what a part of the app needs did not load");
          phase.set_if_alive(Phase::Failed);
          return;
        }
        match loading.await {
          Ok(view) => {
            // Fails only when the area is gone; the view then goes with this task.
            let _ = ready.try_update_value(|slot| *slot = Some(view));
            phase.set_if_alive(Phase::Ready);
          },
          Err(error) => {
            tracing::warn!(%error, "a part of the app did not load");
            phase.set_if_alive(Phase::Failed);
          },
        }
      });
    };
    match &owner {
      Some(owner) => owner.with(run),
      None => run(),
    }
  });
  start();
  // Views are `Send`; the loader stays on this thread.
  let start = StoredValue::new_local(start);
  let class = place.class();
  view! {
    {move || match phase.get() {
      Phase::Loading => {
        view! {
          <div class=format!("lazy-area {class}") role="status" aria-busy="true">
            <span class="spinner" aria-hidden="true"></span>
          </div>
        }
        .into_any()
      },
      Phase::Ready => ready
        .try_update_value(Option::take)
        .flatten()
        .unwrap_or_else(|| ().into_any()),
      Phase::Failed => {
        // A part of the start that failed to load is the first view: the loading screen must go.
        crate::startup::first_view();
        let retry = move |_| start.with_value(|start| start());
        let maybe_close = (place == Place::Overlay).then(|| {
          let maybe_state = use_context::<AppState>();
          view! {
            <button class="btn" on:click=move |_| maybe_state.iter().for_each(AppState::close_overlay)>
              {move || i18n.t("close")}
            </button>
          }
        });
        view! {
          <div class=format!("lazy-area lazy-area-failed {class}") role="alert">
            <div class="lazy-area-card">
              <p>{move || i18n.t("area-load-failed")}</p>
              <div class="lazy-area-actions">
                {maybe_close}
                // After a deploy the chunk is gone and only a reload (into the new release) helps.
                <button class="btn" on:click=move |_| crate::bundle::reload()>{move || i18n.t("area-reload")}</button>
                <button class="btn btn-primary" on:click=retry>{move || i18n.t("loader-retry")}</button>
              </div>
            </div>
          </div>
        }
        .into_any()
      },
    }}
  }
  .into_any()
}

impl Place {
  fn class(self) -> &'static str {
    match self {
      Self::Pane => "lazy-area-pane",
      Self::Overlay => "lazy-area-overlay",
      Self::Screen => "lazy-area-screen",
      Self::Sidebar => "sidebar",
      Self::Panel => "side-panel",
      Self::Inline => "lazy-area-inline",
      Self::Popover => "lazy-area-popover",
    }
  }
}
