//! The building blocks of the settings sections (issue #327): cards with a short heading, and rows
//! with a label, a one-line explanation and the control on the right. No bare checkboxes or
//! unlabelled selects: every control is named by its row and described by its hint.
//!
//! Rows get an element ID from their label key ([`row_id`]), so the settings search can jump to
//! them.

use leptos::prelude::*;

use crate::i18n::use_i18n;

/// One card of rows with a heading (an i18n key) and an optional hint below it.
#[component]
pub fn SettingsCard(
  /// i18n key of the heading.
  heading: &'static str,
  #[prop(optional)] hint: Option<&'static str>,
  children: Children,
) -> impl IntoView {
  let i18n = use_i18n();
  let heading_id = format!("{}-heading", row_id(heading));
  let labelled_by = heading_id.clone();
  view! {
    <section class="set-card" aria-labelledby=labelled_by>
      <h3 class="set-card-title" id=heading_id>{move || i18n.t(heading)}</h3>
      {hint.map(|key| view! { <p class="set-card-hint">{move || i18n.t(key)}</p> })}
      <div class="set-rows">{children()}</div>
    </section>
  }
}

/// A row with any control on the right (buttons, a slider, a list).
#[component]
pub fn SettingRow(
  /// i18n key of the label.
  label: &'static str,
  #[prop(optional)] hint: Option<&'static str>,
  /// A hint that changes (a status); takes the place of `hint`.
  #[prop(optional, into)]
  live_hint: Option<Signal<String>>,
  /// The control sits below the text instead of beside it (wide controls, lists).
  #[prop(optional)]
  is_stacked: bool,
  children: Children,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = row_id(label);
  let hint_id = format!("{id}-hint");
  view! {
    <div class="set-row" class:is-stacked=is_stacked id=id role="group" aria-labelledby=format!("{}-label", row_id(label))>
      <div class="set-row-text">
        <span class="set-row-label" id=format!("{}-label", row_id(label))>{move || i18n.t(label)}</span>
        {row_hint(hint, live_hint, hint_id)}
      </div>
      <div class="set-row-control">{children()}</div>
    </div>
  }
}

/// A row with a switch. The whole row toggles it, as on phones.
#[component]
pub fn SwitchRow(
  label: &'static str,
  #[prop(optional)] hint: Option<&'static str>,
  #[prop(into)] value: Signal<bool>,
  on_change: Callback<bool>,
  #[prop(optional, into)] is_disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = row_id(label);
  let input_id = format!("{id}-input");
  let label_for = input_id.clone();
  let hint_id = format!("{id}-hint");
  view! {
    <label class="set-row is-switch" class:is-disabled=move || is_disabled.get() id=id for=label_for>
      <span class="set-row-text">
        <span class="set-row-label">{move || i18n.t(label)}</span>
        {hint.map(|key| view! { <span class="set-row-hint" id=hint_id.clone()>{move || i18n.t(key)}</span> })}
      </span>
      <input
        id=input_id
        type="checkbox"
        role="switch"
        class="set-switch"
        aria-describedby=hint.map(|_| format!("{}-hint", row_id(label)))
        prop:checked=move || value.get()
        prop:disabled=move || is_disabled.get()
        on:change=move |ev| on_change.run(event_target_checked(&ev))
      />
    </label>
  }
}

/// A row with a select. `options` are (value, i18n key) pairs; with `is_raw_labels` the second
/// item is shown as it is (device names).
#[component]
pub fn SelectRow(
  label: &'static str,
  #[prop(optional)] hint: Option<&'static str>,
  options: Vec<(String, String)>,
  #[prop(into)] value: Signal<String>,
  on_change: Callback<String>,
  #[prop(optional, into)] is_disabled: Signal<bool>,
  #[prop(optional)] is_raw_labels: bool,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = row_id(label);
  let select_id = format!("{id}-input");
  let hint_id = format!("{id}-hint");
  view! {
    <div class="set-row" class:is-disabled=move || is_disabled.get() id=id>
      <span class="set-row-text">
        <label class="set-row-label" for=select_id.clone()>{move || i18n.t(label)}</label>
        {hint.map(|key| view! { <span class="set-row-hint" id=hint_id.clone()>{move || i18n.t(key)}</span> })}
      </span>
      <select
        id=select_id
        class="input set-select"
        aria-describedby=hint.map(|_| format!("{}-hint", row_id(label)))
        prop:disabled=move || is_disabled.get()
        on:change=move |ev| on_change.run(event_target_value(&ev))
      >
        {options
          .into_iter()
          .map(|(slug, text)| {
            let selected_slug = slug.clone();
            view! {
              <option value=slug selected=move || value.get() == selected_slug>
                {move || if is_raw_labels { text.clone() } else { i18n.t(&text) }}
              </option>
            }
          })
          .collect_view()}
      </select>
    </div>
  }
}

/// A row with a few choices side by side (a segmented control), for two or three options that are
/// quicker to see than to open. `options` are (value, i18n key) pairs.
#[component]
pub fn SegmentedRow(
  label: &'static str,
  #[prop(optional)] hint: Option<&'static str>,
  options: Vec<(String, String)>,
  #[prop(into)] value: Signal<String>,
  on_change: Callback<String>,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = row_id(label);
  let label_id = format!("{id}-label");
  let hint_id = format!("{id}-hint");
  view! {
    <div class="set-row" id=id>
      <span class="set-row-text">
        <span class="set-row-label" id=label_id.clone()>{move || i18n.t(label)}</span>
        {hint.map(|key| view! { <span class="set-row-hint" id=hint_id.clone()>{move || i18n.t(key)}</span> })}
      </span>
      <div
        class="set-segmented"
        role="radiogroup"
        aria-labelledby=label_id
        aria-describedby=hint.map(|_| format!("{}-hint", row_id(label)))
      >
        {options
          .into_iter()
          .map(|(slug, key)| {
            let checked_slug = slug.clone();
            let chosen_slug = slug.clone();
            view! {
              <label class="set-segment">
                <input
                  type="radio"
                  name=row_id(label)
                  value=slug
                  prop:checked=move || value.get() == checked_slug
                  on:change=move |_| on_change.run(chosen_slug.clone())
                />
                <span>{move || i18n.t(&key)}</span>
              </label>
            }
          })
          .collect_view()}
      </div>
    </div>
  }
}

/// Most entries a settings list renders at once; a filter finds the others on big instances.
pub const MAX_LIST_ENTRIES: usize = 50;

/// A filter field above a long list, and below it how many of the matches show.
#[component]
pub fn ListFilter(
  /// i18n key of the field's label and placeholder.
  label: &'static str,
  query: RwSignal<String>,
  #[prop(into)] matching: Signal<usize>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="set-row is-field">
      <input
        type="search"
        class="input"
        enterkeyhint="search"
        aria-label=move || i18n.t(label)
        placeholder=move || i18n.t(label)
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
      <Show when=move || { matching.get() > MAX_LIST_ENTRIES } fallback=|| ()>
        <span class="set-row-hint" role="status">
          {move || {
            i18n.t_with(
              "settings-list-limited",
              &[("shown", &MAX_LIST_ENTRIES.to_string()), ("total", &matching.get().to_string())],
            )
          }}
        </span>
      </Show>
    </div>
  }
}

/// Whether `text` contains `query` (any case); an empty query matches everything.
pub fn matches_filter(text: &str, query: &str) -> bool {
  let needle = query.trim().to_lowercase();
  needle.is_empty() || text.to_lowercase().contains(&needle)
}

/// The element ID of the row labelled `label_key`: what the settings search jumps to.
pub fn row_id(label_key: &str) -> String {
  format!("set-{label_key}")
}

/// (slug, i18n key) pairs for a [`SelectRow`]: `{prefix}{slug}` names each option.
pub fn slug_options(slugs: &[&str], prefix: &str) -> Vec<(String, String)> {
  slugs
    .iter()
    .map(|slug| ((*slug).to_owned(), format!("{prefix}{slug}")))
    .collect()
}

fn row_hint(hint: Option<&'static str>, live_hint: Option<Signal<String>>, hint_id: String) -> impl IntoView {
  let i18n = use_i18n();
  match (live_hint, hint) {
    (Some(text), _) => Some(
      view! { <span class="set-row-hint" id=hint_id role="status">{move || text.get()}</span> }.into_any(),
    ),
    (None, Some(key)) => {
      Some(view! { <span class="set-row-hint" id=hint_id>{move || i18n.t(key)}</span> }.into_any())
    },
    (None, None) => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn filters_ignore_case_and_blank_queries() {
    assert!(matches_filter("Design", " des "));
    assert!(matches_filter("Design", ""));
    assert!(!matches_filter("Design", "sales"));
  }

  #[test]
  fn rows_are_found_by_their_label() {
    assert_eq!(row_id("settings-theme"), "set-settings-theme");
    assert_eq!(
      slug_options(&["a", "b"], "x-"),
      vec![
        ("a".to_owned(), "x-a".to_owned()),
        ("b".to_owned(), "x-b".to_owned())
      ]
    );
  }
}
