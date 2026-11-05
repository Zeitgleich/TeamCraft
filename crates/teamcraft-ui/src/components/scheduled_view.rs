//! "Send later" (docs/scheduled-messages.md): the choices in the composer's send menu and the page
//! that lists what waits. The server sends a message at its time, on time unless the group changed and
//! all of the user's devices stayed off; everything here says so and shows a message that is late why.

use leptos::prelude::*;
use teamcraft_client_api::scheduled::{
  ScheduleCheck, ScheduleRefusal, ScheduledMessage, ScheduledOverview, ScheduledStatus, latest_due,
};
use teamcraft_proto::{ScheduledMessageId, ScheduledMessagePolicy};

use crate::clock::{
  format_date, format_date_time, format_local_datetime_input, next_monday_at, now_ms, parse_local_datetime,
  tomorrow_at,
};
use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::rich_text::RichText;
use crate::feedback::PendingAction;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::model::TimestampMs;
use crate::state::use_app_state;

/// The morning the presets go for: 08:00, as the issue's "tomorrow 08:00".
const PRESET_MINUTE_OF_DAY: u16 = 8 * 60;
const MS_PER_HOUR: TimestampMs = 3_600_000;
const TIME_FIELD: &str = "schedule-at";
const FORM_ID: &str = "schedule-custom";

/// A time offered in the send menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulePreset {
  InOneHour,
  TomorrowMorning,
  MondayMorning,
}

impl SchedulePreset {
  const ALL: [Self; 3] = [Self::InOneHour, Self::TomorrowMorning, Self::MondayMorning];

  /// In server time. "In 1 hour" counts from the server's clock, so a wrong local clock doesn't
  /// shift it; the mornings are what this device's calendar and time zone call that morning.
  pub fn due_at(self, local_now: TimestampMs, server_now: TimestampMs) -> TimestampMs {
    match self {
      Self::InOneHour => server_now.saturating_add(MS_PER_HOUR),
      Self::TomorrowMorning => tomorrow_at(local_now, PRESET_MINUTE_OF_DAY),
      Self::MondayMorning => next_monday_at(local_now, PRESET_MINUTE_OF_DAY),
    }
  }

  fn label(self) -> &'static str {
    match self {
      Self::InOneHour => "schedule-in-one-hour",
      Self::TomorrowMorning => "schedule-tomorrow",
      Self::MondayMorning => "schedule-monday",
    }
  }
}

/// The "Send later" part of the send menu. `on_pick` gets the chosen time. Shown only where the
/// workspace allows scheduling; `is_ready` is false with nothing to schedule or with files attached
/// (a scheduled message waits in the account data, which holds text).
#[component]
pub fn SendLaterItems(
  #[prop(into)] is_ready: Signal<bool>,
  /// Why it isn't ready, under the heading.
  #[prop(into)]
  maybe_blocked_hint: Signal<Option<String>>,
  on_pick: Callback<TimestampMs>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let overview = Memo::new(move |_| state.read(|backend| backend.scheduled_overview()));
  let is_custom_open = RwSignal::new(false);
  let custom_at = RwSignal::new(format_local_datetime_input(now_ms() + MS_PER_HOUR));
  let errors = FormErrors::new();
  let check_custom = move || -> Result<TimestampMs, FieldErrors> {
    let mut found = FieldErrors::new();
    let chosen = parse_local_datetime(&custom_at.get_untracked());
    let overview = overview.get_untracked();
    let refusal = chosen.map_or(Some(ScheduleRefusal::NotInFuture), |due_at| {
      time_refusal(due_at, &overview)
    });
    if let Some(refusal) = refusal {
      found.push(TIME_FIELD, refusal_text(i18n, refusal, &overview.policy));
    }
    match chosen {
      Some(due_at) if found.is_empty() => Ok(due_at),
      _ => Err(found),
    }
  };
  let pick_custom = move || match check_custom() {
    Ok(due_at) => on_pick.run(due_at),
    Err(found) => errors.refuse(FORM_ID, found),
  };
  view! {
    <Show when=move || overview.with(|overview| overview.policy.is_enabled) fallback=|| ()>
      <div class="menu-sep" role="separator"></div>
      <p class="menu-heading">{move || i18n.t("schedule-heading")}</p>
      <p class="send-later-hint">
        {move || maybe_blocked_hint.get().unwrap_or_else(|| i18n.t("schedule-online-hint"))}
      </p>
      {SchedulePreset::ALL.map(|preset| {
        let due_at = Memo::new(move |_| {
          overview.with(|overview| preset.due_at(now_ms(), overview.server_now))
        });
        let is_within = move || overview.with(|overview| time_refusal(due_at.get(), overview).is_none());
        view! {
          <button
            type="button"
            class="menu-item"
            role="menuitem"
            prop:disabled=move || !is_ready.get() || !is_within()
            on:click=move |_| on_pick.run(due_at.get_untracked())
          >
            <span class="menu-label">
              {move || i18n.t(preset.label())}
              <small>{move || format_date_time(due_at.get(), i18n.locale().tag())}</small>
            </span>
          </button>
        }
      }).collect_view()}
      <button
        type="button"
        class="menu-item"
        role="menuitem"
        aria-expanded=move || is_custom_open.get().to_string()
        prop:disabled=move || !is_ready.get()
        on:click=move |_| is_custom_open.update(|open| *open = !*open)
      >
        <span class="menu-label">{move || i18n.t("schedule-custom")}</span>
      </button>
      <Show when=move || is_custom_open.get() && is_ready.get() fallback=|| ()>
        <div id=FORM_ID class="send-later-custom">
          <label class="send-later-label" for=TIME_FIELD>{move || i18n.t("schedule-custom-label")}</label>
          <div class="field-row menu-custom-until">
            <input
              id=TIME_FIELD
              class="input grow"
              type="datetime-local"
              aria-invalid=errors.invalid(TIME_FIELD)
              aria-describedby=errors.described(TIME_FIELD)
              prop:value=move || custom_at.get()
              on:input=move |ev| {
                custom_at.set(event_target_value(&ev));
                errors.follow(check_custom().err().unwrap_or_default());
              }
              on:keydown=move |ev: web_sys::KeyboardEvent| {
                if ev.key() == "Enter" {
                  ev.prevent_default();
                  pick_custom();
                }
              }
            />
            <button type="button" class="btn" on:click=move |_| pick_custom()>
              {move || i18n.t("schedule-confirm")}
            </button>
          </div>
          <FieldError errors=errors field=TIME_FIELD />
        </div>
      </Show>
    </Show>
  }
}

/// Everything waiting to be sent, soonest first, each with what happens next.
#[component]
pub fn ScheduledView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let overview = Memo::new(move |_| state.read(|backend| backend.scheduled_overview()));
  view! {
    <section class="scheduled-view page-view" aria-label=move || i18n.t("scheduled-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("scheduled-title")}</h1>
          <span class="panel-subtitle">
            {move || i18n.t_count("scheduled-count", overview.with(|overview| overview.messages.len()))}
          </span>
        </div>
      </header>
      <div class="saved-list panel-scroll">
        <p class="scheduled-note">{move || i18n.t("scheduled-note")}</p>
        <ul class="page-cards">
          <For
            each=move || overview.with(|overview| overview.messages.clone())
            key=|message| (message.id, message.due_at, message.body.clone(), format!("{:?}", message.status))
            let:message
          >
            <ScheduledCard message=message overview=overview />
          </For>
        </ul>
        <Show when=move || overview.with(|overview| overview.messages.is_empty()) fallback=|| ()>
          <EmptyState icon=IconName::Clock title_key="scheduled-empty-title" body_key="scheduled-empty" />
        </Show>
      </div>
    </section>
  }
}

/// One waiting message: read it, change it, send it now, or cancel it.
#[component]
fn ScheduledCard(message: ScheduledMessage, overview: Memo<ScheduledOverview>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let (id, conversation) = (message.id, message.conversation);
  let due_at = message.due_at;
  let status = message.status;
  let is_editing = RwSignal::new(false);
  let is_confirming_cancel = RwSignal::new(false);
  let sending = PendingAction::new();
  let cancelling = PendingAction::new();
  let is_available = can_still_go_out(status);
  let can_edit = is_available && status != ScheduledStatus::PolicyOff;
  // Gone from the server: only the text is left here, to copy, until the user removes it.
  let is_removed = status == ScheduledStatus::RemovedByServer;
  let maybe_removal = message
    .maybe_removes_at
    .filter(|_| !is_removed)
    .map(|removes_at| {
      i18n.t_with(
        "scheduled-removal-date",
        &[("date", &format_date(removes_at, locale))],
      )
    });
  let (cancel_key, question_key, confirm_key) = if is_removed {
    (
      "scheduled-dismiss",
      "scheduled-dismiss-question",
      "scheduled-dismiss-confirm",
    )
  } else {
    (
      "scheduled-cancel",
      "scheduled-cancel-question",
      "scheduled-cancel-confirm",
    )
  };
  let (name, body) = (message.conversation_name.clone(), message.body.clone());
  let edit_body = body.clone();
  view! {
    <li class="page-card saved-card scheduled-card">
      <div class="saved-meta">
        <Icon name=IconName::Clock />
        <span class="saved-author" translate="no">
          {if name.is_empty() { i18n.t("scheduled-unknown-conversation") } else { name }}
        </span>
        <time class="saved-time">{format_date_time(due_at, locale)}</time>
      </div>
      <p
        class="scheduled-status"
        class:needs-you=needs_attention(status)
        role=needs_attention(status).then_some("status")
      >
        {move || i18n.t(status_key(status))}
      </p>
      {maybe_removal.map(|removal| view! { <p class="scheduled-status needs-you">{removal}</p> })}
      <Show
        when=move || is_editing.get()
        fallback={
          let body = body.clone();
          move || view! { <div class="saved-body"><RichText body=body.clone() me=state.me().username /></div> }
        }
      >
        <ScheduledEditor
          id=id
          body=edit_body.clone()
          due_at=due_at
          overview=overview
          on_close=Callback::new(move |()| is_editing.set(false))
        />
      </Show>
      <Show when=move || !is_editing.get() fallback=|| ()>
        <div class="saved-actions">
          <Show
            when=move || !is_confirming_cancel.get()
            fallback=move || view! {
              <span class="scheduled-confirm">{move || i18n.t(question_key)}</span>
              <PendingButton
                pending=cancelling
                class="btn btn-small btn-danger"
                on_click=move |_| cancelling.run(|| state.write(|backend| backend.cancel_scheduled(id)))
              >
                {move || i18n.t(confirm_key)}
              </PendingButton>
              <button class="btn btn-small btn-quiet" on:click=move |_| is_confirming_cancel.set(false)>
                {move || i18n.t("scheduled-keep")}
              </button>
            }
          >
            <Show when=move || !is_removed fallback=|| ()>
              <PendingButton
                pending=sending
                class="btn btn-small"
                is_disabled=Signal::derive(move || !is_available)
                on_click=move |_| sending.run(|| state.write(|backend| backend.send_scheduled_now(id)))
              >
                {move || i18n.t("scheduled-send-now")}
              </PendingButton>
              <button class="btn btn-small btn-quiet" prop:disabled=!can_edit on:click=move |_| is_editing.set(true)>
                {move || i18n.t("scheduled-edit")}
              </button>
            </Show>
            <Show when=move || is_available fallback=|| ()>
              <button class="btn btn-small btn-quiet" on:click=move |_| state.open_conversation(conversation)>
                {move || i18n.t("saved-open")}
              </button>
            </Show>
            <button class="btn btn-small btn-quiet" on:click=move |_| is_confirming_cancel.set(true)>
              {move || i18n.t(cancel_key)}
            </button>
          </Show>
        </div>
      </Show>
    </li>
  }
}

/// Changes the text and time of a waiting message, right in its card.
#[component]
fn ScheduledEditor(
  id: ScheduledMessageId,
  body: String,
  due_at: TimestampMs,
  overview: Memo<ScheduledOverview>,
  on_close: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let text = RwSignal::new(body);
  let at = RwSignal::new(format_local_datetime_input(due_at));
  let errors = FormErrors::new();
  let suffix = id.to_string();
  let form_id = format!("schedule-edit-{suffix}");
  let body_field = format!("schedule-edit-body-{suffix}");
  let time_field = format!("schedule-edit-time-{suffix}");
  let validate = {
    let (body_field, time_field) = (body_field.clone(), time_field.clone());
    move || -> (FieldErrors, Option<TimestampMs>) {
      let overview = overview.get_untracked();
      let chosen = parse_local_datetime(&at.get_untracked());
      let refusals = ScheduleCheck {
        body: &text.get_untracked(),
        due_at: chosen.unwrap_or(0),
        server_now: overview.server_now,
        waiting: overview.messages.len(),
        is_new: false,
      }
      .refusals(&overview.policy);
      let mut found = FieldErrors::new();
      for refusal in refusals {
        let field = match refusal {
          ScheduleRefusal::EmptyBody => &body_field,
          _ => &time_field,
        };
        found.push(field.clone(), refusal_text(i18n, refusal, &overview.policy));
      }
      (found, chosen)
    }
  };
  let saving = PendingAction::new();
  let save = {
    let (validate, form_id) = (validate.clone(), form_id.clone());
    move |_| {
      let (found, chosen) = validate();
      match chosen.filter(|_| found.is_empty()) {
        Some(due_at) => {
          let body = text.get_untracked();
          // Stays open if the core refused it, with the input.
          saving.run_and_then(
            || state.write(|backend| backend.update_scheduled(id, &body, due_at)),
            move || on_close.run(()),
          );
        },
        None => errors.refuse(form_id.clone(), found),
      }
    }
  };
  let on_edit = {
    let validate = validate.clone();
    move || errors.follow(validate().0)
  };
  let on_edit_time = on_edit.clone();
  view! {
    <div id=form_id class="scheduled-editor">
      <label for=body_field.clone()>{move || i18n.t("scheduled-edit-text")}</label>
      <textarea
        id=body_field.clone()
        class="input"
        rows="3"
        data-autofocus
        aria-invalid=errors.invalid(body_field.clone())
        aria-describedby=errors.described(body_field.clone())
        prop:value=move || text.get()
        on:input=move |ev| {
          text.set(event_target_value(&ev));
          on_edit();
        }
      ></textarea>
      <FieldError errors=errors field=body_field.clone() />
      <label for=time_field.clone()>{move || i18n.t("schedule-custom-label")}</label>
      <input
        id=time_field.clone()
        class="input"
        type="datetime-local"
        aria-invalid=errors.invalid(time_field.clone())
        aria-describedby=errors.described(time_field.clone())
        prop:value=move || at.get()
        on:input=move |ev| {
          at.set(event_target_value(&ev));
          on_edit_time();
        }
      />
      <FieldError errors=errors field=time_field.clone() />
      <div class="saved-actions">
        <PendingButton pending=saving class="btn btn-small btn-primary" on_click=save>
          {move || i18n.t("scheduled-save")}
        </PendingButton>
        <button type="button" class="btn btn-small btn-quiet" on:click=move |_| on_close.run(())>
          {move || i18n.t("cancel")}
        </button>
      </div>
    </div>
  }
}

/// What a refusal says, for the field it belongs to or the composer's notice.
pub fn refusal_text(i18n: I18n, refusal: ScheduleRefusal, policy: &ScheduledMessagePolicy) -> String {
  match refusal {
    ScheduleRefusal::Disabled => i18n.t("schedule-refused-off"),
    ScheduleRefusal::EmptyBody => i18n.t("schedule-refused-empty"),
    ScheduleRefusal::NotInFuture => i18n.t("schedule-refused-past"),
    ScheduleRefusal::BeyondHorizon => i18n.t_count("schedule-refused-far", policy.max_horizon_days as usize),
    ScheduleRefusal::TooMany => i18n.t_with(
      "schedule-refused-many",
      &[("max", &policy.max_per_user.to_string())],
    ),
  }
}

/// The refusal only the time can cause.
fn time_refusal(due_at: TimestampMs, overview: &ScheduledOverview) -> Option<ScheduleRefusal> {
  if due_at <= overview.server_now {
    Some(ScheduleRefusal::NotInFuture)
  } else if due_at > latest_due(&overview.policy, overview.server_now) {
    Some(ScheduleRefusal::BeyondHorizon)
  } else {
    None
  }
}

/// Whether sending or changing it can still make it go out.
fn can_still_go_out(status: ScheduledStatus) -> bool {
  matches!(
    status,
    ScheduledStatus::Waiting
      | ScheduledStatus::NeedsSealing
      | ScheduledStatus::Stale
      | ScheduledStatus::PolicyOff
  )
}

/// Whether the message waits for the user to do something.
pub fn needs_attention(status: ScheduledStatus) -> bool {
  matches!(
    status,
    ScheduledStatus::PolicyOff
      | ScheduledStatus::NotAMember
      | ScheduledStatus::NoPermission
      | ScheduledStatus::ConversationGone
      | ScheduledStatus::ThreadGone
      | ScheduledStatus::RemovedByServer
  )
}

fn status_key(status: ScheduledStatus) -> &'static str {
  match status {
    ScheduledStatus::Waiting => "scheduled-status-waiting",
    ScheduledStatus::NeedsSealing => "scheduled-status-needs-sealing",
    ScheduledStatus::Stale => "scheduled-status-stale",
    ScheduledStatus::PolicyOff => "scheduled-status-policy-off",
    ScheduledStatus::NotAMember => "scheduled-status-not-a-member",
    ScheduledStatus::NoPermission => "scheduled-status-no-permission",
    ScheduledStatus::ConversationGone => "scheduled-status-conversation-gone",
    ScheduledStatus::ThreadGone => "scheduled-status-thread-gone",
    ScheduledStatus::Unreadable => "scheduled-status-unreadable",
    ScheduledStatus::RemovedByServer => "scheduled-status-removed",
    ScheduledStatus::Unknown => "scheduled-status-unknown",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_800_000_000_000;

  #[test]
  fn in_one_hour_counts_from_the_server_s_clock() {
    let wrong_local_clock = NOW - 5 * MS_PER_HOUR;
    assert_eq!(
      SchedulePreset::InOneHour.due_at(wrong_local_clock, NOW),
      NOW + MS_PER_HOUR
    );
  }

  #[test]
  fn only_what_waits_for_the_user_asks_for_attention() {
    assert!(needs_attention(ScheduledStatus::NotAMember));
    assert!(needs_attention(ScheduledStatus::PolicyOff));
    assert!(!needs_attention(ScheduledStatus::Waiting));
    // A device of the user seals it again by itself.
    assert!(!needs_attention(ScheduledStatus::Stale));
  }

  #[test]
  fn the_time_refusal_names_both_ends() {
    let overview = ScheduledOverview {
      messages: Vec::new(),
      policy: ScheduledMessagePolicy::default(),
      server_now: NOW,
    };
    assert_eq!(time_refusal(NOW, &overview), Some(ScheduleRefusal::NotInFuture));
    assert_eq!(time_refusal(NOW + 1, &overview), None);
    assert_eq!(
      time_refusal(latest_due(&overview.policy, NOW) + 1, &overview),
      Some(ScheduleRefusal::BeyondHorizon)
    );
  }
}
