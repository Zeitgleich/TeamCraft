//! The small call window of the desktop app (#2077), from the main window's side: whenever the
//! user's call changes, what the window shows ([`CallMini`]) goes to the shell with the user's
//! choice of when it shows (Settings > Audio and video). The shell opens and closes the window
//! from the main window's focus, and the window's buttons come back as tray actions
//! (`tray_menu::run`), so they take the path of the tray menu's call controls.
//!
//! What changes every second (the clock) isn't sent: the window counts from the join itself.

use leptos::prelude::*;
use teamcraft_client_api::call_mini::{CallMini, CallMiniPerson, CallMiniPolicy};
use teamcraft_client_api::call_window::CallWindowTheme;

use crate::auth::tauri;
use crate::calls::CallPhase;
use crate::i18n::I18n;
use crate::meetings::MeetingsHub;
use crate::settings::Theme;
use crate::state::AppState;
use crate::tray_menu::{asks_handover, call_title};

const MS_PER_MINUTE: u64 = 60_000;

/// Keeps the shell's small call window in step with the call (desktop app). Sent again only when
/// what the window shows or the user's choice changed; a call that ends or the end of the session
/// (sign-out) closes it.
pub fn start(state: AppState, i18n: I18n) {
  let maybe_hub = use_context::<MeetingsHub>();
  let call = state.call;
  // The join time is re-read when the call starts and once a minute (a resumed call may correct
  // it), not with every tick of the clock.
  let call_minutes = Memo::new(move |_| call.elapsed_ms.get().map(|ms| ms / MS_PER_MINUTE));
  // Whether the page is out of sight (minimized, hidden to the tray) goes to the shell too: not
  // every window manager moves the focus with a minimize. Every change is sent, also one back to
  // what was sent last: the shell forgets "hidden" on "back to the call".
  let sight_changes = RwSignal::new(0_u32);
  let handle = window_event_listener_untyped("visibilitychange", move |_| {
    sight_changes.try_update(|changes| *changes = changes.wrapping_add(1));
  });
  on_cleanup(move || handle.remove());
  Effect::new(
    move |previous: Option<(Option<CallMini>, CallMiniPolicy, bool, u32)>| {
      let _ = call_minutes.get();
      let policy = state.settings.prefs.with(|prefs| prefs.calls.small_window);
      let maybe_mini = (call.phase.get() == CallPhase::Active)
        .then(|| call.maybe_request.get())
        .flatten()
        .map(|request| {
          let maybe_active = state.read(|backend| backend.active_call());
          let people = state.read(|backend| backend.call_participants(request.conversation).len());
          let maybe_speaker = maybe_active
            .as_ref()
            .and_then(|active| {
              active
                .active_speakers
                .iter()
                .find(|user| **user != state.me().id)
                .copied()
            })
            .and_then(|user| state.read(|backend| backend.user(user)))
            .map(|user| CallMiniPerson {
              initials: user.initials(),
              name: user.display_name,
              avatar_hue: user.avatar_hue,
            });
          CallMini {
            title: call_title(state, i18n, maybe_hub, &request),
            maybe_speaker,
            people: u32::try_from(people + 1).unwrap_or(u32::MAX),
            maybe_joined_at_ms: call.joined_at(),
            is_reconnecting: maybe_active.as_ref().is_some_and(|active| active.is_reconnecting),
            is_muted: !call.is_mic_on.get(),
            is_camera_on: call.is_camera_on.get(),
            is_sound_off: call.is_deafened.get(),
            asks_handover: asks_handover(state, maybe_hub, request.conversation),
            theme: window_theme(state.settings.theme.get()),
          }
        });
      let next = (maybe_mini, policy, is_page_hidden(), sight_changes.get());
      if previous.as_ref() != Some(&next) {
        let (sent, policy, is_hidden, _) = next.clone();
        leptos::task::spawn_local(
          async move { tauri::call_mini_set(sent.as_ref(), policy, is_hidden).await },
        );
      }
      next
    },
  );
  // Signed out (the signed-in app is gone): no call, no window.
  on_cleanup(|| {
    leptos::task::spawn_local(async { tauri::call_mini_set(None, CallMiniPolicy::Never, false).await });
  });
}

/// The page is out of sight: its window is minimized or hidden to the tray.
fn is_page_hidden() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .is_some_and(|document| document.hidden())
}

/// The app's theme, which the window follows.
pub fn window_theme(theme: Theme) -> CallWindowTheme {
  match theme {
    Theme::System => CallWindowTheme::System,
    Theme::Light => CallWindowTheme::Light,
    Theme::Dark => CallWindowTheme::Dark,
  }
}
