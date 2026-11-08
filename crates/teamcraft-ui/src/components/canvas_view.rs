//! A channel's shared page of notes, the "Notes" tab of its header (issue #1221, e2ee.md §4.11):
//! headings, paragraphs, bulleted and numbered lists and checklists that the channel's members
//! edit together.
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

use std::time::Duration;

use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use teamcraft_client_api::canvas::{
  Block, BlockKind, CanvasDoc, CanvasState, CanvasVersion, compaction, editors_since, merge_updates, page_at,
  shift_caret, versions,
};
use teamcraft_client_api::model::OlderHistory;
use wasm_bindgen::JsCast;

use crate::components::empty_state::EmptyState;
use crate::components::icons::IconName;
use crate::components::sliding_indicator::SlidingIndicator;
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
  /// The block whose kind the toolbar shows and changes.
  current: RwSignal<Option<String>>,
  timer: StoredValue<Option<TimeoutHandle>, LocalStorage>,
}

#[component]
pub fn CanvasView(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));
  let canvas = Canvas {
    state,
    conversation,
    editor: StoredValue::new_local(Editor::default()),
    blocks: RwSignal::new(Vec::new()),
    stored: Memo::new(move |_| state.read(|backend| backend.canvas(conversation))),
    older: Memo::new(move |_| state.read(|backend| backend.older_history(conversation))),
    can_edit: Memo::new(move |_| info.get().is_some_and(|conv| conv.can_post && !conv.is_archived)),
    current: RwSignal::new(None),
    timer: StoredValue::new_local(None),
  };

  // Every change of the core's list: merged in, the caret kept, a long list compacted.
  Effect::new(move |_| {
    let stored = canvas.stored.get();
    let next = canvas.editor.try_update_value(|editor| editor.sync(&stored));
    if let Some(next) = next {
      canvas.show(next);
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
  view! {
    <div class="canvas-view">
      <div class="canvas-bar">
        <Show when=move || canvas.can_edit.get() && !show_history.get() fallback=|| ()>
          <KindToolbar canvas=canvas />
        </Show>
        <button
          type="button"
          class="btn canvas-history-button"
          aria-pressed=move || show_history.get().to_string()
          on:click=toggle_history
        >
          {move || i18n.t(if show_history.get() { "canvas-history-close" } else { "canvas-history" })}
        </button>
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
      BlockKind::Paragraph | BlockKind::Heading => ().into_any(),
    };
    view! {
      <div class=format!("canvas-block canvas-block-{}", block.kind.tag())>
        {marker}
        <div class="canvas-input canvas-preview-text">{block.text.clone()}</div>
      </div>
    }
    .into_any()
  })
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

/// The kinds the block with the caret can take.
#[component]
fn KindToolbar(canvas: Canvas) -> impl IntoView {
  let i18n = use_i18n();
  let current_kind = Memo::new(move |_| {
    let maybe_id = canvas.current.get();
    canvas.blocks.with(|blocks| {
      maybe_id.and_then(|id| blocks.iter().find(|block| block.id == id).map(|block| block.kind))
    })
  });
  view! {
    <div class="canvas-toolbar segmented" role="toolbar" aria-label=move || i18n.t("canvas-toolbar")>
      <SlidingIndicator />
      {BlockKind::ALL
        .into_iter()
        .map(|kind| {
          view! {
            <button
              type="button"
              aria-pressed=move || (current_kind.get() == Some(kind)).to_string()
              disabled=move || current_kind.get().is_none()
              // Keeps the caret in the block the button changes.
              on:mousedown=|ev| ev.prevent_default()
              on:click=move |_| canvas.set_kind(kind)
            >
              {move || i18n.t(kind_label(kind))}
            </button>
          }
        })
        .collect_view()}
    </div>
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
  let (input_id, check_id, focus_id) = (id.clone(), id.clone(), id.clone());
  let keydown_id = id.clone();
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
        BlockKind::Paragraph | BlockKind::Heading => ().into_any(),
      }}
      <textarea
        id=block_element_id(&id)
        class="canvas-input"
        rows="1"
        aria-label=move || i18n.t(kind_label(kind()))
        prop:value=move || row.get().map(|(block, _)| block.text).unwrap_or_default()
        readonly=read_only
        on:input=move |ev| canvas.on_input(&input_id, &event_target::<web_sys::HtmlTextAreaElement>(&ev))
        on:keydown=move |ev| canvas.on_keydown(&keydown_id, &ev)
        on:focus=move |_| canvas.current.set(Some(focus_id.clone()))
      ></textarea>
    </div>
  }
}

impl Canvas {
  /// Shows the page's blocks; the text field with the caret gets its new text right away, with
  /// the caret moved along with the text around it.
  fn show(self, next: Vec<Block>) {
    if let Some(area) = focused_block_field() {
      let id = area.id();
      let id = id.trim_start_matches(BLOCK_ID_PREFIX);
      if let Some(block) = next.iter().find(|block| block.id == id) {
        let old = area.value();
        if old != block.text {
          let start = shift_caret(
            &old,
            &block.text,
            area.selection_start().ok().flatten().unwrap_or(0),
          );
          let end = shift_caret(
            &old,
            &block.text,
            area.selection_end().ok().flatten().unwrap_or(0),
          );
          area.set_value(&block.text);
          drop(area.set_selection_range(start, end));
        }
      }
    }
    self.blocks.set(next);
    request_animation_frame(fit_all_heights);
  }

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
      self.schedule_flush();
    }
  }

  fn on_input(self, id: &str, area: &web_sys::HtmlTextAreaElement) {
    fit_height(area);
    let value = area.value();
    let kind = self.kind_of(id);
    // "# ", "- ", "1. " or "[] " typed at the start of a paragraph choose its kind.
    if kind == Some(BlockKind::Paragraph)
      && let Some((new_kind, prefix_len)) = BlockKind::from_prefix(&value)
    {
      let rest = value[prefix_len..].to_owned();
      let is_checked = value.starts_with("[x] ");
      let caret = area.selection_start().ok().flatten().unwrap_or(0);
      area.set_value(&rest);
      let caret = caret.saturating_sub(u32::try_from(prefix_len).unwrap_or(0));
      drop(area.set_selection_range(caret, caret));
      let id = id.to_owned();
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
    let id = id.to_owned();
    self.edit(|page| page.set_text(&id, &value).into_iter().collect());
  }

  fn on_keydown(self, id: &str, ev: &web_sys::KeyboardEvent) {
    if ev.is_composing() || !self.can_edit.get_untracked() {
      return;
    }
    let area = event_target::<web_sys::HtmlTextAreaElement>(ev);
    let start = area.selection_start().ok().flatten().unwrap_or(0);
    let end = area.selection_end().ok().flatten().unwrap_or(0);
    let text = area.value();
    let length = utf16_len(&text);
    match ev.key().as_str() {
      "Enter" if !ev.shift_key() => {
        ev.prevent_default();
        self.split(id, &text, start, end);
      },
      "Backspace" if start == 0 && end == 0 => {
        if self.join_with_previous(id, &text) {
          ev.prevent_default();
        }
      },
      "ArrowUp" if start == 0 && end == 0 => self.focus_neighbour(id, -1),
      "ArrowDown" if start == length && end == length => self.focus_neighbour(id, 1),
      _ => {},
    }
  }

  /// Enter: the text after the caret becomes a new block of the same list (a paragraph after
  /// anything else); Enter in an empty list item ends the list.
  fn split(self, id: &str, text: &str, start: u32, end: u32) {
    let Some(kind) = self.kind_of(id) else {
      return;
    };
    let id = id.to_owned();
    if kind.is_list() && text.is_empty() {
      self.edit(|page| page.set_kind(&id, BlockKind::Paragraph).into_iter().collect());
      return;
    }
    let before = text[..byte_at(text, start)].to_owned();
    let after = text[byte_at(text, end)..].to_owned();
    let next_kind = if kind.is_list() {
      kind
    } else {
      BlockKind::Paragraph
    };
    let mut maybe_new = None;
    self.edit(|page| {
      let (new_id, inserted) = page.insert_block(Some(&id), next_kind, &after);
      maybe_new = Some(new_id);
      page
        .set_text(&id, &before)
        .into_iter()
        .chain([inserted])
        .collect()
    });
    if let Some(new_id) = maybe_new {
      focus_block_later(new_id, 0);
    }
  }

  /// Backspace at the start: a list item or heading becomes a paragraph first, a paragraph joins
  /// the block above. Returns whether it did something.
  fn join_with_previous(self, id: &str, text: &str) -> bool {
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
        let joined = format!("{}{text}", previous.text);
        self.edit(|page| {
          page
            .set_text(&previous.id, &joined)
            .into_iter()
            .chain(page.remove_block(&id))
            .collect()
        });
        focus_block_later(previous.id, caret);
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
      focus_block_later(target, if step < 0 { length } else { 0 });
    }
  }

  /// The toolbar changes the kind of the block with the caret.
  fn set_kind(self, kind: BlockKind) {
    if let Some(id) = self.current.get_untracked() {
      self.edit(|page| page.set_kind(&id, kind).into_iter().collect());
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
      focus_block_later(id, caret);
    }
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

fn kind_label(kind: BlockKind) -> &'static str {
  match kind {
    BlockKind::Paragraph => "canvas-kind-paragraph",
    BlockKind::Heading => "canvas-kind-heading",
    BlockKind::Bullet => "canvas-kind-bullet",
    BlockKind::Numbered => "canvas-kind-numbered",
    BlockKind::Check => "canvas-kind-check",
  }
}

fn block_element_id(id: &str) -> String {
  format!("{BLOCK_ID_PREFIX}{id}")
}

/// The block text field with the caret, if any.
fn focused_block_field() -> Option<web_sys::HtmlTextAreaElement> {
  let active = web_sys::window()?.document()?.active_element()?;
  let area = active.dyn_into::<web_sys::HtmlTextAreaElement>().ok()?;
  area.id().starts_with(BLOCK_ID_PREFIX).then_some(area)
}

/// Puts the caret into a block once it is rendered.
fn focus_block_later(id: String, caret: u32) {
  request_animation_frame(move || {
    let maybe_area = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(&block_element_id(&id)))
      .and_then(|element| element.dyn_into::<web_sys::HtmlTextAreaElement>().ok());
    if let Some(area) = maybe_area {
      drop(area.focus());
      drop(area.set_selection_range(caret, caret));
    }
  });
}

/// Grows a text field with its text (browsers without `field-sizing`).
fn fit_height(area: &web_sys::HtmlTextAreaElement) {
  let style = web_sys::HtmlElement::style(area);
  drop(style.set_property("height", "auto"));
  drop(style.set_property("height", &format!("{}px", area.scroll_height())));
}

fn fit_all_heights() {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let Ok(fields) = document.query_selector_all(".canvas-page .canvas-input") else {
    return;
  };
  for index in 0..fields.length() {
    if let Some(area) = fields
      .item(index)
      .and_then(|node| node.dyn_into::<web_sys::HtmlTextAreaElement>().ok())
    {
      fit_height(&area);
    }
  }
}

fn utf16_len(text: &str) -> u32 {
  u32::try_from(text.encode_utf16().count()).unwrap_or(u32::MAX)
}

/// The byte index of a caret given in UTF-16 code units (as text fields count), on a character
/// boundary.
fn byte_at(text: &str, units: u32) -> usize {
  let mut counted = 0_u32;
  for (index, character) in text.char_indices() {
    if counted >= units {
      return index;
    }
    counted += u32::try_from(character.len_utf16()).unwrap_or(2);
  }
  text.len()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn carets_in_utf16_units_land_on_character_boundaries() {
    assert_eq!(byte_at("abc", 1), 1);
    assert_eq!(byte_at("Grüße", 2), 2);
    assert_eq!(byte_at("Grüße", 3), 4);
    assert_eq!(byte_at("Grüße", 4), 6);
    // An emoji is two UTF-16 units and four bytes.
    assert_eq!(byte_at("😀x", 2), 4);
    assert_eq!(byte_at("ab", 9), 2);
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
