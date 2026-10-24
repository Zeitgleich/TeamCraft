//! The section tabs of the channel and space settings: one tab per section, each section with its
//! own save button, so a long settings dialog reads in parts. Keyboard handling is the shared
//! `tab_list` one.

use leptos::prelude::*;

use crate::components::tab_list::{on_tab_keys, roving_tabindex};
use crate::i18n::use_i18n;

/// One section of a settings dialog: `slug` names its element IDs, `label_key` its tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
  pub slug: &'static str,
  pub label_key: &'static str,
}

impl Section {
  pub const fn new(slug: &'static str, label_key: &'static str) -> Self {
    Self { slug, label_key }
  }

  fn tab_id(self, prefix: &str) -> String {
    format!("{prefix}-tab-{}", self.slug)
  }

  fn panel_id(self, prefix: &str) -> String {
    format!("{prefix}-panel-{}", self.slug)
  }
}

/// The tab row and, below it, the selected section's panel (`children` renders a section).
#[component]
pub fn SectionTabs<F, V>(
  /// Prefix of the element IDs, unique per dialog.
  id_prefix: &'static str,
  /// The i18n key naming the tab list for screen readers.
  label_key: &'static str,
  sections: Vec<Section>,
  selected: RwSignal<Section>,
  children: F,
) -> impl IntoView
where
  F: Fn(Section) -> V + Send + Sync + 'static,
  V: IntoView + 'static,
{
  let i18n = use_i18n();
  let keys_sections = sections.clone();
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    on_tab_keys(&ev, &keys_sections, selected, move |section| {
      section.tab_id(id_prefix)
    })
  };
  view! {
    <div class="view-tabs section-tabs" role="tablist" aria-label=move || i18n.t(label_key) on:keydown=on_keydown>
      {sections
        .into_iter()
        .map(|section| {
          view! {
            <button
              type="button"
              id=section.tab_id(id_prefix)
              role="tab"
              aria-selected=move || (selected.get() == section).to_string()
              // Only the selected section's panel exists, so only its tab points at it.
              aria-controls=move || (selected.get() == section).then(|| section.panel_id(id_prefix))
              tabindex=move || roving_tabindex(selected.get() == section)
              on:click=move |_| selected.set(section)
            >
              {move || i18n.t(section.label_key)}
            </button>
          }
        })
        .collect_view()}
    </div>
    <div
      class="section-panel"
      role="tabpanel"
      id=move || selected.get().panel_id(id_prefix)
      aria-labelledby=move || selected.get().tab_id(id_prefix)
    >
      {move || children(selected.get())}
    </div>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tabs_and_panels_have_distinct_ids_per_dialog() {
    let general = Section::new("general", "settings-section-general");
    assert_eq!(general.tab_id("cs"), "cs-tab-general");
    assert_eq!(general.panel_id("cs"), "cs-panel-general");
    assert_ne!(general.tab_id("cs"), general.tab_id("ss"));
  }
}
