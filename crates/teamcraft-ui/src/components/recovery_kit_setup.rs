//! Making a recovery kit (e2ee.md §6.5.4): a fresh recovery key, its kit file to save, and a check
//! that the user kept it before the key protects anything. Used at first sign-in and in Settings >
//! Security & privacy; [`KitFilePicker`] also reads a saved kit back when recovering.

use leptos::prelude::*;
use leptos::task::spawn_local;
use rand_core::{OsRng, RngCore};
use teamcraft_crypto::recovery_key::{RECOVERY_KEY_GROUPS, RecoveryKey};
use wasm_bindgen::JsCast;

use crate::clock::{format_date, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::reactive::SetIfAlive;
use crate::recovery_kit::{self, KitFile, KitTexts};

/// Files the kit picker offers: the saved kit (HTML), or a text file with the key.
const KIT_FILE_TYPES: &str = ".html,.htm,.txt,text/html,text/plain";

/// Where the user is in making the kit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
  Save,
  Confirm,
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

/// Shows a new recovery key with its kit to download, then asks for one group of it (or the saved
/// file) before `on_confirmed` gets the key as text for the core.
#[component]
pub fn RecoveryKitSetup(
  /// The account's name, printed in the kit.
  account: String,
  /// The button that finishes once the user showed they kept the kit.
  #[prop(into)]
  finish_label: Signal<String>,
  #[prop(into)] is_busy: Signal<bool>,
  on_confirmed: Callback<String>,
  /// Leaves without a new kit (Settings); first sign-in has no way around it.
  #[prop(optional)]
  maybe_on_cancel: Option<Callback<()>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let key = StoredValue::new_local(RecoveryKey::generate(&mut OsRng));
  let groups: Vec<String> = key.with_value(|key| key.groups().to_vec());
  let server = StoredValue::new(server_host());
  let step = RwSignal::new(Step::Save);
  let has_downloaded = RwSignal::new(false);
  let has_download_failed = RwSignal::new(false);
  // Not a secret: which group the user types back, so they look at their saved kit.
  let group_index = StoredValue::new(OsRng.next_u32() as usize % RECOVERY_KEY_GROUPS);
  let group_number = move || (group_index.get_value() + 1).to_string();
  let typed = RwSignal::new(String::new());
  let file_check = RwSignal::new(FileCheck::Unchecked);
  let is_group_right = move || {
    let typed = typed.get();
    key.with_value(|key| recovery_kit::is_group(key, group_index.get_value(), &typed))
  };
  let is_confirmed = move || file_check.get() == FileCheck::Matches || is_group_right();
  let is_group_wrong = move || {
    let typed = typed.get();
    typed.trim().chars().count() >= 4 && !is_group_right()
  };

  let account = StoredValue::new(account);
  let save = move |_| {
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
  // The file is read after an await: the setup may be gone by then.
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
  let finish = move |_| {
    if is_confirmed() && !is_busy.get_untracked() {
      let text = key.with_value(|key| key.to_display().to_string());
      on_confirmed.run(text);
    }
  };

  view! {
    <div class="kit-setup">
      <Show
        when=move || step.get() == Step::Confirm
        fallback=move || {
          let groups = groups.clone();
          view! {
            <p>{move || i18n.t("kit-setup-intro")}</p>
            <p class="kit-key" role="group" aria-label=move || i18n.t("kit-key-label") translate="no">
              {groups.into_iter().map(|group| view! { <span class="kit-group">{group}</span> }).collect_view()}
            </p>
            <div class="kit-actions">
              <button type="button" class="btn" class:btn-primary=move || !has_downloaded.get() on:click=save>
                <Icon name=IconName::Download />
                <span>{move || i18n.t("kit-download")}</span>
              </button>
              <button
                type="button"
                class="btn"
                class:btn-primary=move || has_downloaded.get()
                on:click=move |_| step.set(Step::Confirm)
              >
                {move || i18n.t("kit-saved-continue")}
              </button>
            </div>
            <p class="muted-text small" role="status">
              {move || if has_downloaded.get() { i18n.t("kit-downloaded") } else { i18n.t("kit-save-hint") }}
            </p>
            <Show when=move || has_download_failed.get() fallback=|| ()>
              <p class="field-error" role="alert">{move || i18n.t("kit-download-failed")}</p>
            </Show>
            {maybe_on_cancel.map(|cancel| view! {
              <div class="kit-actions">
                <button type="button" class="btn btn-small" on:click=move |_| cancel.run(())>{move || i18n.t("cancel")}</button>
              </div>
            })}
          }
        }
      >
        <p>{move || i18n.t_with("kit-confirm-intro", &[("number", &group_number())])}</p>
        <div class="field">
          <label for="kit-confirm-group">{move || i18n.t_with("kit-confirm-group", &[("number", &group_number())])}</label>
          <input
            id="kit-confirm-group"
            class="input kit-group-input"
            autocomplete="off"
            autocapitalize="characters"
            spellcheck="false"
            maxlength="6"
            aria-invalid=move || is_group_wrong().to_string()
            prop:value=move || typed.get()
            on:input=move |ev| typed.set(event_target_value(&ev))
          />
          <Show when=is_group_wrong fallback=|| ()>
            <p class="field-error" role="alert">{move || i18n.t_with("kit-confirm-wrong", &[("number", &group_number())])}</p>
          </Show>
        </div>
        <div class="kit-actions">
          <KitFilePicker label=Signal::derive(move || i18n.t("kit-open")) on_read=on_file />
        </div>
        {move || match file_check.get() {
          FileCheck::Unchecked => ().into_any(),
          FileCheck::Matches => view! { <p class="ok-text small" role="status">{move || i18n.t("kit-confirm-file-ok")}</p> }.into_any(),
          FileCheck::OtherKey => view! { <p class="field-error" role="alert">{move || i18n.t("kit-confirm-file-other")}</p> }.into_any(),
          FileCheck::NoKey => view! { <p class="field-error" role="alert">{move || i18n.t("kit-file-no-key")}</p> }.into_any(),
        }}
        <div class="kit-actions">
          <button
            type="button"
            class="btn btn-primary"
            prop:disabled=move || !is_confirmed() || is_busy.get()
            aria-busy=move || is_busy.get().then_some("true")
            on:click=finish
          >
            <Show when=move || is_busy.get() fallback=|| ()>
              <span class="spinner" aria-hidden="true"></span>
            </Show>
            {move || finish_label.get()}
          </button>
          <button type="button" class="btn" on:click=move |_| step.set(Step::Save)>{move || i18n.t("kit-confirm-back")}</button>
        </div>
      </Show>
    </div>
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
