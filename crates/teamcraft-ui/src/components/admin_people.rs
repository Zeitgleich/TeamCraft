use leptos::prelude::*;
use teamcraft_proto::{DisplayNameFormat, PushContentMode, StatusPreset, WorkspacePolicy};

use crate::components::admin_console::SectionHeader;
use crate::components::admin_policy::{
  CheckRow, PolicyBlockerNote, PolicyDraft, PolicyFooter, SelectRow, use_policy_draft,
};
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

const AWAY_MINUTES: [u32; 5] = [0, 5, 10, 15, 30];
const PRESET_DURATIONS: [u32; 6] = [0, 30, 60, 240, 480, 1440];
const MAX_PRESETS: usize = 20;

/// Directory privacy, statuses and notification defaults, all in the signed policy.
#[component]
pub fn PeopleSection() -> impl IntoView {
  let i18n = use_i18n();
  let policy = use_policy_draft();
  let draft = policy.draft;

  let name_options = Signal::derive(move || {
    ["as_provided", "first_last", "last_first", "username"]
      .into_iter()
      .map(|slug| (slug.to_owned(), i18n.t(&format!("policy-name-{slug}"))))
      .collect()
  });
  let away_options = Signal::derive(move || {
    AWAY_MINUTES
      .into_iter()
      .map(|minutes| {
        let label = match minutes {
          0 => i18n.t("policy-away-device"),
          minutes => i18n.t_count("policy-minutes", minutes as usize),
        };
        (minutes.to_string(), label)
      })
      .collect()
  });

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-people-title"))
      subtitle=Signal::derive(move || i18n.t("admin-people-subtitle"))
    />
    <PolicyBlockerNote policy=policy />
    <form
      class="policy-form"
      on:submit=move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        policy.save();
      }
    >
      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-directory")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-directory-hint")}</p>
        <SelectRow
          id="policy-name-format"
          label=Signal::derive(move || i18n.t("policy-name-format"))
          options=name_options
          selected=Signal::derive(move || draft.get().directory.display_name_format.as_str().to_owned())
          on_select=move |value: String| {
            policy.edit(|p| p.directory.display_name_format = name_format_of(&value));
          }
        />
        <CheckRow
          name="shows_full_name"
          label=Signal::derive(move || i18n.t("policy-full-name"))
          hint=Signal::derive(move || i18n.t("policy-full-name-hint"))
          checked=Signal::derive(move || draft.get().directory.shows_full_name)
          on_change=move |value| policy.edit(|p| p.directory.shows_full_name = value)
        />
        <CheckRow
          name="shows_email"
          label=Signal::derive(move || i18n.t("policy-show-email"))
          hint=Signal::derive(move || i18n.t("policy-show-email-hint"))
          checked=Signal::derive(move || draft.get().directory.shows_email)
          on_change=move |value| policy.edit(|p| p.directory.shows_email = value)
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-status")}</legend>
        <CheckRow
          name="statuses_enabled"
          label=Signal::derive(move || i18n.t("policy-statuses"))
          hint=Signal::derive(move || i18n.t("policy-statuses-hint"))
          checked=Signal::derive(move || draft.get().presence.statuses_enabled)
          on_change=move |value| policy.edit(|p| p.presence.statuses_enabled = value)
        />
        <CheckRow
          name="custom_status_enabled"
          label=Signal::derive(move || i18n.t("policy-custom-status"))
          hint=Signal::derive(move || i18n.t("policy-custom-status-hint"))
          checked=Signal::derive(move || draft.get().presence.custom_status_enabled)
          disabled=Signal::derive(move || !draft.get().presence.statuses_enabled)
          on_change=move |value| policy.edit(|p| p.presence.custom_status_enabled = value)
        />
        <CheckRow
          name="is_last_seen_enabled"
          label=Signal::derive(move || i18n.t("policy-last-seen"))
          hint=Signal::derive(move || i18n.t("policy-last-seen-hint"))
          checked=Signal::derive(move || draft.get().presence.is_last_seen_enabled)
          on_change=move |value| policy.edit(|p| p.presence.is_last_seen_enabled = value)
        />
        <SelectRow
          id="policy-away"
          label=Signal::derive(move || i18n.t("policy-away"))
          options=away_options
          selected=Signal::derive(move || draft.get().presence.maybe_away_timeout_minutes.unwrap_or(0).to_string())
          on_select=move |value: String| {
            let minutes = value.parse::<u32>().unwrap_or(0);
            policy.edit(|p| p.presence.maybe_away_timeout_minutes = (minutes > 0).then_some(minutes));
          }
        />
        <PresetEditor policy=policy />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-threads")}</legend>
        <CheckRow
          name="threaded_view"
          label=Signal::derive(move || i18n.t("policy-threaded-view"))
          hint=Signal::derive(move || i18n.t("policy-threaded-view-hint"))
          checked=Signal::derive(move || draft.get().threads.is_threaded_view_enabled)
          on_change=move |value| policy.edit(|p| p.threads.is_threaded_view_enabled = value)
        />
        <CheckRow
          name="auto_follow"
          label=Signal::derive(move || i18n.t("policy-auto-follow"))
          hint=Signal::derive(move || i18n.t("policy-auto-follow-hint"))
          checked=Signal::derive(move || draft.get().threads.auto_follows_mentioned_threads)
          on_change=move |value| policy.edit(|p| p.threads.auto_follows_mentioned_threads = value)
        />
      </fieldset>

      <fieldset class="policy-group" prop:disabled=move || policy.is_locked()>
        <legend>{move || i18n.t("policy-notifications")}</legend>
        <p class="muted-text small">{move || i18n.t("policy-notifications-hint")}</p>
        {[PushContentMode::Generic, PushContentMode::Sender, PushContentMode::Full]
          .into_iter()
          .map(|mode| {
            let slug = mode.as_str().to_owned();
            let label_key = format!("policy-push-{slug}");
            let hint_key = format!("policy-push-{slug}-hint");
            let checked_mode = mode.clone();
            view! {
              <label class="radio">
                <input
                  type="radio"
                  name="push_content"
                  value=slug
                  prop:checked=move || draft.get().notifications.push_content == checked_mode
                  on:change=move |_| policy.edit(|p| p.notifications.push_content = mode.clone())
                />
                <span>
                  <strong>{move || i18n.t(&label_key)}</strong>
                  <span class="muted-text small">{move || i18n.t(&hint_key)}</span>
                </span>
              </label>
            }
          })
          .collect_view()}
      </fieldset>

      <PolicyFooter policy=policy />
    </form>
  }
}

/// The statuses people can pick with one click.
#[component]
fn PresetEditor(policy: PolicyDraft) -> impl IntoView {
  let i18n = use_i18n();
  let draft = policy.draft;
  let presets = Memo::new(move |_| draft.get().presence.presets);
  let can_add = Memo::new(move |_| presets.get().len() < MAX_PRESETS);
  let indices = Memo::new(move |_| (0..presets.get().len()).collect::<Vec<usize>>());
  let duration_options = Signal::derive(move || {
    PRESET_DURATIONS
      .into_iter()
      .map(|minutes| {
        let label = match minutes {
          0 => i18n.t("status-expiry-never"),
          minutes => i18n.t_count("policy-minutes", minutes as usize),
        };
        (minutes.to_string(), label)
      })
      .collect::<Vec<_>>()
  });

  view! {
    <div class="preset-editor">
      <h4 class="detail-heading">{move || i18n.t("policy-presets")}</h4>
      <p class="muted-text small">{move || i18n.t("policy-presets-hint")}</p>
      <ul class="preset-list">
        <For each=move || indices.get() key=|index| *index let:index>
          {move || {
            let preset = presets.get().get(index).cloned();
            preset.map(|preset| {
              view! {
                <li class="preset-row">
                  <input
                    class="input preset-emoji"
                    aria-label=move || i18n.t("status-emoji")
                    maxlength="8"
                    prop:value=preset.maybe_emoji.clone().unwrap_or_default()
                    on:change=move |ev| {
                      let value = event_target_value(&ev);
                      policy.edit(|p| {
                        if let Some(row) = p.presence.presets.get_mut(index) {
                          row.maybe_emoji = (!value.trim().is_empty()).then(|| value.trim().to_owned());
                        }
                      });
                    }
                  />
                  <input
                    class="input preset-text"
                    aria-label=move || i18n.t("status-text")
                    maxlength="100"
                    prop:value=preset.text.clone()
                    on:change=move |ev| {
                      let value = event_target_value(&ev);
                      policy.edit(|p| {
                        if let Some(row) = p.presence.presets.get_mut(index) {
                          row.text = value.clone();
                        }
                      });
                    }
                  />
                  <select
                    class="input"
                    aria-label=move || i18n.t("status-clear-after")
                    on:change=move |ev| {
                      let minutes = event_target_value(&ev).parse::<u32>().unwrap_or(0);
                      policy.edit(|p| {
                        if let Some(row) = p.presence.presets.get_mut(index) {
                          row.maybe_duration_minutes = (minutes > 0).then_some(minutes);
                        }
                      });
                    }
                  >
                    {move || {
                      let current = preset.maybe_duration_minutes.unwrap_or(0).to_string();
                      duration_options
                        .get()
                        .into_iter()
                        .map(|(value, label)| {
                          let is_selected = value == current;
                          view! { <option value=value selected=is_selected>{label}</option> }
                        })
                        .collect_view()
                    }}
                  </select>
                  <button
                    type="button"
                    class="btn btn-small"
                    on:click=move |_| policy.edit(|p| {
                      if index < p.presence.presets.len() {
                        p.presence.presets.remove(index);
                      }
                    })
                  >
                    <Icon name=IconName::Close />
                    <span class="visually-hidden">{move || i18n.t("policy-preset-remove")}</span>
                  </button>
                </li>
              }
            })
          }}
        </For>
      </ul>
      <button
        type="button"
        class="btn btn-small"
        disabled=move || !can_add.get()
        on:click=move |_| policy.edit(|p| add_preset(p, i18n.t("policy-preset-new")))
      >
        <Icon name=IconName::Plus />
        <span>{move || i18n.t("policy-preset-add")}</span>
      </button>
    </div>
  }
}

/// Appends an empty-looking preset for the admin to fill in.
fn add_preset(policy: &mut WorkspacePolicy, text: String) {
  policy.presence.presets.push(StatusPreset {
    maybe_emoji: None,
    text,
    maybe_duration_minutes: None,
  });
}

fn name_format_of(slug: &str) -> DisplayNameFormat {
  match slug {
    "first_last" => DisplayNameFormat::FirstLast,
    "last_first" => DisplayNameFormat::LastFirst,
    "username" => DisplayNameFormat::Username,
    _ => DisplayNameFormat::AsProvided,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn presets_are_appended() {
    let mut policy = WorkspacePolicy::default();
    add_preset(&mut policy, "Focus".into());
    assert_eq!(policy.presence.presets.len(), 1);
    assert_eq!(policy.presence.presets[0].text, "Focus");
  }

  #[test]
  fn name_formats_round_trip_through_their_slugs() {
    for format in [
      DisplayNameFormat::AsProvided,
      DisplayNameFormat::FirstLast,
      DisplayNameFormat::LastFirst,
      DisplayNameFormat::Username,
    ] {
      assert_eq!(name_format_of(format.as_str()), format);
    }
  }
}
