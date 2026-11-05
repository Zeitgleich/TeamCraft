//! The global mute shortcut of the desktop app (off until switched on in Settings > Audio and
//! video): a key combination that mutes and unmutes the microphone while a call runs, whichever
//! app has the focus. The shell grabs the key (`mute_shortcut_set`); a press comes back as the
//! `toggle_mute` tray action (`tray_menu::run`). The key is only held during a call, so it stays
//! free for other apps the rest of the time.

use leptos::prelude::*;
use teamcraft_client_api::tray_menu::MuteShortcutStatus;

use crate::auth::tauri;
use crate::calls::CallPhase;
use crate::components::settings_rows::{SwitchRow, row_id};
use crate::i18n::use_i18n;
use crate::keys::Platform;
use crate::state::AppState;
use crate::state::use_app_state;

/// What a new install uses: Ctrl+Shift+M (Command+Shift+M on macOS).
pub const DEFAULT_SHORTCUT: &str = "CommandOrControl+Shift+M";

/// What the shell answered the last time the shortcut was asked for.
#[derive(Clone, Copy)]
struct ShortcutStatus(RwSignal<MuteShortcutStatus>);

/// Keeps the shell's shortcut in step with the setting: asked for during a call only, tried (and
/// let go again) outside one, so the settings can say whether the key is free. Released when the
/// app goes away.
pub fn start(state: AppState) {
  let status = RwSignal::new(MuteShortcutStatus::Off);
  provide_context(ShortcutStatus(status));
  let prefs = state.settings.prefs;
  let maybe_wanted = Memo::new(move |_| {
    let calls = prefs.get().calls;
    calls.mute_shortcut_enabled.then_some(calls.mute_shortcut)
  });
  let is_in_call = Memo::new(move |_| state.call.phase.get() == CallPhase::Active);
  Effect::new(move |_| {
    let maybe_shortcut = maybe_wanted.get();
    let is_active = is_in_call.get();
    leptos::task::spawn_local(async move {
      let answer = tauri::mute_shortcut_set(maybe_shortcut.as_deref(), is_active).await;
      status.try_set(answer);
    });
  });
  on_cleanup(|| {
    leptos::task::spawn_local(async {
      tauri::mute_shortcut_set(None, false).await;
    });
  });
}

/// The settings rows: the switch, and the key combination to change once it is on.
#[component]
pub fn MuteShortcutRows() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let calls = Memo::new(move |_| prefs.get().calls);
  let maybe_status = use_context::<ShortcutStatus>().map(|status| status.0);
  let is_capturing = RwSignal::new(false);
  let platform = Platform::current();
  let key_id = row_id("calls-mute-shortcut-key");
  view! {
    <SwitchRow
      label="calls-mute-shortcut"
      hint="calls-mute-shortcut-hint"
      value=Signal::derive(move || calls.get().mute_shortcut_enabled)
      on_change=Callback::new(move |value: bool| prefs.update(|all| all.calls.mute_shortcut_enabled = value))
    />
    <Show when=move || calls.get().mute_shortcut_enabled fallback=|| ()>
      <div class="set-row" id=key_id.clone()>
        <span class="set-row-text">
          <span class="set-row-label">{move || i18n.t("calls-mute-shortcut-key")}</span>
        </span>
        <button
          type="button"
          class="btn"
          on:click=move |_| is_capturing.set(true)
          on:blur=move |_| {
            let _ = is_capturing.try_set(false);
          }
          on:keydown=move |ev: web_sys::KeyboardEvent| {
            if !is_capturing.get_untracked() {
              return;
            }
            ev.prevent_default();
            ev.stop_propagation();
            if ev.code() == "Escape" {
              is_capturing.set(false);
              return;
            }
            let keys = HeldKeys {
              is_ctrl: ev.ctrl_key(),
              is_meta: ev.meta_key(),
              is_alt: ev.alt_key(),
              is_shift: ev.shift_key(),
            };
            if let Some(accelerator) = accelerator_from_keys(platform, keys, &ev.code()) {
              prefs.update(|all| all.calls.mute_shortcut = accelerator);
              is_capturing.set(false);
            }
          }
        >
          {move || match is_capturing.get() {
            true => i18n.t("calls-mute-shortcut-press"),
            false => display_accelerator(&calls.get().mute_shortcut, platform),
          }}
        </button>
      </div>
      {move || {
        let key = match maybe_status.map(|status| status.get()) {
          Some(MuteShortcutStatus::Taken) => "calls-mute-shortcut-taken",
          Some(MuteShortcutStatus::Invalid) => "calls-mute-shortcut-invalid",
          Some(MuteShortcutStatus::Unavailable) => "calls-mute-shortcut-unavailable",
          _ => return None,
        };
        Some(view! { <p class="set-note" role="status">{i18n.t(key)}</p> })
      }}
    </Show>
  }
}

/// The modifier keys down while a key was pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeldKeys {
  pub is_ctrl: bool,
  pub is_meta: bool,
  pub is_alt: bool,
  pub is_shift: bool,
}

/// The Tauri accelerator for a key press (`code` is `KeyboardEvent.code`), or `None` while it is
/// no shortcut yet: only modifiers so far, a key the shell can't name, or a plain letter that would
/// take the key away from typing. A shortcut needs Ctrl, Command or Alt, unless it is an F key.
pub fn accelerator_from_keys(platform: Platform, held: HeldKeys, code: &str) -> Option<String> {
  let key = key_name(code)?;
  let is_function_key = key.starts_with('F') && key.len() > 1;
  let (primary, other) = match platform {
    Platform::Mac => (held.is_meta, held.is_ctrl),
    Platform::Other => (held.is_ctrl, held.is_meta),
  };
  if !(primary || other || held.is_alt || is_function_key) {
    return None;
  }
  let mut parts = Vec::new();
  if primary {
    parts.push("CommandOrControl");
  }
  if other {
    parts.push(match platform {
      Platform::Mac => "Control",
      Platform::Other => "Super",
    });
  }
  if held.is_alt {
    parts.push("Alt");
  }
  if held.is_shift {
    parts.push("Shift");
  }
  parts.push(&key);
  Some(parts.join("+"))
}

/// The key of a `KeyboardEvent.code`, as Tauri names it.
fn key_name(code: &str) -> Option<String> {
  if let Some(letter) = code.strip_prefix("Key")
    && letter.len() == 1
  {
    return Some(letter.to_owned());
  }
  if let Some(digit) = code.strip_prefix("Digit")
    && digit.len() == 1
  {
    return Some(digit.to_owned());
  }
  if let Some(number) = code.strip_prefix('F')
    && number.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n))
  {
    return Some(code.to_owned());
  }
  match code {
    "Space" | "Enter" | "Tab" | "Backquote" | "Comma" | "Period" | "Slash" | "Backslash" | "Semicolon"
    | "Quote" | "Minus" | "Equal" | "BracketLeft" | "BracketRight" | "Home" | "End" | "PageUp"
    | "PageDown" | "Insert" | "ArrowUp" | "ArrowDown" | "ArrowLeft" | "ArrowRight" => Some(code.to_owned()),
    _ => None,
  }
}

/// An accelerator as people read it: `Ctrl+Shift+M`, or `⌘⇧M` on macOS.
pub fn display_accelerator(accelerator: &str, platform: Platform) -> String {
  let parts: Vec<String> = accelerator
    .split('+')
    .map(|part| {
      let named = match (part, platform) {
        ("CommandOrControl", Platform::Mac) => "⌘",
        ("CommandOrControl", Platform::Other) => "Ctrl",
        ("Control", Platform::Mac) => "⌃",
        ("Super", Platform::Mac) => "⌘",
        ("Super", Platform::Other) => "Win",
        ("Alt", Platform::Mac) => "⌥",
        ("Shift", Platform::Mac) => "⇧",
        _ => part,
      };
      named.to_owned()
    })
    .collect();
  match platform {
    Platform::Mac => parts.concat(),
    Platform::Other => parts.join("+"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CTRL: HeldKeys = HeldKeys {
    is_ctrl: true,
    is_meta: false,
    is_alt: false,
    is_shift: false,
  };

  #[test]
  fn the_default_is_a_shortcut_the_picker_could_have_made() {
    let held = HeldKeys {
      is_shift: true,
      ..CTRL
    };
    assert_eq!(
      accelerator_from_keys(Platform::Other, held, "KeyM").as_deref(),
      Some(DEFAULT_SHORTCUT)
    );
    let mac = HeldKeys {
      is_ctrl: false,
      is_meta: true,
      is_shift: true,
      ..CTRL
    };
    assert_eq!(
      accelerator_from_keys(Platform::Mac, mac, "KeyM").as_deref(),
      Some(DEFAULT_SHORTCUT)
    );
  }

  #[test]
  fn the_other_primary_key_is_named_for_itself() {
    let mac = HeldKeys {
      is_meta: true,
      ..CTRL
    };
    assert_eq!(
      accelerator_from_keys(Platform::Mac, mac, "Digit1").as_deref(),
      Some("CommandOrControl+Control+1")
    );
    let other = HeldKeys {
      is_meta: true,
      ..CTRL
    };
    assert_eq!(
      accelerator_from_keys(Platform::Other, other, "F9").as_deref(),
      Some("CommandOrControl+Super+F9")
    );
  }

  #[test]
  fn a_plain_key_is_no_shortcut_but_an_f_key_is() {
    let none = HeldKeys::default();
    assert_eq!(accelerator_from_keys(Platform::Other, none, "KeyM"), None);
    let shift = HeldKeys {
      is_shift: true,
      ..none
    };
    assert_eq!(accelerator_from_keys(Platform::Other, shift, "KeyM"), None);
    assert_eq!(
      accelerator_from_keys(Platform::Other, none, "F13").as_deref(),
      Some("F13")
    );
  }

  #[test]
  fn modifiers_alone_and_unknown_keys_wait_for_a_real_key() {
    for code in [
      "ControlLeft",
      "ShiftRight",
      "AltLeft",
      "MetaLeft",
      "Escape",
      "F25",
      "F0",
      "Numpad5",
      "",
    ] {
      assert_eq!(accelerator_from_keys(Platform::Other, CTRL, code), None, "{code}");
    }
  }

  #[test]
  fn named_keys_keep_their_names() {
    let alt = HeldKeys {
      is_alt: true,
      ..HeldKeys::default()
    };
    assert_eq!(
      accelerator_from_keys(Platform::Other, alt, "Space").as_deref(),
      Some("Alt+Space")
    );
    assert_eq!(
      accelerator_from_keys(Platform::Other, alt, "ArrowUp").as_deref(),
      Some("Alt+ArrowUp")
    );
  }

  #[test]
  fn the_shortcut_reads_the_way_the_platform_writes_it() {
    assert_eq!(
      display_accelerator(DEFAULT_SHORTCUT, Platform::Other),
      "Ctrl+Shift+M"
    );
    assert_eq!(display_accelerator(DEFAULT_SHORTCUT, Platform::Mac), "⌘⇧M");
    assert_eq!(display_accelerator("Alt+F9", Platform::Mac), "⌥F9");
    assert_eq!(
      display_accelerator("CommandOrControl+Super+F9", Platform::Other),
      "Ctrl+Win+F9"
    );
  }
}
