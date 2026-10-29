//! The notetaker page of the console (admin.md §3.7, e2ee.md §8.2): the signed `notetaker` group of
//! the workspace policy, and the OpenRouter key, which goes to the server write-only while only its
//! SHA-256 is signed.

use leptos::prelude::*;
use teamcraft_proto::{
  DEFAULT_SUMMARY_MODEL, DEFAULT_TRANSCRIPTION_MODEL, LANGUAGE_AUTO, NotetakerInviters, NotetakerPolicyError,
  SecretUpdate, UserId, WriteOnlySecret,
};

use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{TextAreaField, TextField};
use crate::components::admin_policy::{
  CheckRow, PolicyBlockerNote, PolicyFooter, SelectRow, use_policy_draft,
};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// Languages offered; any ISO 639-1 code is valid policy, these are the common ones.
const LANGUAGES: [&str; 7] = [LANGUAGE_AUTO, "de", "en", "fr", "es", "it", "nl"];

/// Turns the notetaker on and off and sets its account, rights, models, language, instructions and
/// key. Only owners sign; the key is stored by any admin with `manage_workspace` when it is saved.
#[component]
pub fn NotetakerSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;
  let settings = Memo::new(move |_| state.read(|backend| backend.notetaker_settings()));
  // The key as typed: never stored in the page beyond the save.
  let key_input = RwSignal::new(String::new());
  let maybe_error = RwSignal::new(None::<&'static str>);

  let bots = Memo::new(move |_| {
    let mut bots: Vec<(UserId, String)> = state
      .read(|backend| backend.users())
      .into_iter()
      .filter(|user| user.is_bot)
      .map(|user| (user.id, user.display_name))
      .collect();
    bots.sort_by(|a, b| a.1.cmp(&b.1));
    bots
  });
  let bot_options = Signal::derive(move || {
    std::iter::once((String::new(), i18n.t("notetaker-bot-none")))
      .chain(bots.get().into_iter().map(|(id, name)| (id.to_string(), name)))
      .collect()
  });
  let inviter_options = Signal::derive(move || {
    [
      ("members", "notetaker-inviters-members"),
      ("admins", "notetaker-inviters-admins"),
    ]
    .into_iter()
    .map(|(value, key)| (value.to_owned(), i18n.t(key)))
    .collect()
  });
  let language_options = Signal::derive(move || {
    LANGUAGES
      .into_iter()
      .map(|code| (code.to_owned(), i18n.t(&format!("notetaker-language-{code}"))))
      .collect()
  });
  let key_status = move || {
    let fingerprint = draft
      .get()
      .notetaker
      .maybe_api_key_sha256
      .map(|hash| hash.chars().take(16).collect::<String>());
    let stored = match settings.get() {
      None => None,
      Some(settings) if !settings.can_store_secrets => Some(i18n.t("notetaker-key-no-secrets")),
      Some(settings) if settings.is_key_stored => Some(match settings.maybe_key_updated_at {
        Some(at) => i18n.t_with(
          "notetaker-key-stored",
          &[("date", &format_date_time(at.0, i18n.locale().tag()))],
        ),
        None => i18n.t("notetaker-key-stored-undated"),
      }),
      Some(_) => Some(i18n.t("notetaker-key-not-stored")),
    };
    let signed =
      fingerprint.map(|fingerprint| i18n.t_with("notetaker-key-signed", &[("fingerprint", &fingerprint)]));
    [stored, signed]
      .into_iter()
      .flatten()
      .collect::<Vec<_>>()
      .join(" ")
  };
  let save = move || {
    let candidate = draft.get_untracked().notetaker;
    if let Err(error) = candidate.check() {
      maybe_error.set(Some(error_key(&error)));
      return;
    }
    maybe_error.set(None);
    let typed = key_input.get_untracked();
    let can_store = settings
      .get_untracked()
      .is_some_and(|settings| settings.can_store_secrets);
    if !typed.trim().is_empty() && can_store {
      let secret = WriteOnlySecret::new(typed.trim().to_owned());
      state.write(|backend| backend.set_notetaker_key(SecretUpdate::Set(secret)));
    }
    key_input.set(String::new());
    policy.save();
  };

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-notetaker-title"))
      subtitle=Signal::derive(move || i18n.t("admin-notetaker-subtitle"))
    />
    <p class="auth-alert" role="note">{move || i18n.t("notetaker-e2ee-warning")}</p>
    <PolicyBlockerNote policy=policy />
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        save();
      }
    >
      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("admin-notetaker-title")}</legend>
        <CheckRow
          name="notetaker_enabled"
          label=Signal::derive(move || i18n.t("notetaker-enabled"))
          hint=Signal::derive(move || i18n.t("notetaker-enabled-hint"))
          checked=Signal::derive(move || draft.get().notetaker.is_enabled)
          on_change=move |value| policy.edit(|p| p.notetaker.is_enabled = value)
        />
        <SelectRow
          id="notetaker-bot"
          label=Signal::derive(move || i18n.t("notetaker-bot"))
          options=bot_options
          selected=Signal::derive(move || draft.get().notetaker.maybe_bot.map(|id| id.to_string()).unwrap_or_default())
          on_select=move |value: String| policy.edit(|p| p.notetaker.maybe_bot = value.parse().ok())
        />
        <p class="muted-text small">{move || i18n.t("notetaker-bot-hint")}</p>
        <SelectRow
          id="notetaker-inviters"
          label=Signal::derive(move || i18n.t("notetaker-inviters"))
          options=inviter_options
          selected=Signal::derive(move || draft.get().notetaker.inviters.as_str().to_owned())
          on_select=move |value: String| policy.edit(|p| p.notetaker.inviters = NotetakerInviters::from(value))
        />
        <CheckRow
          name="notetaker_anyone_stop"
          label=Signal::derive(move || i18n.t("notetaker-anyone-stop"))
          hint=Signal::derive(move || i18n.t("notetaker-anyone-stop-hint"))
          checked=Signal::derive(move || draft.get().notetaker.can_anyone_stop)
          on_change=move |value| policy.edit(|p| p.notetaker.can_anyone_stop = value)
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("notetaker-models")}</legend>
        <p class="muted-text small">{move || i18n.t("notetaker-models-hint")}</p>
        <TextField
          id="notetaker-transcription-model"
          label="notetaker-transcription-model"
          placeholder=DEFAULT_TRANSCRIPTION_MODEL
          value=Signal::derive(move || draft.get().notetaker.transcription_model)
          on_input=Callback::new(move |value: String| policy.edit(|p| p.notetaker.transcription_model = value.trim().to_owned()))
        />
        <TextField
          id="notetaker-summary-model"
          label="notetaker-summary-model"
          placeholder=DEFAULT_SUMMARY_MODEL
          value=Signal::derive(move || draft.get().notetaker.summary_model)
          on_input=Callback::new(move |value: String| policy.edit(|p| p.notetaker.summary_model = value.trim().to_owned()))
        />
        <SelectRow
          id="notetaker-language"
          label=Signal::derive(move || i18n.t("notetaker-language"))
          options=language_options
          selected=Signal::derive(move || draft.get().notetaker.language)
          on_select=move |value: String| policy.edit(|p| p.notetaker.language = value)
        />
        <TextAreaField
          id="notetaker-prompt"
          label="notetaker-prompt"
          hint="notetaker-prompt-hint"
          rows=8
          value=Signal::derive(move || draft.get().notetaker.maybe_prompt.unwrap_or_default())
          on_input=Callback::new(move |value: String| {
            policy.edit(|p| p.notetaker.maybe_prompt = (!value.trim().is_empty()).then_some(value))
          })
        />
        <CheckRow
          name="notetaker_zdr"
          label=Signal::derive(move || i18n.t("notetaker-zdr"))
          hint=Signal::derive(move || i18n.t("notetaker-zdr-hint"))
          checked=Signal::derive(move || draft.get().notetaker.is_zero_data_retention)
          on_change=move |value| policy.edit(|p| p.notetaker.is_zero_data_retention = value)
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("notetaker-key")}</legend>
        <TextField
          id="notetaker-key"
          label="notetaker-key"
          input_type="password"
          hint="notetaker-key-hint"
          placeholder="sk-or-…"
          value=key_input
          on_input=Callback::new(move |value: String| {
            let hash = (!value.trim().is_empty())
              .then(|| teamcraft_crypto::service_key::api_key_sha256_hex(&value));
            key_input.set(value);
            if let Some(hash) = hash {
              policy.edit(|p| p.notetaker.maybe_api_key_sha256 = Some(hash));
            }
          })
        />
        <p class="muted-text small" role="status">{key_status}</p>
        <Show when=move || settings.get().is_some_and(|settings| settings.is_key_stored) fallback=|| ()>
          <button
            type="button"
            class="btn btn-small"
            on:click=move |_| state.write(|backend| backend.set_notetaker_key(SecretUpdate::Clear))
          >
            {move || i18n.t("notetaker-key-remove")}
          </button>
        </Show>
      </fieldset>
      {move || maybe_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
      <PolicyFooter policy=policy />
    </form>
  }
}

/// The text of a policy the owner can't sign yet.
fn error_key(error: &NotetakerPolicyError) -> &'static str {
  match error {
    NotetakerPolicyError::MissingBot => "notetaker-error-bot",
    NotetakerPolicyError::MissingKeyHash => "notetaker-error-key",
    NotetakerPolicyError::InvalidModel { .. } => "notetaker-error-model",
    NotetakerPolicyError::InvalidLanguage => "notetaker-error-language",
    NotetakerPolicyError::InvalidPrompt => "notetaker-error-prompt",
    NotetakerPolicyError::InvalidKeyHash | NotetakerPolicyError::UnknownInviters(_) => {
      "notetaker-error-other"
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_policy_error_has_a_text() {
    let errors = [
      NotetakerPolicyError::MissingBot,
      NotetakerPolicyError::MissingKeyHash,
      NotetakerPolicyError::InvalidModel { field: "x" },
      NotetakerPolicyError::InvalidLanguage,
      NotetakerPolicyError::InvalidPrompt,
      NotetakerPolicyError::InvalidKeyHash,
      NotetakerPolicyError::UnknownInviters("x".into()),
    ];
    for error in errors {
      let key = error_key(&error);
      for locale in [crate::i18n::Locale::En, crate::i18n::Locale::De] {
        assert_ne!(crate::i18n::translate(locale, key, None), key);
      }
    }
    for code in LANGUAGES {
      let key = format!("notetaker-language-{code}");
      assert_ne!(crate::i18n::translate(crate::i18n::Locale::De, &key, None), key);
    }
  }
}
