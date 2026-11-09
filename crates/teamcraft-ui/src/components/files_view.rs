//! The files the user can see: the Files place on the rail (issues #301, #1510). Everywhere, or one
//! space's (#954), as its sidebar (`files_sidebar.rs`) chooses. Filtered by kind, sorted, with
//! starred files, a page at a time (`teamcraft_client_api::files_query`). It reads
//! what the device already holds; nothing is fetched, so the server learns nothing about what is
//! looked at. The core reads a page through its index of messages with files and keeps it until a
//! file comes or goes (#1509), so the view reads on every change instead of on a timer.

use leptos::prelude::*;
use teamcraft_client_api::files_query::{
  FILES_PAGE_SIZE, FileKind, FileScope, FileSort, FilesPage, FilesQuery, MAX_STARRED_FILES, StarredFile,
  toggle_star,
};
use wasm_bindgen::JsCast;

use crate::components::empty_state::EmptyState;
use crate::components::file_views::{FileCard, FileStar};
use crate::components::files_sidebar::kind_label_key;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::Space;
use crate::state::use_app_state;

/// How far above the end of the list the next page comes in, in pixels.
const PAGE_AHEAD_PX: i32 = 800;

#[component]
pub fn FilesView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let kind = state.files_kind;
  let pages = RwSignal::new(1_usize);
  let spaces = Memo::new(move |_| state.read(|backend| backend.spaces()));
  // A space the user left meanwhile shows everything again.
  let scope = Memo::new(move |_| match state.files_scope.get() {
    FileScope::Space(space) if spaces.with(|all| all.iter().any(|candidate| candidate.id == space)) => {
      FileScope::Space(space)
    },
    FileScope::Space(_) | FileScope::Everything => FileScope::Everything,
  });
  let scope_name = Memo::new(move |_| match scope.get() {
    FileScope::Space(space) => spaces.with(|all| {
      all
        .iter()
        .find(|candidate| candidate.id == space)
        .map(|found| found.name.clone())
    }),
    FileScope::Everything => None,
  });
  // Phones have no sidebar beside the list: the header has the filters and scopes there.
  let is_compact = Memo::new(move |_| state.is_compact());
  let sort = Memo::new(move |_| prefs.with(|all| all.files.sort));
  let starred = Memo::new(move |_| prefs.with(|all| all.files.starred.clone()));
  // Another filter, order or space starts at the first page again.
  Effect::new(move |_| {
    kind.track();
    sort.track();
    scope.track();
    if pages.get_untracked() != 1 {
      pages.set(1);
    }
  });
  // `None` only while a desktop core reads the first page; then the list stays as it was.
  let page = Memo::new(move |previous: Option<&FilesPage>| {
    let (scope, kind, sort, max_files) = (scope.get(), kind.get(), sort.get(), pages.get() * FILES_PAGE_SIZE);
    // A star only reads again while the starred files are listed.
    let starred_now = if kind == FileKind::Starred {
      starred.get()
    } else {
      Vec::new()
    };
    // Read on every change: the core answers from the page it keeps until a file comes or goes.
    state
      .read(|backend| {
        backend.files_page(&FilesQuery {
          scope,
          kind,
          sort,
          starred: &starred_now,
          max_files,
        })
      })
      .or_else(|| previous.cloned())
      .unwrap_or_default()
  });
  let is_sorted_among_loaded =
    Memo::new(move |_| kind.get() != FileKind::Starred && !sort.get().is_by_time());
  let on_scroll = move |ev: leptos::ev::Event| {
    if page.with_untracked(|page| page.has_more) && is_near_end(&ev) {
      pages.update(|pages| *pages += 1);
    }
  };
  let is_full = Signal::derive(move || starred.with(|stars| stars.len() >= MAX_STARRED_FILES));
  let star_of = move |file: &StarredFile| {
    let file = file.clone();
    let key = file.clone();
    FileStar {
      is_starred: Signal::derive(move || starred.with(|stars| stars.contains(&key))),
      can_star: Signal::derive(move || !is_full.get()),
      on_toggle: Callback::new(move |()| {
        prefs.update(|all| {
          let is_starred = all.files.starred.contains(&file);
          if let Some(next) = toggle_star(&all.files.starred, &file, !is_starred) {
            all.files.starred = next;
          }
        });
      }),
    }
  };

  view! {
    <section class="files-view" aria-label=move || i18n.t("files-title")>
      <header class="panel-header files-view-header">
        <div class="panel-title">
          <h1>{move || i18n.t("files-title")}</h1>
          {move || scope_name.get().map(|name| view! { <span class="panel-subtitle" translate="no">{name}</span> })}
        </div>
        // Wide layouts choose the filter and the scope in the sidebar; the order stays here.
        <Show when=move || !is_compact.get() fallback=|| ()>
          <div class="files-sort">
            <label for="files-sort">{move || i18n.t("files-sort")}</label>
            <SortSelect id="files-sort" class="input" />
          </div>
        </Show>
      </header>
      // Phones have no sidebar beside the list: the filters scroll in one row of pills, the scope
      // and the order are two menus under them, as in the Photos and Files apps.
      <Show when=move || is_compact.get() fallback=|| ()>
        <div class="files-phone-bar">
          <div class="files-chips" role="group" aria-label=move || i18n.t("files-filter")>
            {FileKind::ALL
              .into_iter()
              .map(|value| {
                view! {
                  <button
                    type="button"
                    class="files-chip"
                    aria-pressed=move || (kind.get() == value).to_string()
                    on:click=move |_| kind.set(value)
                  >
                    {move || i18n.t(kind_label_key(value))}
                  </button>
                }
              })
              .collect_view()}
          </div>
          <div class="files-menus">
            <label class="files-menu">
              <span class="visually-hidden">{move || i18n.t("files-scope")}</span>
              <ScopeSelect scope=scope spaces=spaces />
              <Icon name=IconName::ChevronDown />
            </label>
            <label class="files-menu">
              <span class="visually-hidden">{move || i18n.t("files-sort")}</span>
              <SortSelect id="files-sort-phone" class="" />
              <Icon name=IconName::ChevronDown />
            </label>
          </div>
        </div>
      </Show>
      <div class="files-view-body panel-scroll" on:scroll=on_scroll>
        <Show when=move || is_sorted_among_loaded.get() fallback=|| ()>
          <p class="files-sort-note">{move || i18n.t("files-sort-loaded")}</p>
        </Show>
        <div class="files-list">
          <For
            each=move || page.with(|page| page.files.clone())
            key=|file| (file.message, file.file_id.clone())
            let:file
          >
            {
              let star = star_of(&StarredFile::of(&file));
              view! { <FileCard file=file maybe_star=star /> }
            }
          </For>
        </div>
        <Show when=move || page.with(|page| page.files.is_empty()) fallback=|| ()>
          {move || {
            let (icon, title_key, body_key) = empty_keys(kind.get(), scope.get());
            view! { <EmptyState icon=icon title_key=title_key body_key=body_key /> }
          }}
        </Show>
      </div>
    </section>
  }
}

/// The order of the list, remembered per account.
#[component]
fn SortSelect(id: &'static str, class: &'static str) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let sort = Memo::new(move |_| prefs.with(|all| all.files.sort));
  view! {
    <select
      id=id
      class=class
      on:change=move |ev| {
        if let Some(value) = sort_from_slug(&event_target_value(&ev)) {
          prefs.update(|all| all.files.sort = value);
        }
      }
    >
      {FileSort::ALL
        .into_iter()
        .map(|value| {
          view! {
            <option value=sort_slug(value) selected=move || sort.get() == value>
              {move || i18n.t(sort_label_key(value))}
            </option>
          }
        })
        .collect_view()}
    </select>
  }
}

/// Whose files, on phones: everywhere or one space's.
#[component]
fn ScopeSelect(scope: Memo<FileScope>, spaces: Memo<Vec<Space>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <select on:change=move |ev| {
      let value = event_target_value(&ev);
      let next = spaces
        .get_untracked()
        .into_iter()
        .find(|space| space.id.to_string() == value)
        .map_or(FileScope::Everything, |space| FileScope::Space(space.id));
      state.files_scope.set(next);
    }>
      <option value="" selected=move || scope.get() == FileScope::Everything>
        {move || i18n.t("files-scope-everywhere")}
      </option>
      <For each=move || spaces.get() key=|space| (space.id, space.name.clone()) let:space>
        <option value=space.id.to_string() selected=move || scope.get() == FileScope::Space(space.id) translate="no">
          {space.name.clone()}
        </option>
      </For>
    </select>
  }
}

fn sort_slug(sort: FileSort) -> &'static str {
  match sort {
    FileSort::Newest | FileSort::Unknown => "newest",
    FileSort::Oldest => "oldest",
    FileSort::Name => "name",
    FileSort::Largest => "largest",
  }
}

fn sort_from_slug(slug: &str) -> Option<FileSort> {
  FileSort::ALL.into_iter().find(|sort| sort_slug(*sort) == slug)
}

fn sort_label_key(sort: FileSort) -> &'static str {
  match sort {
    FileSort::Newest | FileSort::Unknown => "files-sort-newest",
    FileSort::Oldest => "files-sort-oldest",
    FileSort::Name => "files-sort-name",
    FileSort::Largest => "files-sort-largest",
  }
}

/// What an empty list says: no starred files yet, or no files at all here.
fn empty_keys(kind: FileKind, scope: FileScope) -> (IconName, &'static str, &'static str) {
  match (kind, scope) {
    (FileKind::Starred, _) => (IconName::Star, "files-starred-empty-title", "files-starred-empty"),
    (_, FileScope::Space(_)) => (IconName::Paperclip, "files-empty-title", "files-space-empty"),
    (_, FileScope::Everything) => (IconName::Paperclip, "files-empty-title", "files-all-empty"),
  }
}

/// Whether the scrolled list is close enough to its end for the next page.
fn is_near_end(ev: &leptos::ev::Event) -> bool {
  ev.current_target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .is_some_and(|element| {
      element.scroll_top() + element.client_height() + PAGE_AHEAD_PX >= element.scroll_height()
    })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_order_reads_back_from_its_slug() {
    for sort in FileSort::ALL {
      assert_eq!(sort_from_slug(sort_slug(sort)), Some(sort));
    }
  }

  #[test]
  fn keys_built_from_options_exist() {
    let en = include_str!("../../i18n/en/files.ftl");
    let mut keys: Vec<&str> = FileKind::ALL.into_iter().map(kind_label_key).collect();
    keys.extend(FileSort::ALL.into_iter().map(sort_label_key));
    keys.extend([
      "files-star",
      "files-unstar",
      "files-star-limit",
      "files-sort",
      "files-sort-loaded",
    ]);
    keys.extend([
      "files-starred-empty-title",
      "files-starred-empty",
      "files-scope",
      "files-scope-everywhere",
    ]);
    for key in keys {
      assert!(en.contains(&format!("\n{key} = ")), "{key}");
    }
  }
}
