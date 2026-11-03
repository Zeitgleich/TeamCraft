use leptos::prelude::*;

use crate::admin::{AdminChannel, Retention};
use crate::components::admin_console::SectionHeader;
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::state::use_app_state;

/// Instance maximum an owner may not exceed; a real value comes from the instance settings later.
const MAX_DAYS: u32 = 730;
const MAX_MESSAGES: u32 = 1_000_000;

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

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-retention-title"))
      subtitle=Signal::derive(move || i18n.t_with("admin-retention-subtitle", &[("days", &MAX_DAYS.to_string())]))
    />
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
            <RetentionRow channel=channel />
          </For>
        </tbody>
      </table>
    </div>
  }
}

#[component]
fn RetentionRow(channel: AdminChannel) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = channel.id;
  let initial = channel.retention;
  let days = RwSignal::new(
    initial
      .maybe_days
      .map(|value| value.to_string())
      .unwrap_or_default(),
  );
  let messages = RwSignal::new(
    initial
      .maybe_max_messages
      .map(|value| value.to_string())
      .unwrap_or_default(),
  );
  let keeps_pinned = RwSignal::new(initial.keeps_pinned);

  // Blank means "keep everything"; anything else must be a whole number within the instance maximum.
  let days_parsed = move || parse_limit(&days.get(), MAX_DAYS);
  let messages_parsed = move || parse_limit(&messages.get(), MAX_MESSAGES);
  let parsed = move || -> Result<Retention, &'static str> {
    Ok(Retention {
      maybe_days: days_parsed()?,
      maybe_max_messages: messages_parsed()?,
      keeps_pinned: keeps_pinned.get(),
    })
  };
  let is_dirty = move || parsed().is_ok_and(|retention| retention != initial) || parsed().is_err();
  let error_id = format!("retention-error-{id}");

  view! {
    <tr>
      <th scope="row">
        <span class="row-name" translate="no">{format!("# {}", channel.name)}</span>
      </th>
      <td translate="no">{channel.space_name.clone()}</td>
      <td>
        <label class="visually-hidden" for=format!("ret-days-{id}")>{move || i18n.t("retention-days")}</label>
        <input
          id=format!("ret-days-{id}")
          class="input narrow"
          inputmode="numeric"
          autocomplete="off"
          name="days"
          placeholder=move || i18n.t("retention-forever")
          aria-invalid=move || days_parsed().is_err().then_some("true")
          aria-describedby={
            let error_id = error_id.clone();
            move || days_parsed().is_err().then(|| error_id.clone())
          }
          prop:value=move || days.get()
          on:input=move |ev| days.set(event_target_value(&ev))
        />
      </td>
      <td>
        <label class="visually-hidden" for=format!("ret-msgs-{id}")>{move || i18n.t("retention-messages")}</label>
        <input
          id=format!("ret-msgs-{id}")
          class="input narrow"
          inputmode="numeric"
          autocomplete="off"
          name="messages"
          placeholder=move || i18n.t("retention-forever")
          aria-invalid=move || messages_parsed().is_err().then_some("true")
          aria-describedby={
            let error_id = error_id.clone();
            move || messages_parsed().is_err().then(|| error_id.clone())
          }
          prop:value=move || messages.get()
          on:input=move |ev| messages.set(event_target_value(&ev))
        />
      </td>
      <td>
        <label class="checkbox">
          <input type="checkbox" prop:checked=move || keeps_pinned.get() on:change=move |ev| keeps_pinned.set(event_target_checked(&ev)) />
          <span class="visually-hidden">{move || i18n.t("retention-pinned")}</span>
        </label>
      </td>
      <td>
        <span id=error_id class="field-error" role="alert">
          {move || parsed().err().map(|key| i18n.t_with(key, &[("days", &MAX_DAYS.to_string())]))}
        </span>
        <button
          class="btn btn-small btn-primary"
          disabled=move || parsed().is_err() || !is_dirty()
          on:click=move |_| {
            if let Ok(retention) = parsed() {
              state.write(|backend| backend.set_channel_retention(id, retention));
            }
          }
        >
          {move || i18n.t("save")}
        </button>
      </td>
    </tr>
  }
}

/// One limit as typed: blank keeps everything, otherwise a whole number from 1 to `max`.
fn parse_limit(text: &str, max: u32) -> Result<Option<u32>, &'static str> {
  let text = text.trim();
  if text.is_empty() {
    return Ok(None);
  }
  match text.parse::<u32>() {
    Ok(value) if (1..=max).contains(&value) => Ok(Some(value)),
    _ => Err("retention-invalid"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn each_limit_is_checked_on_its_own() {
    assert_eq!(parse_limit(" ", 10), Ok(None));
    assert_eq!(parse_limit("7", 10), Ok(Some(7)));
    assert_eq!(parse_limit("0", 10), Err("retention-invalid"));
    assert_eq!(parse_limit("11", 10), Err("retention-invalid"));
  }
}
