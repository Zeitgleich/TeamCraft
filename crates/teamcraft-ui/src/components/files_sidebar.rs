//! The sidebar of the Files place (#1510): which files (all, pictures and videos, documents,
//! starred) and whose (everywhere, or one space's). The list itself fills the main column
//! (`files_view.rs`); both read the choice from the app state, so the sidebar is the one place it
//! is made on wide layouts. Phones have no sidebar beside the list: the view's header has the
//! filters there.

use leptos::prelude::*;
use teamcraft_client_api::files_query::{FileKind, FileScope};

use crate::components::icons::{Icon, IconName};
use crate::components::space_tile::SpaceTile;
use crate::i18n::use_i18n;
use crate::state::use_app_state;

#[component]
pub fn FilesSidebar() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let kind = state.files_kind;
  let spaces = Memo::new(move |_| state.read(|backend| backend.spaces()));

  view! {
    <nav class="sidebar files-sidebar" aria-label=move || i18n.t("files-title")>
      <div class="sidebar-head">
        <h2 class="space-name">{move || i18n.t("files-title")}</h2>
      </div>
      <div class="sidebar-scroll">
        <div class="files-kinds" role="group" aria-label=move || i18n.t("files-filter")>
          {FileKind::ALL
            .into_iter()
            .map(|value| {
              let is_current = move || kind.get() == value;
              view! {
                <button
                  type="button"
                  class="sb-item"
                  class:current=is_current
                  aria-pressed=move || is_current().to_string()
                  on:click=move |_| kind.set(value)
                >
                  <Icon name=kind_icon(value) />
                  <span class="sb-name">{move || i18n.t(kind_label_key(value))}</span>
                </button>
              }
            })
            .collect_view()}
        </div>
        <section class="files-scopes" aria-labelledby="files-scope-title">
          <h3 class="sb-section-head files-scope-title" id="files-scope-title">
            {move || i18n.t("files-scope")}
          </h3>
          <div role="group" aria-labelledby="files-scope-title">
            <ScopeButton scope=FileScope::Everything>
              <Icon name=IconName::Grid />
              <span class="sb-name">{move || i18n.t("files-scope-everywhere")}</span>
            </ScopeButton>
            <For
              each=move || spaces.get()
              key=|space| (space.id, space.name.clone(), space.hue, space.maybe_image_version.clone())
              let:space
            >
              <ScopeButton scope=FileScope::Space(space.id)>
                <SpaceTile space=space.clone() />
                <span class="sb-name" translate="no">{space.name.clone()}</span>
              </ScopeButton>
            </For>
          </div>
        </section>
      </div>
    </nav>
  }
}

/// One scope: selected while the list shows its files.
#[component]
fn ScopeButton(scope: FileScope, children: Children) -> impl IntoView {
  let state = use_app_state();
  let is_current = move || state.files_scope.get() == scope;
  view! {
    <button
      type="button"
      class="sb-item"
      class:current=is_current
      aria-pressed=move || is_current().to_string()
      on:click=move |_| state.files_scope.set(scope)
    >
      {children()}
    </button>
  }
}

/// The label of a filter, shared with the view's header on phones.
pub fn kind_label_key(kind: FileKind) -> &'static str {
  match kind {
    FileKind::All => "files-filter-all",
    FileKind::Media => "files-filter-media",
    FileKind::Documents => "files-filter-documents",
    FileKind::Starred => "files-filter-starred",
  }
}

fn kind_icon(kind: FileKind) -> IconName {
  match kind {
    FileKind::All => IconName::Paperclip,
    FileKind::Media => IconName::Image,
    FileKind::Documents => IconName::File,
    FileKind::Starred => IconName::Star,
  }
}
