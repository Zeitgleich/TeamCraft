//! Identity provider groups to spaces and public channels (docs/provisioning.md §5), on the console's
//! settings page: one row per rule, the group's path and the IDs of its spaces and channels, as the
//! default spaces above are entered.

use leptos::prelude::*;
use teamcraft_proto::{ConversationId, GroupMappingRule, MAX_GROUP_MAPPINGS, Snowflake, SpaceId};

use crate::components::admin_fields::{list_text, parse_list};
use crate::form_errors::{error_id, focus_first_invalid};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const FORM_ID: &str = "settings-group-mappings";

/// A rule as typed.
#[derive(Clone, Debug, Default, PartialEq)]
struct RuleDraft {
  /// Stable across edits, for the row's DOM IDs.
  key: u64,
  path: String,
  spaces: String,
  channels: String,
}

/// What is wrong with a row, as i18n keys per field.
#[derive(Clone, Debug, Default, PartialEq)]
struct RuleErrors {
  maybe_path: Option<&'static str>,
  maybe_spaces: Option<&'static str>,
  maybe_channels: Option<&'static str>,
}

#[component]
pub fn GroupMappingsForm() -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let stored = Memo::new(move |_| state.read(|backend| backend.group_mappings()));
  let next_key = StoredValue::new(0_u64);
  let draft = RwSignal::new(Vec::<RuleDraft>::new());
  let show_errors = RwSignal::new(false);
  Effect::new(move |_| {
    if let Some(stored) = stored.get() {
      let rules = stored
        .mappings
        .into_iter()
        .map(|rule| RuleDraft {
          key: take_key(next_key),
          path: rule.group_path,
          spaces: list_text(&rule.space_ids.iter().map(ToString::to_string).collect::<Vec<_>>()),
          channels: list_text(
            &rule
              .channel_ids
              .iter()
              .map(ToString::to_string)
              .collect::<Vec<_>>(),
          ),
        })
        .collect();
      draft.set(rules);
      show_errors.set(false);
    }
  });
  let save = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    show_errors.set(true);
    match draft.with_untracked(|rules| to_rules(rules)) {
      Some(rules) => state.write(|backend| backend.save_group_mappings(rules)),
      None => focus_first_invalid(FORM_ID),
    }
  };
  let add = move |_| {
    draft.update(|rules| {
      rules.push(RuleDraft {
        key: take_key(next_key),
        ..RuleDraft::default()
      });
    });
  };
  view! {
    <form id=FORM_ID on:submit=save>
      <fieldset class="policy-group">
        <legend>{move || i18n.t("settings-group-mappings")}</legend>
        <p class="muted-text small">{move || i18n.t("settings-group-mappings-hint")}</p>
        {move || stored.get().is_none().then(|| view! { <p class="muted-text" role="status">{i18n.t("admin-settings-loading")}</p> })}
        <For each=move || draft.get() key=|rule| rule.key let:rule>
          <RuleRow key=rule.key draft=draft show_errors=show_errors />
        </For>
        <div class="policy-footer">
          <button
            type="button"
            class="btn"
            prop:disabled=move || draft.with(|rules| rules.len() >= MAX_GROUP_MAPPINGS)
            on:click=add
          >
            {move || i18n.t("settings-group-mappings-add")}
          </button>
          <span class="grow"></span>
          <button type="submit" class="btn btn-primary">{move || i18n.t("admin-save")}</button>
        </div>
      </fieldset>
    </form>
  }
}

#[component]
fn RuleRow(key: u64, draft: RwSignal<Vec<RuleDraft>>, show_errors: RwSignal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  let rule = move || {
    draft.with(|rules| {
      rules
        .iter()
        .find(|rule| rule.key == key)
        .cloned()
        .unwrap_or_default()
    })
  };
  let errors = move || {
    if show_errors.get() {
      check(&rule())
    } else {
      RuleErrors::default()
    }
  };
  let edit = move |change: fn(&mut RuleDraft, String), text: String| {
    draft.update(|rules| {
      if let Some(rule) = rules.iter_mut().find(|rule| rule.key == key) {
        change(rule, text);
      }
    });
  };
  let remove = move |_| draft.update(|rules| rules.retain(|rule| rule.key != key));
  let path_id = format!("group-mapping-{key}-path");
  let spaces_id = format!("group-mapping-{key}-spaces");
  let channels_id = format!("group-mapping-{key}-channels");
  view! {
    <div class="settings-row-group" role="group" aria-label=move || i18n.t("settings-group-mappings-rule")>
      <Field
        id=path_id
        label="settings-group-mappings-path"
        placeholder="/engineering"
        is_area=false
        value=Signal::derive(move || rule().path)
        on_input=move |text| edit(|rule, text| rule.path = text, text)
        error=Signal::derive(move || errors().maybe_path)
      />
      <Field
        id=spaces_id
        label="settings-group-mappings-spaces"
        placeholder=""
        is_area=true
        value=Signal::derive(move || rule().spaces)
        on_input=move |text| edit(|rule, text| rule.spaces = text, text)
        error=Signal::derive(move || errors().maybe_spaces)
      />
      <Field
        id=channels_id
        label="settings-group-mappings-channels"
        placeholder=""
        is_area=true
        value=Signal::derive(move || rule().channels)
        on_input=move |text| edit(|rule, text| rule.channels = text, text)
        error=Signal::derive(move || errors().maybe_channels)
      />
      <p class="row-actions">
        <button type="button" class="btn btn-small" on:click=remove>
          {move || i18n.t("settings-group-mappings-remove")}
        </button>
      </p>
    </div>
  }
}

/// A labelled input or text area with its error below, marked for assistive technology.
#[component]
fn Field(
  id: String,
  label: &'static str,
  placeholder: &'static str,
  is_area: bool,
  value: Signal<String>,
  on_input: impl Fn(String) + Copy + 'static,
  error: Signal<Option<&'static str>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let error_dom_id = error_id(&id);
  let described = error_dom_id.clone();
  let input = if is_area {
    view! {
      <textarea
        id=id.clone()
        class="input"
        rows="2"
        prop:value=move || value.get()
        aria-invalid=move || error.get().map(|_| "true")
        aria-describedby=move || error.get().map(|_| described.clone())
        on:input=move |ev| on_input(event_target_value(&ev))
      ></textarea>
    }
    .into_any()
  } else {
    view! {
      <input
        id=id.clone()
        class="input"
        type="text"
        placeholder=placeholder
        prop:value=move || value.get()
        aria-invalid=move || error.get().map(|_| "true")
        aria-describedby=move || error.get().map(|_| described.clone())
        on:input=move |ev| on_input(event_target_value(&ev))
      />
    }
    .into_any()
  };
  view! {
    <div class="field">
      <label for=id>{move || i18n.t(label)}</label>
      {input}
      {move || error.get().map(|key| view! { <p class="field-error" id=error_dom_id.clone()>{i18n.t(key)}</p> })}
    </div>
  }
}

fn take_key(next_key: StoredValue<u64>) -> u64 {
  let key = next_key.get_value();
  next_key.set_value(key + 1);
  key
}

/// What is wrong with a typed rule; all `None` when it can be saved.
fn check(rule: &RuleDraft) -> RuleErrors {
  let spaces = parse_list(&rule.spaces);
  let channels = parse_list(&rule.channels);
  let is_id = |item: &String| item.parse::<Snowflake>().is_ok();
  RuleErrors {
    maybe_path: rule
      .path
      .trim()
      .trim_matches('/')
      .is_empty()
      .then_some("settings-group-mappings-path-error"),
    maybe_spaces: if spaces.is_empty() {
      Some("settings-group-mappings-spaces-error")
    } else if !spaces.iter().all(is_id) {
      Some("admin-error-ids")
    } else {
      None
    },
    maybe_channels: (!channels.iter().all(is_id)).then_some("admin-error-ids"),
  }
}

/// The rules to send, or `None` while a row has an error.
fn to_rules(drafts: &[RuleDraft]) -> Option<Vec<GroupMappingRule>> {
  drafts
    .iter()
    .map(|draft| {
      if check(draft) != RuleErrors::default() {
        return None;
      }
      Some(GroupMappingRule {
        group_path: draft.path.trim().to_owned(),
        space_ids: ids::<SpaceId>(&draft.spaces)?,
        channel_ids: ids::<ConversationId>(&draft.channels)?,
      })
    })
    .collect()
}

fn ids<T: teamcraft_proto::Id>(text: &str) -> Option<Vec<T>> {
  parse_list(text)
    .iter()
    .map(|item| item.parse::<Snowflake>().ok().map(T::from_snowflake))
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_rule_needs_a_path_and_spaces_and_only_ids() {
    let good = RuleDraft {
      key: 0,
      path: "engineering".into(),
      spaces: "12\n13".into(),
      channels: String::new(),
    };
    assert_eq!(check(&good), RuleErrors::default());
    let rules = to_rules(std::slice::from_ref(&good)).unwrap();
    assert_eq!(rules[0].space_ids.len(), 2);
    let bad = RuleDraft {
      path: "/".into(),
      spaces: String::new(),
      channels: "abc".into(),
      ..good
    };
    let errors = check(&bad);
    assert!(errors.maybe_path.is_some() && errors.maybe_spaces.is_some() && errors.maybe_channels.is_some());
    assert!(to_rules(&[bad]).is_none());
  }
}
