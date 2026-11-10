//! A poll in a message (issue #205): the question, each option with its count, share and bar
//! (numbers in text, never colour alone), and voting in place. Pressing an option votes for it,
//! pressing it again withdraws; with several answers each option toggles. The poll's sender can
//! close it. Counting happens in the core from the encrypted votes (e2ee.md §4.12).

use leptos::prelude::*;
use teamcraft_client_api::polls::{PollChoice, PollOptionView, PollView, PollVoters};

use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, MessageId, UserId};
use crate::state::use_app_state;

/// Names listed under an option before "and N more".
const MAX_LISTED_VOTERS: usize = 5;

#[component]
pub fn PollCard(
  conversation: ConversationId,
  message: MessageId,
  poll: PollView,
  /// Votes are taken here (the channel lets the user write).
  #[prop(default = true)]
  can_write: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let question_id = format!("poll-question-{message}");
  let labelled_by = question_id.clone();
  let is_confirming_close = RwSignal::new(false);
  let can_vote = can_write && poll.can_vote();
  let leader = poll.leader().filter(|_| poll.is_closed);
  let mode = mode_text(i18n, &poll);
  let options = poll
    .options
    .iter()
    .enumerate()
    .map(|(at, option)| {
      let share = poll.share_percent(option);
      let maybe_choices = u32::try_from(at)
        .ok()
        .and_then(|at| poll.choices_after_pressing(at))
        .filter(|_| can_vote);
      let is_disabled = maybe_choices.is_none();
      let choices = StoredValue::new(maybe_choices.unwrap_or_default());
      let label = i18n.t_count_with(
        "poll-option-label",
        option.count as usize,
        &[("option", &option.text), ("percent", &share.to_string())],
      );
      let voters = voters_line(i18n, state, option);
      view! {
        <li class="poll-option-row">
          <button
            type="button"
            class="poll-option"
            class:is-chosen=option.is_chosen
            class:is-leader=leader == Some(at)
            aria-pressed=option.is_chosen.to_string()
            aria-label=label
            prop:disabled=is_disabled
            on:click=move |_| {
              let choices = choices.get_value();
              state.write(|backend| backend.vote_in_poll(conversation, message, choices));
            }
          >
            <span class="poll-mark" aria-hidden="true">
              {option.is_chosen.then(|| view! { <Icon name=IconName::Check /> })}
            </span>
            <span class="poll-option-text" translate="no">{option.text.clone()}</span>
            <span class="poll-option-count" aria-hidden="true">
              {i18n.t_with("poll-option-count", &[("count", &option.count.to_string()), ("percent", &share.to_string())])}
            </span>
            <span class="poll-bar" aria-hidden="true">
              <span class="poll-bar-fill" style:--poll-share=(f64::from(share) / 100.0).to_string()></span>
            </span>
          </button>
          {voters.map(|names| view! { <p class="poll-voters" translate="no">{names}</p> })}
        </li>
      }
    })
    .collect_view();
  let close = (poll.is_mine && !poll.is_closed).then(|| {
    view! {
      <Show
        when=move || is_confirming_close.get()
        fallback=move || view! {
          <button type="button" class="link-btn" on:click=move |_| is_confirming_close.set(true)>
            {move || i18n.t("poll-close")}
          </button>
        }
      >
        <span class="poll-close-confirm" role="group">
          <span>{move || i18n.t("poll-close-confirm")}</span>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming_close.set(false)>
            {move || i18n.t("cancel")}
          </button>
          <button
            type="button"
            class="btn btn-small btn-primary"
            on:click=move |_| {
              is_confirming_close.set(false);
              state.write(|backend| backend.close_poll(conversation, message));
            }
          >
            {move || i18n.t("poll-close")}
          </button>
        </span>
      </Show>
    }
  });
  let unknown_choice = matches!(poll.choice, PollChoice::Unknown(_))
    .then(|| view! { <p class="hint">{move || i18n.t("poll-unknown-choice")}</p> });
  let voter_count = poll.voter_count as usize;
  let is_closed = poll.is_closed;
  view! {
    <div class="poll" class:is-closed=is_closed role="group" aria-labelledby=labelled_by>
      <div class="poll-head">
        <span class="poll-icon" aria-hidden="true"><Icon name=IconName::Poll /></span>
        <p class="poll-question" id=question_id translate="no">{poll.question.clone()}</p>
      </div>
      <p class="poll-mode">{mode}</p>
      <ul class="poll-options">{options}</ul>
      {unknown_choice}
      <div class="poll-foot">
        <span class="poll-total" role="status">
          {move || i18n.t_count("poll-votes", voter_count)}
          {is_closed.then(|| view! { <span class="poll-closed">{move || i18n.t("poll-closed")}</span> })}
        </span>
        {close}
      </div>
    </div>
  }
}

/// "Choose one · Names hidden": how the poll is answered and whether it names voters.
fn mode_text(i18n: I18n, poll: &PollView) -> String {
  let choice = match poll.choice {
    PollChoice::Multiple => i18n.t("poll-multiple-choice"),
    _ => i18n.t("poll-single-choice"),
  };
  match poll.voters {
    PollVoters::Shown => choice,
    _ => format!("{choice} · {}", i18n.t("poll-hidden-voters")),
  }
}

/// Who chose `option`, when the poll shows it: the first names, then "and N more".
fn voters_line(i18n: I18n, state: crate::state::AppState, option: &PollOptionView) -> Option<String> {
  if option.voters.is_empty() {
    return None;
  }
  let names: Vec<String> = option
    .voters
    .iter()
    .take(MAX_LISTED_VOTERS)
    .map(|user| display_name(state, *user))
    .collect();
  let rest = option.voters.len().saturating_sub(MAX_LISTED_VOTERS);
  let mut line = names.join(", ");
  if rest > 0 {
    line.push(' ');
    line.push_str(&i18n.t_count("poll-voters-more", rest));
  }
  Some(line)
}

fn display_name(state: crate::state::AppState, user: UserId) -> String {
  state
    .read(|backend| backend.user(user))
    .map(|user| user.display_name)
    .unwrap_or_default()
}
