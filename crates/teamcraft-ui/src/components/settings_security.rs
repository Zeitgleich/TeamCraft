//! Settings > Devices and Settings > Security & privacy. Devices: this device first, the others
//! with platform and last seen, signing out (confirmed first) and linking a new one. Security: the
//! state of encryption in plain words with the next step, the recovery passphrase, verifying
//! people, the user's own key one level down, and what others can see.

use leptos::prelude::*;
use teamcraft_client_api::admin::DeviceInfo;
use teamcraft_client_api::privacy::LastSeenVisibility;

use crate::auth::tauri;
use crate::clock::{format_relative, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::settings_rows::{SelectRow, SettingRow, SettingsCard, SwitchRow, slug_options};
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::model::{DeviceId, UserId};
use crate::reactive::SetIfAlive;
use crate::state::{Overlay, use_app_state};

/// Same rule as at enrollment: short passphrases are cheap to guess offline.
const MIN_PASSPHRASE_CHARS: usize = 12;

#[component]
pub fn DevicesSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The list comes from the server: ask again whenever the section opens.
  state.read(|backend| backend.refresh_my_devices());
  let devices = Memo::new(move |_| state.read(|backend| backend.my_devices()));
  let current = Memo::new(move |_| devices.get().into_iter().find(|device| device.is_current));
  let others = Memo::new(move |_| {
    let mut list: Vec<DeviceInfo> = devices
      .get()
      .into_iter()
      .filter(|device| !device.is_current)
      .collect();
    list.sort_by_key(|device| std::cmp::Reverse(device.last_seen));
    list
  });
  let signing_out_all = PendingAction::new();
  let is_confirming_all = RwSignal::new(false);
  let sign_out_all = move |_| {
    let ids: Vec<DeviceId> = others
      .get_untracked()
      .into_iter()
      .map(|device| device.id)
      .collect();
    is_confirming_all.set(false);
    signing_out_all.run(|| {
      state.write(|backend| {
        for id in ids {
          backend.remove_my_device(id);
        }
      })
    });
  };

  view! {
    <SettingsCard heading="settings-device-this" hint="settings-devices-hint">
      {move || match current.get() {
        Some(device) => view! { <DeviceItem device=device /> }.into_any(),
        None => view! { <p class="set-empty">{move || i18n.t("settings-devices-unavailable")}</p> }.into_any(),
      }}
    </SettingsCard>
    <SettingsCard heading="settings-device-others">
      <ul class="set-devices">
        <For each=move || others.get() key=|device| (device.id, device.name.clone(), device.last_seen) let:device>
          <li><DeviceItem device=device /></li>
        </For>
      </ul>
      <Show when=move || others.with(Vec::is_empty) fallback=|| ()>
        <p class="set-empty">{move || i18n.t("settings-device-no-others")}</p>
      </Show>
      <div class="set-card-actions">
        <button type="button" class="btn" on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
          <Icon name=IconName::QrCode />
          <span>{move || i18n.t("settings-device-link")}</span>
        </button>
        <Show when=move || others.with(|list| list.len() > 1) fallback=|| ()>
          <Show
            when=move || is_confirming_all.get()
            fallback=move || view! {
              <PendingButton pending=signing_out_all class="btn btn-danger-quiet" on_click=move |_| is_confirming_all.set(true)>
                {move || i18n.t("settings-device-sign-out-others")}
              </PendingButton>
            }
          >
            <div class="set-confirm" role="alertdialog" aria-labelledby="sign-out-all-text">
              <p id="sign-out-all-text">
                {move || i18n.t_count("settings-device-sign-out-others-confirm", others.with(Vec::len))}
              </p>
              <div class="set-confirm-actions">
                <button type="button" class="btn btn-small" data-autofocus on:click=move |_| is_confirming_all.set(false)>
                  {move || i18n.t("cancel")}
                </button>
                <button type="button" class="btn btn-small btn-danger" on:click=sign_out_all>
                  {move || i18n.t("settings-device-sign-out-others")}
                </button>
              </div>
            </div>
          </Show>
        </Show>
      </div>
    </SettingsCard>
  }
}

/// One device: icon, name, platform and last seen; the others can be signed out after a confirm.
#[component]
fn DeviceItem(device: DeviceInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = device.id;
  let is_current = device.is_current;
  let pending = PendingAction::new();
  let is_confirming = RwSignal::new(false);
  let name = device.name.clone();
  let platform = device.platform.clone();
  let last_seen = device.last_seen;
  let detail = move || {
    let seen = if is_current {
      i18n.t("settings-device-active-now")
    } else {
      format_relative(last_seen, now_ms(), i18n.locale().tag())
    };
    if platform.is_empty() {
      seen
    } else {
      format!("{platform} · {seen}")
    }
  };
  let confirm_name = name.clone();

  view! {
    <div class="set-device" class:is-current=is_current>
      <span class="set-device-icon" aria-hidden="true"><Icon name=device_icon(&device.platform) /></span>
      <div class="set-device-text">
        <span class="set-device-name" translate="no">
          {name}
          {is_current.then(|| view! { <span class="set-badge">{move || i18n.t("settings-device-current")}</span> })}
        </span>
        <span class="set-row-hint">{detail}</span>
      </div>
      {(!is_current).then(|| view! {
        <Show
          when=move || is_confirming.get()
          fallback=move || view! {
            <PendingButton pending=pending class="btn btn-small" on_click=move |_| is_confirming.set(true)>
              {move || i18n.t("settings-device-sign-out")}
            </PendingButton>
          }
        >
          <div class="set-confirm is-inline" role="alertdialog" aria-label=move || i18n.t("settings-device-sign-out-confirm")>
            <p>{
              let confirm_name = confirm_name.clone();
              move || i18n.t_with("settings-device-sign-out-question", &[("name", &confirm_name)])
            }</p>
            <div class="set-confirm-actions">
              <button type="button" class="btn btn-small" data-autofocus on:click=move |_| is_confirming.set(false)>
                {move || i18n.t("cancel")}
              </button>
              <button
                type="button"
                class="btn btn-small btn-danger"
                on:click=move |_| {
                  is_confirming.set(false);
                  pending.run(|| state.write(|backend| backend.remove_my_device(id)));
                }
              >
                {move || i18n.t("settings-device-sign-out-confirm")}
              </button>
            </div>
          </div>
        </Show>
      })}
    </div>
  }
}

#[component]
pub fn SecuritySection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let fingerprint = Memo::new(move |_| {
    crate::security::group_fingerprint(&state.read(|backend| backend.my_identity_fingerprint()))
  });
  let contacts = Memo::new(move |_| {
    let me = state.me().id;
    state.read(|backend| {
      let mut list = backend
        .users()
        .into_iter()
        .filter(|user| user.id != me && !user.is_bot)
        .map(|user| {
          let verification = backend.contact_verification(user.id);
          (user, verification.is_verified)
        })
        .collect::<Vec<_>>();
      // Who still needs comparing comes first.
      list.sort_by_key(|(user, is_verified)| (*is_verified, user.display_name.to_lowercase()));
      list
    })
  });
  let verified_count =
    Memo::new(move |_| contacts.with(|list| list.iter().filter(|(_, is_verified)| *is_verified).count()));
  let has_keys = Memo::new(move |_| !fingerprint.with(String::is_empty));

  view! {
    <div class="set-status" class:is-ok=move || has_keys.get() class:is-warn=move || !has_keys.get() role="status">
      <span class="set-status-icon" aria-hidden="true">
        {move || view! { <Icon name=if has_keys.get() { IconName::Lock } else { IconName::Alert } /> }}
      </span>
      <div class="set-status-text">
        <strong>{move || if has_keys.get() { i18n.t("security-status-ok") } else { i18n.t("security-status-no-keys") }}</strong>
        <span>{move || if has_keys.get() { i18n.t("security-status-ok-hint") } else { i18n.t("security-my-key-unavailable") }}</span>
      </div>
      <Show when=move || !has_keys.get() fallback=|| ()>
        <button type="button" class="btn btn-small btn-primary" on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
          {move || i18n.t("security-status-set-up")}
        </button>
      </Show>
    </div>

    <Show when=tauri::has_core fallback=|| ()>
      <RecoveryCard />
    </Show>

    <SettingsCard heading="security-verify" hint="security-verify-hint">
      <p class="set-progress" role="status">
        {move || {
          i18n.t_with(
            "security-verified-count",
            &[("verified", &verified_count.get().to_string()), ("total", &contacts.with(Vec::len).to_string())],
          )
        }}
      </p>
      <ul class="set-devices">
        <For each=move || contacts.get() key=|(user, verified)| (user.id, *verified) let:entry>
          {
            let (user, is_verified) = entry;
            let user_id: UserId = user.id;
            view! {
              <li class="set-device">
                <span class="set-device-icon" aria-hidden="true"><Icon name=IconName::User /></span>
                <div class="set-device-text">
                  <span class="set-device-name" translate="no">{user.display_name.clone()}</span>
                  <span class="set-row-hint" class:ok-text=is_verified>
                    {move || if is_verified { i18n.t("contact-verified-short") } else { i18n.t("contact-not-verified") }}
                  </span>
                </div>
                <button type="button" class="btn btn-small" on:click=move |_| state.overlay.set(Overlay::VerifyContact(user_id))>
                  {move || i18n.t("security-compare")}
                </button>
              </li>
            }
          }
        </For>
      </ul>
    </SettingsCard>

    <PrivacyCard />

    <details class="set-details">
      <summary>{move || i18n.t("security-advanced")}</summary>
      <SettingsCard heading="security-my-key" hint="security-my-key-hint">
        <Show
          when=move || has_keys.get()
          fallback=move || view! { <p class="set-empty">{move || i18n.t("security-my-key-unavailable")}</p> }
        >
          <p class="fingerprint" translate="no">{move || fingerprint.get()}</p>
        </Show>
      </SettingsCard>
    </details>
  }
}

/// Replaces the recovery passphrase: the key backup is wrapped with the new one (e2ee.md §6.2).
#[component]
fn RecoveryCard() -> impl IntoView {
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let passphrase = RwSignal::new(String::new());
  let confirm = RwSignal::new(String::new());
  let is_done = RwSignal::new(false);
  let saving = PendingAction::new();
  let is_long_enough = move || passphrase.get().chars().count() >= MIN_PASSPHRASE_CHARS;
  let do_match = move || passphrase.get() == confirm.get();
  let can_save = move || is_long_enough() && do_match();
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if !can_save() {
      return;
    }
    let new_passphrase = passphrase.get_untracked();
    saving.spawn(async move {
      crate::remote_backend::change_passphrase(&new_passphrase)
        .await
        .map_err(|error| ActionFailure::new(FailureKind::Other, error))?;
      passphrase.set_if_alive(String::new());
      confirm.set_if_alive(String::new());
      is_open.set_if_alive(false);
      is_done.set_if_alive(true);
      Ok(())
    });
  };
  view! {
    <SettingsCard heading="recovery-title">
      <SettingRow
        label="security-recovery-row"
        live_hint=Signal::derive(move || if is_done.get() { i18n.t("recovery-done") } else { i18n.t("recovery-hint") })
      >
        <Show when=move || !is_open.get() fallback=|| ()>
          <button type="button" class="btn btn-small" on:click=move |_| { is_done.set(false); is_open.set(true); }>
            {move || i18n.t("recovery-save")}
          </button>
        </Show>
      </SettingRow>
      <Show when=move || is_open.get() fallback=|| ()>
        <form class="set-subform" on:submit=save>
          <label class="set-row-label" for="recovery-new">{move || i18n.t("recovery-new")}</label>
          <input
            id="recovery-new"
            class="input"
            type="password"
            autocomplete="new-password"
            data-autofocus
            prop:value=move || passphrase.get()
            on:input=move |ev| passphrase.set(event_target_value(&ev))
          />
          <label class="set-row-label" for="recovery-confirm">{move || i18n.t("enroll-confirm")}</label>
          <input
            id="recovery-confirm"
            class="input"
            type="password"
            autocomplete="new-password"
            prop:value=move || confirm.get()
            on:input=move |ev| confirm.set(event_target_value(&ev))
          />
          <p class="set-row-hint">{move || i18n.t_with("enroll-new-hint", &[("count", &MIN_PASSPHRASE_CHARS.to_string())])}</p>
          <Show when=move || is_long_enough() && !confirm.get().is_empty() && !do_match() fallback=|| ()>
            <p class="field-error" role="alert">{move || i18n.t("security-passphrase-mismatch")}</p>
          </Show>
          <div class="set-confirm-actions">
            <button type="button" class="btn btn-small" on:click=move |_| is_open.set(false)>{move || i18n.t("cancel")}</button>
            <button
              type="submit"
              class="btn btn-small btn-primary"
              prop:disabled=move || saving.is_pending() || !can_save()
              aria-busy=move || saving.is_pending().then_some("true")
            >
              <Show when=move || saving.is_pending() fallback=|| ()>
                <span class="spinner" role="status" aria-label=move || i18n.t("action-running")></span>
              </Show>
              {move || i18n.t("recovery-save")}
            </button>
          </div>
        </form>
      </Show>
    </SettingsCard>
  }
}

/// What others can see: read receipts, typing, last seen, and when the status turns to away.
#[component]
fn PrivacyCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let privacy = Memo::new(move |_| prefs.get().privacy);
  // Who may see when the user was last active lives on the server, not in the local preferences.
  let last_seen_choice = Memo::new(move |_| state.read(|backend| backend.privacy()));

  view! {
    <SettingsCard heading="privacy-share">
      <SwitchRow
        label="privacy-read-receipts"
        hint="privacy-read-receipts-hint"
        value=Signal::derive(move || privacy.get().share_read_receipts)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.privacy.share_read_receipts = value))
      />
      <SwitchRow
        label="privacy-typing"
        hint="privacy-typing-hint"
        value=Signal::derive(move || privacy.get().share_typing)
        on_change=Callback::new(move |value: bool| prefs.update(|all| all.privacy.share_typing = value))
      />
      <SwitchRow
        label="privacy-last-seen"
        hint="privacy-last-seen-hint"
        is_disabled=Signal::derive(move || !last_seen_choice.get().is_last_seen_available)
        value=Signal::derive(move || last_seen_choice.get().last_seen == LastSeenVisibility::Everyone)
        on_change=Callback::new(move |value: bool| {
          state.write(|backend| {
            backend.set_last_seen_visibility(if value {
              LastSeenVisibility::Everyone
            } else {
              LastSeenVisibility::Nobody
            })
          })
        })
      />
      <Show when=move || !last_seen_choice.get().is_last_seen_available fallback=|| ()>
        <p class="set-note">{move || i18n.t("privacy-last-seen-off")}</p>
      </Show>
      <SelectRow
        label="privacy-away"
        hint="privacy-away-hint"
        options=slug_options(&["5", "10", "15", "30", "never"], "privacy-away-")
        value=Signal::derive(move || privacy.get().away_after_minutes.map_or_else(|| "never".to_owned(), |minutes| minutes.to_string()))
        on_change=Callback::new(move |value: String| prefs.update(|all| all.privacy.away_after_minutes = value.parse::<u32>().ok()))
      />
    </SettingsCard>
    <p class="set-note">{move || i18n.t("privacy-offline-hint")}</p>
  }
}

/// A phone for mobile platforms, a screen for everything else.
fn device_icon(platform: &str) -> IconName {
  let platform = platform.to_lowercase();
  let is_mobile = ["android", "ios", "iphone", "ipad"]
    .iter()
    .any(|mobile| platform.contains(mobile));
  if is_mobile {
    IconName::Smartphone
  } else {
    IconName::Monitor
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn phones_and_computers_get_their_own_icon() {
    assert_eq!(device_icon("Android 15"), IconName::Smartphone);
    assert_eq!(device_icon("iOS"), IconName::Smartphone);
    assert_eq!(device_icon("Linux"), IconName::Monitor);
    assert_eq!(device_icon(""), IconName::Monitor);
  }
}
