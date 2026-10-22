use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::auth::tauri;
use crate::backend::Backend;
use crate::calls::CallEvent;
use crate::calls::CallPhase;
use crate::clock::now_ms;
use crate::components::admin_console::AdminConsole;
use crate::components::bookmarks::BookmarksDialog;
use crate::components::bundle_banner::BundleBanner;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::channel_view::ChannelView;
use crate::components::directory_view::DirectoryView;
use crate::components::file_views::{FilesPanel, Gallery};
use crate::components::guest_join::GuestJoinPage;
use crate::components::icons::{Icon, IconName};
use crate::components::incoming_call::IncomingCallDialog;
use crate::components::login::SessionGate;
use crate::components::meetings_view::{MeetingsView, ScheduleMeetingDialog};
use crate::components::members_panel::MembersPanel;
use crate::components::message_actions::{ForwardDialog, PinsPanel, SavedView};
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
use crate::components::thread_panel::ThreadPanel;
use crate::components::threads_inbox::ThreadsInbox;
use crate::components::top_bar::TopBar;
use crate::components::workspace_dialogs::{
  BrowseChannelsDialog, ChannelSettingsDialog, NewChannelDialog, NewSpaceDialog, SpaceSettingsDialog,
};
use crate::i18n::{provide_i18n, use_i18n};
use crate::meetings::parse_guest_link;
use crate::mock::MockBackend;
use crate::model::{ConversationId, SpaceId};
use crate::notifier::{NotificationTitle, Notifier, notifications_for};
use crate::notify_sync::NotificationSync;
use crate::reactive::use_interval;
use crate::settings::{BackendMode, Settings, Theme};
use crate::state::{AppState, MainView, Overlay, SidePanel};

#[component]
pub fn App() -> impl IntoView {
  let settings = Settings::load();
  let i18n = provide_i18n(settings.locale.get_untracked());
  // The settings signal is the source of truth; mirror it into the translator.
  Effect::new(move |_| i18n.set_locale(settings.locale.get()));
  apply_theme(settings.theme);
  apply_density(settings);
  apply_language_attribute();

  // A personal meeting link needs no sign-in: it gets its own page before anything else.
  if let Some((meeting, invite_token)) = guest_link_from_location() {
    return view! { <GuestJoinPage meeting=meeting invite_token=invite_token /> }.into_any();
  }

  // What the service worker says about the code that is running (docs/e2ee.md §9.3).
  let bundle = crate::bundle::Bundle(RwSignal::new(crate::bundle::BundleState::Unknown));
  provide_context(bundle);
  if !tauri::is_available() {
    crate::bundle::watch(bundle.0);
  }

  match BackendMode::load() {
    BackendMode::Mock => {
      let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(now_ms()));
      view! { <Shell backend=backend settings=settings /> }.into_any()
    },
    BackendMode::Server => view! { <SessionGate settings=settings /> }.into_any(),
  }
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
  // Preferences the server enforces (push, do not disturb, mutes) follow the account on the server.
  if let Some(sync) = use_context::<StoredValue<NotificationSync, LocalStorage>>() {
    sync.get_value().start(settings);
  }
  if tauri::has_core() {
    watch_for_notifications(state, i18n);
  }
  if !tauri::is_available() {
    open_conversations_from_notifications(state);
  }
  let indexed_toasts = Memo::new(move |_| {
    state
      .toasts
      .get()
      .into_iter()
      .enumerate()
      .collect::<Vec<(usize, String)>>()
  });
  // `/remind` reminders come due while the app is open.
  crate::reactive::use_interval(REMINDER_CHECK_MS, move || {
    for reminder in crate::components::composer_commands::due_reminders(now_ms()) {
      let message = i18n.t_with("reminder-due", &[("text", &reminder.text)]);
      state.toasts.update(|list| list.push(message.clone()));
      leptos::task::spawn_local(async move { tauri::notify("TeamCraft", Some(&message)).await });
    }
  });
  if tauri::is_available() {
    connect_calls(state);
  }
  watch_idle(state);
  // Call defaults follow the settings (and set the initial state of the call session).
  let call_prefs = Memo::new(move |_| settings.prefs.get().calls);
  Effect::new(move |_| apply_call_prefs(state.call, &call_prefs.get()));
  if let Some((_, conversation)) = start {
    state.open_conversation(conversation);
  }

  let _ = window_event_listener(ev::keydown, move |ev| handle_global_shortcut(&ev, state));

  view! {
    <a class="skip-link" href="#main-content">
      {move || i18n.t("skip-to-content")}
    </a>
    <div class="app">
      <BundleBanner />
      <TopBar />
      <div class="app-body">
        // The administration console takes the whole window, like a system console.
        <Show when=move || !matches!(state.view.get(), MainView::Admin(_)) fallback=|| ()>
          <SpaceRail />
          <Sidebar />
        </Show>
        <main id="main-content" class="main" tabindex="-1">
          {move || match state.view.get() {
            MainView::Conversation(id) => view! { <ChannelView conversation=id /> }.into_any(),
            MainView::Threads => view! { <ThreadsInbox /> }.into_any(),
            MainView::Admin(section) => view! { <AdminConsole section=section /> }.into_any(),
            MainView::Search => view! { <SearchView /> }.into_any(),
            MainView::Meetings => view! { <MeetingsView /> }.into_any(),
            MainView::Saved => view! { <SavedView /> }.into_any(),
            MainView::Directory => view! { <DirectoryView /> }.into_any(),
          }}
        </main>
        <Show when=move || matches!(state.view.get(), MainView::Conversation(_)) fallback=|| ()>
          {move || match state.panel.get() {
            SidePanel::Closed => ().into_any(),
            SidePanel::Thread { conversation, thread } => {
              view! { <ThreadPanel conversation=conversation thread=thread /> }.into_any()
            },
            SidePanel::Files => match state.current_conversation_id() {
              Some(id) => view! { <FilesPanel conversation=id /> }.into_any(),
              None => ().into_any(),
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
          <For each=move || indexed_toasts.get() key=|(index, text)| (*index, text.clone()) let:entry>
            {
              let (index, text) = entry;
              view! {
                <div class="toast">
                  <span>{text}</span>
                  <button class="icon-btn icon-btn-small" aria-label=move || i18n.t("close") on:click=move |_| state.toasts.update(|list| { if index < list.len() { list.remove(index); } })>
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
        Overlay::Gallery(conversation, message) => view! { <Gallery conversation=conversation start=message /> }.into_any(),
        Overlay::NewSpace => view! { <NewSpaceDialog /> }.into_any(),
        Overlay::NewChannel(space) => view! { <NewChannelDialog space=space /> }.into_any(),
        Overlay::BrowseChannels(space) => view! { <BrowseChannelsDialog space=space /> }.into_any(),
        Overlay::ChannelSettings(id) => view! { <ChannelSettingsDialog conversation=id /> }.into_any(),
        Overlay::SpaceSettings(space) => view! { <SpaceSettingsDialog space=space /> }.into_any(),
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
  if modifier && ev.shift_key() && !ev.alt_key() && key == "f" {
    ev.prevent_default();
    state.open_search(None);
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

/// Desktop only: new mentions and DMs become system notifications, decided by the user's preferences.
fn watch_for_notifications(state: AppState, i18n: crate::i18n::I18n) {
  let notifier = Notifier::default();
  let settings = state.settings;
  Effect::new(move |_| {
    // Reading through the state subscribes to every backend change.
    let found = state.read(|backend| notifier.collect(backend));
    if found.is_empty() {
      return;
    }
    let prefs = settings.prefs.get_untracked().notifications;
    let current = state.current_conversation_id();
    let has_focus = window_has_focus();
    let notifications = state.read(|backend| {
      notifications_for(found, &prefs, backend, |conversation| {
        has_focus && current == Some(conversation)
      })
    });
    for notification in notifications {
      let title = match &notification.title {
        NotificationTitle::Sender(name) => name.clone(),
        NotificationTitle::InChannel { sender, channel } => {
          i18n.t_with("notify-in-channel", &[("name", sender), ("channel", channel)])
        },
        NotificationTitle::ThreadReply { sender } => i18n.t_with("notify-thread-reply", &[("name", sender)]),
      };
      let body = notification.body.unwrap_or_else(|| i18n.t("notify-new-message"));
      leptos::task::spawn_local(async move { tauri::notify(&title, Some(&body)).await });
    }
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
fn connect_calls(state: AppState) {
  let call = state.call;
  call.set_handler(move |event| {
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
      CallEvent::ScreenShare(true) => backend.start_screen_share(),
      CallEvent::ScreenShare(false) => backend.stop_screen_share(),
    });
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

/// How often the idle check runs.
const IDLE_CHECK_MS: u64 = 10_000;
/// How often `/remind` reminders are checked.
const REMINDER_CHECK_MS: u64 = 15_000;
const MS_PER_MINUTE: f64 = 60_000.0;

/// No input for the configured time marks this device idle (the server then shows the user as
/// away); the next input marks it active again. The setting is Privacy > "Away after".
fn watch_idle(state: AppState) {
  let last_input = std::rc::Rc::new(std::cell::Cell::new(js_sys::Date::now()));
  let is_idle = std::rc::Rc::new(std::cell::Cell::new(false));
  let mark_active: std::rc::Rc<dyn Fn()> = {
    let (last_input, is_idle) = (last_input.clone(), is_idle.clone());
    std::rc::Rc::new(move || {
      last_input.set(js_sys::Date::now());
      if is_idle.replace(false) {
        state.read(|backend| backend.set_activity(false));
      }
    })
  };
  let (on_move, on_press, on_key, on_touch) = (
    mark_active.clone(),
    mark_active.clone(),
    mark_active.clone(),
    mark_active,
  );
  let _ = window_event_listener(ev::pointermove, move |_| on_move());
  let _ = window_event_listener(ev::pointerdown, move |_| on_press());
  let _ = window_event_listener(ev::keydown, move |_| on_key());
  let _ = window_event_listener(ev::touchstart, move |_| on_touch());
  use_interval(IDLE_CHECK_MS, move || {
    let Some(minutes) = state.settings.prefs.get_untracked().privacy.away_after_minutes else {
      return;
    };
    let idle_for = js_sys::Date::now() - last_input.get();
    if idle_for >= f64::from(minutes) * MS_PER_MINUTE && !is_idle.replace(true) {
      state.read(|backend| backend.set_activity(true));
    }
  });
}
