use std::rc::Rc;

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::backend::Backend;
use crate::calls::CallPhase;
use crate::clock::now_ms;
use crate::components::admin_console::AdminConsole;
use crate::components::call_prejoin::PreJoinScreen;
use crate::components::call_screen::CallScreen;
use crate::components::channel_view::ChannelView;
use crate::components::incoming_call::IncomingCallDialog;
use crate::components::login::SessionGate;
use crate::components::members_panel::MembersPanel;
use crate::components::quick_switcher::QuickSwitcher;
use crate::components::rail::SpaceRail;
use crate::components::security_ui::{DeviceApprovalDialog, LinkDeviceDialog, VerifyKeyDialog};
use crate::components::settings_modal::{SettingsModal, ShortcutsModal};
use crate::components::sidebar::Sidebar;
use crate::components::status_menu::CustomStatusDialog;
use crate::components::thread_panel::ThreadPanel;
use crate::components::threads_inbox::ThreadsInbox;
use crate::components::top_bar::TopBar;
use crate::i18n::{provide_i18n, use_i18n};
use crate::mock::MockBackend;
use crate::model::{ConversationId, SpaceId};
use crate::settings::{BackendMode, Settings, Theme};
use crate::state::{AppState, MainView, Overlay, SidePanel};

#[component]
pub fn App() -> impl IntoView {
  let settings = Settings::load();
  let i18n = provide_i18n(settings.locale.get_untracked());
  // The settings signal is the source of truth; mirror it into the translator.
  Effect::new(move |_| i18n.set_locale(settings.locale.get()));
  apply_theme(settings.theme);
  apply_language_attribute();

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
  let state = AppState::new(backend, settings, start);
  provide_context(state);
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
        Overlay::VerifyKey(conversation) => view! { <VerifyKeyDialog conversation=conversation /> }.into_any(),
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
