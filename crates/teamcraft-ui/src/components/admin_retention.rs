use leptos::prelude::*;

use crate::admin::{AdminChannel, AdminChannelFilter, AdminChannelQuery, Retention};
use crate::components::admin_console::{ListMore, SectionHeader};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::reactive::use_debounced;
use crate::state::use_app_state;

/// Instance maximum an owner may not exceed when the signed policy sets no limit of its own.
const MAX_DAYS: u32 = 730;
const MAX_MESSAGES: u32 = 1_000_000;
/// The wait after the last key before the search asks the server.
const SEARCH_DEBOUNCE_MS: u64 = 250;

/// Retention per channel: keep the last N days and/or N messages, whichever is stricter.
#[component]
pub fn RetentionSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let channels = Memo::new(move |_| {
    state
      .read(|backend| backend.admin_channels())
      .into_iter()
      .filter(|c| c.kind != ConversationKind::Direct)
      .collect::<Vec<_>>()
  });
  // Channels page by page, searched by the server (issue #1241). The Channels page's quick filters
  // don't apply here: this list is of every channel.
  let query = RwSignal::new(state.read(|backend| backend.admin_channel_query()).text);
  let debounced = use_debounced(query, SEARCH_DEBOUNCE_MS);
  Effect::new(move |_| {
    let asked = AdminChannelQuery {
      text: debounced.get(),
      filter: AdminChannelFilter::All,
    };
    state.write(|backend| backend.admin_set_channel_query(asked));
  });
  let limits = Limits::of_policy(state.read(|backend| backend.workspace_policy()).1.retention.limit);

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-retention-title"))
      subtitle=Signal::derive(move || i18n.t_with("admin-retention-subtitle", &[("days", &limits.max_days.to_string())]))
    />
    <div class="admin-toolbar">
      <label class="visually-hidden" for="admin-retention-search">{move || i18n.t("admin-search")}</label>
      <input
        id="admin-retention-search"
        class="input"
        type="search"
        name="q"
        autocomplete="off"
        spellcheck="false"
        placeholder=move || i18n.t("admin-search-placeholder")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
    </div>
    <div class="table-wrap">
      <table class="data-table">
        <caption class="visually-hidden">{move || i18n.t("admin-retention-title")}</caption>
        <thead>
          <tr>
            <th scope="col">{move || i18n.t("admin-col-channel")}</th>
            <th scope="col">{move || i18n.t("admin-col-space")}</th>
            <th scope="col">{move || i18n.t("retention-days")}</th>
            <th scope="col">{move || i18n.t("retention-messages")}</th>
            <th scope="col">{move || i18n.t("retention-pinned")}</th>
            <th scope="col"><span class="visually-hidden">{move || i18n.t("retention-actions")}</span></th>
          </tr>
        </thead>
        <tbody>
          <For each=move || channels.get() key=|channel| (channel.id, channel.retention) let:channel>
            <RetentionRow channel=channel limits=limits />
          </For>
        </tbody>
      </table>
    </div>
    <Show when=move || channels.with(Vec::is_empty) fallback=|| ()>
      <p class="empty-state">{move || i18n.t("admin-empty")}</p>
    </Show>
    <ListMore
      has_more=Signal::derive(move || state.read(|backend| backend.admin_channels_have_more()))
      on_more=Callback::new(move |()| state.write(|backend| backend.admin_load_more_channels()))
    />
  }
}

#[component]
fn RetentionRow(channel: AdminChannel, limits: Limits) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.id;
  let initial = channel.retention;
  let name = channel.name.clone();
  let errors = FormErrors::new();
  let row_id = format!("retention-row-{id}");
  let days_id = format!("retention-days-{id}");
  let messages_id = format!("retention-messages-{id}");

  let days = LimitField::new(initial.maybe_days.map(|value| value.to_string()));
  let messages = LimitField::new(initial.maybe_max_messages.map(|value| value.to_string()));
  let keeps_pinned = RwSignal::new(initial.keeps_pinned);

  // What the fields say now: each limit on its own, with the message of the field that is wrong.
  let check = {
    let (days_id, messages_id) = (days_id.clone(), messages_id.clone());
    move || -> Result<Retention, FieldErrors> {
      let parsed_days = days.parse(limits.max_days);
      let parsed_messages = messages.parse(limits.max_messages);
      let mut found = FieldErrors::new();
      if parsed_days.is_err() {
        found.push(
          days_id.clone(),
          i18n.t_with(
            "admin-retention-error-days",
            &[("max", &limits.max_days.to_string())],
          ),
        );
      }
      if parsed_messages.is_err() {
        found.push(
          messages_id.clone(),
          i18n.t_with(
            "admin-retention-error-messages",
            &[("max", &limits.max_messages.to_string())],
          ),
        );
      }
      match (parsed_days, parsed_messages) {
        (Ok(maybe_days), Ok(maybe_max_messages)) => Ok(Retention {
          maybe_days,
          maybe_max_messages,
          keeps_pinned: keeps_pinned.get(),
        }),
        _ => Err(found),
      }
    }
  };
  let is_dirty = {
    let check = check.clone();
    move || check().is_ok_and(|retention| retention != initial) || check().is_err()
  };
  // After a refused save the errors follow the input (forms.md); before it nothing is judged.
  Effect::new({
    let check = check.clone();
    move |_| {
      if let Err(found) = check() {
        errors.follow(found);
      } else {
        errors.follow(FieldErrors::new());
      }
    }
  });
  let save = {
    let (check, row_id) = (check.clone(), row_id.clone());
    move |_| match check() {
      Ok(retention) => {
        errors.clear();
        state.write(|backend| backend.set_channel_retention(id, retention));
      },
      Err(found) => errors.refuse(row_id.clone(), found),
    }
  };

  view! {
    <tr id=row_id>
      <th scope="row">
        <span class="row-name" translate="no">{format!("# {}", channel.name)}</span>
      </th>
      <td translate="no">{channel.space_name.clone()}</td>
      <td>
        <LimitInput
          field=days
          input_id=days_id
          errors=errors
          unit_key="admin-retention-unit-days"
          choice_label=Signal::derive({
            let name = name.clone();
            move || i18n.t_with("admin-retention-choice-days", &[("channel", &name)])
          })
          max=limits.max_days
          is_everything_allowed=limits.is_forever_days_allowed
        />
      </td>
      <td>
        <LimitInput
          field=messages
          input_id=messages_id
          errors=errors
          unit_key="admin-retention-unit-messages"
          choice_label=Signal::derive({
            let name = name.clone();
            move || i18n.t_with("admin-retention-choice-messages", &[("channel", &name)])
          })
          max=limits.max_messages
          is_everything_allowed=limits.is_forever_messages_allowed
        />
      </td>
      <td>
        <label class="checkbox">
          <input type="checkbox" prop:checked=move || keeps_pinned.get() on:change=move |ev| keeps_pinned.set(event_target_checked(&ev)) />
          <span class="visually-hidden">{move || i18n.t("retention-pinned")}</span>
        </label>
      </td>
      <td>
        <button class="btn btn-small btn-primary" disabled=move || !is_dirty() on:click=save>
          {move || i18n.t("save")}
        </button>
      </td>
    </tr>
  }
}

/// One limit as a choice and a number: "keep everything" or "keep the last N", with its unit next
/// to the number. The number stays visible (and disabled) while the choice is "keep everything", so
/// the layout doesn't move.
#[component]
fn LimitInput(
  field: LimitField,
  input_id: String,
  errors: FormErrors,
  unit_key: &'static str,
  #[prop(into)] choice_label: Signal<String>,
  max: u32,
  is_everything_allowed: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let choice_id = format!("{input_id}-choice");
  let unit = move || {
    let count = field.text.get().trim().parse::<usize>().unwrap_or(0);
    i18n.t_count(unit_key, count)
  };

  view! {
    <div class="retention-limit">
      <select
        id=choice_id
        class="input"
        aria-label=move || choice_label.get()
        on:change=move |ev| field.is_limited.set(event_target_value(&ev) == LIMITED)
      >
        <option value=EVERYTHING selected=move || !field.is_limited.get() disabled=!is_everything_allowed>
          {move || i18n.t("admin-retention-forever")}
        </option>
        <option value=LIMITED selected=move || field.is_limited.get()>
          {move || i18n.t("admin-retention-limited")}
        </option>
      </select>
      <input
        id=input_id.clone()
        class="input narrow"
        inputmode="numeric"
        autocomplete="off"
        min="1"
        max=max.to_string()
        aria-label=move || choice_label.get()
        disabled=move || !field.is_limited.get()
        aria-invalid=errors.invalid(input_id.clone())
        aria-describedby=errors.described(input_id.clone())
        prop:value=move || field.text.get()
        on:input=move |ev| field.text.set(event_target_value(&ev))
      />
      <span class="muted-text small">{unit}</span>
      <FieldError errors=errors field=input_id />
    </div>
  }
}

const EVERYTHING: &str = "everything";
const LIMITED: &str = "limited";

/// What one limit's fields hold.
#[derive(Clone, Copy)]
struct LimitField {
  /// "Keep the last N" (otherwise "keep everything").
  is_limited: RwSignal<bool>,
  text: RwSignal<String>,
}

impl LimitField {
  fn new(maybe_value: Option<String>) -> Self {
    Self {
      is_limited: RwSignal::new(maybe_value.is_some()),
      text: RwSignal::new(maybe_value.unwrap_or_default()),
    }
  }

  fn parse(&self, max: u32) -> Result<Option<u32>, &'static str> {
    parse_limit(self.is_limited.get(), &self.text.get(), max)
  }
}

/// How far the limits may go: the signed policy's limit where it sets one (and then "keep
/// everything" isn't offered), else the console's own upper bound.
#[derive(Clone, Copy)]
struct Limits {
  max_days: u32,
  max_messages: u32,
  is_forever_days_allowed: bool,
  is_forever_messages_allowed: bool,
}

impl Limits {
  fn of_policy(limit: teamcraft_proto::RetentionSetting) -> Self {
    Self {
      max_days: limit.maybe_max_age_days.unwrap_or(MAX_DAYS),
      max_messages: limit
        .maybe_max_messages
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(MAX_MESSAGES),
      is_forever_days_allowed: limit.maybe_max_age_days.is_none(),
      is_forever_messages_allowed: limit.maybe_max_messages.is_none(),
    }
  }
}

/// One limit as chosen: "keep everything" is its own choice, not an empty field; "keep the last N"
/// needs a whole number from 1 to `max`.
fn parse_limit(is_limited: bool, text: &str, max: u32) -> Result<Option<u32>, &'static str> {
  if !is_limited {
    return Ok(None);
  }
  match text.trim().parse::<u32>() {
    Ok(value) if (1..=max).contains(&value) => Ok(Some(value)),
    _ => Err("retention-invalid"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::RetentionSetting;

  #[test]
  fn keeping_everything_is_a_choice_and_an_empty_number_is_an_error() {
    assert_eq!(parse_limit(false, "", 10), Ok(None));
    assert_eq!(parse_limit(false, "99", 10), Ok(None));
    assert_eq!(parse_limit(true, " 7 ", 10), Ok(Some(7)));
    assert_eq!(parse_limit(true, "", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "0", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "11", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "1.5", 10), Err("retention-invalid"));
  }

  #[test]
  fn the_policy_limit_caps_the_choices_and_takes_away_keeping_everything() {
    let open = Limits::of_policy(RetentionSetting::default());
    assert_eq!((open.max_days, open.max_messages), (MAX_DAYS, MAX_MESSAGES));
    assert!(open.is_forever_days_allowed && open.is_forever_messages_allowed);
    let limited = Limits::of_policy(RetentionSetting {
      maybe_max_age_days: Some(90),
      maybe_max_messages: None,
    });
    assert_eq!(limited.max_days, 90);
    assert!(!limited.is_forever_days_allowed);
    assert!(limited.is_forever_messages_allowed);
  }
}
