//! The pages of a channel's shared notes (issue #1456, e2ee.md §4.11): the first page and the
//! further ones its document lists, as tabs, with adding, renaming and deleting a page.
//!
//! The list lives in the first page's Yjs document. This view keeps a copy of that document only
//! for the list: a change goes to the core at once as an edit of the first page, like any other.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::canvas::{CanvasDoc, MAX_PAGE_TITLE_CHARS, PageEntry, page_title};
use teamcraft_proto::{CanvasPageId, MAX_LISTED_CANVAS_PAGES};
use wasm_bindgen::JsCast;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::state::use_app_state;

/// The element ID of a page's tab.
const TAB_ID_PREFIX: &str = "canvas-page-tab-";
const NAME_FIELD_ID: &str = "canvas-page-name";
const NAME_ERROR_ID: &str = "canvas-page-name-error";

/// What the page bar asks.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PageDialog {
  New,
  Rename(CanvasPageId),
  Delete(CanvasPageId),
}

/// The page tabs and their actions. `selected` is the page shown (`None`: the first one); a page
/// that is deleted meanwhile, here or elsewhere, gives way to the first.
#[component]
pub fn CanvasPages(
  conversation: ConversationId,
  selected: RwSignal<Option<CanvasPageId>>,
  can_edit: Memo<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let stored = Memo::new(move |_| state.read(|backend| backend.canvas(conversation, None)));
  let list = StoredValue::new_local(CanvasDoc::new());
  let pages = RwSignal::new(Vec::<PageEntry>::new());
  let dialog = RwSignal::new(None::<PageDialog>);
  // The core keeps the updates of the pages the list names and drops those of deleted ones; it
  // can't read the list itself. Told a frame later, not while the list is read, and only on change.
  let noted = StoredValue::new_local(None::<(Vec<CanvasPageId>, Vec<CanvasPageId>)>);
  let note_pages = move || {
    let Some(reading) = list.try_with_value(|doc| {
      let listed: Vec<CanvasPageId> = doc.pages().into_iter().map(|entry| entry.id).collect();
      (listed, doc.deleted_pages())
    }) else {
      return;
    };
    if noted.try_with_value(|last| last.as_ref() == Some(&reading)) != Some(false) {
      return;
    }
    noted.set_value(Some(reading.clone()));
    request_animation_frame(move || {
      let (listed, deleted) = reading;
      let _ = state.try_write(|backend| backend.note_canvas_pages(conversation, &listed, &deleted));
    });
  };
  // The list shows at most the limit; at it, no page is added (CanvasDoc::add_page refuses too).
  let is_full = Memo::new(move |_| pages.with(|pages| pages.len() >= MAX_LISTED_CANVAS_PAGES));

  Effect::new(move |_| {
    let updates = stored.with(|stored| stored.updates.clone());
    let next = list.try_update_value(|doc| {
      if let Err(error) = doc.sync(&updates) {
        tracing::warn!(%error, "the list of pages could not be read");
      }
      doc.pages()
    });
    if let Some(next) = next {
      pages.set(next);
    }
    note_pages();
  });
  // The page shown was deleted: back to the first.
  Effect::new(move |_| {
    let is_gone = selected
      .get()
      .is_some_and(|page| pages.with(|pages| !pages.iter().any(|entry| entry.id == page)));
    if is_gone {
      selected.set(None);
    }
  });

  // Changes the list here at once and sends the change as an edit of the first page.
  let edit_list = move |change: &mut dyn FnMut(&mut CanvasDoc) -> Option<Vec<u8>>| {
    let Some(Some(update)) = list.try_update_value(|doc| change(doc)) else {
      return;
    };
    pages.set(list.with_value(CanvasDoc::pages));
    let _ = state.try_write(|backend| backend.edit_canvas(conversation, None, update, false));
    note_pages();
  };

  let title_of = move |entry: &PageEntry| {
    if entry.title.is_empty() {
      i18n.t("canvas-page-untitled")
    } else {
      entry.title.clone()
    }
  };
  let selected_title = move || {
    let page = selected.get()?;
    pages.with(|pages| pages.iter().find(|entry| entry.id == page).map(title_of))
  };

  let close = move || {
    dialog.set(None);
    focus_selected_tab(selected.get_untracked().as_ref());
  };
  let create = move |title: String| {
    let mut maybe_new = None;
    edit_list(&mut |doc| {
      let (id, update) = doc.add_page(&title)?;
      maybe_new = Some(id);
      Some(update)
    });
    dialog.set(None);
    if let Some(id) = maybe_new {
      selected.set(Some(id.clone()));
      focus_selected_tab(Some(&id));
    }
  };
  let rename = move |page: CanvasPageId, title: String| {
    edit_list(&mut |doc| doc.rename_page(&page, &title));
    close();
  };
  let delete = move |page: CanvasPageId| {
    edit_list(&mut |doc| doc.delete_page(&page));
    dialog.set(None);
    selected.set(None);
    focus_selected_tab(None);
  };

  let tab = move |maybe_page: Option<CanvasPageId>, label: Signal<String>| {
    let tab_page = maybe_page.clone();
    let is_selected = Memo::new(move |_| selected.get() == tab_page);
    let key_page = maybe_page.clone();
    view! {
      <button
        type="button"
        role="tab"
        id=tab_id(maybe_page.as_ref())
        class="canvas-page-tab"
        aria-selected=move || is_selected.get().to_string()
        tabindex=move || if is_selected.get() { "0" } else { "-1" }
        on:click=move |_| {
          if selected.get_untracked() != maybe_page {
            selected.set(maybe_page.clone());
          }
        }
        on:keydown=move |ev: web_sys::KeyboardEvent| {
          let step = match ev.key().as_str() {
            "ArrowRight" => 1,
            "ArrowLeft" => -1,
            _ => return,
          };
          ev.prevent_default();
          let order: Vec<Option<CanvasPageId>> = std::iter::once(None)
            .chain(pages.with_untracked(|pages| pages.iter().map(|entry| Some(entry.id.clone())).collect::<Vec<_>>()))
            .collect();
          let at = order.iter().position(|candidate| *candidate == key_page).unwrap_or(0);
          let next = (at as isize + step).rem_euclid(order.len() as isize) as usize;
          let next_page = order[next].clone();
          selected.set(next_page.clone());
          focus_selected_tab(next_page.as_ref());
        }
      >
        {move || label.get()}
      </button>
    }
  };

  view! {
    <div class="canvas-pages">
      <div class="canvas-page-tabs" role="tablist" aria-label=move || i18n.t("canvas-pages")>
        {tab(None, Signal::derive(move || i18n.t("canvas-page-first")))}
        <For each=move || pages.get() key=|entry| entry.id.clone() let:entry>
          {
            let id = entry.id.clone();
            let label = Signal::derive(move || {
              pages.with(|pages| pages.iter().find(|candidate| candidate.id == id).map(title_of)).unwrap_or_default()
            });
            tab(Some(entry.id), label)
          }
        </For>
      </div>
      <Show when=move || can_edit.get() fallback=|| ()>
        <div class="canvas-page-actions">
          <button
            type="button"
            class="icon-btn"
            aria-label=move || i18n.t("canvas-page-new")
            title=move || i18n.t("canvas-page-new")
            disabled=move || is_full.get()
            aria-describedby=move || is_full.get().then_some("canvas-pages-full")
            on:click=move |_| dialog.set(Some(PageDialog::New))
          >
            <Icon name=IconName::Plus />
          </button>
          {move || selected.get().map(|page| {
            let (rename_page, delete_page) = (page.clone(), page);
            view! {
              <button
                type="button"
                class="icon-btn"
                aria-label=move || i18n.t("canvas-page-rename")
                title=move || i18n.t("canvas-page-rename")
                on:click=move |_| dialog.set(Some(PageDialog::Rename(rename_page.clone())))
              >
                <Icon name=IconName::Compose />
              </button>
              <button
                type="button"
                class="icon-btn"
                aria-label=move || i18n.t("canvas-page-delete")
                title=move || i18n.t("canvas-page-delete")
                on:click=move |_| dialog.set(Some(PageDialog::Delete(delete_page.clone())))
              >
                <Icon name=IconName::Trash />
              </button>
            }
          })}
        </div>
        <Show when=move || is_full.get() fallback=|| ()>
          <p id="canvas-pages-full" class="canvas-note canvas-pages-full">
            {move || {
              let limit = MAX_LISTED_CANVAS_PAGES.to_string();
              i18n.t_with("canvas-pages-full", &[("count", limit.as_str())])
            }}
          </p>
        </Show>
      </Show>
      {move || match dialog.get() {
        None => ().into_any(),
        Some(PageDialog::New) => view! {
          <PageNameForm
            initial=String::new()
            submit_key="canvas-page-create"
            label_key="canvas-page-new"
            on_submit=Callback::new(create)
            on_close=Callback::new(move |()| close())
          />
        }
        .into_any(),
        Some(PageDialog::Rename(page)) => {
          let current = selected_title().unwrap_or_default();
          view! {
            <PageNameForm
              initial=current
              submit_key="save"
              label_key="canvas-page-rename"
              on_submit=Callback::new(move |title| rename(page.clone(), title))
              on_close=Callback::new(move |()| close())
            />
          }
          .into_any()
        },
        Some(PageDialog::Delete(page)) => {
          let title = selected_title().unwrap_or_default();
          view! {
            <DeletePageForm
              title=title
              on_confirm=Callback::new(move |()| delete(page.clone()))
              on_close=Callback::new(move |()| close())
            />
          }
          .into_any()
        },
      }}
    </div>
  }
}

/// The name of a new page, or the new name of one: required, at most [`MAX_PAGE_TITLE_CHARS`].
#[component]
fn PageNameForm(
  initial: String,
  submit_key: &'static str,
  label_key: &'static str,
  on_submit: Callback<String>,
  on_close: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let name = RwSignal::new(initial);
  let is_invalid = RwSignal::new(false);
  let input = NodeRef::<html::Input>::new();
  Effect::new(move |_| {
    if let Some(input) = input.get() {
      drop(input.focus());
      input.select();
    }
  });
  let save = move || {
    let title = page_title(&name.get_untracked());
    if title.is_empty() {
      is_invalid.set(true);
      if let Some(input) = input.get_untracked() {
        drop(input.focus());
      }
      return;
    }
    on_submit.run(title);
  };
  view! {
    <form
      class="link-dialog canvas-page-dialog"
      novalidate
      role="dialog"
      aria-label=move || i18n.t(label_key)
      on:submit=move |ev| {
        ev.prevent_default();
        save();
      }
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
          ev.prevent_default();
          on_close.run(());
        }
      }
    >
      <label for=NAME_FIELD_ID>
        <span>{move || i18n.t("canvas-page-name")}</span>
      </label>
      <input
        id=NAME_FIELD_ID
        type="text"
        class="input"
        maxlength=MAX_PAGE_TITLE_CHARS.to_string()
        node_ref=input
        prop:value=move || name.get()
        aria-invalid=move || is_invalid.get().then_some("true")
        aria-describedby=move || is_invalid.get().then_some(NAME_ERROR_ID)
        on:input=move |ev| {
          name.set(event_target_value(&ev));
          is_invalid.set(false);
        }
      />
      {move || is_invalid.get().then(|| view! {
        <p id=NAME_ERROR_ID class="composer-error small" role="alert">{move || i18n.t("canvas-page-name-empty")}</p>
      })}
      <div class="link-dialog-actions">
        <button type="button" class="btn btn-small" on:click=move |_| on_close.run(())>{move || i18n.t("cancel")}</button>
        <button type="submit" class="btn btn-small btn-primary">{move || i18n.t(submit_key)}</button>
      </div>
    </form>
  }
}

/// Asks before a page is deleted for everyone.
#[component]
fn DeletePageForm(title: String, on_confirm: Callback<()>, on_close: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let cancel = NodeRef::<html::Button>::new();
  Effect::new(move |_| {
    if let Some(button) = cancel.get() {
      drop(button.focus());
    }
  });
  view! {
    <div
      class="link-dialog canvas-page-dialog"
      role="alertdialog"
      aria-labelledby="canvas-page-delete-title"
      aria-describedby="canvas-page-delete-body"
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
          ev.prevent_default();
          on_close.run(());
        }
      }
    >
      <p id="canvas-page-delete-title" class="canvas-page-dialog-title">
        {move || i18n.t_with("canvas-page-delete-title", &[("title", title.as_str())])}
      </p>
      <p id="canvas-page-delete-body" class="canvas-note">{move || i18n.t("canvas-page-delete-body")}</p>
      <div class="link-dialog-actions">
        <button type="button" class="btn btn-small" node_ref=cancel on:click=move |_| on_close.run(())>
          {move || i18n.t("cancel")}
        </button>
        <button type="button" class="btn btn-small btn-danger" on:click=move |_| on_confirm.run(())>
          {move || i18n.t("canvas-page-delete-confirm")}
        </button>
      </div>
    </div>
  }
}

fn tab_id(maybe_page: Option<&CanvasPageId>) -> String {
  match maybe_page {
    Some(page) => format!("{TAB_ID_PREFIX}{page}"),
    None => format!("{TAB_ID_PREFIX}first"),
  }
}

/// Puts the focus on a page's tab once it is rendered.
fn focus_selected_tab(maybe_page: Option<&CanvasPageId>) {
  let id = tab_id(maybe_page);
  request_animation_frame(move || {
    let maybe_tab = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(&id))
      .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(tab) = maybe_tab {
      drop(tab.focus());
    }
  });
}
