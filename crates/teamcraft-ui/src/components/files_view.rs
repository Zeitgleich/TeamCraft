//! Every file the user can see, across spaces, channels and direct messages: the "Files" entry of
//! the sidebar (issue #301). It gathers what each conversation already holds locally, newest first,
//! with a filter by kind; nothing is fetched.

use leptos::prelude::*;
use teamcraft_client_api::actions::FileInfo;

use crate::components::compact_back::CompactBack;
use crate::components::empty_state::EmptyState;
use crate::components::file_views::FileCard;
use crate::components::icons::IconName;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::i18n::use_i18n;
use crate::lightbox::MediaKind;
use crate::reactive::use_interval_while_visible;
use crate::state::use_app_state;

/// Files taken from one conversation (its newest); a conversation with thousands of attachments
/// must not decide how much work the view does.
const MAX_FILES_PER_CONVERSATION: usize = 100;
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
  let all_files = Memo::new(move |_| {
    reload.track();
    // Looked at again on a timer and when the view opens, not on every change of the workspace.
    untrack(|| state.read(gather_files))
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
          <EmptyState icon=IconName::Paperclip title_key="files-empty-title" body_key="files-all-empty" />
        </Show>
      </div>
    </section>
  }
}

/// The newest files of every conversation, newest first.
fn gather_files(backend: &dyn teamcraft_client_api::backend::Backend) -> Vec<FileInfo> {
  let mut conversations = backend.direct_conversations();
  for space in backend.spaces() {
    conversations.extend(backend.space_conversations(space.id));
  }
  let mut files: Vec<FileInfo> = conversations
    .into_iter()
    .flat_map(|conversation| {
      backend
        .conversation_files(conversation.id)
        .into_iter()
        .take(MAX_FILES_PER_CONVERSATION)
    })
    .collect();
  files.sort_by_key(|file| std::cmp::Reverse(file.maybe_sent_at));
  files
}
