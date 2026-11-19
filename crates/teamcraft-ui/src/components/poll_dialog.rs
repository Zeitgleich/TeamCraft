//! The dialog that makes a poll (issue #205): a question, two or more options, whether people may
//! choose several, whether the poll names who voted, when it shows its counts and when it closes. "Hide who voted" says what it is: the
//! poll shows counts only, but every member's app still receives each vote with its sender
//! (e2ee.md §4.12), so it is no anonymous ballot.

use leptos::prelude::*;
use teamcraft_client_api::polls::{
  NewPoll, PollChoice, PollDraft, PollError, PollField, PollLimits, PollResults, PollVoters, fallback_text,
};
use teamcraft_proto::MIN_POLL_OPTIONS;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::model::ConversationId;
use crate::state::use_app_state;

const POLL_FORM_ID: &str = "poll-form";
const QUESTION_FIELD: &str = "poll-question";
const HIDE_VOTERS_HINT_ID: &str = "poll-hide-voters-hint";
const RESULTS_FIELD: &str = "poll-results";
const CLOSING_SELECT: &str = "poll-closing";
const CLOSING_FIELD: &str = "poll-closes-at";
/// Option fields a new poll starts with.
const FIRST_OPTIONS: usize = 2;
const HOUR_MS: i64 = 3_600_000;

#[component]
pub fn PollDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let limits = state.read(|backend| backend.poll_limits());
  let fields = Fields {
    question: RwSignal::new(String::new()),
    options: RwSignal::new(vec![String::new(); FIRST_OPTIONS]),
    is_multiple: RwSignal::new(false),
    hides_voters: RwSignal::new(false),
    results: RwSignal::new(PollResults::Always),
    closing: RwSignal::new(Closing::Never),
    closes_at: RwSignal::new(crate::clock::format_local_datetime_input(
      crate::clock::now_ms().saturating_add(24 * HOUR_MS),
    )),
  };
  let Fields {
    question,
    options,
    is_multiple,
    hides_voters,
    results,
    closing,
    closes_at,
  } = fields;
  let errors = FormErrors::new();
  // The time is read when the fields change and on send, not ticking: an error about a time that
  // passed while the dialog stood open shows on send.
  let field_errors = move || {
    let now_ms = crate::clock::now_ms();
    field_errors(i18n, &draft_at(&fields, now_ms), &limits, now_ms)
  };
  Effect::new(move |_| errors.follow(field_errors()));
  let send = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(POLL_FORM_ID, found);
      return;
    }
    let spec = untrack(|| draft_at(&fields, crate::clock::now_ms())).spec();
    let fallback = fallback_text(&spec, &i18n.t("poll-title"), &i18n.t("poll-fallback-note"));
    state.write(|backend| backend.post_poll(conversation, NewPoll { spec, fallback }));
    state.close_overlay();
  };
  let add_option = move |_| options.update(|list| list.push(String::new()));
  let can_add = move || options.with(Vec::len) < limits.max_options;
  // Rows are rebuilt when an option comes or goes; typing only updates the value.
  let rows = move || {
    let count = options.with(Vec::len);
    (0..count)
      .map(|index| {
        let field = option_field(index);
        let number = (index + 1).to_string();
        let label = i18n.t_with("poll-option", &[("number", &number)]);
        let remove_label = i18n.t_with("poll-remove-option", &[("number", &number)]);
        let can_remove = count > FIRST_OPTIONS;
        view! {
          <li>
            <div class="poll-edit-row">
              <input
                id=field.clone()
                class="input"
                maxlength=limits.max_option_chars.to_string()
                aria-label=label.clone()
                placeholder=label
                aria-invalid=errors.invalid(field.clone())
                aria-describedby=errors.described(field.clone())
                prop:value=move || options.with(|list| list.get(index).cloned().unwrap_or_default())
                on:input=move |ev| options.update(|list| if let Some(option) = list.get_mut(index) { *option = event_target_value(&ev); })
              />
              {can_remove.then(|| view! {
                <button
                  type="button"
                  class="icon-btn"
                  aria-label=remove_label.clone()
                  title=remove_label
                  on:click=move |_| options.update(|list| if index < list.len() { list.remove(index); })
                >
                  <Icon name=IconName::Trash />
                </button>
              })}
            </div>
            <FieldError errors=errors field=field />
          </li>
        }
      })
      .collect_view()
  };
  view! {
    <Modal labelled_by="poll-dialog-heading" class="poll-dialog">
      <form id=POLL_FORM_ID on:submit=send novalidate>
        <header class="modal-head">
          <h2 id="poll-dialog-heading">{move || i18n.t("poll-new-title")}</h2>
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") title=move || i18n.t("close") on:click=move |_| state.close_overlay()>
            <Icon name=IconName::Close />
          </button>
        </header>
        <div class="modal-body">
          <label for=QUESTION_FIELD>{move || i18n.t("poll-question")}</label>
          <input
            id=QUESTION_FIELD
            class="input"
            data-autofocus
            maxlength=limits.max_question_chars.to_string()
            aria-invalid=errors.invalid(QUESTION_FIELD)
            aria-describedby=errors.described(QUESTION_FIELD)
            prop:value=move || question.get()
            on:input=move |ev| question.set(event_target_value(&ev))
          />
          <FieldError errors=errors field=QUESTION_FIELD.to_owned() />
          <fieldset class="poll-edit-options">
            <legend>{move || i18n.t("poll-options")}</legend>
            <ul class="poll-edit-list">{rows}</ul>
            <button type="button" class="btn btn-small" prop:disabled=move || !can_add() on:click=add_option>
              <Icon name=IconName::Plus />
              <span>{move || i18n.t("poll-add-option")}</span>
            </button>
          </fieldset>
          <label class="check-row">
            <input type="checkbox" prop:checked=move || is_multiple.get() on:change=move |ev| is_multiple.set(event_target_checked(&ev)) />
            <span>{move || i18n.t("poll-multiple")}</span>
          </label>
          <label class="check-row">
            <input
              type="checkbox"
              aria-describedby=HIDE_VOTERS_HINT_ID
              prop:checked=move || hides_voters.get()
              on:change=move |ev| hides_voters.set(event_target_checked(&ev))
            />
            <span>{move || i18n.t("poll-hide-voters")}</span>
          </label>
          <p class="hint" id=HIDE_VOTERS_HINT_ID>{move || i18n.t("poll-hide-voters-hint")}</p>
          <label for=RESULTS_FIELD>{move || i18n.t("poll-results")}</label>
          <select
            id=RESULTS_FIELD
            class="input"
            on:change=move |ev| {
              let value = event_target_value(&ev);
              if let Some((_, _, mode)) = RESULTS.iter().find(|(name, _, _)| *name == value) {
                results.set(mode.clone());
              }
            }
          >
            {RESULTS
              .iter()
              .map(|(value, key, mode)| {
                let mode = mode.clone();
                view! {
                  <option value=*value selected=move || results.get() == mode>{move || i18n.t(key)}</option>
                }
              })
              .collect_view()}
          </select>
          <label for=CLOSING_SELECT>{move || i18n.t("poll-closes")}</label>
          <select
            id=CLOSING_SELECT
            class="input"
            on:change=move |ev| {
              let value = event_target_value(&ev);
              if let Some((_, _, choice)) = CLOSINGS.iter().find(|(name, _, _)| *name == value) {
                closing.set(*choice);
              }
            }
          >
            {CLOSINGS
              .iter()
              .map(|(value, key, choice)| {
                let choice = *choice;
                view! {
                  <option value=*value selected=move || closing.get() == choice>{move || i18n.t(key)}</option>
                }
              })
              .collect_view()}
          </select>
          <Show when=move || closing.get() == Closing::At>
            <label for=CLOSING_FIELD>{move || i18n.t("poll-closes-at-label")}</label>
            <input
              id=CLOSING_FIELD
              class="input"
              type="datetime-local"
              aria-invalid=errors.invalid(CLOSING_FIELD)
              aria-describedby=errors.described(CLOSING_FIELD)
              prop:value=move || closes_at.get()
              on:input=move |ev| closes_at.set(event_target_value(&ev))
            />
          </Show>
          <FieldError errors=errors field=CLOSING_FIELD.to_owned() />
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("cancel")}</button>
          <button type="submit" class="btn btn-primary">{move || i18n.t("poll-send")}</button>
        </footer>
      </form>
    </Modal>
  }
}

/// When the poll closes, as the dialog offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
  Never,
  In(i64),
  /// At the time in the date field.
  At,
}

/// The closing choices of the select: value, i18n key and choice.
const CLOSINGS: [(&str, &str, Closing); 5] = [
  ("never", "poll-closes-never", Closing::Never),
  ("hour", "poll-closes-hour", Closing::In(HOUR_MS)),
  ("day", "poll-closes-day", Closing::In(24 * HOUR_MS)),
  ("week", "poll-closes-week", Closing::In(7 * 24 * HOUR_MS)),
  ("at", "poll-closes-at", Closing::At),
];

/// The results choices of the select: value, i18n key and mode.
const RESULTS: [(&str, &str, PollResults); 3] = [
  ("always", "poll-results-always", PollResults::Always),
  ("after_vote", "poll-results-after-vote", PollResults::AfterVote),
  ("after_close", "poll-results-after-close", PollResults::AfterClose),
];

/// The poll as the dialog's fields hold it at `now_ms`.
fn draft_at(fields: &Fields, now_ms: i64) -> PollDraft {
  let maybe_closes_at_ms = match fields.closing.get() {
    Closing::Never => None,
    Closing::In(after_ms) => Some(now_ms.saturating_add(after_ms)),
    // An unreadable or empty date reads as now, which the check names as passed.
    Closing::At => Some(crate::clock::parse_local_datetime(&fields.closes_at.get()).unwrap_or(now_ms)),
  };
  PollDraft {
    question: fields.question.get(),
    options: fields.options.get(),
    choice: if fields.is_multiple.get() {
      PollChoice::Multiple
    } else {
      PollChoice::Single
    },
    voters: if fields.hides_voters.get() {
      PollVoters::Hidden
    } else {
      PollVoters::Shown
    },
    results: fields.results.get(),
    maybe_closes_at_ms,
  }
}

/// The dialog's inputs.
#[derive(Clone, Copy)]
struct Fields {
  question: RwSignal<String>,
  options: RwSignal<Vec<String>>,
  is_multiple: RwSignal<bool>,
  hides_voters: RwSignal<bool>,
  results: RwSignal<PollResults>,
  closing: RwSignal<Closing>,
  /// The date field's value (`2026-10-22T14:30`), used with [`Closing::At`].
  closes_at: RwSignal<String>,
}

/// Each field's error message under its element id, in the order of the form.
fn field_errors(i18n: I18n, draft: &PollDraft, limits: &PollLimits, now_ms: i64) -> FieldErrors {
  let mut found = FieldErrors::new();
  for problem in draft.problems(limits, now_ms) {
    let (key, args) = message_of(problem.error);
    let args: Vec<(&str, &str)> = args.iter().map(|(name, value)| (*name, value.as_str())).collect();
    found.push(field_id(problem.field), i18n.t_with(key, &args));
  }
  found
}

/// The i18n key and arguments that say what to do about `error`.
fn message_of(error: PollError) -> (&'static str, Vec<(&'static str, String)>) {
  match error {
    PollError::EmptyQuestion => ("poll-error-question", Vec::new()),
    PollError::QuestionTooLong { max } => ("poll-error-question-long", vec![("max", max.to_string())]),
    PollError::TooFewOptions { min } => ("poll-error-options", vec![("min", min.to_string())]),
    PollError::EmptyOption { .. } => ("poll-error-options", vec![("min", MIN_POLL_OPTIONS.to_string())]),
    PollError::TooManyOptions { max } => ("poll-error-options-many", vec![("max", max.to_string())]),
    PollError::OptionTooLong { max, .. } => ("poll-error-option-long", vec![("max", max.to_string())]),
    PollError::DuplicateOption { .. } => ("poll-error-option-duplicate", Vec::new()),
    PollError::ClosingPassed => ("poll-error-closing-passed", Vec::new()),
    PollError::ClosingTooFar { max_days } => ("poll-error-closing-far", vec![("max", max_days.to_string())]),
  }
}

fn field_id(field: PollField) -> String {
  match field {
    PollField::Question => QUESTION_FIELD.to_owned(),
    PollField::Option(index) => option_field(index),
    PollField::Closing => CLOSING_FIELD.to_owned(),
  }
}

fn option_field(index: usize) -> String {
  format!("poll-option-{index}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_error_has_a_message_key() {
    let errors = [
      PollError::EmptyQuestion,
      PollError::QuestionTooLong { max: 1 },
      PollError::TooFewOptions { min: 2 },
      PollError::TooManyOptions { max: 3 },
      PollError::EmptyOption { index: 0 },
      PollError::OptionTooLong { index: 0, max: 1 },
      PollError::DuplicateOption { index: 1 },
      PollError::ClosingPassed,
      PollError::ClosingTooFar { max_days: 365 },
    ];
    for error in errors {
      assert!(message_of(error).0.starts_with("poll-error-"));
    }
  }

  #[test]
  fn fields_are_named_by_their_element_ids() {
    assert_eq!(field_id(PollField::Question), QUESTION_FIELD);
    assert_eq!(field_id(PollField::Option(2)), "poll-option-2");
    assert_eq!(field_id(PollField::Closing), CLOSING_FIELD);
  }
}
