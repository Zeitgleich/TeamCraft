//! Legal hold in the console (issue #211, admin.md §3.14): the held channels with their case
//! reference, a form to place a hold, and "Release". Holds are part of the signed workspace policy,
//! so only owners change them (their app signs); admins see the list read-only.

use leptos::prelude::*;
use teamcraft_proto::{
  ConversationId, LegalHold, MAX_LEGAL_HOLD_REFERENCE_CHARS, MAX_LEGAL_HOLDS, UnixMillis,
};

use crate::admin::AdminChannel;
use crate::clock::{format_date, now_ms};
use crate::components::admin_policy::{PolicyBlockerNote, use_policy_draft};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::state::use_app_state;

const FORM_ID: &str = "legal-hold-form";
const CHANNEL_FIELD: &str = "legal-hold-channel";
const REFERENCE_FIELD: &str = "legal-hold-reference";
const REFERENCE_HINT: &str = "legal-hold-reference-hint";

#[component]
pub fn LegalHoldPanel() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let errors = FormErrors::new();
  let channel = RwSignal::new(String::new());
  let reference = RwSignal::new(String::new());
  let holds = Memo::new(move |_| policy.draft.get().compliance_archive.legal_holds);
  let channels = Memo::new(move |_| {
    state
      .read(|backend| backend.admin_channels())
      .into_iter()
      .filter(|channel| channel.kind != ConversationKind::Direct)
      .collect::<Vec<AdminChannel>>()
  });
  let name_of = move |id: ConversationId| {
    channels
      .with(|list| {
        list
          .iter()
          .find(|channel| channel.id == id)
          .map(|channel| channel.name.clone())
      })
      .or_else(|| {
        state
          .read(|backend| backend.conversation(id))
          .map(|channel| channel.name)
      })
      .unwrap_or_else(|| id.to_string())
  };

  let check = move || -> Result<LegalHold, FieldErrors> {
    let maybe_channel = channel.get().parse::<ConversationId>().ok();
    let text = reference.get();
    let found = form_errors(maybe_channel, &text, &holds.get());
    let mut fields = FieldErrors::new();
    for (field, key) in found {
      let message = match key {
        "legal-hold-error-reference" => {
          i18n.t_with(key, &[("max", &MAX_LEGAL_HOLD_REFERENCE_CHARS.to_string())])
        },
        "legal-hold-error-full" => i18n.t_with(key, &[("max", &MAX_LEGAL_HOLDS.to_string())]),
        key => i18n.t(key),
      };
      fields.push(field, message);
    }
    match (maybe_channel, fields.is_empty()) {
      (Some(conversation_id), true) => Ok(LegalHold {
        conversation_id,
        reference: text.trim().to_owned(),
        placed_at: UnixMillis(now_ms()),
      }),
      _ => Err(fields),
    }
  };
  // After a refused submit the errors follow the input (forms.md).
  Effect::new(move |_| errors.follow(check().err().unwrap_or_default()));
  let place = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    match check() {
      Ok(hold) => {
        errors.clear();
        policy.edit(|p| p.compliance_archive.legal_holds.push(hold));
        policy.save();
        channel.set(String::new());
        reference.set(String::new());
      },
      Err(found) => errors.refuse(FORM_ID, found),
    }
  };
  let release = move |conversation: ConversationId| {
    policy.edit(|p| {
      p.compliance_archive
        .legal_holds
        .retain(|hold| hold.conversation_id != conversation)
    });
    policy.save();
  };

  view! {
    <section class="admin-subsection legal-holds" aria-labelledby="legal-hold-title">
      <h3 id="legal-hold-title">{move || i18n.t("legal-hold-title")}</h3>
      <p class="muted-text small">{move || i18n.t("legal-hold-intro")}</p>
      <PolicyBlockerNote policy=policy />
      <Show
        when=move || !holds.with(Vec::is_empty)
        fallback=move || view! { <p class="empty-state">{move || i18n.t("legal-hold-none")}</p> }
      >
        <div class="table-wrap">
          <table class="data-table">
            <caption class="visually-hidden">{move || i18n.t("legal-hold-title")}</caption>
            <thead>
              <tr>
                <th scope="col">{move || i18n.t("admin-col-channel")}</th>
                <th scope="col">{move || i18n.t("legal-hold-col-reference")}</th>
                <th scope="col">{move || i18n.t("legal-hold-col-since")}</th>
                <th scope="col"><span class="visually-hidden">{move || i18n.t("retention-actions")}</span></th>
              </tr>
            </thead>
            <tbody>
              <For each=move || holds.get() key=|hold| (hold.conversation_id, hold.reference.clone()) let:hold>
                {
                  let id = hold.conversation_id;
                  let name = name_of(id);
                  let label_name = name.clone();
                  view! {
                    <tr>
                      <th scope="row"><span class="row-name" translate="no">{format!("# {name}")}</span></th>
                      <td translate="no">{hold.reference.clone()}</td>
                      <td>{move || format_date(hold.placed_at.0, i18n.locale().tag())}</td>
                      <td>
                        <button
                          type="button"
                          class="btn btn-small"
                          prop:disabled=move || policy.is_locked()
                          aria-label=move || i18n.t_with("legal-hold-release-label", &[("channel", &label_name)])
                          on:click=move |_| release(id)
                        >
                          {move || i18n.t("legal-hold-release")}
                        </button>
                      </td>
                    </tr>
                  }
                }
              </For>
            </tbody>
          </table>
        </div>
      </Show>
      <form id=FORM_ID class="legal-hold-form" novalidate on:submit=place>
        <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
          <legend>{move || i18n.t("legal-hold-place-title")}</legend>
          <div class="field">
            <label for=CHANNEL_FIELD>{move || i18n.t("admin-col-channel")}</label>
            <select
              id=CHANNEL_FIELD
              class="input"
              aria-invalid=errors.invalid(CHANNEL_FIELD)
              aria-describedby=errors.described(CHANNEL_FIELD)
              prop:value=move || channel.get()
              on:change=move |ev| channel.set(event_target_value(&ev))
            >
              <option value="">{move || i18n.t("legal-hold-choose-channel")}</option>
              {move || {
                let held = holds.get();
                channels
                  .get()
                  .into_iter()
                  .filter(|channel| teamcraft_proto::legal_hold_of(&held, channel.id).is_none())
                  .map(|channel| {
                    let label = format!("# {} ({})", channel.name, channel.space_name);
                    view! { <option value=channel.id.to_string()>{label}</option> }
                  })
                  .collect_view()
              }}
            </select>
            <FieldError errors=errors field=CHANNEL_FIELD />
          </div>
          <div class="field">
            <label for=REFERENCE_FIELD>{move || i18n.t("legal-hold-reference")}</label>
            <input
              id=REFERENCE_FIELD
              class="input"
              type="text"
              autocomplete="off"
              maxlength=MAX_LEGAL_HOLD_REFERENCE_CHARS.to_string()
              aria-invalid=errors.invalid(REFERENCE_FIELD)
              aria-describedby=errors.described_or(REFERENCE_FIELD, Some(REFERENCE_HINT))
              prop:value=move || reference.get()
              on:input=move |ev| reference.set(event_target_value(&ev))
            />
            <p class="muted-text small" id=REFERENCE_HINT>{move || i18n.t("legal-hold-reference-hint")}</p>
            <FieldError errors=errors field=REFERENCE_FIELD />
          </div>
          <button type="submit" class="btn btn-primary">{move || i18n.t("legal-hold-place")}</button>
        </fieldset>
      </form>
    </section>
  }
}

/// What is wrong with the form, in the order of the fields: the field and the message's i18n key.
fn form_errors(
  maybe_channel: Option<ConversationId>,
  reference: &str,
  holds: &[LegalHold],
) -> Vec<(&'static str, &'static str)> {
  let mut found = Vec::new();
  match maybe_channel {
    None => found.push((CHANNEL_FIELD, "legal-hold-error-channel")),
    Some(_) if holds.len() >= MAX_LEGAL_HOLDS => found.push((CHANNEL_FIELD, "legal-hold-error-full")),
    Some(id) if teamcraft_proto::legal_hold_of(holds, id).is_some() => {
      found.push((CHANNEL_FIELD, "legal-hold-error-held"));
    },
    Some(_) => {},
  }
  if !LegalHold::is_valid_reference(reference.trim()) {
    found.push((REFERENCE_FIELD, "legal-hold-error-reference"));
  }
  found
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn every_error_shows_at_its_field_in_order() {
    assert_eq!(
      form_errors(None, "  ", &[]),
      vec![
        (CHANNEL_FIELD, "legal-hold-error-channel"),
        (REFERENCE_FIELD, "legal-hold-error-reference"),
      ]
    );
    assert_eq!(form_errors(Some(id(1)), " Case 7 ", &[]), vec![]);
    assert_eq!(
      form_errors(Some(id(1)), &"x".repeat(65), &[]),
      vec![(REFERENCE_FIELD, "legal-hold-error-reference")]
    );
  }

  #[test]
  fn a_held_channel_or_a_full_list_is_refused() {
    let held = [hold(1)];
    assert_eq!(
      form_errors(Some(id(1)), "Case 7", &held),
      vec![(CHANNEL_FIELD, "legal-hold-error-held")]
    );
    let full: Vec<_> = (1..=MAX_LEGAL_HOLDS as i64).map(hold).collect();
    assert_eq!(
      form_errors(Some(id(9999)), "Case 7", &full),
      vec![(CHANNEL_FIELD, "legal-hold-error-full")]
    );
  }

  fn id(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn hold(raw: i64) -> LegalHold {
    LegalHold {
      conversation_id: id(raw),
      reference: "Case".to_owned(),
      placed_at: UnixMillis(0),
    }
  }
}
