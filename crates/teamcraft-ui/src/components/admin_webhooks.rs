use leptos::prelude::*;
use teamcraft_proto::{
  AdminWebhook, CreateWebhookRequest, MAX_WEBHOOK_DESCRIPTION_CHARS, MAX_WEBHOOK_URL_CHARS,
  UpdateWebhookRequest, WebhookDeliveryEntry, WebhookDeliveryOutcome, WebhookEventKind, WebhookId,
};

use crate::admin::SettingsTestKind;
use crate::clock::format_date_time;
use crate::components::admin_console::SectionHeader;
use crate::components::admin_fields::{
  CheckField, FieldErrors, TextField, Validated, accept_or_refuse, bind, bind_bool, is_web_url,
};
use crate::components::copy_button::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::form_errors::error_id;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const CREATE_FORM_ID: &str = "admin-webhook-create-form";
const EDIT_FORM_ID: &str = "admin-webhook-edit-form";

/// Outgoing webhooks (docs/webhooks.md): URLs told about channel events, with the signing secret
/// shown once, a test button and the latest deliveries per webhook.
#[component]
pub fn WebhooksSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let current = Memo::new(move |_| state.read(|backend| backend.admin_webhooks()));
  let revealed = Memo::new(move |_| state.read(|backend| backend.revealed_webhook_secret()));
  let can_store_secrets = Memo::new(move |_| current.get().is_some_and(|list| list.can_store_secrets));
  let allows_private = Memo::new(move |_| current.get().is_some_and(|list| list.allows_private_targets));
  let editing = RwSignal::new(None::<WebhookId>);
  let create = Callback::new(move |draft: WebhookDraft| {
    state.write(|backend| backend.create_webhook(draft.to_create_request()));
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-webhooks-title"))
      subtitle=Signal::derive(move || i18n.t("admin-webhooks-subtitle"))
    />
    {move || {
      revealed
        .get()
        .map(|(id, secret)| {
          let text = secret.expose().to_owned();
          view! {
            <section class="integration-reveal" role="status" aria-live="polite">
              <h3>{move || i18n.t("admin-webhooks-secret-title")}</h3>
              <p class="muted-text">{move || i18n.t("admin-webhooks-secret-once")}</p>
              <label for="webhook-secret">{move || i18n.t("admin-webhooks-secret")}</label>
              <div class="integration-copy-row">
                <input id="webhook-secret" class="input mono" readonly translate="no" prop:value=text.clone() />
                <CopyButton text=text.clone() />
                <span class="grow"></span>
                <button
                  type="button"
                  class="btn"
                  data-webhook=id.to_string()
                  on:click=move |_| state.write(|backend| backend.dismiss_webhook_secret())
                >
                  {move || i18n.t("admin-integrations-reveal-done")}
                </button>
              </div>
            </section>
          }
        })
    }}
    {move || {
      if current.get().is_none() {
        Some(view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> }.into_any())
      } else if !can_store_secrets.get() {
        Some(view! { <p class="auth-alert policy-blocked" role="status">{i18n.t("admin-webhooks-secrets-unavailable")}</p> }.into_any())
      } else {
        None
      }
    }}
    <section class="policy-group" hidden=move || current.get().is_none() || !can_store_secrets.get()>
      <h3>{move || i18n.t("admin-webhooks-new")}</h3>
      <p class="muted-text small">
        {move || i18n.t(if allows_private.get() { "admin-webhooks-url-hint-private" } else { "admin-webhooks-url-hint" })}
      </p>
      <WebhookForm
        form_id=CREATE_FORM_ID
        ids=CREATE_IDS
        initial=WebhookDraft::new()
        submit_label="admin-webhooks-create"
        on_submit=create
        reset_after_submit=true
      />
    </section>
    <For
      each=move || current.get().map(|list| list.webhooks).unwrap_or_default()
      // The whole webhook: a changed health or field renders the card again.
      key=|webhook| serde_json::to_string(webhook).unwrap_or_default()
      let:webhook
    >
      <WebhookCard webhook=webhook editing=editing />
    </For>
    <Show when=move || current.get().is_some_and(|list| list.webhooks.is_empty()) fallback=|| ()>
      <p class="empty-state">{move || i18n.t("admin-webhooks-empty")}</p>
    </Show>
  }
}

/// One webhook: its target, health, events and actions, the edit form while it is edited and the
/// latest deliveries on demand.
#[component]
fn WebhookCard(webhook: AdminWebhook, editing: RwSignal<Option<WebhookId>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = webhook.id;
  let is_confirming_delete = RwSignal::new(false);
  let is_confirming_rotate = RwSignal::new(false);
  let is_showing_log = RwSignal::new(false);
  let is_testing = RwSignal::new(false);
  let test_result = Memo::new(move |_| {
    state
      .read(|backend| backend.last_settings_test())
      .filter(|(kind, _)| *kind == SettingsTestKind::Webhook(id))
      .map(|(_, response)| response)
  });
  Effect::new(move |_| {
    if test_result.get().is_some() {
      is_testing.set(false);
    }
  });
  let health = webhook.health.clone();
  let status_key = if !webhook.is_enabled {
    "admin-webhooks-status-off"
  } else if health.consecutive_failures > 0 {
    "admin-webhooks-status-failing"
  } else {
    "admin-webhooks-status-on"
  };
  let is_failing = webhook.is_enabled && health.consecutive_failures > 0;
  let locale = i18n.locale().tag();
  let last_success = health
    .maybe_last_success_at
    .map(|at| format_date_time(at.0, locale));
  let last_failure = health.maybe_last_failure_at.map(|at| {
    (
      format_date_time(at.0, locale),
      health.maybe_last_error.clone().unwrap_or_default(),
    )
  });
  let paused_until = health.maybe_paused_until.map(|at| format_date_time(at.0, locale));
  let queued = health.queued;
  let dropped = health.dropped;
  let events: Vec<WebhookEventKind> = webhook.events.clone();
  let edit_draft = WebhookDraft::from_webhook(&webhook);
  let update = Callback::new(move |draft: WebhookDraft| {
    state.write(|backend| backend.update_webhook(id, draft.to_update_request()));
    editing.set(None);
  });
  let url = webhook.url.clone();
  let description = webhook.description.clone();

  view! {
    <section class="policy-group webhook-card">
      <h3 class="webhook-url" translate="no">{url}</h3>
      {(!description.is_empty()).then(|| view! { <p class="muted-text">{description}</p> })}
      <p class="small">
        <span class="badge" class:badge-failing=is_failing>{move || i18n.t(status_key)}</span>
        " "
        <span class="muted-text">
          {move || i18n.t_count("admin-webhooks-queued", usize::try_from(queued).unwrap_or(usize::MAX))}
          " · "
          {move || i18n.t_count("admin-webhooks-dropped", usize::try_from(dropped).unwrap_or(usize::MAX))}
        </span>
      </p>
      <dl class="webhook-health small">
        {last_success.map(|at| view! {
          <dt>{move || i18n.t("admin-webhooks-last-success")}</dt>
          <dd>{at}</dd>
        })}
        {last_failure.map(|(at, error)| view! {
          <dt>{move || i18n.t("admin-webhooks-last-failure")}</dt>
          <dd>{at}" — "<span translate="no">{error}</span></dd>
        })}
        {paused_until.map(|at| view! {
          <dt>{move || i18n.t("admin-webhooks-paused-until")}</dt>
          <dd>{at}</dd>
        })}
      </dl>
      <p class="small">
        <span class="muted-text">{move || i18n.t("admin-webhooks-events")}": "</span>
        {events
          .iter()
          .map(|event| {
            let key = event_key(event);
            view! { <span class="badge">{move || i18n.t(&key)}</span>" " }
          })
          .collect_view()}
      </p>
      <div class="policy-footer">
        <button
          type="button"
          class="btn btn-small"
          on:click=move |_| editing.update(|current| *current = if *current == Some(id) { None } else { Some(id) })
        >
          {move || i18n.t("admin-webhooks-edit")}
        </button>
        <button
          type="button"
          class="btn btn-small"
          prop:disabled=move || is_testing.get()
          on:click=move |_| {
            is_testing.set(true);
            state.write(|backend| backend.test_webhook(id));
          }
        >
          {move || i18n.t("admin-webhooks-test")}
        </button>
        <Show
          when=move || is_confirming_rotate.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small" on:click=move |_| is_confirming_rotate.set(true)>
              {move || i18n.t("admin-webhooks-rotate")}
            </button>
          }
        >
          <span class="muted-text small" role="status">{move || i18n.t("admin-webhooks-rotate-warning")}</span>
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| {
              is_confirming_rotate.set(false);
              state.write(|backend| backend.rotate_webhook_secret(id));
            }
          >
            {move || i18n.t("admin-webhooks-rotate-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_rotate.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
        <button
          type="button"
          class="btn btn-small btn-quiet"
          aria-expanded=move || is_showing_log.get().to_string()
          on:click=move |_| is_showing_log.update(|is_showing| *is_showing = !*is_showing)
        >
          {move || i18n.t("admin-webhooks-deliveries")}
        </button>
        <Show
          when=move || is_confirming_delete.get()
          fallback=move || view! {
            <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming_delete.set(true)>
              {move || i18n.t("admin-webhooks-delete")}
            </button>
          }
        >
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| state.write(|backend| backend.delete_webhook(id))
          >
            {move || i18n.t("admin-webhooks-delete-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_delete.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </div>
      <p class="muted-text small" role="status" aria-live="polite">
        {move || {
          if is_testing.get() {
            i18n.t("admin-test-running")
          } else {
            match test_result.get() {
              Some(response) if response.is_ok => i18n.t("admin-webhooks-test-ok"),
              Some(response) => {
                i18n.t_with("admin-test-failed", &[("detail", &response.maybe_error.unwrap_or_default())])
              },
              None => String::new(),
            }
          }
        }}
      </p>
      <Show when=move || editing.get() == Some(id) fallback=|| ()>
        <WebhookForm
          form_id=EDIT_FORM_ID
          ids=EDIT_IDS
          initial=edit_draft.clone()
          submit_label="admin-save"
          on_submit=update
          reset_after_submit=false
        />
      </Show>
      <Show when=move || is_showing_log.get() fallback=|| ()>
        <DeliveryLog id=id />
      </Show>
    </section>
  }
}

/// The latest delivery attempts of a webhook, newest first.
#[component]
fn DeliveryLog(id: WebhookId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let entries = Memo::new(move |_| state.read(|backend| backend.webhook_deliveries(id)));
  view! {
    {move || match entries.get() {
      None => view! { <p class="muted-text small" role="status">{i18n.t("admin-settings-loading")}</p> }.into_any(),
      Some(log) if log.entries.is_empty() => {
        view! { <p class="empty-state">{i18n.t("admin-webhooks-deliveries-empty")}</p> }.into_any()
      },
      Some(log) => view! {
        <div class="table-wrap">
          <table class="data-table">
            <caption class="visually-hidden">{i18n.t("admin-webhooks-deliveries")}</caption>
            <thead>
              <tr>
                <th scope="col">{i18n.t("admin-webhooks-col-time")}</th>
                <th scope="col">{i18n.t("admin-webhooks-events")}</th>
                <th scope="col">{i18n.t("admin-webhooks-col-attempt")}</th>
                <th scope="col">{i18n.t("admin-webhooks-col-result")}</th>
              </tr>
            </thead>
            <tbody>
              {log.entries.into_iter().map(|entry| view! { <DeliveryRow entry=entry /> }).collect_view()}
            </tbody>
          </table>
        </div>
      }
      .into_any(),
    }}
  }
}

#[component]
fn DeliveryRow(entry: WebhookDeliveryEntry) -> impl IntoView {
  let i18n = use_i18n();
  let outcome_key = match entry.outcome {
    WebhookDeliveryOutcome::Delivered => "admin-webhooks-outcome-delivered",
    WebhookDeliveryOutcome::Retrying => "admin-webhooks-outcome-retrying",
    WebhookDeliveryOutcome::Failed | WebhookDeliveryOutcome::Unknown(_) => "admin-webhooks-outcome-failed",
  };
  let detail = match (entry.maybe_status_code, entry.maybe_error.clone()) {
    (Some(code), _) => format!("HTTP {code} · {} ms", entry.duration_ms),
    (None, Some(error)) => error,
    (None, None) => format!("{} ms", entry.duration_ms),
  };
  let event_key = event_key(&entry.event);
  view! {
    <tr>
      <td>{format_date_time(entry.at.0, i18n.locale().tag())}</td>
      <td>{move || i18n.t(&event_key)}</td>
      <td>{entry.attempt}</td>
      <td>
        {move || i18n.t(outcome_key)}
        " "
        <span class="muted-text small" translate="no">{detail.clone()}</span>
      </td>
    </tr>
  }
}

/// The element IDs of one webhook form; the console has one create form and one edit form.
#[derive(Clone, Copy)]
struct FormIds {
  url: &'static str,
  description: &'static str,
  events: &'static str,
}

const CREATE_IDS: FormIds = FormIds {
  url: "webhook-new-url",
  description: "webhook-new-description",
  events: "webhook-new-events",
};
const EDIT_IDS: FormIds = FormIds {
  url: "webhook-edit-url",
  description: "webhook-edit-description",
  events: "webhook-edit-events",
};

#[component]
fn WebhookForm(
  form_id: &'static str,
  ids: FormIds,
  initial: WebhookDraft,
  submit_label: &'static str,
  on_submit: Callback<WebhookDraft>,
  reset_after_submit: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let draft = RwSignal::new(initial);
  let show_errors = RwSignal::new(false);
  let url = bind(draft, show_errors, "url", |d| &d.url, |d| &mut d.url);
  let description = bind(
    draft,
    show_errors,
    "description",
    |d| &d.description,
    |d| &mut d.description,
  );
  let (is_enabled, set_enabled) = bind_bool(draft, |d| d.is_enabled, |d, v| d.is_enabled = v);
  let events_error = Signal::derive(move || {
    if show_errors.get() {
      draft.with(|draft| {
        draft
          .errors()
          .into_iter()
          .find(|(field, _)| *field == "events")
          .map(|(_, key)| key)
      })
    } else {
      None
    }
  });
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if !accept_or_refuse(draft, show_errors, form_id) {
      return;
    }
    on_submit.run(draft.get_untracked());
    if reset_after_submit {
      draft.set(WebhookDraft::new());
      show_errors.set(false);
    }
  };
  view! {
    <form id=form_id class="policy-form" on:submit=submit novalidate>
      <TextField
        id=ids.url
        label="admin-webhooks-url"
        placeholder="https://hooks.example.com/teamcraft"
        input_type="url"
        value=url.value
        on_input=url.on_input
        error=url.error
      />
      <TextField
        id=ids.description
        label="admin-webhooks-description"
        hint="admin-webhooks-description-hint"
        value=description.value
        on_input=description.on_input
        error=description.error
      />
      <fieldset
        id=ids.events
        class="field"
        role="group"
        tabindex="-1"
        aria-invalid=move || events_error.get().map(|_| "true")
        aria-describedby=move || events_error.get().map(|_| error_id(ids.events))
      >
        <legend>{move || i18n.t("admin-webhooks-events")}</legend>
        {WebhookEventKind::SUBSCRIBABLE
          .into_iter()
          .map(|event| {
            let key = event_key(&event);
            let checked_event = event.clone();
            let toggled_event = event.clone();
            view! {
              <label class="checkbox check-field">
                <input
                  type="checkbox"
                  prop:checked=move || draft.with(|draft| draft.events.contains(&checked_event))
                  on:change=move |ev| {
                    let is_on = event_target_checked(&ev);
                    let event = toggled_event.clone();
                    draft.update(|draft| {
                      draft.events.retain(|existing| *existing != event);
                      if is_on {
                        draft.events.push(event);
                      }
                    });
                  }
                />
                <span>{move || i18n.t(&key)}</span>
              </label>
            }
          })
          .collect_view()}
        <p class="field-error" id=error_id(ids.events)>{move || events_error.get().map(|key| i18n.t(key))}</p>
      </fieldset>
      <CheckField label="admin-webhooks-enabled" checked=is_enabled on_change=set_enabled />
      <div class="policy-footer">
        <button type="submit" class="btn btn-primary">
          <Icon name=IconName::Plus />
          {move || i18n.t(submit_label)}
        </button>
      </div>
    </form>
  }
}

/// What a webhook form holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WebhookDraft {
  url: String,
  description: String,
  events: Vec<WebhookEventKind>,
  is_enabled: bool,
}

impl WebhookDraft {
  fn new() -> Self {
    Self {
      url: String::new(),
      description: String::new(),
      events: Vec::new(),
      is_enabled: true,
    }
  }

  fn from_webhook(webhook: &AdminWebhook) -> Self {
    Self {
      url: webhook.url.clone(),
      description: webhook.description.clone(),
      events: webhook.events.clone(),
      is_enabled: webhook.is_enabled,
    }
  }

  /// The events in the console's order.
  fn ordered_events(&self) -> Vec<WebhookEventKind> {
    WebhookEventKind::SUBSCRIBABLE
      .into_iter()
      .filter(|event| self.events.contains(event))
      .collect()
  }

  fn to_create_request(&self) -> CreateWebhookRequest {
    CreateWebhookRequest {
      url: self.url.trim().to_owned(),
      description: self.description.trim().to_owned(),
      events: self.ordered_events(),
      is_enabled: self.is_enabled,
    }
  }

  fn to_update_request(&self) -> UpdateWebhookRequest {
    UpdateWebhookRequest {
      url: self.url.trim().to_owned(),
      description: self.description.trim().to_owned(),
      events: self.ordered_events(),
      is_enabled: self.is_enabled,
    }
  }
}

impl Validated for WebhookDraft {
  fn errors(&self) -> FieldErrors {
    let mut errors = FieldErrors::new();
    let url = self.url.trim();
    if !is_web_url(url) || url.chars().count() > MAX_WEBHOOK_URL_CHARS {
      errors.push(("url", "admin-error-url"));
    }
    if self.description.trim().chars().count() > MAX_WEBHOOK_DESCRIPTION_CHARS {
      errors.push(("description", "admin-webhooks-error-description"));
    }
    if self.ordered_events().is_empty() {
      errors.push(("events", "admin-webhooks-error-events"));
    }
    errors
  }
}

/// The i18n key of an event: `admin-webhooks-event-member-joined` for `member.joined`.
fn event_key(event: &WebhookEventKind) -> String {
  format!("admin-webhooks-event-{}", event.as_str().replace('.', "-"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_draft_needs_a_url_and_an_event() {
    let mut draft = WebhookDraft::new();
    let fields = |draft: &WebhookDraft| -> Vec<&'static str> {
      draft.errors().into_iter().map(|(field, _)| field).collect()
    };
    assert_eq!(fields(&draft), ["url", "events"]);
    draft.url = "https://hooks.example.com/teamcraft".into();
    draft.events = vec![WebhookEventKind::CallEnded, WebhookEventKind::ChannelCreated];
    assert!(fields(&draft).is_empty());
    assert_eq!(
      draft.to_create_request().events,
      [WebhookEventKind::ChannelCreated, WebhookEventKind::CallEnded],
      "in the console's order"
    );
  }

  #[test]
  fn every_label_is_translated() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<String> = [
      "admin-nav-webhooks",
      "admin-webhooks-title",
      "admin-webhooks-subtitle",
      "admin-webhooks-secret-title",
      "admin-webhooks-secret-once",
      "admin-webhooks-secret",
      "admin-webhooks-secrets-unavailable",
      "admin-webhooks-new",
      "admin-webhooks-url-hint",
      "admin-webhooks-url-hint-private",
      "admin-webhooks-create",
      "admin-webhooks-empty",
      "admin-webhooks-status-off",
      "admin-webhooks-status-failing",
      "admin-webhooks-status-on",
      "admin-webhooks-last-success",
      "admin-webhooks-last-failure",
      "admin-webhooks-paused-until",
      "admin-webhooks-events",
      "admin-webhooks-edit",
      "admin-webhooks-test",
      "admin-webhooks-test-ok",
      "admin-webhooks-rotate",
      "admin-webhooks-rotate-warning",
      "admin-webhooks-rotate-confirm",
      "admin-webhooks-deliveries",
      "admin-webhooks-deliveries-empty",
      "admin-webhooks-delete",
      "admin-webhooks-delete-confirm",
      "admin-webhooks-col-time",
      "admin-webhooks-col-attempt",
      "admin-webhooks-col-result",
      "admin-webhooks-outcome-delivered",
      "admin-webhooks-outcome-retrying",
      "admin-webhooks-outcome-failed",
      "admin-webhooks-url",
      "admin-webhooks-description",
      "admin-webhooks-description-hint",
      "admin-webhooks-enabled",
      "admin-webhooks-error-description",
      "admin-webhooks-error-events",
      "admin-webhooks-queued",
      "admin-webhooks-dropped",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    keys.extend(WebhookEventKind::SUBSCRIBABLE.iter().map(event_key));
    keys.push(event_key(&WebhookEventKind::Ping));
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(translate(locale, key, None), *key, "{locale:?} lacks {key}");
      }
    }
  }
}
