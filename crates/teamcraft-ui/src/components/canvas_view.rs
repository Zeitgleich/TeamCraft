//! A channel's shared page of notes, the "Notes" tab of its header (issue #1221, e2ee.md §4.11):
//! headings, paragraphs, quotes, code blocks, lists and checklists with inline formatting and links
//! that the channel's members edit together.
//!
//! The page is a Yjs document ([`CanvasDoc`]) that this view keeps while it is open; the core keeps
//! only the updates. Each edit applies here at once and goes to the core a moment later, merged
//! with the edits made meanwhile ([`FLUSH_IDLE`], at most [`FLUSH_MAX_WAIT_MS`] after the first),
//! which encrypts it as a hidden message of the channel. Updates of other members come back through
//! the core and are merged in with the caret kept where it was. On the web this view is a wasm
//! chunk of its own (`areas::canvas`), so the CRDT loads with the first page that opens.
//!
//! "Version history" (issue #1245) lists the page's versions as this device holds them, who made
//! each and when ([`versions`]), shows one read-only and restores it as a new edit. The line under
//! the toolbar names who else edited the page in the last minutes, from the same updates: no
//! presence of its own goes to the server.
//!
//! Each block is an element of its own that the browser edits ([`canvas_editor`]): after every
//! input the view reads its text and writes the change to the document, and the document's content
//! comes back into the element when it differs. Undo and redo are the document's, of this editor's
//! own edits (`CanvasDoc::undo`), and go to the other members like any edit.

use std::time::Duration;

use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use teamcraft_client_api::canvas::{
  Block, BlockKind, CanvasDoc, CanvasState, CanvasVersion, InlineMark, TextStyle, byte_offset,
  caret_after_change, compaction, editors_since, is_page_link, merge_updates, page_at, typed_range, versions,
};
use teamcraft_client_api::model::OlderHistory;
use wasm_bindgen::JsCast;

use crate::components::canvas_editor::{self, Caret};
use crate::components::canvas_toolbar::{
  CanvasAction, CanvasActive, CanvasToolbar, canvas_action_for, kind_label, kind_of, mark_of,
};
use crate::components::composer::with_scheme;
use crate::components::composer_toolbar::FormatAction;
use crate::components::empty_state::EmptyState;
use crate::components::icons::IconName;
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, UserId};
use crate::state::{AppState, use_app_state};

/// How long typing pauses before the edits go out.
const FLUSH_IDLE: Duration = Duration::from_millis(600);
/// The longest an edit waits while typing goes on.
const FLUSH_MAX_WAIT_MS: f64 = 2_000.0;
/// Updates since the last snapshot after which an edit is sent as the whole page instead.
const SNAPSHOT_EVERY: u32 = 50;
/// The largest snapshot sent: with base64 and encryption well below the server's 1 MiB message.
const MAX_SNAPSHOT_BYTES: usize = 512 * 1024;
/// Updates the core holds beyond which the view hands it their merge.
const COMPACT_ABOVE: usize = 32;
/// Pages of older history read per opening, so the page holds the edits from before this device.
const MAX_HISTORY_PAGES: u32 = 20;
/// Who edited the page this recently counts as editing it now.
const EDITING_WINDOW_MS: i64 = 2 * 60 * 1000;
/// How often that line is looked at again.
const EDITING_CHECK_MS: u64 = 20_000;
/// The element IDs of the blocks' text fields.
const BLOCK_ID_PREFIX: &str = "canvas-block-";

/// The page as this view edits it.
#[derive(Default)]
struct Editor {
  page: CanvasDoc,
  /// Own updates the core doesn't hold yet.
  unsent: Vec<Vec<u8>>,
  /// When the first of them was made (`Date.now()`).
  maybe_unsent_since: Option<f64>,
}

impl Editor {
  /// Brings the page up to the core's updates and the own ones not sent yet.
  fn sync(&mut self, stored: &CanvasState) -> Vec<Block> {
    let mut all: Vec<&[u8]> = stored.updates.iter().map(AsRef::as_ref).collect();
    all.extend(self.unsent.iter().map(Vec::as_slice));
    if let Err(error) = self.page.sync(&all) {
      tracing::warn!(%error, "a page update was not applied");
    }
    self.page.blocks()
  }

  /// What goes to the core: the edits since the last time merged, or the whole page when a
  /// snapshot is due and this device holds the history it needs.
  fn outgoing(&mut self, stored: &CanvasState, holds_history: bool) -> Option<(Vec<u8>, bool)> {
    if self.unsent.is_empty() {
      return None;
    }
    let unsent = std::mem::take(&mut self.unsent);
    self.maybe_unsent_since = None;
    if holds_history && stored.updates_since_snapshot.saturating_add(1) >= SNAPSHOT_EVERY {
      let mut all: Vec<&[u8]> = stored.updates.iter().map(AsRef::as_ref).collect();
      all.extend(unsent.iter().map(Vec::as_slice));
      if let Ok(Some(whole)) = teamcraft_client_api::canvas::snapshot(&all)
        && whole.len() <= MAX_SNAPSHOT_BYTES
      {
        return Some((whole, true));
      }
    }
    match merge_updates(&unsent) {
      Ok(merged) => Some((merged, false)),
      Err(error) => {
        tracing::warn!(%error, "own page edits could not be merged");
        None
      },
    }
  }
}

/// Everything the page's handlers share; all handles, so it is `Copy`.
#[derive(Clone, Copy)]
struct Canvas {
  state: AppState,
  conversation: ConversationId,
  editor: StoredValue<Editor, LocalStorage>,
  blocks: RwSignal<Vec<Block>>,
  stored: Memo<CanvasState>,
  older: Memo<OlderHistory>,
  can_edit: Memo<bool>,
  /// Where the toolbar's formatting applies: the block and selection last in use.
  selection: RwSignal<Option<BlockSelection>>,
  /// Formatting picked with no text selected, for the text typed next.
  pending: RwSignal<PendingMarks>,
  /// The selection the open link dialog links.
  link_target: RwSignal<Option<BlockSelection>>,
  can_undo: RwSignal<bool>,
  can_redo: RwSignal<bool>,
  is_composing: StoredValue<bool>,
  timer: StoredValue<Option<TimeoutHandle>, LocalStorage>,
}

/// A selection in one of the page's blocks.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockSelection {
  id: String,
  caret: Caret,
}

/// Formatting chosen for text not typed yet, at the caret it was chosen at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PendingMarks {
  maybe_at: Option<BlockSelection>,
  marks: Vec<(InlineMark, bool)>,
}

impl PendingMarks {
  /// Flips `mark` for the next text; flipped back to what the text there has anyway, it is gone.
  fn toggled(&self, at: &BlockSelection, mark: InlineMark, has_mark: bool) -> Self {
    let mut marks = if self.maybe_at.as_ref() == Some(at) {
      self.marks.clone()
    } else {
      Vec::new()
    };
    let is_on = !marks
      .iter()
      .find(|(pending, _)| *pending == mark)
      .map_or(has_mark, |(_, is_on)| *is_on);
    marks.retain(|(pending, _)| *pending != mark);
    if is_on != has_mark {
      marks.push((mark, is_on));
    }
    Self {
      maybe_at: Some(at.clone()),
      marks,
    }
  }
}

#[component]
pub fn CanvasView(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));
  let mut editor = Editor::default();
  editor.page.enable_undo(|| js_sys::Date::now() as u64);
  let canvas = Canvas {
    state,
    conversation,
    editor: StoredValue::new_local(editor),
    blocks: RwSignal::new(Vec::new()),
    stored: Memo::new(move |_| state.read(|backend| backend.canvas(conversation))),
    older: Memo::new(move |_| state.read(|backend| backend.older_history(conversation))),
    can_edit: Memo::new(move |_| info.get().is_some_and(|conv| conv.can_post && !conv.is_archived)),
    selection: RwSignal::new(None),
    pending: RwSignal::new(PendingMarks::default()),
    link_target: RwSignal::new(None),
    can_undo: RwSignal::new(false),
    can_redo: RwSignal::new(false),
    is_composing: StoredValue::new(false),
    timer: StoredValue::new_local(None),
  };

  // Every change of the core's list: merged in, the caret kept, a long list compacted.
  Effect::new(move |_| {
    let stored = canvas.stored.get();
    let next = canvas.editor.try_update_value(|editor| editor.sync(&stored));
    if let Some(next) = next {
      canvas.blocks.set(next);
    }
    canvas.compact_if_long(&stored);
  });

  // The edits from before this device are in the channel's older history: read it while the
  // page is open, a page at a time.
  let history_pages = StoredValue::new(0_u32);
  Effect::new(move |_| {
    if canvas.older.get() != OlderHistory::Available || history_pages.get_value() >= MAX_HISTORY_PAGES {
      return;
    }
    history_pages.update_value(|pages| *pages += 1);
    request_animation_frame(move || {
      let _ = state.try_write(|backend| backend.load_older_history(conversation));
    });
  });

  // Whatever waits goes out when the tab closes.
  on_cleanup(move || canvas.flush());

  // Who else edited the page lately: "Anna is editing".
  let now = RwSignal::new(crate::clock::now_ms());
  crate::reactive::use_interval_while_visible(EDITING_CHECK_MS, move || {
    let _ = now.try_set(crate::clock::now_ms());
  });
  let me = state.me().id;
  let editors = Memo::new(move |_| {
    let since = now.get() - EDITING_WINDOW_MS;
    canvas.stored.with(|stored| editors_since(stored, since, me))
  });
  let editing = move || {
    let names: Vec<String> = editors
      .get()
      .into_iter()
      .map(|user| name_of(state, i18n, user))
      .collect();
    (!names.is_empty()).then(|| {
      let joined = names.join(", ");
      i18n.t_count_with("canvas-editing", names.len(), &[("names", joined.as_str())])
    })
  };

  let show_history = RwSignal::new(false);
  let toggle_history = move |_| {
    // Own edits not handed over yet belong in the history.
    canvas.flush();
    show_history.update(|open| *open = !*open);
  };

  let has_blocks = move || canvas.blocks.with(|blocks| !blocks.is_empty());
  let active = Memo::new(move |_| canvas.active());
  let on_action = Callback::new(move |action: CanvasAction| canvas.run_action(action));
  view! {
    <div class="canvas-view">
      <div class="canvas-bar">
        <Show when=move || canvas.can_edit.get() && !show_history.get() fallback=|| ()>
          <CanvasToolbar active=active on_action=on_action />
        </Show>
        <button
          type="button"
          class="btn canvas-history-button"
          aria-pressed=move || show_history.get().to_string()
          on:click=toggle_history
        >
          {move || i18n.t(if show_history.get() { "canvas-history-close" } else { "canvas-history" })}
        </button>
        {move || canvas.link_target.get().map(|target| view! { <LinkDialog canvas=canvas target=target /> })}
      </div>
      <p class="canvas-note">
        {move || {
          let mut parts = vec![i18n.t("canvas-encrypted")];
          if !canvas.can_edit.get() {
            parts.push(i18n.t("canvas-read-only"));
          }
          match canvas.older.get() {
            OlderHistory::Loading => parts.push(i18n.t("canvas-loading-history")),
            OlderHistory::Failed => parts.push(i18n.t("canvas-history-failed")),
            OlderHistory::Available | OlderHistory::Complete => {},
          }
          parts.join(" ")
        }}
      </p>
      <p class="canvas-presence" aria-live="polite">{editing}</p>
      <Show when=move || show_history.get() fallback=|| ()>
        <CanvasHistory canvas=canvas open=show_history />
      </Show>
      <div
        class="canvas-page"
        role="group"
        aria-label=move || i18n.t("canvas-page-label")
        hidden=move || show_history.get()
      >
        <For each=move || canvas.blocks.get() key=|block| block.id.clone() let:block>
          <BlockRow canvas=canvas id=block.id />
        </For>
        <Show when=move || !has_blocks() fallback=|| ()>
          {move || if canvas.can_edit.get() {
            view! {
              <textarea
                class="canvas-input canvas-first"
                rows="1"
                aria-label=move || i18n.t("canvas-page-label")
                placeholder=move || i18n.t("canvas-placeholder")
                on:input=move |ev| canvas.start_page(&event_target::<web_sys::HtmlTextAreaElement>(&ev))
              ></textarea>
            }
              .into_any()
          } else {
            view! { <EmptyState icon=IconName::List title_key="canvas-empty-title" body_key="canvas-empty" /> }.into_any()
          }}
        </Show>
      </div>
    </div>
  }
}

/// The page's versions, newest first, and the one picked shown read-only with a button that
/// restores it.
#[component]
fn CanvasHistory(canvas: Canvas, open: RwSignal<bool>) -> impl IntoView {
  let i18n = use_i18n();
  let state = canvas.state;
  let versions = Memo::new(move |_| {
    canvas.stored.with(|stored| {
      versions(stored).unwrap_or_else(|error| {
        tracing::warn!(%error, "the page's history could not be read");
        Vec::new()
      })
    })
  });
  // Picked by who and when, so a version arriving meanwhile doesn't move the pick; none: the
  // newest.
  let picked = RwSignal::new(None::<(UserId, i64)>);
  let shown = Memo::new(move |_| {
    let maybe_pick = picked.get();
    versions.with(|versions| {
      maybe_pick
        .and_then(|pick| versions.iter().position(|version| version_key(version) == pick))
        .or_else(|| (!versions.is_empty()).then_some(0))
    })
  });
  let preview = Memo::new(move |_| {
    let index = shown.get()?;
    let version = versions.with(|versions| versions.get(index).cloned())?;
    canvas.stored.with(|stored| match page_at(stored, &version) {
      Ok(blocks) => Some(blocks),
      Err(error) => {
        tracing::warn!(%error, "a version of the page could not be read");
        None
      },
    })
  });
  let restore = move |_| {
    let Some(blocks) = preview.get_untracked() else {
      return;
    };
    canvas.flush();
    canvas.edit(|page| page.restore(&blocks).into_iter().collect());
    canvas.flush();
    open.set(false);
  };
  let can_restore = move || canvas.can_edit.get() && shown.get().is_some_and(|index| index > 0);
  view! {
    <section class="canvas-history" aria-label=move || i18n.t("canvas-history")>
      <p class="canvas-note">{move || i18n.t("canvas-history-note")}</p>
      <Show
        when=move || versions.with(|versions| !versions.is_empty())
        fallback=move || view! { <p class="canvas-history-empty">{move || i18n.t("canvas-history-empty")}</p> }
      >
        <div class="canvas-history-body">
          <ul class="canvas-versions">
            {move || {
              versions
                .get()
                .into_iter()
                .enumerate()
                .map(|(index, version)| {
                  let key = version_key(&version);
                  let who = name_of(state, i18n, version.by);
                  let when = crate::clock::format_date_time(version.until_ms, i18n.locale().tag());
                  view! {
                    <li>
                      <button
                        type="button"
                        class="canvas-version"
                        aria-pressed=move || (shown.get() == Some(index)).to_string()
                        on:click=move |_| picked.set(Some(key))
                      >
                        <span class="canvas-version-when">{when}</span>
                        <span class="canvas-version-who">{who}</span>
                        {(index == 0)
                          .then(|| view! { <span class="canvas-version-current">{move || i18n.t("canvas-history-current")}</span> })}
                      </button>
                    </li>
                  }
                })
                .collect_view()
            }}
          </ul>
          <div class="canvas-history-preview">
            <div class="canvas-page canvas-preview" role="group" aria-label=move || i18n.t("canvas-history-preview")>
              {move || preview.get().map(|blocks| preview_blocks(&blocks).collect_view())}
            </div>
            <Show when=can_restore fallback=|| ()>
              <div class="canvas-history-actions">
                <button type="button" class="btn btn-primary" on:click=restore>
                  {move || i18n.t("canvas-history-restore")}
                </button>
              </div>
            </Show>
          </div>
        </div>
      </Show>
    </section>
  }
}

/// A version's blocks, read-only.
fn preview_blocks(blocks: &[Block]) -> impl Iterator<Item = AnyView> + '_ {
  blocks.iter().enumerate().map(move |(index, block)| {
    let marker = match block.kind {
      BlockKind::Bullet => view! { <span class="canvas-marker" aria-hidden="true">"•"</span> }.into_any(),
      BlockKind::Numbered => {
        let number = numbered_position(blocks, index);
        view! { <span class="canvas-marker" aria-hidden="true">{format!("{number}.")}</span> }.into_any()
      },
      BlockKind::Check => {
        view! { <input type="checkbox" class="canvas-check" disabled=true prop:checked=block.is_checked /> }
          .into_any()
      },
      _ => ().into_any(),
    };
    let runs = if block.kind.is_code() {
      vec![(block.text.clone(), None)]
    } else {
      block
        .runs()
        .into_iter()
        .map(|(text, maybe_style)| (text.to_owned(), maybe_style.cloned()))
        .collect()
    };
    view! {
      <div class=format!("canvas-block canvas-block-{}", block.kind.tag())>
        {marker}
        <div class="canvas-input canvas-preview-text">
          {runs.into_iter().map(|(text, maybe_style)| preview_run(text, maybe_style)).collect_view()}
        </div>
      </div>
    }
    .into_any()
  })
}

/// A stretch of a version's text with its formatting.
fn preview_run(text: String, maybe_style: Option<TextStyle>) -> AnyView {
  let Some(style) = maybe_style else {
    return text.into_any();
  };
  let mut content = text.into_any();
  if style.is_code {
    content = view! { <code class="md-inline-code">{content}</code> }.into_any();
  }
  if style.is_strike {
    content = view! { <s>{content}</s> }.into_any();
  }
  if style.is_italic {
    content = view! { <em>{content}</em> }.into_any();
  }
  if style.is_bold {
    content = view! { <strong>{content}</strong> }.into_any();
  }
  match style.maybe_link {
    Some(url) => view! {
      <a href=url.clone() title=url target="_blank" rel="noopener noreferrer" class="md-link">
        {content}
      </a>
    }
    .into_any(),
    None => content,
  }
}

/// The number of a numbered item: its place in the run of numbered items it ends.
fn numbered_position(blocks: &[Block], index: usize) -> usize {
  blocks[..index]
    .iter()
    .rev()
    .take_while(|block| block.kind == BlockKind::Numbered)
    .count()
    + 1
}

fn version_key(version: &CanvasVersion) -> (UserId, i64) {
  (version.by, version.from_ms)
}

/// A member's name as the app shows it; one the device doesn't know reads as such.
fn name_of(state: AppState, i18n: I18n, user: UserId) -> String {
  state
    .read(|backend| backend.user(user))
    .map(|person| person.display_name)
    .unwrap_or_else(|| i18n.t("canvas-history-someone"))
}

/// The dialog of the toolbar's link button: links the selection, or adds a linked text at the caret.
#[component]
fn LinkDialog(canvas: Canvas, target: BlockSelection) -> impl IntoView {
  let i18n = use_i18n();
  let is_caret = target.caret.start == target.caret.end;
  let maybe_current = canvas.blocks.with_untracked(|blocks| {
    blocks
      .iter()
      .find(|block| block.id == target.id)
      .and_then(|block| block.link_at(target.caret.start))
      .map(str::to_owned)
  });
  let has_link = maybe_current.is_some();
  let url = RwSignal::new(maybe_current.unwrap_or_default());
  let label = RwSignal::new(String::new());
  let is_invalid = RwSignal::new(false);
  let input = NodeRef::<html::Input>::new();
  Effect::new(move |_| {
    if let Some(input) = input.get() {
      drop(input.focus());
    }
  });
  let error_id = format!("canvas-link-error-{}", target.id);
  let (save_target, remove_target, close_target) = (target.clone(), target.clone(), target);
  let save = move || {
    let address = with_scheme(url.get_untracked().trim());
    if !is_page_link(&address) {
      is_invalid.set(true);
      return;
    }
    canvas.apply_link(&save_target, &address, &label.get_untracked());
  };
  let close = move || canvas.close_link(&close_target);
  let close_on_key = close.clone();
  let close_on_click = close;
  let error_for_input = error_id.clone();
  let error_for_message = error_id;
  view! {
    <form
      class="link-dialog canvas-link-dialog"
      novalidate
      role="dialog"
      aria-label=move || i18n.t("format-link")
      on:submit=move |ev| {
        ev.prevent_default();
        save();
      }
      on:keydown=move |ev: web_sys::KeyboardEvent| {
        if ev.key() == "Escape" {
          ev.prevent_default();
          close_on_key();
        }
      }
    >
      {is_caret.then(|| view! {
        <label>
          <span>{move || i18n.t("composer-link-text")}</span>
          <input
            type="text"
            class="input"
            prop:value=move || label.get()
            on:input=move |ev| label.set(event_target_value(&ev))
          />
        </label>
      })}
      <label>
        <span>{move || i18n.t("composer-link-url")}</span>
        <input
          type="text"
          class="input"
          autocomplete="url"
          inputmode="url"
          placeholder="https://"
          node_ref=input
          prop:value=move || url.get()
          aria-invalid=move || is_invalid.get().then_some("true")
          aria-describedby=move || is_invalid.get().then(|| error_for_input.clone())
          on:input=move |ev| {
            url.set(event_target_value(&ev));
            is_invalid.set(false);
          }
        />
      </label>
      {move || is_invalid.get().then(|| view! { <p id=error_for_message.clone() class="composer-error small" role="alert">{move || i18n.t("composer-link-invalid")}</p> })}
      <div class="link-dialog-actions">
        {has_link.then(|| view! {
          <button type="button" class="btn btn-small" on:click=move |_| canvas.remove_link(&remove_target)>
            {move || i18n.t("canvas-link-remove")}
          </button>
        })}
        <button type="button" class="btn btn-small" on:click=move |_| close_on_click()>{move || i18n.t("cancel")}</button>
        <button type="submit" class="btn btn-small btn-primary">{move || i18n.t("save")}</button>
      </div>
    </form>
  }
}

/// One block: its marker (bullet, number or check box) and its text.
#[component]
fn BlockRow(canvas: Canvas, id: String) -> impl IntoView {
  let i18n = use_i18n();
  let row_id = id.clone();
  // The block, and its number in a run of numbered items.
  let row = Memo::new(move |_| {
    canvas.blocks.with(|blocks| {
      let index = blocks.iter().position(|block| block.id == row_id)?;
      Some((blocks[index].clone(), numbered_position(blocks, index)))
    })
  });
  let kind = move || row.get().map_or(BlockKind::Paragraph, |(block, _)| block.kind);
  let read_only = move || !canvas.can_edit.get();
  let field = NodeRef::<html::Div>::new();
  // The element follows the document: every change of the block, and whether it can be edited.
  Effect::new(move |_| {
    let Some(element) = field.get() else {
      return;
    };
    let Some((block, _)) = row.get() else {
      return;
    };
    let is_editable = canvas.can_edit.get();
    canvas_editor::show_block(&element, &block, is_editable, canvas.is_composing.get_value());
  });
  let with_field = move |action: &dyn Fn(&web_sys::HtmlElement)| {
    if let Some(element) = field.get_untracked() {
      action(&element);
    }
  };
  let check_id = id.clone();
  let (input_id, keydown_id, select_id, before_id, paste_id, compose_id) = (
    id.clone(),
    id.clone(),
    id.clone(),
    id.clone(),
    id.clone(),
    id.clone(),
  );
  let select = move |target: &str| with_field(&|element| canvas.note_selection(target, element));
  let select_key = select_id.clone();
  let select_up = select_id.clone();
  let select_focus = select_id;
  view! {
    <div class=move || format!("canvas-block canvas-block-{}", kind().tag())>
      {move || match kind() {
        BlockKind::Bullet => view! { <span class="canvas-marker" aria-hidden="true">"•"</span> }.into_any(),
        BlockKind::Numbered => {
          let number = row.get().map_or(1, |(_, number)| number);
          view! { <span class="canvas-marker" aria-hidden="true">{format!("{number}.")}</span> }.into_any()
        },
        BlockKind::Check => {
          let check_id = check_id.clone();
          view! {
            <input
              type="checkbox"
              class="canvas-check"
              aria-label=move || i18n.t("canvas-check-label")
              prop:checked=move || row.get().is_some_and(|(block, _)| block.is_checked)
              disabled=read_only
              on:change=move |ev| {
                let is_checked = event_target::<web_sys::HtmlInputElement>(&ev).checked();
                canvas.edit(|page| page.set_checked(&check_id, is_checked).into_iter().collect());
              }
            />
          }
            .into_any()
        },
        _ => ().into_any(),
      }}
      <div
        id=block_element_id(&id)
        class="canvas-input"
        node_ref=field
        role="textbox"
        aria-multiline="true"
        aria-label=move || i18n.t(kind_label(kind()))
        aria-readonly=move || read_only().then_some("true")
        contenteditable=move || (!read_only()).to_string()
        spellcheck=move || (!kind().is_code()).to_string()
        on:input=move |_| {
          if let Some(element) = field.get_untracked() {
            canvas.on_input(&input_id, &element);
          }
        }
        on:beforeinput=move |ev: web_sys::InputEvent| {
          if let Some(element) = field.get_untracked() {
            canvas.on_before_input(&before_id, &element, &ev);
          }
        }
        on:keydown=move |ev: web_sys::KeyboardEvent| {
          if let Some(element) = field.get_untracked() {
            canvas.on_keydown(&keydown_id, &element, &ev);
          }
        }
        on:keyup=move |_| select(&select_key)
        on:mouseup=move |_| select(&select_up)
        on:focus=move |_| select(&select_focus)
        on:paste=move |ev: web_sys::ClipboardEvent| {
          ev.prevent_default();
          if let Some(element) = field.get_untracked() {
            canvas.on_paste(&paste_id, &element, &ev);
          }
        }
        on:drop=|ev: web_sys::DragEvent| ev.prevent_default()
        on:click=move |ev: web_sys::MouseEvent| open_link(&ev)
        on:compositionstart=move |_| canvas.is_composing.set_value(true)
        on:compositionend=move |_| {
          canvas.is_composing.set_value(false);
          if let Some(element) = field.get_untracked() {
            canvas.on_input(&compose_id, &element);
            if let Some((block, _)) = row.get_untracked() {
              canvas_editor::show_block(&element, &block, true, false);
            }
          }
        }
      ></div>
    </div>
  }
}

/// Ctrl or Cmd and a click open a link in an editable block; a plain click puts the caret there.
fn open_link(ev: &web_sys::MouseEvent) {
  if !(ev.ctrl_key() || ev.meta_key()) {
    return;
  }
  let Some(target) = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
  else {
    return;
  };
  if let Some(url) = canvas_editor::link_under(&target) {
    ev.prevent_default();
    crate::external_links::open(&url);
  }
}

impl Canvas {
  /// Applies an edit to the page now and sends it soon.
  fn edit(self, change: impl FnOnce(&mut CanvasDoc) -> Vec<Vec<u8>>) {
    if !self.can_edit.get_untracked() {
      return;
    }
    let next = self.editor.try_update_value(|editor| {
      let updates = change(&mut editor.page);
      if updates.is_empty() {
        return None;
      }
      editor.unsent.extend(updates);
      editor.maybe_unsent_since.get_or_insert_with(js_sys::Date::now);
      Some(editor.page.blocks())
    });
    if let Some(Some(next)) = next {
      self.blocks.set(next);
      self.note_undo_state();
      self.schedule_flush();
    }
  }

  fn note_undo_state(self) {
    let state = self
      .editor
      .try_with_value(|editor| (editor.page.can_undo(), editor.page.can_redo()));
    if let Some((can_undo, can_redo)) = state {
      let _ = self.can_undo.try_set(can_undo);
      let _ = self.can_redo.try_set(can_redo);
    }
  }

  /// What the toolbar shows for the current selection.
  fn active(self) -> CanvasActive {
    let mut active = CanvasActive {
      can_undo: self.can_undo.get(),
      can_redo: self.can_redo.get(),
      ..CanvasActive::default()
    };
    let Some(selection) = self.selection.get() else {
      return active;
    };
    let pending = self.pending.get();
    self.blocks.with(|blocks| {
      let Some(block) = blocks.iter().find(|block| block.id == selection.id) else {
        return;
      };
      active.maybe_kind = Some(block.kind);
      for (index, mark) in InlineMark::ALL.into_iter().enumerate() {
        let has_mark = block.has_mark(selection.caret.start, selection.caret.end, mark);
        let maybe_pending = pending.marks.iter().find(|(candidate, _)| *candidate == mark);
        active.marks[index] = maybe_pending.map_or(has_mark, |(_, is_on)| *is_on);
      }
    });
    active
  }

  /// The selection now: of the block with the focus, else the one last in use (a toolbar button
  /// reached with the keyboard).
  fn current_selection(self) -> Option<BlockSelection> {
    focused_block_field()
      .and_then(|element| {
        let id = element.id().trim_start_matches(BLOCK_ID_PREFIX).to_owned();
        canvas_editor::selection_in(&element).map(|caret| BlockSelection { id, caret })
      })
      .or_else(|| self.selection.get_untracked())
  }

  fn run_action(self, action: CanvasAction) {
    match action {
      CanvasAction::Undo => self.undo_or_redo(false),
      CanvasAction::Redo => self.undo_or_redo(true),
      CanvasAction::Check => self.toggle_kind(BlockKind::Check),
      CanvasAction::Format(FormatAction::Inline(style)) => {
        if let Some(mark) = mark_of(style) {
          self.toggle_mark(mark);
        }
      },
      CanvasAction::Format(FormatAction::Block(format)) => self.toggle_kind(kind_of(format)),
      CanvasAction::Format(FormatAction::Link) => self.open_link_dialog(),
    }
  }

  /// A kind applied again goes back to a paragraph.
  fn toggle_kind(self, kind: BlockKind) {
    let Some(selection) = self.current_selection() else {
      return;
    };
    let next = if self.kind_of(&selection.id) == Some(kind) {
      BlockKind::Paragraph
    } else {
      kind
    };
    self.edit(|page| page.set_kind(&selection.id, next).into_iter().collect());
  }

  /// The selection's `mark` toggled; with only a caret, for the text typed next.
  fn toggle_mark(self, mark: InlineMark) {
    let Some(selection) = self.current_selection() else {
      return;
    };
    let Some(kind) = self.kind_of(&selection.id) else {
      return;
    };
    if kind.is_code() {
      return;
    }
    if selection.caret.start == selection.caret.end {
      let has_mark = self.blocks.with_untracked(|blocks| {
        blocks
          .iter()
          .find(|block| block.id == selection.id)
          .is_some_and(|block| block.has_mark(selection.caret.start, selection.caret.end, mark))
      });
      self.selection.set(Some(selection.clone()));
      self
        .pending
        .update(|pending| *pending = pending.toggled(&selection, mark, has_mark));
      return;
    }
    self.edit(|page| {
      page
        .toggle_mark(&selection.id, selection.caret.start, selection.caret.end, mark)
        .into_iter()
        .collect()
    });
  }

  fn open_link_dialog(self) {
    if let Some(selection) = self.current_selection()
      && self.kind_of(&selection.id).is_some_and(|kind| !kind.is_code())
    {
      self.link_target.set(Some(selection));
    }
  }

  fn apply_link(self, target: &BlockSelection, url: &str, label: &str) {
    self.link_target.set(None);
    let Caret { start, end } = target.caret;
    if start != end {
      self.edit(|page| {
        page
          .set_link(&target.id, start, end, Some(url))
          .into_iter()
          .collect()
      });
      focus_block_later(target.id.clone(), target.caret);
      return;
    }
    let Some(old) = self.text_of_block(&target.id) else {
      return;
    };
    let inserted = if label.trim().is_empty() { url } else { label };
    let at = byte_offset(&old, start);
    let new = format!("{}{inserted}{}", &old[..at], &old[at..]);
    let after = start + utf16_len(inserted);
    self.edit(|page| {
      page
        .set_text(&target.id, &new)
        .into_iter()
        .chain(page.set_link(&target.id, start, after, Some(url)))
        .collect()
    });
    focus_block_later(
      target.id.clone(),
      Caret {
        start: after,
        end: after,
      },
    );
  }

  /// Takes the link at the target away, all of its stretch when only a caret stands in it.
  fn remove_link(self, target: &BlockSelection) {
    self.link_target.set(None);
    let range = self.blocks.with_untracked(|blocks| {
      let block = blocks.iter().find(|block| block.id == target.id)?;
      link_range(block, target.caret)
    });
    if let Some((start, end)) = range {
      self.edit(|page| page.set_link(&target.id, start, end, None).into_iter().collect());
    }
    focus_block_later(target.id.clone(), target.caret);
  }

  fn close_link(self, target: &BlockSelection) {
    self.link_target.set(None);
    focus_block_later(target.id.clone(), target.caret);
  }

  /// Takes back this editor's last edit or does it again, with the caret where it happened.
  fn undo_or_redo(self, is_redo: bool) {
    let before = self.blocks.get_untracked();
    self.edit(|page| {
      let maybe_update = if is_redo { page.redo() } else { page.undo() };
      maybe_update.into_iter().collect()
    });
    let maybe_focus = self.blocks.with_untracked(|after| change_after(&before, after));
    if let Some((id, caret)) = maybe_focus {
      focus_block_later(
        id,
        Caret {
          start: caret,
          end: caret,
        },
      );
    }
  }

  /// Keeps what the toolbar works on; pending formatting ends once the caret moved on.
  fn note_selection(self, id: &str, element: &web_sys::HtmlElement) {
    let Some(caret) = canvas_editor::selection_in(element) else {
      return;
    };
    let selection = BlockSelection {
      id: id.to_owned(),
      caret,
    };
    if self.selection.get_untracked().as_ref() != Some(&selection) {
      self.selection.set(Some(selection.clone()));
    }
    if self
      .pending
      .with_untracked(|pending| !pending.marks.is_empty() && pending.maybe_at.as_ref() != Some(&selection))
    {
      self.pending.set(PendingMarks::default());
    }
  }

  fn on_input(self, id: &str, element: &web_sys::HtmlElement) {
    let value = canvas_editor::text_of(element);
    let id = id.to_owned();
    // "# ", "- ", "1. ", "> " or "[] " typed at the start of a paragraph choose its kind.
    if !self.is_composing.get_value()
      && self.kind_of(&id) == Some(BlockKind::Paragraph)
      && let Some((new_kind, prefix_len)) = BlockKind::from_prefix(&value)
    {
      let rest = value[prefix_len..].to_owned();
      let is_checked = value.starts_with("[x] ");
      self.edit(|page| {
        [
          page.set_kind(&id, new_kind),
          page.set_text(&id, &rest),
          is_checked.then(|| page.set_checked(&id, true)).flatten(),
        ]
        .into_iter()
        .flatten()
        .collect()
      });
      return;
    }
    let pending = self.take_pending();
    let Some(old) = self.text_of_block(&id) else {
      return;
    };
    self.edit(|page| {
      let mut updates: Vec<Vec<u8>> = page.set_text(&id, &value).into_iter().collect();
      if let Some((start, end)) = typed_range(&old, &value) {
        for (mark, is_on) in pending {
          updates.extend(page.set_mark(&id, start, end, mark, is_on));
        }
      }
      updates
    });
  }

  /// Formatting waiting for the next text, gone with this.
  fn take_pending(self) -> Vec<(InlineMark, bool)> {
    let pending = self.pending.get_untracked();
    if !pending.marks.is_empty() {
      self.pending.set(PendingMarks::default());
    }
    pending.marks
  }

  /// What the browser would do itself and the page does its own way.
  fn on_before_input(self, id: &str, element: &web_sys::HtmlElement, ev: &web_sys::InputEvent) {
    match ev.input_type().as_str() {
      "historyUndo" => {
        ev.prevent_default();
        self.undo_or_redo(false);
      },
      "historyRedo" => {
        ev.prevent_default();
        self.undo_or_redo(true);
      },
      "insertParagraph" => {
        ev.prevent_default();
        self.on_enter(id, element, false);
      },
      "insertLineBreak" => {
        ev.prevent_default();
        self.on_enter(id, element, true);
      },
      "insertFromDrop" => ev.prevent_default(),
      kind if kind.starts_with("format") => ev.prevent_default(),
      _ => {},
    }
  }

  fn on_keydown(self, id: &str, element: &web_sys::HtmlElement, ev: &web_sys::KeyboardEvent) {
    if ev.is_composing() || !self.can_edit.get_untracked() {
      return;
    }
    if let Some(action) = canvas_action_for(ev) {
      ev.prevent_default();
      self.run_action(action);
      return;
    }
    let Some(Caret { start, end }) = canvas_editor::selection_in(element) else {
      return;
    };
    let text = canvas_editor::text_of(element);
    let length = utf16_len(&text);
    match ev.key().as_str() {
      "Enter" => {
        ev.prevent_default();
        self.on_enter(id, element, ev.shift_key());
      },
      "Backspace" if start == 0 && end == 0 => {
        if self.join_with_previous(id) {
          ev.prevent_default();
        }
      },
      "ArrowUp" if start == 0 && end == 0 => self.focus_neighbour(id, -1),
      "ArrowDown" if start == length && end == length => self.focus_neighbour(id, 1),
      _ => {},
    }
  }

  /// Enter, or Shift+Enter for a line break in the block. In a code block Enter is a line break too;
  /// on an empty last line it leaves the block.
  fn on_enter(self, id: &str, element: &web_sys::HtmlElement, is_line_break: bool) {
    let Some(caret) = canvas_editor::selection_in(element) else {
      return;
    };
    let text = canvas_editor::text_of(element);
    let kind = self.kind_of(id);
    if is_line_break {
      self.insert_at(id, &text, caret, "\n");
      return;
    }
    if kind.is_some_and(BlockKind::is_code) {
      if caret.start == caret.end && caret.end == utf16_len(&text) && text.ends_with('\n') {
        self.leave_code(id, &text);
      } else {
        self.insert_at(id, &text, caret, "\n");
      }
      return;
    }
    self.split(id, &text, caret);
  }

  fn insert_at(self, id: &str, text: &str, caret: Caret, inserted: &str) {
    let (from, to) = (byte_offset(text, caret.start), byte_offset(text, caret.end));
    let new = format!("{}{inserted}{}", &text[..from], &text[to..]);
    let id = id.to_owned();
    self.edit(|page| page.set_text(&id, &new).into_iter().collect());
    let after = caret.start + utf16_len(inserted);
    focus_block_later(
      id,
      Caret {
        start: after,
        end: after,
      },
    );
  }

  /// The empty last line of a code block goes, a paragraph follows it.
  fn leave_code(self, id: &str, text: &str) {
    let trimmed = text.strip_suffix('\n').unwrap_or(text).to_owned();
    let id = id.to_owned();
    let mut maybe_new = None;
    self.edit(|page| {
      let (new_id, inserted) = page.insert_block(Some(&id), BlockKind::Paragraph, "");
      maybe_new = Some(new_id);
      page
        .set_text(&id, &trimmed)
        .into_iter()
        .chain([inserted])
        .collect()
    });
    if let Some(new_id) = maybe_new {
      focus_block_later(new_id, Caret { start: 0, end: 0 });
    }
  }

  /// A paste is plain text. In a code block or on one line it goes in at the caret; further lines
  /// become blocks of their own, the last with what stood after the caret.
  fn on_paste(self, id: &str, element: &web_sys::HtmlElement, ev: &web_sys::ClipboardEvent) {
    if !self.can_edit.get_untracked() {
      return;
    }
    let Some(caret) = canvas_editor::selection_in(element) else {
      return;
    };
    let raw = ev
      .clipboard_data()
      .and_then(|data| data.get_data("text/plain").ok())
      .unwrap_or_default();
    let pasted = raw.replace("\r\n", "\n").replace('\r', "\n");
    if pasted.is_empty() {
      return;
    }
    let text = canvas_editor::text_of(element);
    let Some(kind) = self.kind_of(id) else {
      return;
    };
    let lines: Vec<&str> = pasted.lines().collect();
    if kind.is_code() || lines.len() <= 1 {
      self.insert_at(id, &text, caret, pasted.trim_end_matches('\n'));
      return;
    }
    let next_kind = if kind.continues_on_enter() {
      kind
    } else {
      BlockKind::Paragraph
    };
    let mut maybe_last = None;
    let id = id.to_owned();
    self.edit(|page| {
      let Some((tail, split)) = page.split_block(&id, caret.start, caret.end, next_kind) else {
        return Vec::new();
      };
      let before = &text[..byte_offset(&text, caret.start)];
      let mut updates: Vec<Vec<u8>> = vec![split];
      updates.extend(page.set_text(&id, &format!("{before}{}", lines[0])));
      let mut previous = id.clone();
      for line in &lines[1..lines.len() - 1] {
        let (new_id, update) = page.insert_block(Some(&previous), next_kind, line);
        previous = new_id;
        updates.push(update);
      }
      let last = lines[lines.len() - 1];
      let after = &text[byte_offset(&text, caret.end)..];
      updates.extend(page.set_text(&tail, &format!("{last}{after}")));
      maybe_last = Some((tail, utf16_len(last)));
      updates
    });
    if let Some((tail, caret)) = maybe_last {
      focus_block_later(
        tail,
        Caret {
          start: caret,
          end: caret,
        },
      );
    }
  }

  /// Enter: the text after the caret becomes a new block of the same list or quote (a paragraph
  /// after anything else); Enter in an empty one ends it.
  fn split(self, id: &str, text: &str, caret: Caret) {
    let Some(kind) = self.kind_of(id) else {
      return;
    };
    let id = id.to_owned();
    if kind.continues_on_enter() && text.is_empty() {
      self.edit(|page| page.set_kind(&id, BlockKind::Paragraph).into_iter().collect());
      return;
    }
    let next_kind = if kind.continues_on_enter() {
      kind
    } else {
      BlockKind::Paragraph
    };
    let mut maybe_new = None;
    self.edit(
      |page| match page.split_block(&id, caret.start, caret.end, next_kind) {
        Some((new_id, update)) => {
          maybe_new = Some(new_id);
          vec![update]
        },
        None => Vec::new(),
      },
    );
    if let Some(new_id) = maybe_new {
      focus_block_later(new_id, Caret { start: 0, end: 0 });
    }
  }

  /// Backspace at the start: a list item, quote or heading becomes a paragraph first, a paragraph
  /// joins the block above. Returns whether it did something.
  fn join_with_previous(self, id: &str) -> bool {
    let id = id.to_owned();
    let (maybe_kind, maybe_previous) = self.blocks.with_untracked(|blocks| {
      let index = blocks.iter().position(|block| block.id == id);
      (
        index.map(|index| blocks[index].kind),
        index
          .and_then(|index| index.checked_sub(1))
          .map(|index| blocks[index].clone()),
      )
    });
    match (maybe_kind, maybe_previous) {
      (Some(kind), _) if kind != BlockKind::Paragraph => {
        self.edit(|page| page.set_kind(&id, BlockKind::Paragraph).into_iter().collect());
        true
      },
      (Some(_), Some(previous)) => {
        let caret = utf16_len(&previous.text);
        self.edit(|page| page.join_blocks(&previous.id, &id).into_iter().collect());
        focus_block_later(
          previous.id,
          Caret {
            start: caret,
            end: caret,
          },
        );
        true
      },
      _ => false,
    }
  }

  fn focus_neighbour(self, id: &str, step: isize) {
    let maybe_target = self.blocks.with_untracked(|blocks| {
      let index = blocks.iter().position(|block| block.id == id)?;
      let target = index.checked_add_signed(step)?;
      blocks
        .get(target)
        .map(|block| (block.id.clone(), utf16_len(&block.text)))
    });
    if let Some((target, length)) = maybe_target {
      let caret = if step < 0 { length } else { 0 };
      focus_block_later(
        target,
        Caret {
          start: caret,
          end: caret,
        },
      );
    }
  }

  /// The first text typed into an empty page makes its first block.
  fn start_page(self, area: &web_sys::HtmlTextAreaElement) {
    let value = area.value();
    let caret = area.selection_start().ok().flatten().unwrap_or(0);
    area.set_value("");
    let mut maybe_new = None;
    self.edit(|page| {
      let (id, update) = page.insert_block(None, BlockKind::Paragraph, &value);
      maybe_new = Some(id);
      vec![update]
    });
    if let Some(id) = maybe_new {
      focus_block_later(
        id,
        Caret {
          start: caret,
          end: caret,
        },
      );
    }
  }

  fn text_of_block(self, id: &str) -> Option<String> {
    self.blocks.with_untracked(|blocks| {
      blocks
        .iter()
        .find(|block| block.id == id)
        .map(|block| block.text.clone())
    })
  }

  fn kind_of(self, id: &str) -> Option<BlockKind> {
    self
      .blocks
      .with_untracked(|blocks| blocks.iter().find(|block| block.id == id).map(|block| block.kind))
  }

  /// Sends the waiting edits once typing pauses, or now when the first waited long enough.
  fn schedule_flush(self) {
    if let Some(Some(running)) = self.timer.try_get_value() {
      running.clear();
    }
    let since = self
      .editor
      .try_with_value(|editor| editor.maybe_unsent_since)
      .flatten();
    if since.is_some_and(|since| js_sys::Date::now() - since >= FLUSH_MAX_WAIT_MS) {
      self.flush();
      return;
    }
    match set_timeout_with_handle(move || self.flush(), FLUSH_IDLE) {
      Ok(started) => {
        let _ = self.timer.try_set_value(Some(started));
      },
      Err(_) => self.flush(),
    }
  }

  /// Hands the waiting edits to the core. Also runs from the timer and when the view goes, so it
  /// only touches reactive values with their `try_` forms.
  fn flush(self) {
    if let Some(Some(running)) = self.timer.try_get_value() {
      running.clear();
      let _ = self.timer.try_set_value(None);
    }
    // Gone with the view: the edits still go out, as they are.
    let stored = self.stored.try_get_untracked().unwrap_or_default();
    let holds_history = self.older.try_get_untracked() == Some(OlderHistory::Complete);
    let Some(Some((update, is_snapshot))) = self
      .editor
      .try_update_value(|editor| editor.outgoing(&stored, holds_history))
    else {
      return;
    };
    let conversation = self.conversation;
    let sent = self
      .state
      .try_write(|backend| backend.edit_canvas(conversation, update.clone(), is_snapshot));
    if sent.is_none() {
      // The app is going away: the backend may still take it.
      let _ = self
        .state
        .try_read(|backend| backend.edit_canvas(conversation, update, is_snapshot));
    }
  }

  /// Hands the core the merge of a long list with the history of what it merged, a frame later
  /// (not while the list is read).
  fn compact_if_long(self, stored: &CanvasState) {
    if stored.updates.len() <= COMPACT_ABOVE {
      return;
    }
    let (merged, history) = match compaction(stored) {
      Ok(compacted) => compacted,
      Err(error) => {
        tracing::warn!(%error, "the page's updates could not be merged");
        return;
      },
    };
    let (state, conversation, revision) = (self.state, self.conversation, stored.revision);
    request_animation_frame(move || {
      let _ = state.try_write(|backend| backend.compact_canvas(conversation, revision, merged, history));
    });
  }
}

fn block_element_id(id: &str) -> String {
  format!("{BLOCK_ID_PREFIX}{id}")
}

/// Where undo or redo changed the page: the first block that differs and the caret at the end of
/// its change; when blocks only went, the block before the first of them.
fn change_after(before: &[Block], after: &[Block]) -> Option<(String, u32)> {
  let previous = |id: &str| before.iter().find(|block| block.id == id);
  for block in after {
    match previous(&block.id) {
      None => return Some((block.id.clone(), utf16_len(&block.text))),
      Some(old) if old != block => {
        return Some((block.id.clone(), caret_after_change(&old.text, &block.text)));
      },
      Some(_) => {},
    }
  }
  let removed = before
    .iter()
    .position(|block| !after.iter().any(|other| other.id == block.id))?;
  before[..removed]
    .iter()
    .rev()
    .find(|block| after.iter().any(|other| other.id == block.id))
    .or_else(|| after.first())
    .map(|block| (block.id.clone(), utf16_len(&block.text)))
}

/// The UTF-16 range the link at `caret` covers, or the selection if it has one.
fn link_range(block: &Block, caret: Caret) -> Option<(u32, u32)> {
  if caret.start != caret.end {
    return Some((caret.start, caret.end));
  }
  let at = byte_offset(&block.text, caret.start);
  let span = block
    .spans
    .iter()
    .find(|span| span.style.maybe_link.is_some() && (span.start..=span.end).contains(&at))?;
  Some((
    utf16_len(&block.text[..span.start]),
    utf16_len(&block.text[..span.end]),
  ))
}

/// The block text field with the caret, if any.
fn focused_block_field() -> Option<web_sys::HtmlElement> {
  let active = web_sys::window()?.document()?.active_element()?;
  let element = active.dyn_into::<web_sys::HtmlElement>().ok()?;
  element.id().starts_with(BLOCK_ID_PREFIX).then_some(element)
}

/// Puts the caret or selection into a block once it is rendered: right after the reactive update,
/// not at the next frame, so what is typed straight after Enter lands in the new block. A block
/// that isn't there yet gets one more try at the next frame.
fn focus_block_later(id: String, caret: Caret) {
  leptos::task::spawn_local(async move {
    leptos::task::tick().await;
    if !focus_block(&id, caret) {
      request_animation_frame(move || {
        focus_block(&id, caret);
      });
    }
  });
}

/// Whether the block's element exists to take the caret.
fn focus_block(id: &str, caret: Caret) -> bool {
  let maybe_element = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.get_element_by_id(&block_element_id(id)))
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  let Some(element) = maybe_element else {
    return false;
  };
  drop(element.focus());
  canvas_editor::set_selection(&element, caret);
  true
}

fn utf16_len(text: &str) -> u32 {
  u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn carets_in_utf16_units_land_on_character_boundaries() {
    assert_eq!(byte_offset("abc", 1), 1);
    assert_eq!(byte_offset("Grüße", 2), 2);
    assert_eq!(byte_offset("Grüße", 3), 4);
    assert_eq!(byte_offset("Grüße", 4), 6);
    // An emoji is two UTF-16 units and four bytes.
    assert_eq!(byte_offset("😀x", 2), 4);
    assert_eq!(byte_offset("ab", 9), 2);
  }

  fn at(id: &str, caret: u32) -> BlockSelection {
    BlockSelection {
      id: id.to_owned(),
      caret: Caret {
        start: caret,
        end: caret,
      },
    }
  }

  #[test]
  fn pending_marks_flip_and_stay_with_their_caret() {
    let here = at("a", 3);
    let pending = PendingMarks::default().toggled(&here, InlineMark::Bold, false);
    assert_eq!(pending.marks, [(InlineMark::Bold, true)]);
    let both = pending.toggled(&here, InlineMark::Italic, true);
    assert_eq!(
      both.marks,
      [(InlineMark::Bold, true), (InlineMark::Italic, false)]
    );
    // Back to what the text there has anyway: nothing pending for it.
    let again = both.toggled(&here, InlineMark::Bold, false);
    assert_eq!(again.marks, [(InlineMark::Italic, false)]);
    // Picked at another caret: what was pending there is gone.
    let moved = again.toggled(&at("a", 7), InlineMark::Strike, false);
    assert_eq!(moved.marks, [(InlineMark::Strike, true)]);
  }

  fn block(id: &str, text: &str) -> Block {
    Block {
      id: id.to_owned(),
      kind: BlockKind::Paragraph,
      text: text.to_owned(),
      is_checked: false,
      spans: Vec::new(),
    }
  }

  #[test]
  fn undo_puts_the_caret_where_the_change_is() {
    let before = [block("a", "one"), block("b", "two three")];
    // The text of the second block got shorter.
    assert_eq!(
      change_after(&before, &[block("a", "one"), block("b", "two")]),
      Some(("b".to_owned(), 3))
    );
    // A block came back.
    assert_eq!(
      change_after(&[block("a", "one")], &[block("a", "one"), block("b", "new")]),
      Some(("b".to_owned(), 3))
    );
    // The second block went: the caret goes to the first.
    assert_eq!(
      change_after(&before, &[block("a", "one")]),
      Some(("a".to_owned(), 3))
    );
    assert_eq!(change_after(&before, &before), None);
  }

  #[test]
  fn the_link_at_a_caret_is_found_at_both_of_its_ends() {
    let mut linked = block("a", "see docs now");
    linked.spans = vec![teamcraft_client_api::canvas::TextSpan {
      start: 4,
      end: 8,
      style: TextStyle {
        maybe_link: Some("https://example.com".to_owned()),
        ..TextStyle::default()
      },
    }];
    assert_eq!(link_range(&linked, Caret { start: 6, end: 6 }), Some((4, 8)));
    assert_eq!(link_range(&linked, Caret { start: 8, end: 8 }), Some((4, 8)));
    assert_eq!(link_range(&linked, Caret { start: 2, end: 2 }), None);
    assert_eq!(link_range(&linked, Caret { start: 1, end: 2 }), Some((1, 2)));
  }

  #[test]
  fn edits_go_out_merged_and_a_snapshot_when_due() {
    let mut editor = Editor::default();
    let (id, first) = editor.page.insert_block(None, BlockKind::Paragraph, "a");
    let second = editor.page.set_text(&id, "ab").unwrap();
    editor.unsent = vec![first, second];
    let stored = CanvasState::default();
    let (merged, is_snapshot) = editor.outgoing(&stored, true).unwrap();
    assert!(!is_snapshot);
    assert!(editor.unsent.is_empty());
    let page = CanvasDoc::from_updates(std::slice::from_ref(&merged)).unwrap();
    assert_eq!(page.blocks()[0].text, "ab");
    assert!(editor.outgoing(&stored, true).is_none(), "nothing waits");

    let third = editor.page.set_text(&id, "abc").unwrap();
    editor.unsent = vec![third];
    let due = CanvasState {
      revision: 1,
      updates: vec![merged.into()],
      updates_since_snapshot: SNAPSHOT_EVERY,
      ..CanvasState::default()
    };
    let (whole, is_snapshot) = editor.outgoing(&due, true).unwrap();
    assert!(is_snapshot);
    assert_eq!(CanvasDoc::from_updates(&[whole]).unwrap().blocks()[0].text, "abc");

    // Without the history it needs, a device sends no snapshot.
    let fourth = editor.page.set_text(&id, "abcd").unwrap();
    editor.unsent = vec![fourth];
    assert!(!editor.outgoing(&due, false).unwrap().1);
  }
}
