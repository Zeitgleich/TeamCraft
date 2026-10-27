//! Areas of the app in wasm chunks of their own (#334): the code an area alone uses (the
//! administration console, …) is split off the app and loads the first time the area opens, so the
//! first visit downloads less (robustness.md, "Download size"; docs/performance.md, "Web bundle
//! size"). Code several areas share goes into shared chunks; what the first screen needs stays in
//! the app.
//!
//! The signed-in app itself is one (`session`): a guest who opens a meeting link loads only the
//! guest pages, and `init.js` fetches the session's chunks next to the app for every other page, so
//! signed-in users don't wait for them one after the other.
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
use crate::calls::CallRequest;
use crate::components::activity_view::{ActivityHome, ActivitySidebar};
use crate::components::add_to_call::AddToCallDialog;
use crate::components::admin_console::AdminConsole;
use crate::components::bookmarks::BookmarksDialog;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::calls_unavailable::CallsUnavailableDialog;
use crate::components::calls_view::CallsView;
use crate::components::channel_preview::ChannelPreview;
use crate::components::directory_view::DirectoryView;
use crate::components::file_preview::FilePreview;
use crate::components::files_view::FilesView;
use crate::components::guest_meeting::{GuestMeeting, GuestSession};
use crate::components::home::NewMessageDialog;
use crate::components::lightbox::Lightbox;
use crate::components::log_view::IntegrationFeed;
use crate::components::login::SessionGate;
use crate::components::meetings_view::{MeetingCreatedDialog, MeetingsView, ScheduleMeetingDialog};
use crate::components::members_panel::MembersPanel;
use crate::components::message_actions::{EditHistoryDialog, ForwardDialog, PinsPanel, SavedView};
use crate::components::profile_views::UserProfileDialog;
use crate::components::quick_switcher::QuickSwitcher;
use crate::components::search_view::SearchView;
use crate::components::security_ui::{
  DeviceApprovalDialog, LinkDeviceDialog, VerifyContactDialog, VerifyKeyDialog,
};
use crate::components::settings_modal::{SettingsModal, ShortcutsModal};
use crate::components::share_link_dialog::ShareLinkDialog;
use crate::components::space_invites::{AddToChannelDialog, BrowseSpacesDialog, InvitePeopleDialog};
use crate::components::status_menu::{CustomStatusDialog, StatusMenu};
use crate::components::video_player::VideoPlayer;
use crate::components::workspace_dialogs::{
  BrowseChannelsDialog, ChannelSettingsDialog, NewChannelDialog, NewSpaceDialog, SpaceSettingsDialog,
};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::model::{ConversationId, MessageId, SpaceId, UserId};
use crate::reactive::SetIfAlive;
use crate::settings::Settings;
use crate::state::AppState;
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
  /// The administration console: the largest part of the app, for admins only.
  admin_console(section: AdminSection) => admin: view! { <AdminConsole section=section /> };
  settings(_unit: ()) => settings: view! { <SettingsModal /> };
  /// The keyboard shortcuts, next to their settings (#695).
  shortcuts(_unit: ()) => settings: view! { <ShortcutsModal /> };
  lightbox(conversation: ConversationId, start: MediaKey) => lightbox: view! { <Lightbox conversation=conversation start=start /> };
  /// The app's own video player in a message row, once play is pressed (#638): a first visit
  /// doesn't need it (#695). The lightbox's chunk uses it as well.
  inline_video_player(src: String, video: NodeRef<html::Video>, label: String, on_error: Callback<()>) => video: view! {
    <VideoPlayer src=src video=video label=label class="media-video-player" is_autoplay=true on_error=on_error />
  };
  /// The preview of Markdown, HTML and PDF files (#557).
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
  /// A channel of a space before joining it, opened from browsing the channels.
  channel_preview(space: SpaceId, channel: ConversationId) => workspace: view! { <ChannelPreview space=space channel=channel /> };
  /// The call screens (the incoming call card stays in the app, so a call rings at once).
  call_prejoin(request: CallRequest) => calls: view! { <PreJoinScreen request=request /> };
  call_screen(request: CallRequest) => calls: view! { <CallScreen request=request /> };
  add_to_call(conversation: ConversationId) => calls: view! { <AddToCallDialog conversation=conversation /> };
  meetings(_unit: ()) => meetings: view! { <MeetingsView /> };
  schedule_meeting(_unit: ()) => meetings: view! { <ScheduleMeetingDialog /> };
  edit_meeting(meeting: MeetingId) => meetings: view! { <ScheduleMeetingDialog maybe_edit=meeting /> };
  meeting_created(_unit: ()) => meetings: view! { <MeetingCreatedDialog /> };
  /// What a guest sees after a host let them in: the meeting chat and its call.
  guest_meeting(session: GuestSession) => session: view! { <GuestMeeting session=session /> };
  directory(_unit: ()) => directory: view! { <DirectoryView /> };
  search(_unit: ()) => search: view! { <SearchView /> };
  /// The top bar's search (Ctrl/Cmd+K), in the chunk of the search results it leads to (#695).
  quick_switcher(_unit: ()) => search: view! { <QuickSwitcher /> };
  /// Dialogs a first visit doesn't open: on a message, a file or a contact's keys (#695).
  forward(from: ConversationId, message: MessageId) => dialogs: view! { <ForwardDialog from=from message=message /> };
  edit_history(conversation: ConversationId, message: MessageId) => dialogs: view! { <EditHistoryDialog conversation=conversation message=message /> };
  bookmarks(conversation: ConversationId) => dialogs: view! { <BookmarksDialog conversation=conversation /> };
  share_file(conversation: ConversationId, message: MessageId, file: AttachmentId) => dialogs: view! { <ShareLinkDialog conversation=conversation message=message file=file /> };
  verify_key(conversation: ConversationId) => dialogs: view! { <VerifyKeyDialog conversation=conversation /> };
  verify_contact(user: UserId) => dialogs: view! { <VerifyContactDialog user=user /> };
  /// More dialogs a first visit doesn't open: a person's profile, a new direct message, linking or
  /// approving a device, why calls are unavailable.
  user_profile(user: UserId) => dialogs: view! { <UserProfileDialog user=user /> };
  new_message(_unit: ()) => dialogs: view! { <NewMessageDialog /> };
  link_device(_unit: ()) => dialogs: view! { <LinkDeviceDialog /> };
  device_approval(_unit: ()) => dialogs: view! { <DeviceApprovalDialog /> };
  calls_unavailable(_unit: ()) => dialogs: view! { <CallsUnavailableDialog /> };
  /// The account menu behind the user's picture and the custom status it opens.
  status_menu(_unit: ()) => account: view! { <StatusMenu /> };
  custom_status(_unit: ()) => account: view! { <CustomStatusDialog /> };
  /// Home pages a first visit doesn't open (Meetings and People have their own chunks), and a
  /// conversation's side panels except the thread.
  saved(_unit: ()) => places: view! { <SavedView /> };
  files(_unit: ()) => places: view! { <FilesView /> };
  call_history(_unit: ()) => places: view! { <CallsView /> };
  pins(conversation: ConversationId) => places: view! { <PinsPanel conversation=conversation /> };
  members(conversation: ConversationId) => places: view! { <MembersPanel conversation=conversation /> };
  /// Activity (#752): its list in the sidebar's place and its main column.
  activity_list(_unit: ()) => activity: view! { <ActivitySidebar /> };
  activity_home(_unit: ()) => activity: view! { <ActivityHome /> };
  /// The log view of an integration channel: its toolbar and the filtered log.
  integration_feed(conversation: ConversationId) => log: view! { <IntegrationFeed conversation=conversation /> };
}

/// Thumbnails of the pictures this page sends, by teamcraft-core's image codecs (its
/// `make_thumbnail`, installed in `web_core.rs`): a large part of the app that a first visit doesn't
/// need, so they load with the first picture.
#[cfg(target_arch = "wasm32")]
#[wasm_split(images, wasm_split_path = ::leptos::wasm_split, fallible)]
pub fn thumbnail(bytes: &[u8], media_type: &str) -> Result<Option<Vec<u8>>, SplitLoaderError> {
  Ok(teamcraft_core::thumbnail::thumbnail(bytes, media_type))
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

/// Shows the view `load` builds once its chunk is there: a placeholder until then, and a message
/// with a retry when the chunk can't be loaded.
pub fn lazy_area<F, Fut>(place: Place, load: F) -> AnyView
where
  F: Fn() -> Fut + 'static,
  Fut: Future<Output = Result<AnyView, SplitLoaderError>> + 'static,
{
  // Only this boxing is per caller: the views below are built once for every area. Generic, they
  // were copied for each caller's closure types, a few kB of the first visit each.
  area_view(place, Rc::new(move || Box::pin(load())))
}

/// [`lazy_area`] for any loader.
fn area_view(place: Place, load: Rc<dyn Fn() -> AreaLoad>) -> AnyView {
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
      let loading = load();
      spawn_local_scoped_with_cancellation(async move {
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
    }
  }
}
