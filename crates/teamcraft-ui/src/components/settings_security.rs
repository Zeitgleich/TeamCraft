//! Settings > Devices and Settings > Security & privacy. Devices: this device first, the others
//! with platform and last seen, what this device verified of each and how it gets pushes;
//! renaming, a test push, signing out (confirmed first) and linking a new one. Security: the state
//! of encryption and of the key backup in plain words with the next step, the recovery kit (or a
//! legacy passphrase), verifying people, the user's own key one level down, and what others can see.

use leptos::prelude::*;
use teamcraft_client_api::admin::{DeviceInfo, DevicePushStatus, DeviceVerification};
use teamcraft_client_api::privacy::LastSeenVisibility;
use teamcraft_client_api::security::{KeyBackupState, RecoveryMethod};
use teamcraft_proto::{PushProvider, TestPushOutcome};

use crate::auth::tauri;
use crate::clock::{format_relative, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::recovery_kit_setup::RecoveryKitSetup;
use crate::components::settings_rows::{
  ListFilter, MAX_LIST_ENTRIES, SelectRow, SettingRow, SettingsCard, SwitchRow, matches_filter, row_id,
  slug_options,
};
use crate::feedback::{ActionFailure, PendingAction};
use crate::i18n::{I18n, use_i18n};
use crate::model::{DeviceId, TimestampMs, UserId};
use crate::reactive::SetIfAlive;
use crate::state::{Overlay, use_app_state};

/// Same rule as at enrollment: short passphrases are cheap to guess offline.
const MIN_PASSPHRASE_CHARS: usize = 12;
/// The server's default limit for names (`limits.max_name_length`).
const MAX_DEVICE_NAME_CHARS: usize = 100;

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
        <For each=move || others.get() key=|device| device.clone() let:device>
          <li><DeviceItem device=device /></li>
        </For>
      </ul>
      <Show when=move || others.with(Vec::is_empty) fallback=|| ()>
        <p class="set-empty">{move || i18n.t("settings-device-no-others")}</p>
      </Show>
      <div class="set-card-actions">
        <button type="button" class="btn" id=row_id("settings-device-link") on:click=move |_| state.overlay.set(Overlay::LinkDevice)>
          <Icon name=IconName::QrCode />
          <span>{move || i18n.t("settings-device-link")}</span>
        </button>
        <Show when=move || others.with(|list| list.len() > 1) fallback=|| ()>
          <span class="set-inline" id=row_id("settings-device-sign-out-others")>
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
          </span>
        </Show>
      </div>
    </SettingsCard>
  }
}

/// One device: icon, name, platform and last seen, what this device verified of it and how it gets
/// pushes. Every device can be renamed and sent a test push (when it gets pushes); the others can
/// be signed out after a confirm.
#[component]
fn DeviceItem(device: DeviceInfo) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = device.id;
  let is_current = device.is_current;
  let signing_out = PendingAction::new();
  let testing = PendingAction::new();
  let is_confirming = RwSignal::new(false);
  let is_renaming = RwSignal::new(false);
  let name = device.name.clone();
  let platform = device.platform.clone();
  let last_seen = device.last_seen;
  let verification = device.verification;
  let maybe_push = device.maybe_push.clone();
  let can_test = maybe_push
    .as_ref()
    .is_some_and(|push| push.maybe_provider.is_some());
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
  let rename_name = name.clone();
  let send_test = move |_| testing.run(|| state.write(|backend| backend.test_push_my_device(id)));

  view! {
    <div class="set-device" class:is-current=is_current>
      <span class="set-device-icon" aria-hidden="true"><Icon name=device_icon(&device.platform) /></span>
      <div class="set-device-text">
        <span class="set-device-name" translate="no">
          {name}
          {is_current.then(|| view! { <span class="set-badge">{move || i18n.t("settings-device-current")}</span> })}
        </span>
        <span class="set-row-hint">{detail}</span>
        {verification_line(verification)}
        {maybe_push.map(push_lines)}
      </div>
      <div class="set-device-actions">
        <Show when=move || !is_renaming.get() fallback=|| ()>
          <button type="button" class="btn btn-small" on:click=move |_| { is_confirming.set(false); is_renaming.set(true); }>
            {move || i18n.t("settings-device-rename")}
          </button>
        </Show>
        {can_test.then(|| view! {
          <PendingButton pending=testing class="btn btn-small" on_click=send_test>
            {move || i18n.t("settings-device-test-push")}
          </PendingButton>
        })}
        {(!is_current).then(|| view! {
          <Show when=move || !is_confirming.get() fallback=|| ()>
            <PendingButton
              pending=signing_out
              class="btn btn-small btn-danger-quiet"
              on_click=move |_| { is_renaming.set(false); is_confirming.set(true); }
            >
              {move || i18n.t("settings-device-sign-out")}
            </PendingButton>
          </Show>
        })}
      </div>
      <Show when=move || is_renaming.get() fallback=|| ()>
        <RenameForm device=id current=rename_name.clone() is_open=is_renaming />
      </Show>
      {(!is_current).then(|| view! {
        <Show when=move || is_confirming.get() fallback=|| ()>
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
                  signing_out.run(|| state.write(|backend| backend.remove_my_device(id)));
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

/// Renames a device in place: the name is selected for typing over, Enter saves, Escape or
/// Cancel keeps the old one. The list shows the new name once the server answered.
#[component]
fn RenameForm(device: DeviceId, current: String, is_open: RwSignal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let draft = RwSignal::new(current.clone());
  let saving = PendingAction::new();
  let input = NodeRef::<leptos::html::Input>::new();
  // The form opens on a click: take the focus with the name selected for typing over.
  Effect::new(move |_| {
    if let Some(element) = input.get() {
      drop(element.focus());
      element.select();
    }
  });
  let input_id = format!("device-name-{device}");
  let label_for = input_id.clone();
  let current = StoredValue::new(current.trim().to_owned());
  let can_save = move || {
    let name = draft.get();
    let name = name.trim();
    !name.is_empty()
      && name.chars().count() <= MAX_DEVICE_NAME_CHARS
      && current.with_value(|current| name != current)
  };
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if !can_save() {
      return;
    }
    let name = draft.get_untracked().trim().to_owned();
    saving.run_and_then(
      || state.write(|backend| backend.rename_my_device(device, name)),
      move || is_open.set_if_alive(false),
    );
  };
  view! {
    <form class="set-device-rename" on:submit=save>
      <label class="visually-hidden" for=label_for>{move || i18n.t("settings-device-name")}</label>
      <input
        id=input_id
        class="input"
        type="text"
        autocomplete="off"
        maxlength=MAX_DEVICE_NAME_CHARS.to_string()
        node_ref=input
        prop:value=move || draft.get()
        on:input=move |ev| draft.set(event_target_value(&ev))
        on:keydown=move |ev| {
          if ev.key() == "Escape" {
            ev.stop_propagation();
            is_open.set(false);
          }
        }
      />
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
          {move || i18n.t("settings-device-rename-save")}
        </button>
      </div>
    </form>
  }
}

/// What this device verified of the device, in plain words; nothing when it couldn't check.
fn verification_line(verification: DeviceVerification) -> Option<AnyView> {
  let i18n = use_i18n();
  let (class, icon, key) = match verification {
    DeviceVerification::Verified => ("is-verified", IconName::Check, "settings-device-verified"),
    DeviceVerification::Pending => ("is-pending", IconName::Clock, "settings-device-pending"),
    DeviceVerification::Unverified => ("is-unverified", IconName::Alert, "settings-device-unverified"),
    DeviceVerification::Unknown => return None,
  };
  Some(
    view! {
      <span class=format!("set-device-check {class}")>
        <Icon name=icon />
        <span>{move || i18n.t(key)}</span>
      </span>
    }
    .into_any(),
  )
}

/// How the device gets pushes and when the last one went out, and the last test's outcome.
fn push_lines(push: DevicePushStatus) -> impl IntoView {
  let i18n = use_i18n();
  let summary = {
    let push = push.clone();
    move || push_summary(&push, i18n, now_ms())
  };
  let maybe_test = push.maybe_test.clone();
  view! {
    <span class="set-row-hint">{summary}</span>
    {maybe_test.map(|outcome| {
      let is_ok = outcome == TestPushOutcome::Sent;
      view! {
        <span class="set-row-hint set-device-test" class:is-warn=!is_ok role="status">
          {move || i18n.t(test_outcome_key(&outcome))}
        </span>
      }
    })}
  }
}

/// "Push notifications via Google (FCM) · Last push 5 minutes ago".
fn push_summary(push: &DevicePushStatus, i18n: I18n, now: TimestampMs) -> String {
  let Some(provider) = &push.maybe_provider else {
    return i18n.t("settings-device-push-off");
  };
  let service = match provider {
    PushProvider::Apns => i18n.t("settings-device-push-apns"),
    PushProvider::Fcm => i18n.t("settings-device-push-fcm"),
    PushProvider::UnifiedPush => i18n.t("settings-device-push-unifiedpush"),
    PushProvider::WebPush => i18n.t("settings-device-push-webpush"),
    PushProvider::Unknown(name) => name.clone(),
  };
  let via = i18n.t_with("settings-device-push-via", &[("service", &service)]);
  let locale = i18n.locale().tag();
  let has_failed_last = match (push.maybe_last_failed_at, push.maybe_last_sent_at) {
    (Some(failed), Some(sent)) => failed > sent,
    (Some(_), None) => true,
    _ => false,
  };
  let last = if has_failed_last {
    push.maybe_last_failed_at.map(|at| {
      i18n.t_with(
        "settings-device-push-failed",
        &[("when", &format_relative(at, now, locale))],
      )
    })
  } else {
    push.maybe_last_sent_at.map(|at| {
      i18n.t_with(
        "settings-device-push-last",
        &[("when", &format_relative(at, now, locale))],
      )
    })
  };
  match last {
    Some(last) => format!("{via} · {last}"),
    None => via,
  }
}

/// The i18n key for how the push service took a test push.
fn test_outcome_key(outcome: &TestPushOutcome) -> &'static str {
  match outcome {
    TestPushOutcome::Sent => "settings-device-test-sent",
    TestPushOutcome::NotRegistered => "settings-device-test-not-registered",
    TestPushOutcome::NotConfigured => "settings-device-test-not-configured",
    TestPushOutcome::Unavailable => "settings-device-test-unavailable",
    TestPushOutcome::Rejected => "settings-device-test-rejected",
    TestPushOutcome::Gone => "settings-device-test-gone",
    TestPushOutcome::Unknown(_) => "settings-device-test-unknown",
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
  let filter = RwSignal::new(String::new());
  let matching = Memo::new(move |_| {
    let query = filter.get();
    contacts.with(|list| {
      list
        .iter()
        .filter(|(user, _)| matches_filter(&user.display_name, &query))
        .cloned()
        .collect::<Vec<_>>()
    })
  });
  let shown =
    Memo::new(move |_| matching.with(|list| list.iter().take(MAX_LIST_ENTRIES).cloned().collect::<Vec<_>>()));
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
      <ListFilter label="security-verify-filter" query=filter matching=Signal::derive(move || matching.with(Vec::len)) />
      <ul class="set-devices">
        <For each=move || shown.get() key=|(user, verified)| (user.id, *verified) let:entry>
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

/// The key backup in plain words with the next step (issue #410), and how it is protected: the
/// recovery kit (e2ee.md §6.5) or, for accounts from before it, a passphrase (§6.2). A new kit
/// replaces the passphrase or the previous kit.
#[component]
fn RecoveryCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let backup = Memo::new(move |_| state.read(|backend| backend.key_backup()));
  let has =
    move |method: RecoveryMethod| backup.with(|maybe| maybe.as_ref().is_some_and(|state| state.has(method)));
  let has_kit = Memo::new(move |_| has(RecoveryMethod::RecoveryKey));
  let has_passphrase = Memo::new(move |_| has(RecoveryMethod::Passphrase));
  let is_passphrase_allowed =
    Memo::new(move |_| backup.with(|maybe| maybe.as_ref().is_none_or(|state| state.is_passphrase_allowed)));
  let editing = RwSignal::new(Editing::Nothing);
  let maybe_done = RwSignal::new(None::<&'static str>);
  let saving = PendingAction::new();
  let account = state.me().display_name;
  let save_kit = Callback::new(move |recovery_key: String| {
    saving.spawn(async move {
      crate::remote_backend::set_recovery_key(&recovery_key)
        .await
        .map_err(|error| ActionFailure::from_wire(&error))?;
      editing.set_if_alive(Editing::Nothing);
      maybe_done.set_if_alive(Some("security-kit-done"));
      Ok(())
    });
  });
  let kit_hint = Signal::derive(move || match maybe_done.get() {
    Some(key) => i18n.t(key),
    None => i18n.t("security-kit-hint"),
  });

  view! {
    <SettingsCard heading="security-backup-title">
      {move || backup.get().map(|state| backup_status(&state, i18n))}
      <SettingRow label="security-kit-row" live_hint=kit_hint>
        <Show when=move || editing.get() == Editing::Nothing fallback=|| ()>
          <button
            type="button"
            class="btn btn-small"
            class:btn-primary=move || !has_kit.get()
            on:click=move |_| {
              maybe_done.set(None);
              editing.set(Editing::Kit);
            }
          >
            {move || {
              if has_kit.get() {
                i18n.t("security-kit-new")
              } else if has_passphrase.get() {
                i18n.t("security-kit-replace")
              } else {
                i18n.t("security-kit-create")
              }
            }}
          </button>
        </Show>
      </SettingRow>
      <Show when=move || editing.get() == Editing::Kit fallback=|| ()>
        <div class="set-subform">
          <RecoveryKitSetup
            account=account.clone()
            finish_label=Signal::derive(move || i18n.t("security-kit-save"))
            is_busy=Signal::derive(move || saving.is_pending())
            on_confirmed=save_kit
            maybe_on_cancel=Callback::new(move |()| editing.set(Editing::Nothing))
          />
        </div>
      </Show>
      // A passphrase is only kept up for accounts that have one and may (e2ee.md §6.5.8).
      <Show
        when=move || has_passphrase.get() && !has_kit.get() && is_passphrase_allowed.get()
        fallback=|| ()
      >
        <PassphraseRow editing=editing maybe_done=maybe_done />
      </Show>
    </SettingsCard>
  }
}

/// Which form of the recovery card is open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Editing {
  Nothing,
  Kit,
  Passphrase,
}

/// Changing a legacy recovery passphrase (e2ee.md §6.2): the new one replaces the old wrap.
#[component]
fn PassphraseRow(editing: RwSignal<Editing>, maybe_done: RwSignal<Option<&'static str>>) -> impl IntoView {
  let i18n = use_i18n();
  let passphrase = RwSignal::new(String::new());
  let confirm = RwSignal::new(String::new());
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
        .map_err(|error| ActionFailure::from_wire(&error))?;
      passphrase.set_if_alive(String::new());
      confirm.set_if_alive(String::new());
      editing.set_if_alive(Editing::Nothing);
      maybe_done.set_if_alive(Some("recovery-done"));
      Ok(())
    });
  };
  let is_mismatch = move || is_long_enough() && !confirm.get().is_empty() && !do_match();
  view! {
    <SettingRow label="security-recovery-row" hint="recovery-hint">
      <Show when=move || editing.get() == Editing::Nothing fallback=|| ()>
        <button
          type="button"
          class="btn btn-small"
          on:click=move |_| {
            maybe_done.set(None);
            editing.set(Editing::Passphrase);
          }
        >
          {move || i18n.t("recovery-save")}
        </button>
      </Show>
    </SettingRow>
    <Show when=move || editing.get() == Editing::Passphrase fallback=|| ()>
      <form class="set-subform" on:submit=save>
        <label class="set-row-label" for="recovery-new">{move || i18n.t("recovery-new")}</label>
        <input
          id="recovery-new"
          class="input"
          type="password"
          autocomplete="new-password"
          data-autofocus
          aria-describedby="recovery-length-hint"
          prop:value=move || passphrase.get()
          on:input=move |ev| passphrase.set(event_target_value(&ev))
        />
        <label class="set-row-label" for="recovery-confirm">{move || i18n.t("recovery-confirm")}</label>
        <input
          id="recovery-confirm"
          class="input"
          type="password"
          autocomplete="new-password"
          aria-invalid=move || is_mismatch().then_some("true")
          aria-describedby=move || if is_mismatch() { "recovery-length-hint recovery-mismatch" } else { "recovery-length-hint" }
          prop:value=move || confirm.get()
          on:input=move |ev| confirm.set(event_target_value(&ev))
        />
        <p id="recovery-length-hint" class="set-row-hint">{move || i18n.t_with("recovery-length-hint", &[("count", &MIN_PASSPHRASE_CHARS.to_string())])}</p>
        <Show when=is_mismatch fallback=|| ()>
          <p id="recovery-mismatch" class="field-error" role="alert">{move || i18n.t("security-passphrase-mismatch")}</p>
        </Show>
        <div class="set-confirm-actions">
          <button type="button" class="btn btn-small" on:click=move |_| editing.set(Editing::Nothing)>{move || i18n.t("cancel")}</button>
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
  }
}

/// The key backup in plain words: whether it is on the server and how it can be restored, with
/// when the kit was made or the passphrase last set. No crypto terms on this level.
fn backup_status(state: &KeyBackupState, i18n: I18n) -> impl IntoView + use<> {
  let is_protected = state.has(RecoveryMethod::RecoveryKey) || state.has(RecoveryMethod::Passphrase);
  let is_ok = state.is_stored && is_protected;
  let title = if !state.is_stored {
    i18n.t("security-backup-missing")
  } else if is_protected {
    i18n.t("security-backup-ok")
  } else {
    i18n.t("security-backup-no-recovery")
  };
  let detail = backup_detail(state, i18n, now_ms());
  view! {
    <div class="set-backup" class:is-ok=is_ok class:is-warn=!is_ok role="status">
      <span class="set-backup-icon" aria-hidden="true">
        <Icon name=if is_ok { IconName::Shield } else { IconName::Alert } />
      </span>
      <div class="set-row-text">
        <span class="set-row-label">{title}</span>
        <span class="set-row-hint">{detail}</span>
      </div>
    </div>
  }
}

/// The sentences under the backup's state: how it is protected and what is still to do.
fn backup_detail(state: &KeyBackupState, i18n: I18n, now: TimestampMs) -> String {
  let locale = i18n.locale().tag();
  let mut parts = Vec::new();
  if !state.is_stored {
    parts.push(i18n.t("security-backup-missing-hint"));
  } else if state.has(RecoveryMethod::RecoveryKey) {
    parts.push(match state.maybe_recovery_key_set_at {
      Some(at) => i18n.t_with(
        "security-backup-kit-set",
        &[("when", &format_relative(at, now, locale))],
      ),
      None => i18n.t("security-backup-kit"),
    });
  } else if state.has(RecoveryMethod::Passphrase) {
    parts.push(match state.maybe_passphrase_set_at {
      Some(at) => i18n.t_with(
        "security-backup-passphrase-set",
        &[("when", &format_relative(at, now, locale))],
      ),
      None => i18n.t("security-backup-passphrase"),
    });
    parts.push(if state.is_passphrase_allowed {
      i18n.t("security-backup-replace-hint")
    } else {
      i18n.t("security-backup-passphrase-off")
    });
  } else {
    parts.push(i18n.t("security-backup-no-recovery-hint"));
  }
  if state.has(RecoveryMethod::Organization) {
    parts.push(i18n.t("security-backup-organization"));
  }
  if state.is_stored && state.has_pending_upload {
    parts.push(i18n.t("security-backup-pending"));
  }
  parts.join(" ")
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
