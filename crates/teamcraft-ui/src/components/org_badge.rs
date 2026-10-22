//! The organization of a person (from the identity provider) as a small badge. Display only: no
//! permission depends on it (docs/branding.md §9).

use leptos::prelude::*;

use crate::i18n::use_i18n;

/// Shows the organization next to a name; nothing when the person has none.
#[component]
pub fn OrgBadge(#[prop(into)] organization: Option<String>) -> impl IntoView {
  let i18n = use_i18n();
  organization.map(|name| {
    view! {
      <span class="badge badge-org" translate="no" title=move || i18n.t("badge-organization")>{name}</span>
    }
  })
}
