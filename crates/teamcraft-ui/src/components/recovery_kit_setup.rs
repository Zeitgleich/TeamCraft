//! Making a recovery kit (e2ee.md §6.5.4): a fresh recovery key, its kit file to save, and a check
//! that the user kept it before the key protects anything. The two steps ([`KitSaveStep`],
//! [`KitCheckStep`]) are the guided first sign-in's (onboarding.rs) and, joined by
//! [`RecoveryKitSetup`], Settings > Security & privacy's. [`KitFilePicker`] also reads a saved kit
//! back when recovering.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::recovery_key::{RECOVERY_KEY_GROUPS, RecoveryKey};
use wasm_bindgen::JsCast;

use crate::clock::{format_date, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::form_errors::focus_first_invalid;
use crate::i18n::{I18n, use_i18n};
use crate::reactive::SetIfAlive;
use crate::recovery_kit::{self, KitFile, KitTexts};

/// Files the kit picker offers: the saved kit (HTML), or a text file with the key.
const KIT_FILE_TYPES: &str = ".html,.htm,.txt,text/html,text/plain";
/// The check step's form, so a refused check can focus its field (forms.md).
const CHECK_FORM_ID: &str = "kit-check-form";
const CHECK_FIELD_ERROR_ID: &str = "kit-confirm-group-error";

/// The recovery key being made, kept in the page until it protects the account.
pub type KitKey = StoredValue<RecoveryKey, LocalStorage>;

/// Makes a fresh recovery key for [`KitSaveStep`] and [`KitCheckStep`].
pub fn new_kit_key() -> KitKey {
  StoredValue::new_local(RecoveryKey::generate(&mut OsRng))
}

/// The quieter button of a step's action row: "Back", "Cancel".
#[derive(Clone, Copy)]
pub struct SecondaryAction {
  pub label: Signal<String>,
  pub on_click: Callback<()>,
}

/// Where the user is in making the kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
  Save,
  Check,
}

/// What a file opened at the check showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileCheck {
  Unchecked,
  Matches,
  /// A kit, but of another key (an older kit, another account's).
  OtherKey,
  NoKey,
}

/// Settings: shows a new recovery key with its kit to download, then asks for one group of it (or
/// the saved file) before `on_confirmed` gets the key as text for the core.
#[component]
pub fn RecoveryKitSetup(
  /// The account's name, printed in the kit.
  account: String,
  /// The button that finishes once the user showed they kept the kit.
  #[prop(into)]
  finish_label: Signal<String>,
  #[prop(into)] is_busy: Signal<bool>,
  on_confirmed: Callback<String>,
  /// Leaves without a new kit.
  #[prop(optional)]
  maybe_on_cancel: Option<Callback<()>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let key = new_kit_key();
  let step = RwSignal::new(Step::Save);
  let account = StoredValue::new(account);
  let cancel = maybe_on_cancel.map(|on_click| SecondaryAction {
    label: Signal::derive(move || i18n.t("cancel")),
    on_click,
  });
  let back = SecondaryAction {
    label: Signal::derive(move || i18n.t("kit-confirm-back")),
    on_click: Callback::new(move |()| step.set(Step::Save)),
  };

  view! {
    <div class="kit-setup">
      {move || match step.get() {
        Step::Save => view! {
          <p>{move || i18n.t("kit-setup-intro")}</p>
          <KitSaveStep
            key=key
            account=account.get_value()
            on_continue=Callback::new(move |()| step.set(Step::Check))
            maybe_secondary=cancel
          />
        }
        .into_any(),
        Step::Check => view! {
          <p>{move || i18n.t("kit-confirm-intro-short")}</p>
          <KitCheckStep
            key=key
            finish_label=finish_label
            is_busy=is_busy
            on_confirmed=on_confirmed
            secondary=back
          />
        }
        .into_any(),
      }}
    </div>
  }
}

/// The key in large groups and **Download recovery kit**. Once downloaded, the main action moves
/// on; a user who can't download writes the key down and moves on from the link below.
#[component]
pub fn KitSaveStep(
  key: KitKey,
  /// The account's name, printed in the kit.
  account: String,
  on_continue: Callback<()>,
  #[prop(default = None)] maybe_secondary: Option<SecondaryAction>,
) -> impl IntoView {
  let i18n = use_i18n();
  let groups: Vec<String> = key.with_value(|key| key.groups().to_vec());
  let server = StoredValue::new(server_host());
  let account = StoredValue::new(account);
  let has_downloaded = RwSignal::new(false);
  let has_download_failed = RwSignal::new(false);
  let save = move || {
    let server = server.get_value();
    let texts = kit_texts(i18n, &account.get_value(), &server);
    let html = key.with_value(|key| recovery_kit::kit_html(key, &texts));
    match recovery_kit::download(&html, &recovery_kit::kit_file_name(&server)) {
      Ok(()) => {
        has_download_failed.set(false);
        has_downloaded.set(true);
      },
      Err(error) => {
        tracing::warn!(%error, "could not offer the recovery kit for download");
        has_download_failed.set(true);
      },
    }
  };
  let primary = move |_| {
    if has_downloaded.get_untracked() {
      on_continue.run(());
    } else {
      save();
    }
  };

  view! {
    <p class="kit-key" role="group" aria-label=move || i18n.t("kit-key-label") translate="no">
      {groups.into_iter().map(|group| view! { <span class="kit-group">{group}</span> }).collect_view()}
    </p>
    <p class="kit-status muted-text small" role="status">
      {move || if has_downloaded.get() {
        view! {
          <Icon name=IconName::Check />
          <span>{move || i18n.t("kit-downloaded")}</span>
          " "
          <button type="button" class="link-btn" on:click=move |_| save()>{move || i18n.t("kit-download-again")}</button>
        }
        .into_any()
      } else {
        view! { <span>{move || i18n.t("kit-save-hint")}</span> }.into_any()
      }}
    </p>
    <Show when=move || has_download_failed.get() fallback=|| ()>
      <p class="field-error" role="alert">{move || i18n.t("kit-download-failed")}</p>
    </Show>
    <div class="step-actions">
      <button type="button" class="btn btn-primary" data-autofocus on:click=primary>
        {move || if has_downloaded.get() {
          view! { <span>{move || i18n.t("onboarding-continue")}</span> }.into_any()
        } else {
          view! { <Icon name=IconName::Download /><span>{move || i18n.t("kit-download")}</span> }.into_any()
        }}
      </button>
      {maybe_secondary.map(|secondary| view! {
        <button type="button" class="btn" on:click=move |_| secondary.on_click.run(())>{move || secondary.label.get()}</button>
      })}
    </div>
    <Show when=move || !has_downloaded.get() fallback=|| ()>
      <p class="step-aside small">
        <button type="button" class="link-btn" on:click=move |_| on_continue.run(())>{move || i18n.t("kit-wrote-down")}</button>
      </p>
    </Show>
  }
}

/// Asks for one group of the key (chosen at random, the key is hidden here) or the saved file.
/// A refused check says what to do at the field and focuses it (forms.md); a right one gives
/// `on_confirmed` the key as text for the core.
#[component]
pub fn KitCheckStep(
  key: KitKey,
  #[prop(into)] finish_label: Signal<String>,
  #[prop(into)] is_busy: Signal<bool>,
  on_confirmed: Callback<String>,
  secondary: SecondaryAction,
) -> impl IntoView {
  let i18n = use_i18n();
  // Not a secret: which group the user types back, so they look at their saved kit.
  let group_index = StoredValue::new(OsRng.next_u32() as usize % RECOVERY_KEY_GROUPS);
  let group_number = move || (group_index.get_value() + 1).to_string();
  let typed = RwSignal::new(String::new());
  let file_check = RwSignal::new(FileCheck::Unchecked);
  let has_tried = RwSignal::new(false);
  let is_group_right = move || {
    let typed = typed.get();
    key
      .try_with_value(|key| recovery_kit::is_group(key, group_index.get_value(), &typed))
      .unwrap_or(false)
  };
  let is_confirmed = move || file_check.get() == FileCheck::Matches || is_group_right();
  // Wrong while typing (a full group that isn't it), or missing after a refused check.
  let maybe_field_error = move || {
    if is_confirmed() {
      None
    } else if typed.with(|typed| typed.trim().chars().count() >= 4) {
      Some(i18n.t_with("kit-confirm-wrong", &[("number", &group_number())]))
    } else if has_tried.get() {
      Some(i18n.t_with("kit-check-missing", &[("number", &group_number())]))
    } else {
      None
    }
  };
  let has_field_error = move || maybe_field_error().is_some();
  let on_file = Callback::new(move |read: KitFile| {
    let check = match read {
      KitFile::Key(found) if key.try_with_value(|key| key.is_same_key(&found)) == Some(true) => {
        FileCheck::Matches
      },
      KitFile::Key(_) => FileCheck::OtherKey,
      KitFile::NoKey => FileCheck::NoKey,
    };
    file_check.set_if_alive(check);
  });
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if is_busy.get_untracked() {
      return;
    }
    if !is_confirmed() {
      has_tried.set(true);
      focus_first_invalid(CHECK_FORM_ID);
      return;
    }
    if let Some(text) = key.try_with_value(|key| key.to_display().to_string()) {
      on_confirmed.run(text);
    }
  };

  view! {
    <form id=CHECK_FORM_ID class="kit-check" novalidate on:submit=submit>
      <div class="field">
        <label for="kit-confirm-group">{move || i18n.t_with("kit-confirm-group", &[("number", &group_number())])}</label>
        <input
          id="kit-confirm-group"
          class="input kit-group-input"
          autocomplete="off"
          autocapitalize="characters"
          spellcheck="false"
          maxlength="6"
          data-autofocus
          aria-invalid=move || has_field_error().then_some("true")
          aria-describedby=move || has_field_error().then_some(CHECK_FIELD_ERROR_ID)
          prop:value=move || typed.get()
          on:input=move |ev| typed.set(event_target_value(&ev))
        />
        {move || maybe_field_error().map(|message| view! {
          <p id=CHECK_FIELD_ERROR_ID class="field-error" role="alert">{message}</p>
        })}
        <Show when=is_group_right fallback=|| ()>
          <p class="ok-text small" role="status">{move || i18n.t("kit-check-group-ok")}</p>
        </Show>
      </div>
      <div class="kit-or">
        <span class="muted-text small">{move || i18n.t("kit-check-or")}</span>
        <KitFilePicker label=Signal::derive(move || i18n.t("kit-open")) on_read=on_file />
      </div>
      {move || match file_check.get() {
        FileCheck::Unchecked => ().into_any(),
        FileCheck::Matches => view! { <p class="ok-text small" role="status">{move || i18n.t("kit-confirm-file-ok")}</p> }.into_any(),
        FileCheck::OtherKey => view! { <p class="field-error" role="alert">{move || i18n.t("kit-confirm-file-other")}</p> }.into_any(),
        FileCheck::NoKey => view! { <p class="field-error" role="alert">{move || i18n.t("kit-file-no-key")}</p> }.into_any(),
      }}
      <div class="step-actions">
        <button type="submit" class="btn btn-primary" aria-busy=move || is_busy.get().then_some("true")>
          <Show when=move || is_busy.get() fallback=|| ()>
            <span class="spinner" aria-hidden="true"></span>
          </Show>
          {move || finish_label.get()}
        </button>
        <button type="button" class="btn" on:click=move |_| secondary.on_click.run(())>{move || secondary.label.get()}</button>
      </div>
    </form>
  }
}

/// "Open recovery kit…": the system's file picker, then the key read out of the file in the page.
#[component]
pub fn KitFilePicker(#[prop(into)] label: Signal<String>, on_read: Callback<KitFile>) -> impl IntoView {
  let input = NodeRef::<leptos::html::Input>::new();
  let on_change = move |ev: web_sys::Event| {
    let Some(element) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
      return;
    };
    let Some(file) = element.files().and_then(|files| files.get(0)) else {
      return;
    };
    // The same file can be picked again after a failed try.
    element.set_value("");
    spawn_local(async move {
      let read = recovery_kit::read_kit_file(file).await.unwrap_or_else(|error| {
        tracing::warn!(%error, "could not read the picked file");
        KitFile::NoKey
      });
      on_read.run(read);
    });
  };
  view! {
    <button
      type="button"
      class="btn btn-small"
      on:click=move |_| {
        if let Some(element) = input.get() {
          element.click();
        }
      }
    >
      <Icon name=IconName::File />
      <span>{move || label.get()}</span>
    </button>
    <input
      type="file"
      class="visually-hidden"
      tabindex="-1"
      aria-hidden="true"
      accept=KIT_FILE_TYPES
      node_ref=input
      on:change=on_change
    />
  }
}

/// The kit's words in the app's language.
fn kit_texts(i18n: I18n, account: &str, server: &str) -> KitTexts {
  let locale = i18n.locale().tag();
  KitTexts {
    language: locale.to_owned(),
    title: i18n.t("kit-file-title"),
    account_line: i18n.t_with(
      "kit-file-account",
      &[
        ("account", account),
        ("server", server),
        ("date", &format_date(now_ms(), locale)),
      ],
    ),
    intro: i18n.t("kit-file-intro"),
    key_label: i18n.t("kit-key-label"),
    keep_title: i18n.t("kit-file-keep-title"),
    keep: vec![
      i18n.t("kit-file-keep-print"),
      i18n.t("kit-file-keep-share"),
      i18n.t("kit-file-keep-anyone"),
    ],
    use_title: i18n.t("kit-file-use-title"),
    use_text: i18n.t("kit-file-use"),
    lost_title: i18n.t("kit-file-lost-title"),
    lost_text: i18n.t("kit-file-lost"),
    never_asked: i18n.t("kit-file-never"),
  }
}

/// The server the account is on: the desktop app's chosen server, else the page's own host.
fn server_host() -> String {
  if let Some(server) = crate::session::use_session().and_then(|session| session.server.get_untracked()) {
    return server.host;
  }
  web_sys::window()
    .and_then(|window| window.location().host().ok())
    .unwrap_or_default()
}
