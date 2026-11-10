//! First run on this device, as a guided sequence (issue #1701): one thing per step, a step
//! indicator, one main action and quieter ways back.
//!
//! - **New account** (e2ee.md §6.5.4): what encryption and the recovery kit are → save the kit →
//!   check it → how to keep it safe, name the device, finish. The key protects nothing until the
//!   last step.
//! - **Account with keys** (§6.5.5, §5): choose how to bring the key here → this computer's
//!   keychain (at once), another device (link), or the recovery kit.
//!
//! A restart mid-way resumes where it can without keeping a secret: the chosen way for an account
//! with keys, and for a new account the kit step, with a note that the key shown before (and a kit
//! saved from it) no longer counts. Only these markers go to `localStorage`, never the key.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::failure::{ActionFailure, FailureKind};
use teamcraft_client_api::remote::{CoreStatus, EnrollSecret, LinkStatus};
use teamcraft_crypto::recovery_key::{RecoveryKey, RecoveryKeyError};
use wasm_bindgen::JsCast;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::login::AuthFrame;
use crate::components::recovery_kit_setup::{
  KitCheckStep, KitFilePicker, KitSaveStep, SecondaryAction, new_kit_key,
};
use crate::components::security_ui::QrImage;
use crate::feedback::failure_message;
use crate::form_errors::focus_first_invalid;
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::reactive::{SetIfAlive, use_interval};
use crate::recovery_kit::KitFile;
use crate::remote_backend;
use crate::session::SessionControl;
use crate::settings::{load_string, store_string};

/// The messages the recovery secret field points to (forms.md).
const KIT_FILE_ERROR_ID: &str = "enroll-kit-file-error";
const ENROLL_ERROR_ID: &str = "enroll-error";
const SECRET_FIELD_ID: &str = "enroll-secret";
const UNLOCK_FORM_ID: &str = "enroll-unlock-form";
/// Each step's heading takes the focus when the step changes, so a screen reader reads it.
const STEP_TITLE_ID: &str = "onboarding-title";
const LINK_POLL_MS: u64 = 1000;
/// Resume markers in `localStorage`, per account (never the key).
const KIT_SHOWN_PREFIX: &str = "teamcraft.onboarding.kit-shown.";
const WAY_PREFIX: &str = "teamcraft.onboarding.way.";

/// The steps of a new account, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewStep {
  Intro,
  Save,
  Check,
  Finish,
}

/// The steps of an account with keys: the choice, then one of the ways.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Way {
  Choose,
  Kit,
  Link,
  /// No device and no kit: what the IT department can do (the request itself starts on a phone).
  It,
}

/// What the unlock form says about a picked file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileNote {
  Read,
  NoKey,
}

/// Whether the user asked the account's other devices to approve this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AskState {
  Idle,
  Asking,
  Asked,
  Failed,
}

/// First run on this device. A new account saves its recovery kit, then creates its keys; an
/// account that has keys unlocks them with this computer's keychain, another device or the kit.
#[component]
pub(crate) fn EnrollScreen(
  name: String,
  has_existing_identity: bool,
  was_data_lost: bool,
  can_restore_on_device: bool,
  control: SessionControl,
) -> impl IntoView {
  let i18n = use_i18n();
  let device_name = RwSignal::new(default_device_name(i18n));
  let maybe_user = control.enrolling_user();
  view! {
    <AuthFrame>
      <div class="onboarding">
        {if has_existing_identity {
          view! {
            <ExistingAccountFlow
              maybe_user=maybe_user
              device_name=device_name
              was_data_lost=was_data_lost
              can_restore_on_device=can_restore_on_device
              control=control
            />
          }
          .into_any()
        } else {
          view! { <NewAccountFlow name=name maybe_user=maybe_user device_name=device_name control=control /> }.into_any()
        }}
        <p class="onboarding-exit small">
          <button type="button" class="link-btn" on:click=move |_| control.sign_out()>{move || i18n.t("menu-log-out")}</button>
        </p>
      </div>
    </AuthFrame>
  }
}

/// A new account: explain, save the kit, check it, keep it safe and finish.
#[component]
fn NewAccountFlow(
  name: String,
  maybe_user: Option<UserId>,
  device_name: RwSignal<String>,
  control: SessionControl,
) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_marker = maybe_user.map(|user| format!("{KIT_SHOWN_PREFIX}{user}"));
  // Shown a key before and restarted: that key is gone, so a kit saved from it doesn't work.
  let was_restarted = maybe_marker
    .as_deref()
    .is_some_and(|marker| load_string(marker).is_some());
  let step = RwSignal::new(if was_restarted {
    NewStep::Save
  } else {
    NewStep::Intro
  });
  let key = new_kit_key();
  let name = StoredValue::new(name);
  Effect::new(move |_| {
    if step.get() == NewStep::Save
      && let Some(marker) = maybe_marker.as_deref()
    {
      store_string(marker, "1");
    }
  });
  focus_title_on_change(step);
  let go = move |to: NewStep| Callback::new(move |()| step.set(to));
  let finish = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if control.is_enrolling.get_untracked() {
      return;
    }
    if let Some(recovery_key) = key.try_with_value(|key| key.to_display().to_string()) {
      control.enroll(
        EnrollSecret::RecoveryKey { recovery_key },
        device_name.get_untracked().trim().to_owned(),
      );
    }
  };

  view! {
    {move || {
      let current = step.get();
      let number = match current {
        NewStep::Intro => 1,
        NewStep::Save => 2,
        NewStep::Check => 3,
        NewStep::Finish => 4,
      };
      let body = match current {
        NewStep::Intro => view! {
          <StepTitle text=Signal::derive(move || i18n.t_with("onboarding-intro-title", &[("name", &name.get_value())])) />
          <p class="step-lead">{move || i18n.t("onboarding-intro-lead")}</p>
          <ul class="step-points">
            <StepPoint icon=IconName::Lock title="onboarding-intro-locked" text="onboarding-intro-locked-text" />
            <StepPoint icon=IconName::Key title="onboarding-intro-kit" text="onboarding-intro-kit-text" />
            <StepPoint icon=IconName::Shield title="onboarding-intro-keep" text="onboarding-intro-keep-text" />
          </ul>
          <div class="step-actions">
            <button type="button" class="btn btn-primary" data-autofocus on:click=move |_| step.set(NewStep::Save)>
              {move || i18n.t("onboarding-start")}
            </button>
          </div>
        }
        .into_any(),
        NewStep::Save => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-save-title")) />
          {was_restarted.then(|| view! {
            <div class="auth-alert" role="status">
              <Icon name=IconName::Info />
              <p>{move || i18n.t("onboarding-save-new-key")}</p>
            </div>
          })}
          <p class="step-lead">{move || i18n.t("onboarding-save-lead")}</p>
          <KitSaveStep
            key=key
            account=name.get_value()
            on_continue=go(NewStep::Check)
            maybe_secondary=Some(SecondaryAction { label: Signal::derive(move || i18n.t("onboarding-back")), on_click: go(NewStep::Intro) })
          />
        }
        .into_any(),
        NewStep::Check => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-check-title")) />
          <p class="step-lead">{move || i18n.t("onboarding-check-lead")}</p>
          <KitCheckStep
            key=key
            finish_label=Signal::derive(move || i18n.t("onboarding-continue"))
            is_busy=Signal::derive(|| false)
            on_confirmed=Callback::new(move |_key: String| step.set(NewStep::Finish))
            secondary=SecondaryAction { label: Signal::derive(move || i18n.t("kit-confirm-back")), on_click: go(NewStep::Save) }
          />
        }
        .into_any(),
        NewStep::Finish => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-finish-title")) />
          <p class="step-lead">{move || i18n.t("onboarding-finish-lead")}</p>
          <ul class="step-points">
            <StepPoint icon=IconName::File title="onboarding-finish-where" text="onboarding-finish-where-text" />
            <StepPoint icon=IconName::Lock title="onboarding-finish-share" text="onboarding-finish-share-text" />
            <StepPoint icon=IconName::Shield title="onboarding-finish-ask" text="onboarding-finish-ask-text" />
          </ul>
          <form class="auth-form" novalidate on:submit=finish>
            <DeviceNameField device_name=device_name />
            <EnrollError message_key="enroll-failed" control=control />
            <div class="step-actions">
              <button type="submit" class="btn btn-primary" aria-busy=move || control.is_enrolling.get().then_some("true")>
                <Show when=move || control.is_enrolling.get() fallback=|| ()>
                  <span class="spinner" aria-hidden="true"></span>
                </Show>
                {move || if control.is_enrolling.get() { i18n.t("enroll-working") } else { i18n.t("onboarding-finish") }}
              </button>
              <button
                type="button"
                class="btn"
                prop:disabled=move || control.is_enrolling.get()
                on:click=move |_| step.set(NewStep::Check)
              >
                {move || i18n.t("onboarding-back")}
              </button>
            </div>
          </form>
        }
        .into_any(),
      };
      view! {
        <StepProgress current=number total=4 />
        <div class="step-body">{body}</div>
      }
    }}
  }
}

/// An account with keys: choose how to bring them here, then that way.
#[component]
fn ExistingAccountFlow(
  maybe_user: Option<UserId>,
  device_name: RwSignal<String>,
  was_data_lost: bool,
  can_restore_on_device: bool,
  control: SessionControl,
) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_marker = StoredValue::new(maybe_user.map(|user| format!("{WAY_PREFIX}{user}")));
  let resumed = maybe_marker
    .with_value(|marker| marker.as_deref().and_then(load_string))
    .map_or(Way::Choose, |way| match way.as_str() {
      "kit" => Way::Kit,
      "link" => Way::Link,
      "it" => Way::It,
      _ => Way::Choose,
    });
  let way = RwSignal::new(resumed);
  // The chosen way survives a restart; going back to the choice forgets it.
  let choose = move |to: Way| {
    let stored = match to {
      Way::Kit => "kit",
      Way::Link => "link",
      Way::It => "it",
      Way::Choose => "",
    };
    maybe_marker.with_value(|marker| {
      if let Some(marker) = marker.as_deref() {
        store_string(marker, stored);
      }
    });
    way.set(to);
  };
  focus_title_on_change(way);
  let back = Callback::new(move |()| choose(Way::Choose));
  let restore_here = move |_| {
    control.enroll(
      EnrollSecret::KeptOnDevice,
      device_name.get_untracked().trim().to_owned(),
    );
  };

  view! {
    {move || {
      let current = way.get();
      let body = match current {
        Way::Choose => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-unlock-title")) />
          {was_data_lost.then(|| view! {
            <div class="auth-alert" role="alert">
              <Icon name=IconName::Alert />
              <p>{move || i18n.t("enroll-data-lost")}</p>
            </div>
          })}
          <p class="step-lead">{move || i18n.t("onboarding-unlock-lead")}</p>
          <div class="step-choices" role="list">
            {can_restore_on_device.then(|| view! {
              <ChoiceRow
                icon=IconName::Monitor
                title="enroll-restore-device"
                text="onboarding-way-device-text"
                is_recommended=true
                is_busy=Signal::derive(move || control.is_enrolling.get())
                on_click=Callback::new(move |()| restore_here(()))
              />
            })}
            <ChoiceRow
              icon=IconName::Smartphone
              title="onboarding-way-link"
              text="onboarding-way-link-text"
              is_recommended=!can_restore_on_device
              is_busy=Signal::derive(|| false)
              on_click=Callback::new(move |()| choose(Way::Link))
            />
            <ChoiceRow
              icon=IconName::Key
              title="onboarding-way-kit"
              text="onboarding-way-kit-text"
              is_recommended=false
              is_busy=Signal::derive(|| false)
              on_click=Callback::new(move |()| choose(Way::Kit))
            />
            <ChoiceRow
              icon=IconName::Users
              title="onboarding-way-it"
              text="onboarding-way-it-text"
              is_recommended=false
              is_busy=Signal::derive(|| false)
              on_click=Callback::new(move |()| choose(Way::It))
            />
          </div>
          <EnrollError message_key="enroll-recover-failed" control=control />
        }
        .into_any(),
        Way::Kit => view! { <UnlockWithKit device_name=device_name control=control on_back=back /> }.into_any(),
        Way::Link => view! { <LinkThisDevice device_name=device_name.get_untracked() control=control on_back=back /> }.into_any(),
        Way::It => view! { <AskTheItDepartment on_back=back /> }.into_any(),
      };
      view! {
        <StepProgress current=if current == Way::Choose { 1 } else { 2 } total=2 />
        <div class="step-body">{body}</div>
      }
    }}
  }
}

/// The recovery kit's file or key (or a legacy passphrase) unlocks the account's keys here.
#[component]
fn UnlockWithKit(
  device_name: RwSignal<String>,
  control: SessionControl,
  on_back: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let secret = RwSignal::new(String::new());
  let is_revealed = RwSignal::new(false);
  let has_tried = RwSignal::new(false);
  let maybe_file_note = RwSignal::new(None::<FileNote>);
  let input_type = move || if is_revealed.get() { "text" } else { "password" };
  // A key with a typo still goes to the core as a passphrase (it may be one); the hint says why
  // it may not open anything.
  let looks_like_typo =
    move || secret.with(|text| matches!(RecoveryKey::parse(text), Err(RecoveryKeyError::ChecksumMismatch)));
  let is_empty = move || secret.with(|text| text.trim().is_empty());
  let is_missing = move || has_tried.get() && is_empty() && maybe_file_note.get() != Some(FileNote::NoKey);
  // The message the secret field points to: nothing given yet, a kit file without a key, or a
  // secret the core refused.
  let secret_error_id = move || {
    if maybe_file_note.get() == Some(FileNote::NoKey) {
      Some(KIT_FILE_ERROR_ID)
    } else if is_missing() {
      Some("enroll-secret-missing")
    } else {
      control
        .enroll_error
        .with(Option::is_some)
        .then_some(ENROLL_ERROR_ID)
    }
  };
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if control.is_enrolling.get_untracked() {
      return;
    }
    if is_empty() {
      has_tried.set(true);
      focus_first_invalid(UNLOCK_FORM_ID);
      return;
    }
    control.enroll(
      enroll_secret(secret.get_untracked()),
      device_name.get_untracked().trim().to_owned(),
    );
  };
  let on_file = Callback::new(move |read: KitFile| {
    let note = match read {
      KitFile::Key(key) => {
        secret.set_if_alive(key.to_display().to_string());
        FileNote::Read
      },
      KitFile::NoKey => FileNote::NoKey,
    };
    maybe_file_note.set_if_alive(Some(note));
  });

  view! {
    <StepTitle text=Signal::derive(move || i18n.t("onboarding-kit-title")) />
    <p class="step-lead">{move || i18n.t("onboarding-kit-lead")}</p>
    <form id=UNLOCK_FORM_ID class="auth-form" novalidate on:submit=submit>
      <div class="kit-open">
        <KitFilePicker label=Signal::derive(move || i18n.t("kit-open")) on_read=on_file />
        {move || maybe_file_note.get().map(|note| match note {
          FileNote::Read => view! { <p class="ok-text small" role="status">{move || i18n.t("enroll-kit-read")}</p> }.into_any(),
          FileNote::NoKey => view! { <p id=KIT_FILE_ERROR_ID class="field-error" role="alert">{move || i18n.t("kit-file-no-key")}</p> }.into_any(),
        })}
      </div>
      <div class="field">
        <label for=SECRET_FIELD_ID>{move || i18n.t("enroll-secret-label")}</label>
        <div class="input-with-action">
          <input
            id=SECRET_FIELD_ID
            class="input"
            type=input_type
            name="recovery-key"
            autocomplete="current-password"
            autocapitalize="off"
            spellcheck="false"
            aria-invalid=move || secret_error_id().map(|_| "true")
            aria-describedby=move || match secret_error_id() {
              Some(error) => format!("enroll-secret-hint {error}"),
              None => "enroll-secret-hint".to_owned(),
            }
            prop:value=move || secret.get()
            on:input=move |ev| {
              maybe_file_note.set(None);
              secret.set(event_target_value(&ev));
            }
          />
          <button
            type="button"
            class="btn btn-small"
            aria-pressed=move || is_revealed.get().to_string()
            aria-controls=SECRET_FIELD_ID
            on:click=move |_| is_revealed.update(|value| *value = !*value)
          >
            {move || if is_revealed.get() { i18n.t("passphrase-hide") } else { i18n.t("passphrase-show") }}
          </button>
        </div>
        <Show when=is_missing fallback=|| ()>
          <p id="enroll-secret-missing" class="field-error" role="alert">{move || i18n.t("enroll-secret-missing")}</p>
        </Show>
        <p id="enroll-secret-hint" class="muted-text small" class:warn-text=looks_like_typo>
          {move || if looks_like_typo() { i18n.t("enroll-key-typo") } else { i18n.t("enroll-secret-hint") }}
        </p>
      </div>
      <DeviceNameField device_name=device_name />
      <EnrollError message_key="enroll-recover-failed" control=control />
      <div class="step-actions">
        <button type="submit" class="btn btn-primary" aria-busy=move || control.is_enrolling.get().then_some("true")>
          <Show when=move || control.is_enrolling.get() fallback=|| ()>
            <span class="spinner" aria-hidden="true"></span>
          </Show>
          {move || if control.is_enrolling.get() { i18n.t("enroll-working") } else { i18n.t("enroll-recover") }}
        </button>
        <button type="button" class="btn" prop:disabled=move || control.is_enrolling.get() on:click=move |_| on_back.run(())>
          {move || i18n.t("onboarding-back")}
        </button>
      </div>
    </form>
  }
}

/// New device of an existing account: shows a code (QR), waits for an existing device to answer,
/// shows the short code to compare, then receives the keys (e2ee.md §5).
#[component]
fn LinkThisDevice(device_name: String, control: SessionControl, on_back: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let status = RwSignal::new(LinkStatus::Idle);
  let device_name = StoredValue::new(device_name);
  // Asking the other devices hands the code's key to the server: only on the user's click.
  let asking = RwSignal::new(AskState::Idle);

  // A new code each time: after a failure (the code expired, the other device went away) "Try
  // again" starts over instead of leaving the user on a dead screen (issue #645).
  let start = move || {
    status.set(LinkStatus::Idle);
    asking.set(AskState::Idle);
    spawn_local(async move {
      if let Err(message) = remote_backend::link_start(&device_name.get_value()).await {
        status.set_if_alive(LinkStatus::Failed { message });
      }
    });
  };
  start();
  use_interval(LINK_POLL_MS, move || {
    spawn_local(async move {
      if matches!(status.get_untracked(), LinkStatus::Failed { .. }) {
        return;
      }
      if let Some(user) = control.enrolling_user()
        && let Ok(CoreStatus::Ready { .. }) = remote_backend::core_status(user).await
      {
        control.finish_link();
        return;
      }
      if let Ok(latest) = remote_backend::link_status().await {
        status.set_if_alive(latest);
      }
    });
  });
  let back = move |_| {
    spawn_local(async { drop(remote_backend::link_cancel().await) });
    on_back.run(());
  };
  let ask = move |_| {
    asking.set(AskState::Asking);
    spawn_local(async move {
      asking.set_if_alive(match remote_backend::link_announce().await {
        Ok(()) => AskState::Asked,
        Err(_) => AskState::Failed,
      });
    });
  };

  view! {
    <StepTitle text=Signal::derive(move || i18n.t("link-new-title")) />
    {move || match status.get() {
      LinkStatus::Idle => view! { <p class="auth-status" role="status"><span class="spinner" aria-hidden="true"></span></p> }.into_any(),
      LinkStatus::WaitingForScan { code, .. } | LinkStatus::Confirm { code, .. } => {
        let (short, maybe_expires_at_ms) = match status.get() {
          LinkStatus::Confirm { short_code, .. } => (Some(short_code), None),
          LinkStatus::WaitingForScan { maybe_expires_at_ms, .. } => (None, maybe_expires_at_ms),
          _ => (None, None),
        };
        // The status is read every second, so this ticks; at the end the core reports the expiry
        // and "Try again" brings a fresh code (issue #998).
        let time_left = maybe_expires_at_ms.map(|expires_at_ms| {
          let seconds = ((expires_at_ms - now_ms()).max(0) + 999) / 1000;
          format!("{}:{:02}", seconds / 60, seconds % 60)
        });
        match short {
          // Connected: only the comparison matters now.
          Some(short_code) => view! {
            <p class="step-lead">{move || i18n.t("link-new-compare")}</p>
            <p class="approval-code" translate="no">{short_code}</p>
            <p class="status-line" role="status">{move || i18n.t("link-waiting")}</p>
          }
          .into_any(),
          None => view! {
            <ol class="step-numbered">
              <li>{move || i18n.t("onboarding-link-open")}</li>
              <li>{move || i18n.t("onboarding-link-scan")}</li>
              <li>{move || i18n.t("onboarding-link-compare")}</li>
            </ol>
            <div class="link-qr"><QrImage data=code.clone() /></div>
            <p class="small mono link-code" translate="no">{code}</p>
            {time_left.map(|time| view! {
              <p class="muted-text small link-valid">{move || i18n.t_with("link-code-valid", &[("time", &time)])}</p>
            })}
            {move || match asking.get() {
              AskState::Asked => view! { <p class="status-line" role="status">{move || i18n.t("link-new-asked")}</p> }.into_any(),
              state => view! {
                <div class="step-aside">
                  <p class="muted-text small">{move || i18n.t("onboarding-link-no-camera")}</p>
                  <button type="button" class="btn btn-small" prop:disabled=move || asking.get() == AskState::Asking on:click=ask>
                    {move || i18n.t("link-new-ask")}
                  </button>
                  <p class="muted-text small">{move || i18n.t("link-new-ask-hint")}</p>
                  {(state == AskState::Failed).then(|| view! { <p class="field-error" role="alert">{move || i18n.t("link-new-ask-failed")}</p> })}
                </div>
              }
              .into_any(),
            }}
          }
          .into_any(),
        }
      },
      LinkStatus::Failed { message } => {
        let failure = ActionFailure::from_wire(&message);
        let is_known = !matches!(failure.kind, FailureKind::Other);
        view! {
          <p class="field-error" role="alert">
            {move || if is_known { failure_message(i18n, &failure) } else { i18n.t("link-new-failed") }}
          </p>
          {(!is_known).then(|| view! { <p class="small mono" translate="no">{message.clone()}</p> })}
          <div class="step-actions">
            <button type="button" class="btn btn-primary" on:click=move |_| start()>{move || i18n.t("link-new-retry")}</button>
          </div>
        }
        .into_any()
      },
    }}
    <div class="step-actions">
      <button type="button" class="btn" on:click=back>{move || i18n.t("onboarding-back")}</button>
    </div>
    <p class="muted-text small">{move || i18n.t("link-new-no-device")}</p>
  }
}

/// No other device and no kit: what to tell the IT department. Not a self-service: the IT
/// department resets the account, or (if the organization set up recovery) asks for the code the
/// phone app shows (e2ee.md §6.3.1; the request doesn't start in the browser or the desktop app).
#[component]
fn AskTheItDepartment(on_back: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <StepTitle text=Signal::derive(move || i18n.t("onboarding-it-title")) />
    <p class="step-lead">{move || i18n.t("onboarding-it-lead")}</p>
    <p class="muted-text small">{move || i18n.t("onboarding-it-code")}</p>
    <div class="step-actions">
      <button type="button" class="btn" on:click=move |_| on_back.run(())>{move || i18n.t("onboarding-back")}</button>
    </div>
  }
}

/// "Step 2 of 4" with a bar of as many segments; the text is what screen readers read.
#[component]
fn StepProgress(current: u8, total: u8) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="step-progress">
      <div class="step-progress-bar" aria-hidden="true">
        {(1..=total).map(|n| view! { <span class="step-progress-segment" class:is-done=n <= current></span> }).collect_view()}
      </div>
      <p class="step-progress-label small">
        {move || i18n.t_with("onboarding-step", &[("current", &current.to_string()), ("total", &total.to_string())])}
      </p>
    </div>
  }
}

/// The step's heading: focusable, so a new step can take the focus (see [`focus_title_on_change`]).
#[component]
fn StepTitle(#[prop(into)] text: Signal<String>) -> impl IntoView {
  view! { <h1 id=STEP_TITLE_ID tabindex="-1" translate="no">{move || text.get()}</h1> }
}

/// One explained point of a step: an icon, a short title and a plain sentence.
#[component]
fn StepPoint(icon: IconName, title: &'static str, text: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <li class="step-point">
      <span class="step-point-icon" aria-hidden="true"><Icon name=icon /></span>
      <span class="step-point-text">
        <strong>{move || i18n.t(title)}</strong>
        <span class="muted-text">{move || i18n.t(text)}</span>
      </span>
    </li>
  }
}

/// A way to choose, as a full-width row: icon, title, one line of why, and a chevron.
#[component]
fn ChoiceRow(
  icon: IconName,
  title: &'static str,
  text: &'static str,
  is_recommended: bool,
  is_busy: Signal<bool>,
  on_click: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div role="listitem">
      <button
        type="button"
        class="step-choice"
        class:is-recommended=is_recommended
        aria-busy=move || is_busy.get().then_some("true")
        prop:disabled=move || is_busy.get()
        on:click=move |_| on_click.run(())
      >
        <span class="step-point-icon" aria-hidden="true"><Icon name=icon /></span>
        <span class="step-point-text">
          <strong>
            {move || i18n.t(title)}
            {is_recommended.then(|| view! { <span class="step-badge">{move || i18n.t("onboarding-recommended")}</span> })}
          </strong>
          <span class="muted-text">{move || i18n.t(text)}</span>
        </span>
        {move || if is_busy.get() {
          view! { <span class="spinner" aria-hidden="true"></span> }.into_any()
        } else {
          view! { <span class="step-choice-chevron" aria-hidden="true"><Icon name=IconName::ChevronRight /></span> }.into_any()
        }}
      </button>
    </div>
  }
}

/// The name this device gets in the account's device list.
#[component]
fn DeviceNameField(device_name: RwSignal<String>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="field">
      <label for="enroll-device">{move || i18n.t("enroll-device-name")}</label>
      <input
        id="enroll-device"
        class="input"
        name="device"
        autocomplete="off"
        aria-describedby="enroll-device-hint"
        prop:value=move || device_name.get()
        on:input=move |ev| device_name.set(event_target_value(&ev))
      />
      <p id="enroll-device-hint" class="muted-text small">{move || i18n.t("onboarding-device-hint")}</p>
    </div>
  }
}

/// The core's answer to the last setup attempt, under `message_key` with the details.
#[component]
fn EnrollError(message_key: &'static str, control: SessionControl) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    control.enroll_error.get().map(|message| {
      let failure = ActionFailure::from_wire(&message);
      let text = match failure.kind {
        FailureKind::Other => i18n.t(message_key),
        _ => failure_message(i18n, &failure),
      };
      view! {
        <p id=ENROLL_ERROR_ID class="field-error" role="alert">
          {text}
          " "
          <span class="small mono" translate="no">{failure.detail}</span>
        </p>
      }
    })
  }
}

/// After the first render, every change of `step` moves the focus to the new step's heading (the
/// step's content was replaced, the old focus is gone). The first step keeps `data-autofocus`.
fn focus_title_on_change<T: Clone + PartialEq + Send + Sync + 'static>(step: RwSignal<T>) {
  Effect::new(move |previous: Option<T>| {
    let current = step.get();
    if previous.is_some_and(|previous| previous != current) {
      request_animation_frame(|| {
        if let Some(title) = web_sys::window()
          .and_then(|window| window.document())
          .and_then(|document| document.get_element_by_id(STEP_TITLE_ID))
          .and_then(|title| title.dyn_into::<web_sys::HtmlElement>().ok())
        {
          drop(title.focus());
        }
      });
    }
    current
  });
}

/// What the unlock field holds, as the core takes it: a recovery key when it reads as one (valid
/// checksum), anything else as a legacy passphrase (e2ee.md §6.5.5).
fn enroll_secret(text: String) -> EnrollSecret {
  match RecoveryKey::parse(&text) {
    Ok(key) => EnrollSecret::RecoveryKey {
      recovery_key: key.to_display().to_string(),
    },
    Err(_) => EnrollSecret::Passphrase { passphrase: text },
  }
}

/// "Desktop (Linux x86_64)" in the desktop app, "Browser (Linux x86_64)" in a browser: the name
/// the user sees later in the device list, so it should say what this device is.
fn default_device_name(i18n: I18n) -> String {
  let platform = web_sys::window()
    .and_then(|window| window.navigator().platform().ok())
    .unwrap_or_default();
  let kind = if tauri::is_available() {
    i18n.t("device-kind-desktop")
  } else {
    i18n.t("device-kind-browser")
  };
  if platform.is_empty() {
    kind
  } else {
    format!("{kind} ({platform})")
  }
}
