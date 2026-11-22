//! Legal hold in the console (issue #211, admin.md §3.14): the held channels and people with their
//! case reference and what each hold covers, a form to place a hold on a channel or a person, and
//! "Release". A person's hold (issue #2135) covers every conversation they are in, or only their
//! direct messages and groups: the console never lists other people's direct messages, so that is
//! how they are held. Holds are part of the signed workspace policy, so only owners change them
//! (their app signs); admins see the list read-only.

use leptos::prelude::*;
use teamcraft_proto::{
  ConversationId, Id, LegalHold, MAX_LEGAL_HOLD_REFERENCE_CHARS, MAX_LEGAL_HOLDS, PersonHoldScope,
  PersonLegalHold, UnixMillis, UserId,
};

use crate::admin::{AdminChannel, AdminUser};
use crate::clock::{format_date, now_ms};
use crate::components::admin_policy::{PolicyBlockerNote, use_policy_draft};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::state::use_app_state;

const FORM_ID: &str = "legal-hold-form";
const CHANNEL_FIELD: &str = "legal-hold-channel";
const PERSON_FIELD: &str = "legal-hold-person";
const REFERENCE_FIELD: &str = "legal-hold-reference";
const REFERENCE_HINT: &str = "legal-hold-reference-hint";

/// What the form places a hold on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HoldTarget {
  Channel,
  Person,
}

/// One row of the table: a conversation's hold or a person's.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HeldRow {
  Conversation(LegalHold),
  Person(PersonLegalHold),
}

impl HeldRow {
  /// Stable per row: what is held, and the reference (a re-placed hold is a new row).
  fn key(&self) -> (bool, i64, String) {
    match self {
      Self::Conversation(hold) => (
        false,
        hold.conversation_id.snowflake().get(),
        hold.reference.clone(),
      ),
      Self::Person(hold) => (true, hold.user_id.snowflake().get(), hold.reference.clone()),
    }
  }

  fn reference(&self) -> &str {
    match self {
      Self::Conversation(hold) => &hold.reference,
      Self::Person(hold) => &hold.reference,
    }
  }

  fn placed_at(&self) -> UnixMillis {
    match self {
      Self::Conversation(hold) => hold.placed_at,
      Self::Person(hold) => hold.placed_at,
    }
  }
}

#[component]
pub fn LegalHoldPanel() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let errors = FormErrors::new();
  let target = RwSignal::new(HoldTarget::Channel);
  let channel = RwSignal::new(String::new());
  let person = RwSignal::new(String::new());
  let scope = RwSignal::new(PersonHoldScope::All);
  let reference = RwSignal::new(String::new());
  let holds = Memo::new(move |_| policy.draft.get().compliance_archive.legal_holds);
  let people = Memo::new(move |_| policy.draft.get().compliance_archive.person_holds);
  let rows = Memo::new(move |_| {
    holds
      .get()
      .into_iter()
      .map(HeldRow::Conversation)
      .chain(people.get().into_iter().map(HeldRow::Person))
      .collect::<Vec<_>>()
  });
  let channels = Memo::new(move |_| {
    state
      .read(|backend| backend.admin_channels())
      .into_iter()
      // Meeting chats go with their meeting, which a hold can't stop; direct messages are held
      // through a person, as the console never lists them (§4.2).
      .filter(|channel| matches!(channel.kind, ConversationKind::Public | ConversationKind::Private))
      .collect::<Vec<AdminChannel>>()
  });
  let users = Memo::new(move |_| {
    state
      .read(|backend| backend.admin_users())
      .into_iter()
      .filter(|user| !user.user.is_deleted && !user.user.is_bot)
      .collect::<Vec<AdminUser>>()
  });
  let channel_name = move |id: ConversationId| {
    channels
      .with(|list| {
        list
          .iter()
          .find(|channel| channel.id == id)
          .map(|channel| format!("# {}", channel.name))
      })
      .or_else(|| {
        state.read(|backend| backend.conversation(id)).map(|known| {
          if known.kind.is_direct_like() {
            known.name
          } else {
            format!("# {}", known.name)
          }
        })
      })
      .unwrap_or_else(|| id.to_string())
  };
  let person_name = move |id: UserId| {
    users
      .with(|list| {
        list
          .iter()
          .find(|user| user.user.id == id)
          .map(|user| user.user.display_name.clone())
      })
      .or_else(|| {
        state
          .read(|backend| backend.user(id))
          .map(|user| user.display_name)
      })
      .unwrap_or_else(|| id.to_string())
  };

  let check = move || -> Result<HeldRow, FieldErrors> {
    let chosen = Choice {
      target: target.get(),
      maybe_channel: channel.get().parse::<ConversationId>().ok(),
      maybe_person: person.get().parse::<UserId>().ok(),
    };
    let text = reference.get();
    let found = form_errors(&chosen, &text, &holds.get(), &people.get());
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
    if !fields.is_empty() {
      return Err(fields);
    }
    let reference = text.trim().to_owned();
    let placed_at = UnixMillis(now_ms());
    match (chosen.target, chosen.maybe_channel, chosen.maybe_person) {
      (HoldTarget::Channel, Some(conversation_id), _) => Ok(HeldRow::Conversation(LegalHold {
        conversation_id,
        reference,
        placed_at,
      })),
      (HoldTarget::Person, _, Some(user_id)) => Ok(HeldRow::Person(PersonLegalHold {
        user_id,
        scope: scope.get(),
        reference,
        placed_at,
      })),
      _ => Err(fields),
    }
  };
  // After a refused submit the errors follow the input (forms.md).
  Effect::new(move |_| errors.follow(check().err().unwrap_or_default()));
  let place = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    match check() {
      Ok(row) => {
        errors.clear();
        policy.edit(|p| match row {
          HeldRow::Conversation(hold) => p.compliance_archive.legal_holds.push(hold),
          HeldRow::Person(hold) => p.compliance_archive.person_holds.push(hold),
        });
        policy.save();
        channel.set(String::new());
        person.set(String::new());
        scope.set(PersonHoldScope::All);
        reference.set(String::new());
      },
      Err(found) => errors.refuse(FORM_ID, found),
    }
  };
  let release = move |row: HeldRow| {
    policy.edit(|p| match row {
      HeldRow::Conversation(released) => p
        .compliance_archive
        .legal_holds
        .retain(|hold| hold.conversation_id != released.conversation_id),
      HeldRow::Person(released) => p
        .compliance_archive
        .person_holds
        .retain(|hold| hold.user_id != released.user_id),
    });
    policy.save();
  };

  view! {
    <section class="admin-subsection legal-holds" aria-labelledby="legal-hold-title">
      <h3 id="legal-hold-title">{move || i18n.t("legal-hold-title")}</h3>
      <p class="muted-text small">{move || i18n.t("legal-hold-intro")}</p>
      <PolicyBlockerNote policy=policy />
      <Show
        when=move || !rows.with(Vec::is_empty)
        fallback=move || view! { <p class="empty-state">{move || i18n.t("legal-hold-none")}</p> }
      >
        <div class="table-wrap">
          <table class="data-table">
            <caption class="visually-hidden">{move || i18n.t("legal-hold-title")}</caption>
            <thead>
              <tr>
                <th scope="col">{move || i18n.t("legal-hold-col-held")}</th>
                <th scope="col">{move || i18n.t("legal-hold-col-covers")}</th>
                <th scope="col">{move || i18n.t("legal-hold-col-reference")}</th>
                <th scope="col">{move || i18n.t("legal-hold-col-since")}</th>
                <th scope="col"><span class="visually-hidden">{move || i18n.t("retention-actions")}</span></th>
              </tr>
            </thead>
            <tbody>
              <For each=move || rows.get() key=HeldRow::key let:row>
                {
                  let (name, covers_key, release_key) = match &row {
                    HeldRow::Conversation(hold) => {
                      let is_direct = state
                        .read(|backend| backend.conversation(hold.conversation_id))
                        .is_some_and(|known| known.kind.is_direct_like());
                      let covers = if is_direct { "legal-hold-covers-direct" } else { "legal-hold-covers-channel" };
                      (channel_name(hold.conversation_id), covers, "legal-hold-release-label")
                    },
                    HeldRow::Person(hold) => {
                      let covers = match hold.scope {
                        PersonHoldScope::Direct => "legal-hold-scope-direct",
                        _ => "legal-hold-scope-all",
                      };
                      (person_name(hold.user_id), covers, "legal-hold-person-release-label")
                    },
                  };
                  let label_name = name.clone();
                  let reference = row.reference().to_owned();
                  let placed_at = row.placed_at().0;
                  let released = row.clone();
                  view! {
                    <tr>
                      <th scope="row"><span class="row-name" translate="no">{name}</span></th>
                      <td>{move || i18n.t(covers_key)}</td>
                      <td translate="no">{reference}</td>
                      <td>{move || format_date(placed_at, i18n.locale().tag())}</td>
                      <td>
                        <button
                          type="button"
                          class="btn btn-small"
                          prop:disabled=move || policy.is_locked()
                          aria-label=move || {
                            i18n.t_with(release_key, &[("channel", &label_name), ("person", &label_name)])
                          }
                          on:click=move |_| release(released.clone())
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
          <div class="segmented" role="group" aria-label=move || i18n.t("legal-hold-target")>
            {[(HoldTarget::Channel, "legal-hold-target-channel"), (HoldTarget::Person, "legal-hold-target-person")]
              .into_iter()
              .map(|(option, key)| {
                view! {
                  <button
                    type="button"
                    aria-pressed=move || (target.get() == option).to_string()
                    on:click=move |_| target.set(option)
                  >
                    {move || i18n.t(key)}
                  </button>
                }
              })
              .collect_view()}
          </div>
          <Show when=move || target.get() == HoldTarget::Channel>
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
          </Show>
          <Show when=move || target.get() == HoldTarget::Person>
            <div class="field">
              <label for=PERSON_FIELD>{move || i18n.t("legal-hold-person")}</label>
              <select
                id=PERSON_FIELD
                class="input"
                aria-invalid=errors.invalid(PERSON_FIELD)
                aria-describedby=errors.described(PERSON_FIELD)
                prop:value=move || person.get()
                on:change=move |ev| person.set(event_target_value(&ev))
              >
                <option value="">{move || i18n.t("legal-hold-choose-person")}</option>
                {move || {
                  let held = people.get();
                  users
                    .get()
                    .into_iter()
                    .filter(|user| teamcraft_proto::person_hold_of(&held, user.user.id).is_none())
                    .map(|user| {
                      let label = format!("{} ({})", user.user.display_name, user.user.username);
                      view! { <option value=user.user.id.to_string()>{label}</option> }
                    })
                    .collect_view()
                }}
              </select>
              <FieldError errors=errors field=PERSON_FIELD />
            </div>
            <fieldset class="legal-hold-scope">
              <legend>{move || i18n.t("legal-hold-scope")}</legend>
              {[
                (PersonHoldScope::All, "legal-hold-scope-all", "legal-hold-scope-all-hint"),
                (PersonHoldScope::Direct, "legal-hold-scope-direct", "legal-hold-scope-direct-hint"),
              ]
                .into_iter()
                .map(|(option, label_key, hint_key)| {
                  let value = option.as_str().to_owned();
                  let is_chosen = option.clone();
                  view! {
                    <label class="radio">
                      <input
                        type="radio"
                        name="legal_hold_scope"
                        value=value
                        prop:checked=move || scope.get() == is_chosen
                        on:change=move |_| scope.set(option.clone())
                      />
                      <span>
                        <strong>{move || i18n.t(label_key)}</strong>
                        <span class="muted-text small">{move || i18n.t(hint_key)}</span>
                      </span>
                    </label>
                  }
                })
                .collect_view()}
            </fieldset>
          </Show>
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

/// What the form has chosen to hold.
struct Choice {
  target: HoldTarget,
  maybe_channel: Option<ConversationId>,
  maybe_person: Option<UserId>,
}

/// What is wrong with the form, in the order of the fields: the field and the message's i18n key.
fn form_errors(
  chosen: &Choice,
  reference: &str,
  holds: &[LegalHold],
  people: &[PersonLegalHold],
) -> Vec<(&'static str, &'static str)> {
  let mut found = Vec::new();
  let is_full = holds.len() + people.len() >= MAX_LEGAL_HOLDS;
  match (chosen.target, chosen.maybe_channel, chosen.maybe_person) {
    (HoldTarget::Channel, None, _) => found.push((CHANNEL_FIELD, "legal-hold-error-channel")),
    (HoldTarget::Channel, Some(_), _) if is_full => found.push((CHANNEL_FIELD, "legal-hold-error-full")),
    (HoldTarget::Channel, Some(id), _) if teamcraft_proto::legal_hold_of(holds, id).is_some() => {
      found.push((CHANNEL_FIELD, "legal-hold-error-held"));
    },
    (HoldTarget::Person, _, None) => found.push((PERSON_FIELD, "legal-hold-error-person")),
    (HoldTarget::Person, _, Some(_)) if is_full => found.push((PERSON_FIELD, "legal-hold-error-full")),
    (HoldTarget::Person, _, Some(id)) if teamcraft_proto::person_hold_of(people, id).is_some() => {
      found.push((PERSON_FIELD, "legal-hold-error-person-held"));
    },
    _ => {},
  }
  if !LegalHold::is_valid_reference(reference.trim()) {
    found.push((REFERENCE_FIELD, "legal-hold-error-reference"));
  }
  found
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;

  #[test]
  fn every_error_shows_at_its_field_in_order() {
    assert_eq!(
      form_errors(&channel_choice(None), "  ", &[], &[]),
      vec![
        (CHANNEL_FIELD, "legal-hold-error-channel"),
        (REFERENCE_FIELD, "legal-hold-error-reference"),
      ]
    );
    assert_eq!(
      form_errors(&channel_choice(Some(1)), " Case 7 ", &[], &[]),
      vec![]
    );
    assert_eq!(
      form_errors(&channel_choice(Some(1)), &"x".repeat(65), &[], &[]),
      vec![(REFERENCE_FIELD, "legal-hold-error-reference")]
    );
    assert_eq!(
      form_errors(&person_choice(None), "Case 7", &[], &[]),
      vec![(PERSON_FIELD, "legal-hold-error-person")]
    );
    assert_eq!(form_errors(&person_choice(Some(2)), "Case 7", &[], &[]), vec![]);
  }

  #[test]
  fn a_held_channel_or_person_or_a_full_list_is_refused() {
    let held = [hold(1)];
    assert_eq!(
      form_errors(&channel_choice(Some(1)), "Case 7", &held, &[]),
      vec![(CHANNEL_FIELD, "legal-hold-error-held")]
    );
    let held_people = [person_hold(2)];
    assert_eq!(
      form_errors(&person_choice(Some(2)), "Case 7", &[], &held_people),
      vec![(PERSON_FIELD, "legal-hold-error-person-held")]
    );
    // Channels and people count together.
    let full: Vec<_> = (1..MAX_LEGAL_HOLDS as i64).map(hold).collect();
    assert_eq!(
      form_errors(&channel_choice(Some(9999)), "Case 7", &full, &held_people),
      vec![(CHANNEL_FIELD, "legal-hold-error-full")]
    );
    assert_eq!(
      form_errors(&person_choice(Some(9999)), "Case 7", &full, &held_people),
      vec![(PERSON_FIELD, "legal-hold-error-full")]
    );
  }

  fn channel_choice(maybe_raw: Option<i64>) -> Choice {
    Choice {
      target: HoldTarget::Channel,
      maybe_channel: maybe_raw.map(id),
      maybe_person: None,
    }
  }

  fn person_choice(maybe_raw: Option<i64>) -> Choice {
    Choice {
      target: HoldTarget::Person,
      maybe_channel: None,
      maybe_person: maybe_raw.map(user),
    }
  }

  fn id(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn hold(raw: i64) -> LegalHold {
    LegalHold {
      conversation_id: id(raw),
      reference: "Case".to_owned(),
      placed_at: UnixMillis(0),
    }
  }

  fn person_hold(raw: i64) -> PersonLegalHold {
    PersonLegalHold {
      user_id: user(raw),
      scope: PersonHoldScope::All,
      reference: "Case".to_owned(),
      placed_at: UnixMillis(0),
    }
  }
}
