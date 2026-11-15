//! Files of the channel shown in its shared notes (issue #1456, e2ee.md §4.11): a file block names
//! a message of the channel and its attachment, and shows the file as the message does. The file is
//! the channel's encrypted attachment; the page holds no key, so a device that doesn't hold the
//! message (it joined later, or the message was deleted) says so instead.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::canvas::FileRef;

use crate::components::file_views::MessageFiles;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::state::use_app_state;

/// Files the picker lists at first and adds with "Show more".
const PICKER_PAGE: usize = 30;

/// A file block's content: the file as its message shows it, or why it isn't there.
#[component]
pub fn CanvasFile(conversation: ConversationId, file: FileRef, name: Signal<String>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let found = Memo::new(move |_| {
    state.read(|backend| backend.conversation_file(conversation, file.message, &file.file_id))
  });
  move || match found.get() {
    Some(info) => view! {
      <div class="canvas-file">
        <MessageFiles files=vec![info] upload=Signal::derive(|| None) />
      </div>
    }
    .into_any(),
    None => view! {
      <div class="canvas-file canvas-file-missing">
        <Icon name=IconName::File />
        <span class="canvas-file-name">{move || name.get()}</span>
        <span class="canvas-note">{move || i18n.t("canvas-file-missing")}</span>
      </div>
    }
    .into_any(),
  }
}

/// The channel's files, newest first, to pick one for the page.
#[component]
pub fn CanvasFilePicker(
  conversation: ConversationId,
  on_pick: Callback<FileInfo>,
  on_close: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let limit = RwSignal::new(PICKER_PAGE);
  let page = Memo::new(move |_| {
    let limit = limit.get();
    state.read(|backend| backend.conversation_files_page(conversation, limit))
  });
  // Only files on the server can be named: one still going up has no attachment ID yet.
  let files = move || {
    page.get().map(|page| {
      page
        .files
        .into_iter()
        .filter(|file| file.file_id.parse::<u64>().is_ok())
        .collect::<Vec<_>>()
    })
  };
  let has_more = move || page.get().is_some_and(|page| page.has_more);
  let close = NodeRef::<html::Button>::new();
  Effect::new(move |_| {
    if let Some(button) = close.get() {
      drop(button.focus());
    }
  });
  view! {
    <section
      class="canvas-file-picker"
      role="dialog"
      aria-labelledby="canvas-file-picker-title"
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
          ev.prevent_default();
          on_close.run(());
        }
      }
    >
      <header class="canvas-file-picker-head">
        <h3 id="canvas-file-picker-title">{move || i18n.t("canvas-insert-file")}</h3>
        <button
          type="button"
          class="icon-btn"
          node_ref=close
          aria-label=move || i18n.t("close") title=move || i18n.t("close")
          on:click=move |_| on_close.run(())
        >
          <Icon name=IconName::Close />
        </button>
      </header>
      <p class="canvas-note">{move || i18n.t("canvas-file-picker-note")}</p>
      {move || match files() {
        None => view! { <p class="canvas-note">{move || i18n.t("canvas-file-picker-loading")}</p> }.into_any(),
        Some(files) if files.is_empty() => {
          view! { <p class="canvas-history-empty">{move || i18n.t("canvas-file-picker-empty")}</p> }.into_any()
        },
        Some(files) => view! {
          <ul class="canvas-file-list">
            {files
              .into_iter()
              .map(|file| {
                let icon = if file.media_type.starts_with("image/") { IconName::Image } else { IconName::File };
                let size = format_bytes(file.size_bytes);
                let name = file.file_name.clone();
                view! {
                  <li>
                    <button type="button" class="canvas-file-choice" on:click=move |_| on_pick.run(file.clone())>
                      <Icon name=icon />
                      <span class="canvas-file-name">{name}</span>
                      <span class="canvas-file-size">{size}</span>
                    </button>
                  </li>
                }
              })
              .collect_view()}
          </ul>
        }
        .into_any(),
      }}
      <Show when=has_more fallback=|| ()>
        <button type="button" class="btn btn-small" on:click=move |_| limit.update(|limit| *limit += PICKER_PAGE)>
          {move || i18n.t("canvas-file-picker-more")}
        </button>
      </Show>
    </section>
  }
}
