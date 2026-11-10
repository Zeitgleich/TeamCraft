//! The dialog that makes a poll (issue #205): a question, two or more options, and whether people
//! may choose several and whether the poll names who voted. "Hide who voted" says what it is: the
//! poll shows counts only, but every member's app still receives each vote with its sender
//! (e2ee.md §4.12), so it is no anonymous ballot.

use leptos::prelude::*;
use teamcraft_client_api::polls::{
  NewPoll, PollChoice, PollError, PollLimits, PollSpec, PollVoters, fallback_text,
};

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::model::ConversationId;
use crate::state::use_app_state;

const POLL_FORM_ID: &str = "poll-form";
const QUESTION_FIELD: &str = "poll-question";
const HIDE_VOTERS_HINT_ID: &str = "poll-hide-voters-hint";
/// Option fields a new poll starts with.
const FIRST_OPTIONS: usize = 2;

/// A field's problem: its element id, the i18n key of the message and the message's arguments.
type FieldProblem = (String, &'static str, Vec<(&'static str, String)>);

#[component]
pub fn PollDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let limits = PollLimits::default();
  let question = RwSignal::new(String::new());
  let options = RwSignal::new(vec![String::new(); FIRST_OPTIONS]);
  let is_multiple = RwSignal::new(false);
  let hides_voters = RwSignal::new(false);
  let errors = FormErrors::new();
  let draft = move || PollDraft {
    question: question.get(),
    options: options.get(),
    choice: if is_multiple.get() {
      PollChoice::Multiple
    } else {
      PollChoice::Single
    },
    voters: if hides_voters.get() {
      PollVoters::Hidden
    } else {
      PollVoters::Shown
    },
  };
  let field_errors = move || draft().field_errors(i18n, &limits);
  Effect::new(move |_| errors.follow(field_errors()));
  let send = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let found = untrack(field_errors);
    if !found.is_empty() {
      errors.refuse(POLL_FORM_ID, found);
      return;
    }
    let spec = untrack(draft).spec();
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
                  aria-label=remove_label
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
          <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
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

/// What the dialog holds, as typed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PollDraft {
  question: String,
  options: Vec<String>,
  choice: PollChoice,
  voters: PollVoters,
}

impl PollDraft {
  /// The poll to send: question and options trimmed, empty option fields left out.
  fn spec(&self) -> PollSpec {
    PollSpec::from_input(
      &self.question,
      &self.options,
      self.choice.clone(),
      self.voters.clone(),
    )
  }

  /// Each field's problem, in the order of the fields, as i18n keys and arguments.
  fn problems(&self, limits: &PollLimits) -> Vec<FieldProblem> {
    let mut problems = Vec::new();
    let spec = self.spec();
    // The typed field each option of the spec came from (empty fields are left out of it).
    let fields: Vec<usize> = self
      .options
      .iter()
      .enumerate()
      .filter(|(_, option)| !option.trim().is_empty())
      .map(|(at, _)| at)
      .collect();
    let mut checked = spec.clone();
    // Each error names one field; checking again after setting it right finds the next one.
    for _ in 0..=self.options.len() + 1 {
      let Err(error) = checked.check(limits) else {
        break;
      };
      let (field, key, args) = match error {
        PollError::EmptyQuestion => (QUESTION_FIELD.to_owned(), "poll-error-question", Vec::new()),
        PollError::QuestionTooLong { max } => (
          QUESTION_FIELD.to_owned(),
          "poll-error-question-long",
          vec![("max", max.to_string())],
        ),
        PollError::TooFewOptions { min } => {
          let first_empty = self
            .options
            .iter()
            .position(|option| option.trim().is_empty())
            .unwrap_or(self.options.len().saturating_sub(1));
          (
            option_field(first_empty),
            "poll-error-options",
            vec![("min", min.to_string())],
          )
        },
        PollError::TooManyOptions { max } => (
          option_field(self.options.len().saturating_sub(1)),
          "poll-error-options-many",
          vec![("max", max.to_string())],
        ),
        PollError::EmptyOption { index } => (
          option_field(fields.get(index).copied().unwrap_or(index)),
          "poll-error-options",
          vec![("min", "2".to_owned())],
        ),
        PollError::OptionTooLong { index, max } => (
          option_field(fields.get(index).copied().unwrap_or(index)),
          "poll-error-option-long",
          vec![("max", max.to_string())],
        ),
        PollError::DuplicateOption { index } => (
          option_field(fields.get(index).copied().unwrap_or(index)),
          "poll-error-option-duplicate",
          Vec::new(),
        ),
      };
      problems.push((field, key, args));
      if !settle(&mut checked, error) {
        break;
      }
    }
    problems
  }

  fn field_errors(&self, i18n: I18n, limits: &PollLimits) -> FieldErrors {
    let mut found = FieldErrors::new();
    let mut problems = self.problems(limits);
    // In the order of the form: the question first, then the options top down.
    problems.sort_by_key(|(field, _, _)| field_order(field));
    for (field, key, args) in problems {
      let args: Vec<(&str, &str)> = args.iter().map(|(name, value)| (*name, value.as_str())).collect();
      found.push(field, i18n.t_with(key, &args));
    }
    found
  }
}

/// Sets right what `error` names in `spec`, so the next check finds the next problem. `false` when
/// nothing further can be found.
fn settle(spec: &mut PollSpec, error: PollError) -> bool {
  match error {
    PollError::EmptyQuestion | PollError::QuestionTooLong { .. } => {
      spec.question = "?".to_owned();
      true
    },
    PollError::OptionTooLong { index, .. }
    | PollError::DuplicateOption { index }
    | PollError::EmptyOption { index } => {
      if let Some(option) = spec.options.get_mut(index) {
        *option = format!("\u{0}{index}");
      }
      true
    },
    PollError::TooFewOptions { .. } | PollError::TooManyOptions { .. } => false,
  }
}

fn option_field(index: usize) -> String {
  format!("poll-option-{index}")
}

fn field_order(field: &str) -> usize {
  field
    .strip_prefix("poll-option-")
    .and_then(|index| index.parse::<usize>().ok())
    .map_or(0, |index| index + 1)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_complete_draft_has_no_problems() {
    assert!(
      draft("Lunch?", &["Pizza", "", "Salad"])
        .problems(&PollLimits::default())
        .is_empty()
    );
  }

  #[test]
  fn problems_land_on_the_fields_as_typed() {
    let problems = draft(" ", &["Pizza", "", "pizza"]).problems(&PollLimits::default());
    let fields: Vec<(&str, &str)> = problems
      .iter()
      .map(|(field, key, _)| (field.as_str(), *key))
      .collect();
    assert_eq!(
      fields,
      [
        ("poll-question", "poll-error-question"),
        ("poll-option-2", "poll-error-option-duplicate"),
      ]
    );
  }

  #[test]
  fn too_few_options_point_at_the_first_empty_field() {
    let problems = draft("Lunch?", &["Pizza", " "]).problems(&PollLimits::default());
    assert_eq!(problems[0].0, "poll-option-1");
    assert_eq!(problems[0].1, "poll-error-options");
  }

  #[test]
  fn each_long_field_is_named() {
    let limits = PollLimits {
      max_option_chars: 3,
      ..PollLimits::default()
    };
    let problems = draft("Q", &["Long", "No", "Longer"]).problems(&limits);
    let fields: Vec<&str> = problems.iter().map(|(field, _, _)| field.as_str()).collect();
    assert_eq!(fields, ["poll-option-0", "poll-option-2"]);
  }

  #[test]
  fn the_spec_carries_the_choice_and_voters() {
    let mut typed = draft("Q", &["A", "B"]);
    typed.choice = PollChoice::Multiple;
    typed.voters = PollVoters::Hidden;
    let spec = typed.spec();
    assert_eq!(spec.choice, PollChoice::Multiple);
    assert_eq!(spec.voters, PollVoters::Hidden);
  }

  fn draft(question: &str, options: &[&str]) -> PollDraft {
    PollDraft {
      question: question.to_owned(),
      options: options.iter().map(|option| (*option).to_owned()).collect(),
      choice: PollChoice::Single,
      voters: PollVoters::Shown,
    }
  }
}
