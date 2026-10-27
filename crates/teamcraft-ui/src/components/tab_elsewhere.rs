//! The page a browser tab shows while another tab of the account runs the app (#744,
//! tab_handover.rs): "already open in another tab" or "opened in another tab", with "Use here".

use std::cell::OnceCell;

use leptos::prelude::*;

use crate::components::login::AuthFrame;
use crate::i18n::use_i18n;
use crate::tab_handover::{Blocker, InactiveCause, TabRole, TabState, Takeover};

thread_local! {
  /// The tab's state for the UI, fed by the core's tab listener once per page.
  static TAB: OnceCell<ArcRwSignal<TabState>> = const { OnceCell::new() };
}

/// The app while it runs in this tab; the notice while another tab runs it.
#[component]
pub fn TabGate(children: ChildrenFn) -> impl IntoView {
  let tab = use_tab_state();
  let is_active = Memo::new(move |_| tab.get().is_active());
  move || {
    if is_active.get() {
      children().into_any()
    } else {
      view! { <TabElsewhere tab=tab /> }.into_any()
    }
  }
}

/// Where this tab stands. Outside the browser (the desktop app) it is always the active one.
fn use_tab_state() -> Signal<TabState> {
  let signal = TAB.with(|cell| {
    cell
      .get_or_init(|| {
        let signal = ArcRwSignal::new(current_state());
        let feed = signal.clone();
        listen(move || feed.set(current_state()));
        signal
      })
      .clone()
  });
  Signal::derive(move || signal.get())
}

#[cfg(target_arch = "wasm32")]
fn current_state() -> TabState {
  crate::web_core::tab_state()
}

#[cfg(not(target_arch = "wasm32"))]
fn current_state() -> TabState {
  TabState::default()
}

#[cfg(target_arch = "wasm32")]
fn listen(callback: impl FnMut() + 'static) {
  crate::web_core::listen_tab(callback);
}

#[cfg(not(target_arch = "wasm32"))]
fn listen(_callback: impl FnMut() + 'static) {}

#[cfg(target_arch = "wasm32")]
fn use_here() {
  leptos::task::spawn_local(crate::web_core::use_here());
}

#[cfg(not(target_arch = "wasm32"))]
fn use_here() {}

#[cfg(target_arch = "wasm32")]
fn cancel_takeover() {
  crate::web_core::cancel_takeover();
}

#[cfg(not(target_arch = "wasm32"))]
fn cancel_takeover() {}

/// The notice: what happened, "Use here" and "Close"; after a refusal, the call that blocks it.
#[component]
fn TabElsewhere(tab: Signal<TabState>) -> impl IntoView {
  let i18n = use_i18n();
  let is_close_refused = RwSignal::new(false);
  // The loader hands over to the first screen that waits for the user; this is one.
  crate::startup::first_view_shown();
  let is_blocked = move || matches!(tab.get().takeover, Takeover::Blocked(_));
  let title_key = move || match (tab.get().role, tab.get().takeover) {
    (_, Takeover::Blocked(Blocker::Call)) => "tab-call-title",
    (TabRole::Inactive(InactiveCause::MovedAway), _) => "tab-moved-title",
    _ => "tab-elsewhere-title",
  };
  let body_key = move || match (tab.get().role, tab.get().takeover) {
    (_, Takeover::Blocked(Blocker::Call)) => "tab-call-body",
    (TabRole::Inactive(InactiveCause::MovedAway), _) => "tab-moved-body",
    _ => "tab-elsewhere-body",
  };
  let is_asking = move || tab.get().takeover == Takeover::Asking;
  let close = move |_| {
    // Browsers only let a page close a window a script opened; otherwise say how.
    if let Some(window) = web_sys::window() {
      drop(window.close());
    }
    is_close_refused.set(true);
  };

  view! {
    <AuthFrame>
      <div class="tab-elsewhere">
        <h1>{move || i18n.t(title_key())}</h1>
        <p class="tab-elsewhere-body">{move || i18n.t(body_key())}</p>
        <Show when=move || tab.get().takeover == Takeover::TimedOut fallback=|| ()>
          <p class="tab-elsewhere-error" role="alert">{move || i18n.t("tab-timed-out")}</p>
        </Show>
        <div class="auth-actions">
          <button
            type="button"
            class=move || if is_blocked() { "btn btn-danger auth-wide" } else { "btn btn-primary auth-wide" }
            prop:disabled=is_asking
            aria-busy=move || is_asking().then_some("true")
            on:click=move |_| use_here()
          >
            <Show when=is_asking fallback=|| ()>
              <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
            </Show>
            {move || i18n.t(if is_blocked() { "tab-end-call-use-here" } else { "tab-use-here" })}
          </button>
          {move || {
            if is_blocked() {
              view! {
                <button type="button" class="btn auth-wide" on:click=move |_| cancel_takeover()>
                  {move || i18n.t("tab-cancel")}
                </button>
              }
              .into_any()
            } else {
              view! {
                <button type="button" class="btn auth-wide" on:click=close>
                  {move || i18n.t("tab-close")}
                </button>
              }
              .into_any()
            }
          }}
        </div>
        <Show when=move || is_close_refused.get() fallback=|| ()>
          <p class="tab-elsewhere-hint" role="status">{move || i18n.t("tab-close-hint")}</p>
        </Show>
      </div>
    </AuthFrame>
  }
}
