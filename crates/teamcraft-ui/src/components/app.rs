use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use leptos::ev;
use leptos::prelude::*;
use teamcraft_client_api::os_notify::{NotificationTag, OsNotification, OsNotificationKind};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::attention::{AttentionReporter, AttentionSignals, is_attentive};
use crate::auth::tauri;
use crate::backend::Backend;
use crate::calls::CallPhase;
use crate::calls::{CallEvent, CallRequest};
use crate::clock::now_ms;
use crate::components::activity_view::ActivityView;
use crate::components::admin_console::AdminConsole;
use crate::components::announcement_banner::AnnouncementBanner;
use crate::components::bookmarks::BookmarksDialog;
use crate::components::bundle_banner::{BundleBanner, ReleaseBanner};
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::calls_view::CallsView;
use crate::components::channel_preview::ChannelPreview;
use crate::components::channel_view::ChannelView;
use crate::components::directory_view::DirectoryView;
use crate::components::getting_started::GettingStarted;
use crate::components::guest_join::GuestJoinPage;
use crate::components::home::{HomeSidebar, HomeStart, NewMessageDialog};
use crate::components::icons::{Icon, IconName};
use crate::components::incoming_call::IncomingCallDialog;
use crate::components::lightbox::Lightbox;
use crate::components::login::SessionGate;
use crate::components::meetings_view::{MeetingsView, ScheduleMeetingDialog};
use crate::components::members_panel::MembersPanel;
use crate::components::message_actions::{EditHistoryDialog, ForwardDialog, PinsPanel, SavedView};
use crate::components::offline_banner::OfflineBanner;
use crate::components::profile_views::{EditProfileDialog, UserProfileDialog};
use crate::components::quick_switcher::QuickSwitcher;
use crate::components::rail::SpaceRail;
use crate::components::search_view::SearchView;
use crate::components::security_ui::{
  DeviceApprovalDialog, LinkDeviceDialog, VerifyContactDialog, VerifyKeyDialog,
};
use crate::components::settings_modal::{SettingsModal, ShortcutsModal};
use crate::components::settings_tabs::apply_call_prefs;
use crate::components::sidebar::Sidebar;
use crate::components::status_menu::CustomStatusDialog;
use crate::components::storage_banner::StorageBanner;
use crate::components::thread_panel::ThreadPanel;
use crate::components::threads_inbox::ThreadsInbox;
use crate::components::top_bar::TopBar;
use crate::components::update_notice::UpdateNotice;
use crate::components::window_controls::{WindowControls, provide_window_chrome};
use crate::components::workspace_dialogs::{
  BrowseChannelsDialog, ChannelSettingsDialog, JoinSpaceDialog, NewChannelDialog, NewSpaceDialog,
  SpaceSettingsDialog,
};
use crate::history::{Travel, travel};
use crate::i18n::{provide_i18n, use_i18n};
use crate::meetings::parse_guest_link;
use crate::model::TimestampMs;
use crate::model::{ConversationId, SpaceId};
use crate::motion::use_leave_ghost;
use crate::navigation::Step;
use crate::notify_sync::NotificationSync;
use crate::prefs::Zoom;
use crate::reactive::use_interval;
use crate::sections::badge_count;
use crate::settings::{Settings, Theme};
use crate::state::{AppState, MainView, Overlay, RailEntry, SidePanel, ToastTone};

#[component]
pub fn App() -> impl IntoView {
  let settings = Settings::load();
  let i18n = provide_i18n(settings.locale.get_untracked());
  // The settings signal is the source of truth; mirror it into the translator.
  Effect::new(move |_| i18n.set_locale(settings.locale.get()));
  apply_theme(settings.theme);
  apply_density(settings);
  apply_zoom(settings);
  apply_language_attribute();
  crate::brand::provide_brand(settings);
  provide_window_chrome();
  if tauri::is_available() {
    sync_tray_labels(i18n);
  }
  // Above every screen: sign-in has no top bar, but the window still needs its controls.
  view! {
    <WindowControls />
    {screen(settings)}
  }
}

/// The guest page, the sign-in gate or (development) the demo shell.
fn screen(settings: Settings) -> AnyView {
  // A personal meeting link needs no sign-in: it gets its own page before anything else.
  if let Some((meeting, invite_token)) = guest_link_from_location() {
    return view! { <GuestJoinPage meeting=meeting invite_token=invite_token /> }.into_any();
  }

  // What the service worker says about the code that is running (docs/e2ee.md §9.3).
  let bundle = crate::bundle::Bundle(RwSignal::new(crate::bundle::BundleState::Unknown));
  provide_context(bundle);
  let release = crate::bundle::Release(RwSignal::new(crate::bundle::ReleaseUpdate::Current));
  provide_context(release);
  if !tauri::is_available() {
    crate::bundle::watch(bundle.0);
    crate::bundle::watch_releases(release.0);
  }

  #[cfg(feature = "demo")]
  if crate::settings::BackendMode::load() == crate::settings::BackendMode::Mock {
    let backend: Rc<dyn Backend> = Rc::new(crate::mock::MockBackend::new(now_ms()));
    return view! { <Shell backend=backend settings=settings /> }.into_any();
  }
  view! { <SessionGate settings=settings /> }.into_any()
}

/// Everything after sign-in: the chat shell on top of a backend.
#[component]
pub fn Shell(backend: Rc<dyn Backend>, settings: Settings) -> impl IntoView {
  let i18n = use_i18n();
  let start = initial_location(backend.as_ref());
  let change_source = backend.clone();
  let state = AppState::new(backend, settings, start);
  provide_context(state);
  // Backends that load in the background (the desktop core) say so through this hook.
  change_source.set_change_notifier(Rc::new(move || state.backend_changed()));
  // A failed action is never silent (issue #322).
  crate::feedback::set_failure_sink(move |failure| {
    state.show_error(crate::feedback::failure_message(i18n, &failure), failure.detail);
  });
  // Preferences the server enforces (push, do not disturb, mutes) follow the account on the server.
  if let Some(sync) = use_context::<StoredValue<NotificationSync, LocalStorage>>() {
    sync.get_value().start(settings);
  }
  if tauri::has_core() {
    crate::os_alerts::start(state, i18n);
  }
  if tauri::is_available() {
    sync_tray(state, i18n);
  }
  if let Some((_, conversation)) = start {
    state.open_conversation(conversation);
  }
  // The URL's place wins over the start; a notification click (below) wins over the URL.
  crate::history::start(state, i18n);
  if !tauri::is_available() {
    open_conversations_from_notifications(state);
  }
  // `/remind` reminders come due while the app is open.
  let brand = crate::brand::Brand::use_current();
  crate::reactive::use_interval(REMINDER_CHECK_MS, move || {
    for reminder in crate::components::composer_commands::due_reminders(now_ms()) {
      let message = i18n.t_with("reminder-due", &[("text", &reminder.text)]);
      state.show_toast(message.clone());
      let shown = OsNotification {
        tag: NotificationTag::reminder(reminder.due_at),
        kind: OsNotificationKind::Reminder,
        title: brand.0.with_untracked(|brand| brand.display_name.clone()),
        maybe_body: Some(message),
        actions: Vec::new(),
        maybe_reply: None,
      };
      leptos::task::spawn_local(async move { tauri::show_notification(&shown).await });
    }
  });
  if tauri::is_available() {
    connect_calls(state);
  }
  watch_activity(state);
  // Call defaults follow the settings (and set the initial state of the call session).
  let call_prefs = Memo::new(move |_| settings.prefs.get().calls);
  Effect::new(move |_| apply_call_prefs(state.call, &call_prefs.get()));

  let _ = window_event_listener(ev::keydown, move |ev| handle_global_shortcut(&ev, state));
  let banners = NodeRef::<leptos::html::Div>::new();
  track_banner_height(banners);

  view! {
    <a class="skip-link" href="#main-content">
      {move || i18n.t("skip-to-content")}
    </a>
    <div class="app">
      // One grid row for all banners, so the top bar and the body keep their own rows.
      <div class="app-banners" node_ref=banners>
        <BundleBanner />
        <ReleaseBanner />
        <OfflineBanner />
        <StorageBanner />
        <AnnouncementBanner />
        <UpdateNotice />
      </div>
      <TopBar />
      <div class="app-body">
        // The administration console takes the whole window, like a system console.
        <Show when=move || !matches!(state.view.get(), MainView::Admin(_)) fallback=|| ()>
          <SpaceRail />
          <Show when=move || state.rail.get() == RailEntry::Home fallback=|| view! { <Sidebar /> }>
            <HomeSidebar />
          </Show>
        </Show>
        <main id="main-content" class="main" tabindex="-1">
          {move || match state.view.get() {
            MainView::Conversation(id) => view! { <ChannelView conversation=id /> }.into_any(),
            MainView::Threads => view! { <ThreadsInbox /> }.into_any(),
            MainView::Admin(section) => view! { <AdminConsole section=section /> }.into_any(),
            MainView::Search => view! { <SearchView /> }.into_any(),
            MainView::Meetings => view! { <MeetingsView /> }.into_any(),
            MainView::Saved => view! { <SavedView /> }.into_any(),
            MainView::Activity => view! { <ActivityView /> }.into_any(),
            MainView::Calls => view! { <CallsView /> }.into_any(),
            MainView::Directory => view! { <DirectoryView /> }.into_any(),
            MainView::GettingStarted => view! { <GettingStarted /> }.into_any(),
            MainView::Home => view! { <HomeStart /> }.into_any(),
            MainView::ChannelPreview(space, channel) => view! { <ChannelPreview space=space channel=channel /> }.into_any(),
          }}
        </main>
        <Show when=move || matches!(state.view.get(), MainView::Conversation(_)) fallback=|| ()>
          {move || match state.panel.get() {
            SidePanel::Closed => ().into_any(),
            SidePanel::Thread { conversation, thread } => {
              view! { <ThreadPanel conversation=conversation thread=thread /> }.into_any()
            },
            SidePanel::Pins => match state.current_conversation_id() {
              Some(id) => view! { <PinsPanel conversation=id /> }.into_any(),
              None => ().into_any(),
            },
            SidePanel::Members => match state.current_conversation_id() {
              Some(id) => view! { <MembersPanel conversation=id /> }.into_any(),
              None => ().into_any(),
            },
          }}
        </Show>
      </div>
      {move || match state.call.phase.get() {
        CallPhase::PreJoin => {
          state.call.maybe_request.get().map(|request| view! { <PreJoinScreen request=request /> }.into_any())
        },
        CallPhase::Active if !state.call.is_minimized.get() => {
          state.call.maybe_request.get().map(|request| view! { <CallScreen request=request /> }.into_any())
        },
        _ => None,
      }}
      <IncomingCallDialog />
      <Show when=move || !state.toasts.with(Vec::is_empty) fallback=|| ()>
        <div class="toasts" role="status">
          <For each=move || state.toasts.get() key=|toast| toast.id let:toast>
            {
              let id = toast.id;
              let node = NodeRef::<leptos::html::Div>::new();
              use_leave_ghost(node);
              let is_error = matches!(toast.tone, ToastTone::Error { .. });
              view! {
                <div class=if is_error { "toast toast-error" } else { "toast" } role=is_error.then_some("alert") node_ref=node>
                  <div class="toast-body">
                    <span>{toast.text}</span>
                    {match toast.tone {
                      ToastTone::Info => None,
                      ToastTone::Error { detail } => Some(view! {
                        <details class="toast-details">
                          <summary>{move || i18n.t("error-details")}</summary>
                          <code class="toast-detail-text">{detail}</code>
                        </details>
                      }),
                    }}
                  </div>
                  {toast.maybe_undo.map(|undo| view! {
                    <button class="btn btn-small" on:click=move |_| state.undo(id, undo)>
                      {move || i18n.t("undo")}
                    </button>
                  })}
                  <button class="icon-btn icon-btn-small" aria-label=move || i18n.t("close") on:click=move |_| state.dismiss_toast(id)>
                    <Icon name=IconName::Close />
                  </button>
                </div>
              }
            }
          </For>
        </div>
      </Show>
      {move || match state.overlay.get() {
        Overlay::QuickSwitcher => view! { <QuickSwitcher /> }.into_any(),
        Overlay::CustomStatus => view! { <CustomStatusDialog /> }.into_any(),
        Overlay::Settings => view! { <SettingsModal /> }.into_any(),
        Overlay::Shortcuts => view! { <ShortcutsModal /> }.into_any(),
        Overlay::DeviceApproval => view! { <DeviceApprovalDialog /> }.into_any(),
        Overlay::LinkDevice => view! { <LinkDeviceDialog /> }.into_any(),
        Overlay::VerifyContact(user) => view! { <VerifyContactDialog user=user /> }.into_any(),
        Overlay::VerifyKey(conversation) => view! { <VerifyKeyDialog conversation=conversation /> }.into_any(),
        Overlay::ScheduleMeeting => view! { <ScheduleMeetingDialog /> }.into_any(),
        Overlay::Forward(from, message) => view! { <ForwardDialog from=from message=message /> }.into_any(),
        Overlay::Lightbox(conversation, start) => view! { <Lightbox conversation=conversation start=start /> }.into_any(),
        Overlay::NewSpace => view! { <NewSpaceDialog /> }.into_any(),
        Overlay::NewMessage => view! { <NewMessageDialog /> }.into_any(),
        Overlay::JoinSpace => view! { <JoinSpaceDialog /> }.into_any(),
        Overlay::NewChannel(space) => view! { <NewChannelDialog space=space /> }.into_any(),
        Overlay::BrowseChannels(space) => view! { <BrowseChannelsDialog space=space /> }.into_any(),
        Overlay::ChannelSettings(id) => view! { <ChannelSettingsDialog conversation=id /> }.into_any(),
        Overlay::SpaceSettings(space) => view! { <SpaceSettingsDialog space=space /> }.into_any(),
        Overlay::EditHistory(conversation, message) => view! { <EditHistoryDialog conversation=conversation message=message /> }.into_any(),
        Overlay::Bookmarks(conversation) => view! { <BookmarksDialog conversation=conversation /> }.into_any(),
        Overlay::EditProfile => view! { <EditProfileDialog /> }.into_any(),
        Overlay::UserProfile(user) => view! { <UserProfileDialog user=user /> }.into_any(),
        Overlay::None | Overlay::StatusMenu => ().into_any(),
      }}
    </div>
  }
}

fn handle_global_shortcut(ev: &web_sys::KeyboardEvent, state: AppState) {
  let modifier = ev.ctrl_key() || ev.meta_key();
  let key = ev.key().to_lowercase();

  if modifier && !ev.alt_key() && !ev.shift_key() {
    let overlay = match key.as_str() {
      "k" => Some(Overlay::QuickSwitcher),
      "," => Some(Overlay::Settings),
      "/" => Some(Overlay::Shortcuts),
      _ => None,
    };
    if let Some(overlay) = overlay {
      ev.prevent_default();
      if overlay == Overlay::Settings && state.overlay.get_untracked() != Overlay::Settings {
        state.open_settings();
        return;
      }
      state.overlay.update(|current| {
        *current = if *current == overlay {
          Overlay::None
        } else {
          overlay
        }
      });
      return;
    }
  }
  // In a browser, Ctrl/Cmd + +/-/0 stays the browser's own zoom (it reflows the layout properly).
  if modifier
    && !ev.alt_key()
    && tauri::is_available()
    && let Some(change) = zoom_change(&key)
  {
    ev.prevent_default();
    state
      .settings
      .prefs
      .update(|prefs| prefs.display.zoom = change(prefs.display.zoom));
    return;
  }
  // Everything opens the one search of the top bar: Ctrl/Cmd+F scoped to the open conversation
  // (as in Slack and Teams), Ctrl/Cmd+Shift+F unscoped.
  if modifier && !ev.shift_key() && !ev.alt_key() && key == "f" {
    ev.prevent_default();
    let scope = state
      .current_conversation_id()
      .and_then(|id| state.read(|backend| backend.conversation(id)))
      .filter(|conversation| !conversation.name.contains(char::is_whitespace))
      .map(|conversation| format!("in:#{} ", conversation.name))
      .unwrap_or_default();
    state.open_switcher(&scope);
    return;
  }
  if modifier && ev.shift_key() && !ev.alt_key() && key == "f" {
    ev.prevent_default();
    state.open_switcher("");
    return;
  }
  if ev.alt_key() && ev.shift_key() && !modifier {
    let maybe_step = match ev.code().as_str() {
      "ArrowUp" => Some(Step::Previous),
      "ArrowDown" => Some(Step::Next),
      _ => None,
    };
    if let Some(step) = maybe_step {
      ev.prevent_default();
      state.step_unread(step);
    }
    return;
  }
  if ev.alt_key() && !modifier {
    match ev.code().as_str() {
      "ArrowUp" => {
        ev.prevent_default();
        state.step_conversation(-1);
      },
      "ArrowDown" => {
        ev.prevent_default();
        state.step_conversation(1);
      },
      "KeyT" => {
        ev.prevent_default();
        state.open_threads();
      },
      // Browsers do this themselves; the desktop webviews don't. In a text field Alt+←/→ moves
      // the cursor (by word on macOS).
      "ArrowLeft" | "ArrowRight" if tauri::is_available() && !is_text_input_focused() => {
        ev.prevent_default();
        travel(if ev.code() == "ArrowLeft" {
          Travel::Back
        } else {
          Travel::Forward
        });
      },
      _ => {},
    }
    return;
  }
  if key == "escape"
    && state.overlay.get_untracked() == Overlay::None
    && !is_text_input_focused()
    && state.panel.get_untracked() != SidePanel::Closed
  {
    state.panel.set(SidePanel::Closed);
  }
}

/// Ctrl/Cmd with `+` / `=` zooms in, `-` zooms out, `0` resets (as in browsers).
fn zoom_change(key: &str) -> Option<fn(Zoom) -> Zoom> {
  match key {
    "+" | "=" => Some(Zoom::larger),
    "-" | "_" => Some(Zoom::smaller),
    "0" => Some(|_| Zoom::DEFAULT),
    _ => None,
  }
}

fn is_text_input_focused() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .is_some_and(|element| matches!(element.tag_name().as_str(), "INPUT" | "TEXTAREA" | "SELECT"))
}

/// Mirrors the theme setting into `data-theme` on `<html>`; "system" removes it so CSS media queries apply.
fn apply_theme(theme: RwSignal<Theme>) {
  Effect::new(move |_| {
    let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
    else {
      return;
    };
    let theme = theme.get();
    let result = match theme {
      Theme::System => root.remove_attribute("data-theme"),
      Theme::Light | Theme::Dark => root.set_attribute("data-theme", theme.slug()),
    };
    if result.is_err() {
      tracing::debug!("could not apply theme attribute");
    }
  });
}

/// Mirrors the message density into `data-density` on `<html>` for the CSS.
fn apply_density(settings: Settings) {
  Effect::new(move |_| {
    let density = settings.prefs.get().display.density;
    if let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
      && root.set_attribute("data-density", density.slug()).is_err()
    {
      tracing::debug!("could not apply density attribute");
    }
  });
}

/// Desktop app: scales the webview natively, like browser zoom, so the layout reflows (narrow
/// layouts apply at large sizes). Browsers have their own zoom, which the app leaves alone.
fn apply_zoom(settings: Settings) {
  if !tauri::is_available() {
    return;
  }
  Effect::new(move |_| {
    let percent = settings.prefs.get().display.zoom.percent();
    leptos::task::spawn_local(async move { tauri::set_zoom(percent).await });
  });
}

fn apply_language_attribute() {
  let i18n = use_i18n();
  Effect::new(move |_| {
    let tag = i18n.locale().tag();
    if let Some(root) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|doc| doc.document_element())
    {
      drop(root.set_attribute("lang", tag));
      let _ = root.dyn_ref::<web_sys::HtmlElement>();
    }
  });
}

/// First space and conversation to show, if the backend has any.
fn initial_location(backend: &dyn Backend) -> Option<(SpaceId, ConversationId)> {
  let space = backend.spaces().into_iter().next()?;
  let conversation = backend.space_conversations(space.id).into_iter().next()?;
  Some((space.id, conversation.id))
}

/// Keeps the tray icon's badge, tooltip and menu texts in step with the unread messages and the
/// language (desktop app).
/// The tray texts in the UI language from the start, also on the sign-in screen where `sync_tray`
/// is not running yet. The badge is left alone.
fn sync_tray_labels(i18n: crate::i18n::I18n) {
  let brand = crate::brand::Brand::use_current();
  Effect::new(move |_| {
    let name = brand.0.with(|brand| brand.display_name.clone());
    let (show, quit) = (i18n.t("tray-show"), i18n.t("tray-quit"));
    leptos::task::spawn_local(async move { tauri::tray_update(None, &name, &show, &quit).await });
  });
}

fn sync_tray(state: AppState, i18n: crate::i18n::I18n) {
  let brand = crate::brand::Brand::use_current();
  Effect::new(move |previous: Option<(u32, String, String)>| {
    let unread = state.read(|backend| {
      let mut list: Vec<_> = backend
        .spaces()
        .into_iter()
        .flat_map(|space| backend.space_conversations(space.id))
        .collect();
      list.extend(backend.direct_conversations());
      badge_count(&list)
    });
    let current = (
      unread,
      i18n.locale().tag().to_owned(),
      brand.0.with(|brand| brand.display_name.clone()),
    );
    if previous.as_ref() != Some(&current) {
      let name = current.2.clone();
      let tooltip = match unread {
        0 => name,
        count => i18n.t_with(
          "tray-tooltip-unread",
          &[("name", &name), ("count", &count.to_string())],
        ),
      };
      let (show, quit) = (i18n.t("tray-show"), i18n.t("tray-quit"));
      leptos::task::spawn_local(
        async move { tauri::tray_update(Some(unread), &tooltip, &show, &quit).await },
      );
    }
    current
  });
}

/// Browser: a click on a push notification names a conversation, either as a message from the service
/// worker (a page is open) or as `?conversation_id=` of the page it opened.
fn open_conversations_from_notifications(state: AppState) {
  const FIELD: &str = "conversation_id";
  let Some(window) = web_sys::window() else { return };
  let open = move |text: &str| {
    if let Ok(conversation) = text.parse::<ConversationId>() {
      state.open_conversation(conversation);
    }
  };
  if let Ok(search) = window.location().search()
    && let Ok(params) = web_sys::UrlSearchParams::new_with_str(&search)
    && let Some(value) = params.get(FIELD)
  {
    open(&value);
  }
  let listener = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
    let data = event.data();
    let is_click = js_sys::Reflect::get(&data, &"type".into())
      .ok()
      .and_then(|value| value.as_string())
      .is_some_and(|kind| kind == "open_conversation");
    if is_click
      && let Some(value) = js_sys::Reflect::get(&data, &FIELD.into())
        .ok()
        .and_then(|value| value.as_string())
    {
      open(&value);
    }
  });
  if window
    .navigator()
    .service_worker()
    .add_event_listener_with_callback("message", listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

fn window_has_focus() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| document.has_focus().unwrap_or(false))
}

/// The meeting and token when the page was opened from a personal meeting link.
fn guest_link_from_location() -> Option<(teamcraft_proto::MeetingId, String)> {
  let location = web_sys::window()?.location();
  parse_guest_link(&location.pathname().ok()?, &location.hash().ok()?)
}

/// Desktop: the user's call actions go to the shell, and calls that ring there show the dialog.
/// Keeps `--banners-h` on the root element at the height of the banner row: overlays that are fixed
/// below the top bar (call screens, side panels on narrow windows) start below the banners too.
fn track_banner_height(banners: NodeRef<leptos::html::Div>) {
  let update = move || {
    let Some(element) = banners.get_untracked() else {
      return;
    };
    let height = element.get_bounding_client_rect().height();
    let root = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.document_element())
      .and_then(|root| root.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(root) = root
      && root
        .style()
        .set_property("--banners-h", &format!("{height}px"))
        .is_err()
    {
      tracing::debug!("could not set the banner height");
    }
  };
  let callback = Closure::<dyn FnMut()>::new(update);
  let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
    tracing::debug!("ResizeObserver unavailable");
    return;
  };
  let handle = StoredValue::new_local((observer, callback));
  Effect::new(move |_| {
    if let Some(element) = banners.get() {
      handle.with_value(|(observer, _)| observer.observe(&element));
    }
  });
  on_cleanup(move || {
    handle.try_with_value(|(observer, _)| observer.disconnect());
  });
}

fn connect_calls(state: AppState) {
  let call = state.call;
  let maybe_alerts = use_context::<crate::os_alerts::CallAlertsCell>();
  call.set_handler(move |event| {
    // Answered or declined here: the ring that ends now was not missed.
    if let (
      Some(alerts),
      CallEvent::Join(CallRequest { conversation, .. }) | CallEvent::Decline(conversation),
    ) = (maybe_alerts, &event)
    {
      alerts.handled_here(*conversation);
    }
    state.read(|backend| match event {
      CallEvent::Join(request) => backend.join_call(request),
      CallEvent::Leave => backend.leave_call(),
      CallEvent::Decline(conversation) => backend.decline_call(conversation),
      CallEvent::Muted(is_muted) => backend.set_call_muted(is_muted),
      CallEvent::Deafened(is_deafened) => backend.set_call_deafened(is_deafened),
      CallEvent::Microphone(device) => backend.select_microphone(Some(device)),
      CallEvent::Speakers(device) => backend.select_speakers(Some(device)),
      CallEvent::Camera(is_on) => backend.set_call_camera(is_on),
      CallEvent::CameraDevice(device) => backend.select_camera(Some(device)),
      CallEvent::Effect(effect) => backend.set_background_effect(effect),
      CallEvent::StartScreenShare { has_sound } => backend.start_screen_share(has_sound),
      CallEvent::StopScreenShare => backend.stop_screen_share(),
      CallEvent::StatusSounds(sounds) => backend.set_call_status_sounds(sounds),
    });
  });
  // The status sounds setting reaches the shell at start and on every change.
  Effect::new(move |_| {
    let sounds = state.settings.prefs.with(|prefs| prefs.calls.status_sounds);
    call.emit(CallEvent::StatusSounds(sounds));
  });
  // The effect picker writes the signal; the media layer applies it to the camera image.
  Effect::new(move |previous: Option<()>| {
    let effect = call.effect.get();
    if previous.is_some() {
      call.emit(CallEvent::Effect(effect));
    }
  });
  // The shell ended the call (the other side hung up, the connection dropped): leave the screen.
  let was_connected = std::cell::Cell::new(false);
  Effect::new(move |_| {
    let is_connected = state.read(|backend| backend.active_call().is_some());
    if was_connected.replace(is_connected) && !is_connected && call.phase.get_untracked() == CallPhase::Active
    {
      call.leave();
    }
  });
  Effect::new(move |_| {
    let ringing = state.read(|backend| backend.incoming_calls().first().copied());
    if call.phase.get_untracked() == CallPhase::Idle && call.incoming.get_untracked() != ringing {
      call.incoming.set(ringing);
    }
  });
}

/// How often idle time, OS state and attention are checked (events re-check at once).
const ACTIVITY_CHECK_MS: u64 = 5_000;
/// How often `/remind` reminders are checked.
const REMINDER_CHECK_MS: u64 = 15_000;
const MS_PER_MINUTE: f64 = 60_000.0;

/// What the activity watcher keeps between events.
struct ActivityWatch {
  state: AppState,
  last_input_at: Cell<TimestampMs>,
  is_idle: Cell<bool>,
  maybe_os: Cell<Option<tauri::OsAttention>>,
  reporter: RefCell<AttentionReporter>,
}

/// Two reports from the same input: no input for "Away after" marks this device idle (the server
/// shows the user as away), and attention (focused, visible, recent input, OS not idle, screen not
/// locked) tells the server to hold phone pushes back (notifications.md, "Attention").
fn watch_activity(state: AppState) {
  let watch = Rc::new(ActivityWatch {
    state,
    last_input_at: Cell::new(now_ms()),
    is_idle: Cell::new(false),
    maybe_os: Cell::new(None),
    reporter: RefCell::new(AttentionReporter::default()),
  });
  for event in ["pointermove", "pointerdown", "keydown", "touchstart", "wheel"] {
    let watch = watch.clone();
    on_window(event, move || watch.on_input());
  }
  for event in ["focus", "blur", "pageshow"] {
    let watch = watch.clone();
    on_window(event, move || watch.check());
  }
  {
    let watch = watch.clone();
    on_document("visibilitychange", move || watch.check());
  }
  // The window goes away: the server should not wait for the lease to run out.
  {
    let watch = watch.clone();
    on_window("pagehide", move || watch.leave());
  }
  watch.check();
  use_interval(ACTIVITY_CHECK_MS, move || {
    watch.check_idle();
    watch.check();
    let watch = watch.clone();
    leptos::task::spawn_local(async move {
      if let Some(os) = tauri::os_attention().await {
        watch.maybe_os.set(Some(os));
        watch.check();
      }
    });
  });
}

impl ActivityWatch {
  fn on_input(&self) {
    self.last_input_at.set(now_ms());
    if self.is_idle.replace(false) {
      self.state.read(|backend| backend.set_activity(false));
    }
    self.check();
  }

  fn check_idle(&self) {
    let Some(minutes) = self
      .state
      .settings
      .prefs
      .get_untracked()
      .privacy
      .away_after_minutes
    else {
      return;
    };
    let idle_for = (now_ms() - self.last_input_at.get()) as f64;
    if idle_for >= f64::from(minutes) * MS_PER_MINUTE && !self.is_idle.replace(true) {
      self.state.read(|backend| backend.set_activity(true));
    }
  }

  fn check(&self) {
    let notes = self.state.settings.prefs.get_untracked().notifications;
    // The user (or the instance) doesn't want attention used: tell the server once, then nothing.
    if !notes.uses_desktop_attention {
      self.leave();
      return;
    }
    let maybe_os = self.maybe_os.get();
    let signals = AttentionSignals {
      is_focused: window_has_focus(),
      is_visible: is_document_visible(),
      last_input_at: self.last_input_at.get(),
      maybe_os_idle: maybe_os
        .and_then(|os| os.maybe_idle_ms)
        .map(Duration::from_millis),
      is_screen_locked: maybe_os.is_some_and(|os| os.is_locked),
    };
    let window = Duration::from_secs(u64::from(notes.attention_input_secs));
    self.report(is_attentive(&signals, now_ms(), window));
  }

  fn report(&self, is_attentive: bool) {
    let maybe_due = self.reporter.borrow_mut().next(is_attentive, now_ms());
    self.send(maybe_due);
  }

  fn leave(&self) {
    let maybe_due = self.reporter.borrow_mut().leave();
    self.send(maybe_due);
  }

  fn send(&self, maybe_due: Option<bool>) {
    if let Some(is_attentive) = maybe_due {
      self.state.read(|backend| backend.set_attention(is_attentive));
    }
  }
}

pub(crate) fn on_window(event: &str, callback: impl Fn() + 'static) {
  let Some(window) = web_sys::window() else { return };
  listen_on(&window, event, callback);
}

pub(crate) fn on_document(event: &str, callback: impl Fn() + 'static) {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  listen_on(&document, event, callback);
}

fn listen_on(target: &web_sys::EventTarget, event: &str, callback: impl Fn() + 'static) {
  let listener = Closure::<dyn Fn()>::new(callback);
  if target
    .add_event_listener_with_callback(event, listener.as_ref().unchecked_ref())
    .is_ok()
  {
    // Lives as long as the page.
    listener.forget();
  }
}

fn is_document_visible() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| !document.hidden())
}
