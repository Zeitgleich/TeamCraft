//! A poll in a message (issue #205): the question, each option with its count, share and bar
//! (numbers in text, never colour alone), and voting in place. Pressing an option votes for it,
//! pressing it again withdraws; with several answers each option toggles. The poll's sender can
//! close it, and a closing time ends it by itself. Counts hide until the user voted or the poll is
//! closed when the poll says so. Counting happens in the core from the encrypted votes (e2ee.md
//! §4.12).

use leptos::prelude::*;
use teamcraft_client_api::polls::{PollChoice, PollOptionView, PollResults, PollView, PollVoters};

use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, MessageId, UserId};
use crate::state::use_app_state;

/// Names listed under an option before "and N more".
const MAX_LISTED_VOTERS: usize = 5;
/// After the closing time, how long the card waits before the core counts again, so the backend
/// reads the time as passed.
const CLOSING_MARGIN_MS: i64 = 500;

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
  let leader = poll.leader().filter(|_| poll.is_closed && poll.are_results_shown);
  let are_results_shown = poll.are_results_shown;
  let mode = mode_text(i18n, &poll);
  if !poll.is_closed
    && let Some(closes_at) = poll.maybe_closes_at_ms
  {
    recount_at(state, closes_at);
  }
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
      let label = if are_results_shown {
        i18n.t_count_with(
          "poll-option-label",
          option.count as usize,
          &[("option", &option.text), ("percent", &share.to_string())],
        )
      } else {
        option.text.clone()
      };
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
            {are_results_shown.then(|| view! {
              <span class="poll-option-count" aria-hidden="true">
                {i18n.t_with("poll-option-count", &[("count", &option.count.to_string()), ("percent", &share.to_string())])}
              </span>
              <span class="poll-bar" aria-hidden="true">
                <span class="poll-bar-fill" style:--poll-share=(f64::from(share) / 100.0).to_string()></span>
              </span>
            })}
          </button>
          {voters.map(|names| view! { <p class="poll-voters" translate="no">{names}</p> })}
        </li>
      }
    })
    .collect_view();
  let close = poll.is_closable().then(|| {
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
  let results_hint = (!are_results_shown).then(|| {
    let key = match poll.results {
      PollResults::AfterVote => "poll-results-hidden-vote",
      _ => "poll-results-hidden-close",
    };
    view! { <p class="hint">{move || i18n.t(key)}</p> }
  });
  let closes_line = poll
    .maybe_closes_at_ms
    .filter(|_| !poll.is_closed)
    .map(|closes_at| {
      view! {
        <span class="poll-closes">
          {move || i18n.t_with("poll-closes-on", &[("time", &crate::clock::format_date_time(closes_at, i18n.locale().tag()))])}
        </span>
      }
    });
  let voter_count = poll.voter_count as usize;
  let is_closed = poll.is_closed;
  view! {
    <div class="poll" class:is-closed=is_closed role="group" aria-labelledby=labelled_by>
      <div class="poll-head">
        <span class="poll-icon" aria-hidden="true"><Icon name=IconName::Poll /></span>
        <p class="poll-question" id=question_id translate="no">{poll.question.clone()}</p>
      </div>
      <p class="poll-mode">{mode}</p>
      <ul class="poll-options" class:is-counting=are_results_shown>{options}</ul>
      {results_hint}
      {unknown_choice}
      <div class="poll-foot">
        <span class="poll-total" role="status">
          {move || i18n.t_count("poll-votes", voter_count)}
          {is_closed.then(|| view! { <span class="poll-closed">{move || i18n.t("poll-closed")}</span> })}
          {closes_line}
        </span>
        {close}
      </div>
    </div>
  }
}

/// "Choose one · Names hidden · Results after voting": how the poll is answered, whether it names
/// voters and when it shows counts.
fn mode_text(i18n: I18n, poll: &PollView) -> String {
  let mut parts = vec![match poll.choice {
    PollChoice::Multiple => i18n.t("poll-multiple-choice"),
    _ => i18n.t("poll-single-choice"),
  }];
  match poll.voters {
    PollVoters::Shown => {},
    PollVoters::Anonymous => parts.push(i18n.t("poll-anonymous")),
    _ => parts.push(i18n.t("poll-hidden-voters")),
  }
  match poll.results {
    PollResults::Always => {},
    PollResults::AfterVote => parts.push(i18n.t("poll-results-after-vote")),
    _ => parts.push(i18n.t("poll-results-after-close")),
  }
  parts.join(" · ")
}

/// One timer, while the card is shown, for its closing time: then the core counts again (it takes
/// the time as passed), so voting ends and counts held back until the close appear.
fn recount_at(state: crate::state::AppState, closes_at_ms: i64) {
  use leptos::leptos_dom::helpers::set_timeout_with_handle;

  let wait_ms = closes_at_ms
    .saturating_sub(crate::clock::now_ms())
    .saturating_add(CLOSING_MARGIN_MS)
    .max(0);
  let Ok(wait_ms) = u64::try_from(wait_ms) else {
    return;
  };
  // A browser timer holds at most about 24.8 days; a later close is counted on the next change.
  if wait_ms > u64::from(i32::MAX.unsigned_abs()) {
    return;
  }
  match set_timeout_with_handle(
    move || state.backend_changed(),
    std::time::Duration::from_millis(wait_ms),
  ) {
    Ok(timer) => on_cleanup(move || timer.clear()),
    Err(_) => tracing::debug!("could not plan a poll's closing"),
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
