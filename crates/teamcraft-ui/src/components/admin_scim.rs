use leptos::prelude::*;
use teamcraft_proto::scim::SCIM_BASE_PATH;
use teamcraft_proto::{
  CreateScimClientRequest, MAX_SCIM_CLIENT_NAME_CHARS, MAX_SCIM_CLIENTS, ScimClientResponse,
  ScimIdentityAttribute,
};

use crate::clock::{format_date, format_relative, now_ms};
use crate::components::admin_fields::{FieldErrors, TextField, Validated, accept_or_refuse, bind};
use crate::components::copy_button::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const CREATE_FORM_ID: &str = "scim-client-create-form";
const NAME_FIELD_ID: &str = "scim-client-name";
/// The identity attributes an admin can pick, in the form's order.
const ATTRIBUTES: [ScimIdentityAttribute; 2] =
  [ScimIdentityAttribute::ExternalId, ScimIdentityAttribute::UserName];

/// Identity provider applications that push accounts and groups through SCIM
/// (docs/provisioning.md §7): the base URL to give them, one token per client (shown once, rotated
/// or revoked here) and a form to add one.
#[component]
pub fn ScimClientsCard() -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let current = Memo::new(move |_| state.read(|backend| backend.scim_clients()));
  let revealed = Memo::new(move |_| state.read(|backend| backend.revealed_scim_token()));
  let base_url = Memo::new(move |_| {
    current
      .get()
      .and_then(|list| list.maybe_base_url)
      .unwrap_or_else(|| SCIM_BASE_PATH.to_owned())
  });
  let is_full = Memo::new(move |_| {
    current
      .get()
      .is_some_and(|list| list.clients.len() >= MAX_SCIM_CLIENTS)
  });
  let create = Callback::new(move |draft: ScimClientDraft| {
    state.write(|backend| backend.create_scim_client(draft.to_request()));
  });

  view! {
    <section class="instance-card" aria-labelledby="health-scim">
      <h3 id="health-scim"><Icon name=IconName::Users /> {move || i18n.t("scim-title")}</h3>
      <p class="muted-text small">{move || i18n.t("scim-hint")}</p>
      <label for="scim-base-url">{move || i18n.t("scim-base-url")}</label>
      <div class="integration-copy-row">
        <input id="scim-base-url" class="input mono" readonly translate="no" prop:value=move || base_url.get() />
        <CopyButton text=Signal::derive(move || base_url.get()) />
      </div>
      {move || revealed.get().map(|(id, token)| {
        let text = token.expose().to_owned();
        view! {
          <section class="integration-reveal" role="status" aria-live="polite">
            <p class="muted-text">{move || i18n.t("scim-token-once")}</p>
            <label for="scim-token">{move || i18n.t("scim-token")}</label>
            <div class="integration-copy-row">
              <input id="scim-token" class="input mono" readonly translate="no" prop:value=text.clone() />
              <CopyButton text=text.clone() />
              <span class="grow"></span>
              <button
                type="button"
                class="btn"
                data-scim-client=id.to_string()
                on:click=move |_| state.write(|backend| backend.dismiss_scim_token())
              >
                {move || i18n.t("admin-integrations-reveal-done")}
              </button>
            </div>
          </section>
        }
      })}
      {move || match current.get() {
        None => view! { <p class="muted-text" role="status">{move || i18n.t("scim-loading")}</p> }.into_any(),
        Some(list) if list.clients.is_empty() => view! { <p class="muted-text">{move || i18n.t("scim-empty")}</p> }.into_any(),
        Some(list) => view! {
          <ul class="health-alert-list">
            {list.clients.into_iter().map(|client| view! { <ScimClientRow client=client /> }).collect_view()}
          </ul>
        }.into_any(),
      }}
      <Show
        when=move || is_full.get()
        fallback=move || view! { <ScimClientForm on_submit=create /> }
      >
        <p class="muted-text small">{move || i18n.t_count("scim-limit-reached", MAX_SCIM_CLIENTS)}</p>
      </Show>
    </section>
  }
}

/// One SCIM client: what it matches sign-ins by, what it provisioned, when it was last used, and
/// a new token or revoking it, each confirmed inline.
#[component]
fn ScimClientRow(client: ScimClientResponse) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let id = client.id;
  let is_confirming_rotate = RwSignal::new(false);
  let is_confirming_revoke = RwSignal::new(false);
  let locale = move || i18n.locale().tag();
  let attribute = attribute_name(&client.identity_attribute);
  let users = usize::try_from(client.user_count).unwrap_or(usize::MAX);
  let groups = usize::try_from(client.group_count).unwrap_or(usize::MAX);
  let maybe_last_used = client.maybe_last_used_at.map(|at| at.0);
  let rotated_at = client.token_rotated_at.0;
  view! {
    <li>
      <strong translate="no">{client.name}</strong>
      <p class="muted-text small">
        {move || i18n.t_with("scim-client-attribute", &[("attribute", &attribute)])}
        " · "
        {move || i18n.t_count("scim-client-users", users)}
        " · "
        {move || i18n.t_count("scim-client-groups", groups)}
      </p>
      <p class="muted-text small">
        {move || match maybe_last_used {
          Some(at) => i18n.t_with("scim-client-last-used", &[("when", &format_relative(at, now_ms(), locale()))]),
          None => i18n.t("scim-client-never-used"),
        }}
        " · "
        {move || i18n.t_with("scim-client-token-rotated", &[("date", &format_date(rotated_at, locale()))])}
      </p>
      <p class="row-actions">
        <Show
          when=move || is_confirming_rotate.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small" on:click=move |_| is_confirming_rotate.set(true)>
              {move || i18n.t("scim-rotate")}
            </button>
          }
        >
          <span>{move || i18n.t("scim-rotate-question")}</span>
          " "
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| {
              is_confirming_rotate.set(false);
              state.write(|backend| backend.rotate_scim_client_token(id));
            }
          >
            {move || i18n.t("scim-rotate-yes")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_rotate.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
        " "
        <Show
          when=move || is_confirming_revoke.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming_revoke.set(true)>
              {move || i18n.t("scim-revoke")}
            </button>
          }
        >
          <span>{move || i18n.t("scim-revoke-question")}</span>
          " "
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| {
              is_confirming_revoke.set(false);
              state.write(|backend| backend.delete_scim_client(id));
            }
          >
            {move || i18n.t("scim-revoke-yes")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_revoke.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </p>
    </li>
  }
}

/// Adds a SCIM client: a name the admin recognises and the attribute that equals the sign-in
/// subject.
#[component]
fn ScimClientForm(on_submit: Callback<ScimClientDraft>) -> impl IntoView {
  let i18n = use_i18n();
  let draft = RwSignal::new(ScimClientDraft::new());
  let show_errors = RwSignal::new(false);
  let name = bind(draft, show_errors, NAME_FIELD_ID, |d| &d.name, |d| &mut d.name);
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !accept_or_refuse(draft, show_errors, CREATE_FORM_ID) {
      return;
    }
    on_submit.run(draft.get_untracked());
    draft.set(ScimClientDraft::new());
    show_errors.set(false);
  };
  view! {
    <form id=CREATE_FORM_ID class="policy-form" on:submit=submit novalidate>
      <h4>{move || i18n.t("scim-new")}</h4>
      <TextField
        id=NAME_FIELD_ID
        label="scim-name"
        hint="scim-name-hint"
        value=name.value
        on_input=name.on_input
        error=name.error
      />
      <fieldset class="radio-group" aria-describedby="scim-attribute-hint">
        <legend>{move || i18n.t("scim-attribute")}</legend>
        <p id="scim-attribute-hint" class="muted-text small">{move || i18n.t("scim-attribute-hint")}</p>
        {ATTRIBUTES
          .into_iter()
          .map(|attribute| {
            let slug = attribute.as_str().to_owned();
            let hint_key = format!("scim-attribute-{}-hint", slug.replace('_', "-"));
            let name = attribute_name(&attribute);
            let checked = attribute.clone();
            view! {
              <label class="radio radio-explained">
                <input
                  type="radio"
                  name="scim_identity_attribute"
                  value=slug
                  prop:checked=move || draft.with(|draft| draft.identity_attribute == checked)
                  on:change=move |_| draft.update(|draft| draft.identity_attribute = attribute.clone())
                />
                <span>
                  <strong translate="no">{name}</strong>
                  <span class="muted-text small">{move || i18n.t(&hint_key)}</span>
                </span>
              </label>
            }
          })
          .collect_view()}
      </fieldset>
      <div class="policy-footer">
        <button type="submit" class="btn btn-primary">
          <Icon name=IconName::Plus />
          {move || i18n.t("scim-create")}
        </button>
      </div>
    </form>
  }
}

/// What the add form holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ScimClientDraft {
  name: String,
  identity_attribute: ScimIdentityAttribute,
}

impl ScimClientDraft {
  fn new() -> Self {
    Self {
      name: String::new(),
      identity_attribute: ScimIdentityAttribute::ExternalId,
    }
  }

  fn to_request(&self) -> CreateScimClientRequest {
    CreateScimClientRequest {
      name: self.name.trim().to_owned(),
      identity_attribute: self.identity_attribute.clone(),
    }
  }
}

impl Validated for ScimClientDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    let length = self.name.trim().chars().count();
    if length == 0 || length > MAX_SCIM_CLIENT_NAME_CHARS {
      errors.push((NAME_FIELD_ID, "scim-error-name"));
    }
    errors
  }
}

/// The attribute's name as SCIM spells it; a technical name, the same in every language.
fn attribute_name(attribute: &ScimIdentityAttribute) -> String {
  match attribute {
    ScimIdentityAttribute::ExternalId => "externalId".to_owned(),
    ScimIdentityAttribute::UserName => "userName".to_owned(),
    ScimIdentityAttribute::Unknown(other) => other.clone(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_draft_needs_a_name_within_the_limit() {
    let mut draft = ScimClientDraft::new();
    assert_eq!(draft.errors(), [(NAME_FIELD_ID, "scim-error-name")]);
    draft.name = "   ".into();
    assert_eq!(draft.errors().len(), 1, "blank is empty");
    draft.name = "x".repeat(MAX_SCIM_CLIENT_NAME_CHARS + 1);
    assert_eq!(draft.errors().len(), 1, "too long");
    draft.name = "  Identity provider  ".into();
    assert!(draft.errors().is_empty());
    assert_eq!(draft.to_request().name, "Identity provider");
    assert_eq!(
      draft.to_request().identity_attribute,
      ScimIdentityAttribute::ExternalId
    );
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let keys = [
      "scim-title",
      "scim-hint",
      "scim-base-url",
      "scim-token",
      "scim-token-once",
      "scim-loading",
      "scim-empty",
      "scim-limit-reached",
      "scim-client-attribute",
      "scim-client-users",
      "scim-client-groups",
      "scim-client-last-used",
      "scim-client-never-used",
      "scim-client-token-rotated",
      "scim-rotate",
      "scim-rotate-question",
      "scim-rotate-yes",
      "scim-revoke",
      "scim-revoke-question",
      "scim-revoke-yes",
      "scim-new",
      "scim-name",
      "scim-name-hint",
      "scim-attribute",
      "scim-attribute-hint",
      "scim-attribute-external-id-hint",
      "scim-attribute-user-name-hint",
      "scim-create",
      "scim-error-name",
    ];
    for locale in [Locale::En, Locale::De] {
      for key in keys {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
