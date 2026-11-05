//! The files the user can see: the "Files" entry of the sidebar (issue #301). Inside a space those of
//! its conversations (#954), elsewhere those of all spaces, channels and direct messages. It gathers
//! what each conversation already holds locally, newest first, with a filter by kind; nothing is
//! fetched, so the server learns nothing about what is looked at.

use leptos::prelude::*;
use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::space_scope::{files_of_everything, files_of_space};

use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::file_views::FileCard;
use crate::components::icons::IconName;
use crate::components::page::ScopedSpaceName;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::use_i18n;
use crate::lightbox::MediaKind;
use crate::reactive::use_interval_while_visible;
use crate::state::use_app_state;

/// Files shown at once.
const MAX_FILES: usize = 300;
/// New files arrive without an event this view hears, so it looks again this often while it is
/// open and visible.
const REFRESH_MS: u64 = 15_000;

/// What the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileFilter {
  All,
  Media,
  Documents,
}

impl FileFilter {
  const ALL: [Self; 3] = [Self::All, Self::Media, Self::Documents];

  fn label_key(self) -> &'static str {
    match self {
      Self::All => "files-filter-all",
      Self::Media => "files-filter-media",
      Self::Documents => "files-filter-documents",
    }
  }

  fn matches(self, file: &FileInfo) -> bool {
    let is_media = MediaKind::of(&file.media_type).is_some();
    match self {
      Self::All => true,
      Self::Media => is_media,
      Self::Documents => !is_media,
    }
  }
}

#[component]
pub fn FilesView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let filter = RwSignal::new(FileFilter::All);
  let reload = RwSignal::new(0_u32);
  use_interval_while_visible(REFRESH_MS, move || reload.update(|count| *count += 1));
  let scope = Memo::new(move |_| state.maybe_scoped_space());
  let all_files = Memo::new(move |_| {
    reload.track();
    let maybe_space = scope.get();
    // Looked at again on a timer, when the view opens and when the space changes, not on every
    // change of the workspace.
    untrack(|| {
      state.read(|backend| match maybe_space {
        Some(space) => files_of_space(backend, space),
        None => files_of_everything(backend),
      })
    })
  });
  let files = Memo::new(move |_| {
    let wanted = filter.get();
    all_files.with(|all| {
      all
        .iter()
        .filter(|file| wanted.matches(file))
        .take(MAX_FILES)
        .cloned()
        .collect::<Vec<_>>()
    })
  });
  view! {
    <section class="files-view" aria-label=move || i18n.t("files-title")>
      <header class="panel-header">
        <CompactBack />
        <div class="panel-title">
          <h1>{move || i18n.t("files-title")}</h1>
          <ScopedSpaceName />
        </div>
        <div class="segmented" role="group" aria-label=move || i18n.t("files-filter")>
          <SlidingIndicator />
          {FileFilter::ALL
            .into_iter()
            .map(|value| {
              view! {
                <button aria-pressed=move || (filter.get() == value).to_string() on:click=move |_| filter.set(value)>
                  {move || i18n.t(value.label_key())}
                </button>
              }
            })
            .collect_view()}
        </div>
      </header>
      <div class="files-view-body panel-scroll">
        <div class="files-list">
          <For each=move || files.get() key=|file| (file.message, file.file_id.clone()) let:file>
            <FileCard file=file />
          </For>
        </div>
        <Show when=move || files.with(Vec::is_empty) fallback=|| ()>
          {move || {
            let body_key = if scope.get().is_some() { "files-space-empty" } else { "files-all-empty" };
            view! { <EmptyState icon=IconName::Paperclip title_key="files-empty-title" body_key=body_key /> }
          }}
        </Show>
      </div>
    </section>
  }
}
