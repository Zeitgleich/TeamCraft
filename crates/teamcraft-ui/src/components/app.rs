use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::backend::Backend;
use crate::calls::CallPhase;
use crate::clock::now_ms;
use crate::components::admin_console::AdminConsole;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::channel_view::ChannelView;
use crate::components::guest_join::GuestJoinPage;
use crate::components::incoming_call::IncomingCallDialog;
use crate::components::login::SessionGate;
use crate::components::meetings_view::{MeetingsView, ScheduleMeetingDialog};
use crate::components::members_panel::MembersPanel;
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
use crate::i18n::{provide_i18n, use_i18n};
use crate::meetings::parse_guest_link;
use crate::mock::MockBackend;
use crate::model::{ConversationId, SpaceId};
use crate::notifier::{NotificationTitle, Notifier, notifications_for};
use crate::notify_sync::NotificationSync;
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
  if tauri::is_available() {
    watch_for_notifications(state, i18n);
  }
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
          }}
        </main>
        <Show when=move || matches!(state.view.get(), MainView::Conversation(_)) fallback=|| ()>
          {move || match state.panel.get() {
            SidePanel::Closed => ().into_any(),
            SidePanel::Thread { conversation, thread } => {
              view! { <ThreadPanel conversation=conversation thread=thread /> }.into_any()
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
