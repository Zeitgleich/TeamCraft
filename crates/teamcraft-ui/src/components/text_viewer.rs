//! The text viewer of file previews (#821): plain text, logs and data files in monospace with line
//! numbers, wrapping, find, go to line and copy; logs with their levels and times highlighted and a
//! filter by level; JSON formatted on demand; terminal colours drawn or left out.
//!
//! The file is untrusted and only ever shown as text (`text_preview`): no markup, no links, escape
//! sequences reduced to colours. It is decrypted in memory like the other previews and prepared in
//! steps, with the browser getting a turn in between, so a large file never freezes the app. Only
//! the rows in view are in the page (`text_preview::layout`). The viewer is part of the preview's
//! chunk (`areas::file_preview`), so a first visit doesn't load it.

use std::rc::Rc;
use std::time::Duration;

use js_sys::Uint8Array;
use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_text;
use crate::i18n::I18n;
use crate::model::LogLevel;
use crate::preview::TextFormat;
use crate::reactive::SetIfAlive;
use crate::state::AppState;
use crate::text_preview::ansi::{AnsiStyle, Attributes};
use crate::text_preview::decode::{self, TextDecodeError, TextEncoding};
use crate::text_preview::json::{self, JsonFormatError};
use crate::text_preview::layout::{self, Deco, Heights, Mark, Piece, Row, ScrollMap};
use crate::text_preview::log::{entry_levels, level_of, looks_like_log, timestamp_of};
use crate::text_preview::search::{self, Found};
use crate::text_preview::{MAX_LINES, Span, TextDoc, ansi, index_lines};

/// Lines indexed (and levels read) per step before the browser gets a turn.
const LINES_PER_STEP: usize = 100_000;
/// Screen rows drawn above and below the viewport, so scrolling shows no blank rows.
const OVERSCAN_ROWS: u32 = 24;
/// From this size a query applies after a short pause instead of at every key: finding in a large
/// file takes a few milliseconds.
const DEBOUNCE_FROM_BYTES: usize = 1024 * 1024;
const FIND_PAUSE: Duration = Duration::from_millis(150);
/// The longest query; a find field is no place for a paragraph.
const MAX_QUERY_CHARS: usize = 200;
/// Columns that are not text in a row, besides the line number's digits: the gutter's padding
/// around the number ([`GUTTER_PADDING_COLUMNS`]) and the padding at the end (the stylesheet's `.tv-row`).
const ROW_CHROME_COLUMNS: f64 = GUTTER_PADDING_COLUMNS + 1.0;
/// The gutter's padding around the line number, in `ch` (the stylesheet's `.tv-row::before`).
const GUTTER_PADDING_COLUMNS: f64 = 2.5;

/// What the viewer shows.
#[derive(Clone)]
enum Phase {
  Preparing,
  NotText,
  Ready(Rc<Prepared>),
}

/// A file's text, ready to show, and how it was read.
struct Prepared {
  doc: Rc<TextDoc>,
  encoding: TextEncoding,
  format: TextFormat,
}

/// The shown document's rows and their widths.
#[derive(Default)]
struct Layout {
  rows: Vec<Row>,
  columns: Vec<u32>,
  max_columns: u32,
  /// Some line is longer than a row and shown in parts.
  has_split_lines: bool,
}

/// Sizes measured in the page: a text row, a character, and the scroller.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
  row_px: f64,
  char_px: f64,
  viewport_px: f64,
  width_px: f64,
}

/// Where formatting JSON stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JsonView {
  Original,
  Formatting,
  Formatted,
  /// Not formatted: invalid, too deep or too large.
  Refused(JsonFormatError),
}

/// What the drawn rows depend on besides their index: when it changes, they are drawn anew.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Stamp {
  doc: usize,
  layout: usize,
  found: usize,
  maybe_current: Option<usize>,
  has_colors: bool,
  maybe_target: Option<u32>,
}

/// The viewer of a text file: prepares `bytes` in steps, then shows them, or says the file is no
/// text with the download next to it.
pub fn text_viewer(
  state: AppState,
  i18n: I18n,
  bytes: Uint8Array,
  format: TextFormat,
  name: String,
  on_download: Callback<()>,
) -> AnyView {
  let phase: RwSignal<Phase, LocalStorage> = RwSignal::new_local(Phase::Preparing);
  spawn_local(async move {
    let is_alive = move || phase.try_with_untracked(|_| ()).is_some();
    match prepare(bytes.to_vec(), format, is_alive).await {
      Some(Ok(prepared)) => phase.set_if_alive(Phase::Ready(Rc::new(prepared))),
      Some(Err(TextDecodeError::Binary)) => phase.set_if_alive(Phase::NotText),
      // Closed while preparing.
      None => {},
    }
  });
  let name = StoredValue::new(name);
  view! {
    {move || match phase.get() {
      Phase::Preparing => view! {
        <div class="preview-status" role="status" aria-busy="true">
          <span class="spinner" aria-hidden="true"></span>
          <p>{move || i18n.t("text-preview-preparing")}</p>
        </div>
      }
      .into_any(),
      Phase::NotText => view! {
        <div class="preview-status" role="alert">
          <p>{move || i18n.t("text-preview-binary")}</p>
          <button class="btn btn-primary" on:click=move |_| on_download.run(())>{move || i18n.t("file-download")}</button>
        </div>
      }
      .into_any(),
      Phase::Ready(prepared) => text_pane(state, i18n, &prepared, name.get_value()),
    }}
  }
  .into_any()
}

/// Reads, strips and indexes the file in steps; `None` when the viewer went away meanwhile.
async fn prepare(
  bytes: Vec<u8>,
  format: TextFormat,
  is_alive: impl Fn() -> bool,
) -> Option<Result<Prepared, TextDecodeError>> {
  let decoded = match decode::decode(bytes) {
    Ok(decoded) => decoded,
    Err(error) => return Some(Err(error)),
  };
  let encoding = decoded.encoding;
  yield_to_browser().await;
  if !is_alive() {
    return None;
  }
  let (text, runs) = if ansi::has_escapes(&decoded.text) {
    let stripped = ansi::strip(&decoded.text);
    (stripped.text, stripped.runs)
  } else {
    (decoded.text, Vec::new())
  };
  let mut lines = Vec::new();
  let mut maybe_from = Some(0);
  let mut is_truncated = false;
  while let Some(from) = maybe_from {
    let room = MAX_LINES.saturating_sub(lines.len());
    if room == 0 {
      is_truncated = true;
      break;
    }
    maybe_from = index_lines(&text, from, LINES_PER_STEP.min(room), &mut lines);
    yield_to_browser().await;
    if !is_alive() {
      return None;
    }
  }
  let is_log = match format {
    TextFormat::Log => true,
    TextFormat::Plain => looks_like_log(&text, &lines),
    TextFormat::Json => false,
  };
  let mut maybe_levels = None;
  if is_log {
    let mut levels: Vec<LogLevel> = Vec::with_capacity(lines.len());
    for step in lines.chunks(LINES_PER_STEP) {
      let previous = levels.last().copied().unwrap_or(LogLevel::Info);
      levels.extend(entry_levels(&text, step, previous));
      yield_to_browser().await;
      if !is_alive() {
        return None;
      }
    }
    maybe_levels = Some(levels);
  }
  Some(Ok(Prepared {
    doc: Rc::new(TextDoc {
      text,
      lines,
      runs,
      maybe_levels,
      is_truncated,
    }),
    encoding,
    format,
  }))
}

/// Lets the browser paint and handle input before the next step.
async fn yield_to_browser() {
  let timer = js_sys::Promise::new(&mut |resolve, _reject| {
    if let Some(window) = web_sys::window() {
      drop(window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 0));
    }
  });
  // A timer that couldn't be set resolves never; one that failed has nothing to wait for.
  drop(JsFuture::from(timer).await);
}

/// The toolbar, the notes and the rows of a prepared file.
fn text_pane(state: AppState, i18n: I18n, prepared: &Prepared, name: String) -> AnyView {
  let original = Rc::clone(&prepared.doc);
  let is_log = original.is_log();
  let has_ansi = !original.runs.is_empty();
  let is_json = prepared.format == TextFormat::Json;
  let encoding = prepared.encoding;
  let shown: RwSignal<Rc<TextDoc>, LocalStorage> = RwSignal::new_local(Rc::clone(&original));
  let original = StoredValue::new_local(original);
  let formatted: StoredValue<Option<Rc<TextDoc>>, LocalStorage> = StoredValue::new_local(None);
  let json_view = RwSignal::new(JsonView::Original);
  let min_level = RwSignal::new(LogLevel::Debug);
  // Prose reads better wrapped; logs and data keep their columns.
  let is_wrapped = RwSignal::new(!is_log && !is_json);
  let has_colors = RwSignal::new(true);
  let metrics = RwSignal::new(Metrics::default());
  let scroll_top = RwSignal::new(0.0f64);
  let query = RwSignal::new(String::new());
  let maybe_current = RwSignal::new(None::<usize>);
  let maybe_target = RwSignal::new(None::<u32>);
  let pending_query: StoredValue<Option<TimeoutHandle>, LocalStorage> = StoredValue::new_local(None);
  let scroller = NodeRef::<html::Div>::new();
  let probe = NodeRef::<html::Span>::new();
  let find_input = NodeRef::<html::Input>::new();

  let layout: RwSignal<Rc<Layout>, LocalStorage> = RwSignal::new_local(Rc::new(
    shown.with_untracked(|doc| Layout::of(doc, LogLevel::Debug)),
  ));
  let found: RwSignal<Rc<Found>, LocalStorage> = RwSignal::new_local(Rc::new(Found::default()));
  let heights: RwSignal<Rc<Heights>, LocalStorage> = RwSignal::new_local(Rc::new(Heights::Single(0)));
  let digits = Memo::new(move |_| shown.with(|doc| digit_count(doc.lines.len())));
  let wrap_columns = Memo::new(move |_| {
    let sizes = metrics.get();
    (is_wrapped.get() && sizes.char_px > 0.0).then(|| {
      let columns = sizes.width_px / sizes.char_px - f64::from(digits.get()) - ROW_CHROME_COLUMNS;
      // Saturating: a measured width is far below u32::MAX columns.
      columns.floor().max(1.0) as u32
    })
  });

  Effect::watch(
    move || (shown.get(), min_level.get()),
    move |(doc, level), _, _| layout.set(Rc::new(Layout::of(doc, *level))),
    false,
  );
  Effect::watch(
    move || (layout.get(), wrap_columns.get()),
    move |(rows, maybe_wrap), _, _| heights.set(Rc::new(Heights::new(&rows.columns, *maybe_wrap))),
    true,
  );
  Effect::watch(
    move || (layout.get(), query.get()),
    move |(rows, query), _, _| {
      let result = shown.with_untracked(|doc| find_in(doc, rows, query));
      found.set(Rc::new(result));
      maybe_current.set(None);
    },
    false,
  );

  let window_now = Memo::new(move |_| {
    let sizes = metrics.get();
    heights.with(|heights| {
      let map = ScrollMap::new(heights, sizes.row_px, sizes.viewport_px);
      layout::window(heights, &map, sizes.row_px, scroll_top.get(), OVERSCAN_ROWS)
    })
  });
  let canvas_px = move || {
    let sizes = metrics.get();
    heights.with(|heights| ScrollMap::new(heights, sizes.row_px, sizes.viewport_px).canvas_px)
  };

  observe_size(scroller, probe, metrics);

  let view_state = ViewState {
    shown,
    layout,
    heights,
    found,
    metrics,
    maybe_current,
    maybe_target,
    scroller,
  };

  listen_for_find_keys(find_input, view_state);
  let cancel_pending_query = move || {
    pending_query.update_value(|maybe_pending| {
      if let Some(pending) = maybe_pending.take() {
        pending.clear();
      }
    });
  };
  on_cleanup(cancel_pending_query);

  let on_query_input = move |ev: web_sys::Event| {
    let value: String = event_target_value(&ev).chars().take(MAX_QUERY_CHARS).collect();
    cancel_pending_query();
    if shown.with_untracked(|doc| doc.text.len()) < DEBOUNCE_FROM_BYTES {
      query.set(value);
      return;
    }
    let apply = move || {
      let _ = query.try_set(value);
    };
    match set_timeout_with_handle(apply, FIND_PAUSE) {
      Ok(handle) => pending_query.set_value(Some(handle)),
      Err(_) => tracing::debug!("no timer for the find field"),
    }
  };
  // The first match from the top of the view on, once a query found something.
  Effect::watch(
    move || found.get(),
    move |result, _, _| {
      if result.matches.is_empty() {
        return;
      }
      let first_row = window_now.get_untracked().rows.start;
      let start =
        layout.with_untracked(|rows| rows.rows.get(first_row).map(|row| (row.line, row.span.start)));
      let index = start
        .and_then(|from| {
          result
            .matches
            .iter()
            .position(|found| (found.line, found.span.start) >= from)
        })
        .unwrap_or(0);
      view_state.show_match(index);
    },
    false,
  );
  let on_find_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Enter" => {
      ev.prevent_default();
      view_state.step(!ev.shift_key());
    },
    "Escape" if !query.get_untracked().is_empty() => {
      // Clears the field first; a second Escape closes the preview.
      ev.stop_propagation();
      cancel_pending_query();
      if let Some(field) = find_input.get_untracked() {
        field.set_value("");
      }
      query.set(String::new());
    },
    _ => {},
  };

  let on_go_to_line = move |ev: web_sys::Event| {
    if let Ok(number) = event_target_value(&ev).trim().parse::<u32>() {
      view_state.go_to_line(number.saturating_sub(1));
    }
  };
  let on_level = move |ev: web_sys::Event| {
    let level = LogLevel::from_slug(&event_target_value(&ev)).unwrap_or(LogLevel::Debug);
    min_level.set(level);
    maybe_target.set(None);
    view_state.scroll_to_top();
  };
  let toggle_wrap = move |_| {
    let anchor = view_state.top_row();
    is_wrapped.update(|on| *on = !*on);
    // Keeps the row at the top in place once the heights follow (an animation frame later).
    request_animation_frame(move || view_state.scroll_to_row_top(anchor));
  };
  let toggle_json = move |_| match json_view.get_untracked() {
    JsonView::Formatted => {
      json_view.set(JsonView::Original);
      shown.set(original.get_value());
      view_state.scroll_to_top();
    },
    JsonView::Formatting => {},
    JsonView::Original | JsonView::Refused(_) => {
      if let Some(doc) = formatted.get_value() {
        json_view.set(JsonView::Formatted);
        shown.set(doc);
        view_state.scroll_to_top();
        return;
      }
      json_view.set(JsonView::Formatting);
      spawn_local(async move {
        yield_to_browser().await;
        let Some(result) = original.try_with_value(|doc| json::pretty(&doc.text)) else {
          return;
        };
        match result {
          Ok(text) => {
            let doc = Rc::new(TextDoc::plain(text));
            let _ = formatted.try_set_value(Some(Rc::clone(&doc)));
            shown.set_if_alive(doc);
            json_view.set_if_alive(JsonView::Formatted);
            view_state.try_scroll_to_top();
          },
          Err(error) => json_view.set_if_alive(JsonView::Refused(error)),
        }
      });
    },
  };
  let copy = move |_| {
    let text = selected_text(scroller).unwrap_or_else(|| shown_text(shown, layout));
    let copying = copy_text(&text);
    let (copied, failed) = (i18n.t("action-copied"), i18n.t("action-copy-failed"));
    spawn_local(async move {
      // `show_error` does nothing once the shell is gone (issue #322); the toast checks first.
      if copying.await {
        if state.is_alive() {
          state.show_toast(copied);
        }
      } else {
        state.show_error(failed, String::new());
      }
    });
  };

  let find_status = move || {
    let total = found.with(|result| (result.matches.len(), result.is_capped));
    match (query.with(String::is_empty), total, maybe_current.get()) {
      (true, _, _) => String::new(),
      (false, (0, _), _) => i18n.t("text-preview-find-none"),
      (false, (count, is_capped), maybe_index) => {
        let total = if is_capped {
          format!("{count}+")
        } else {
          count.to_string()
        };
        let current = maybe_index.map_or(0, |index| index + 1).to_string();
        i18n.t_with(
          "text-preview-find-count",
          &[("current", &current), ("total", &total)],
        )
      },
    }
  };
  let has_matches = move || found.with(|result| !result.matches.is_empty());
  let line_count = move || shown.with(|doc| doc.lines.len());
  let notes = move || {
    let mut notes: Vec<String> = Vec::new();
    match encoding {
      TextEncoding::Utf8 => {},
      TextEncoding::Utf8Repaired => notes.push(i18n.t("text-preview-encoding-repaired")),
      TextEncoding::Utf16 => notes.push(i18n.t("text-preview-encoding-utf16")),
      TextEncoding::Latin1 => notes.push(i18n.t("text-preview-encoding-latin1")),
    }
    match json_view.get() {
      JsonView::Refused(JsonFormatError::Invalid { line, column }) => notes.push(i18n.t_with(
        "text-preview-json-invalid",
        &[("line", &line.to_string()), ("column", &column.to_string())],
      )),
      JsonView::Refused(JsonFormatError::TooDeep) => notes.push(i18n.t("text-preview-json-too-deep")),
      JsonView::Refused(JsonFormatError::TooLarge) => notes.push(i18n.t("text-preview-json-too-large")),
      JsonView::Original | JsonView::Formatting | JsonView::Formatted => {},
    }
    if min_level.get() > LogLevel::Debug {
      let shown_lines = layout.with(|rows| rows.rows.iter().filter(|row| row.is_line_start).count());
      notes.push(i18n.t_with(
        "text-preview-filtered",
        &[
          ("shown", &shown_lines.to_string()),
          ("total", &line_count().to_string()),
        ],
      ));
    }
    if shown.with(|doc| doc.is_truncated) {
      notes.push(i18n.t_with("text-preview-truncated", &[("count", &MAX_LINES.to_string())]));
    }
    if layout.with(|rows| rows.has_split_lines) {
      notes.push(i18n.t("text-preview-long-lines"));
    }
    notes
  };
  let label = i18n.t_with("text-preview-content", &[("name", &name)]);
  let level_option = move |level: LogLevel, key: &'static str| {
    view! { <option value=level.slug() selected=move || min_level.get() == level>{move || i18n.t(key)}</option> }
  };

  view! {
    <div class="tv">
      <div class="tv-tools" role="toolbar" aria-label=move || i18n.t("text-preview-tools")>
        <div class="tv-find" role="search">
          <Icon name=IconName::Search class="tv-find-icon" />
          <input
            class="input tv-find-input"
            type="search"
            node_ref=find_input
            maxlength=MAX_QUERY_CHARS.to_string()
            autocomplete="off"
            spellcheck="false"
            placeholder=move || i18n.t("text-preview-find")
            aria-label=move || i18n.t("text-preview-find")
            on:input=on_query_input
            on:keydown=on_find_keydown
          />
          <span class="tv-find-count" role="status">{find_status}</span>
          <button
            type="button"
            class="icon-btn"
            aria-label=move || i18n.t("text-preview-find-previous")
            data-tip=move || i18n.t("text-preview-find-previous")
            prop:disabled=move || !has_matches()
            on:click=move |_| view_state.step(false)
          >
            <Icon name=IconName::ChevronDown class="is-flipped" />
          </button>
          <button
            type="button"
            class="icon-btn"
            aria-label=move || i18n.t("text-preview-find-next")
            data-tip=move || i18n.t("text-preview-find-next")
            prop:disabled=move || !has_matches()
            on:click=move |_| view_state.step(true)
          >
            <Icon name=IconName::ChevronDown />
          </button>
        </div>
        <label class="tv-go-to">
          <span class="visually-hidden">{move || i18n.t("text-preview-go-to-line")}</span>
          <input
            class="input tv-go-to-input"
            type="number"
            inputmode="numeric"
            min="1"
            prop:max=move || line_count().to_string()
            placeholder=move || i18n.t("text-preview-line")
            on:change=on_go_to_line
          />
          <span class="tv-line-count" aria-hidden="true">{move || format!("/ {}", line_count())}</span>
        </label>
        {is_log.then(|| view! {
          <select class="input tv-level" aria-label=move || i18n.t("text-preview-level") on:change=on_level>
            {level_option(LogLevel::Debug, "text-preview-level-all")}
            {level_option(LogLevel::Info, "text-preview-level-info")}
            {level_option(LogLevel::Warn, "text-preview-level-warn")}
            {level_option(LogLevel::Error, "text-preview-level-error")}
          </select>
        })}
        <div class="tv-toggles">
          <button class="btn btn-small" aria-pressed=move || is_wrapped.get().to_string() on:click=toggle_wrap>
            {move || i18n.t("text-preview-wrap")}
          </button>
          {has_ansi.then(|| view! {
            <button class="btn btn-small" aria-pressed=move || has_colors.get().to_string() on:click=move |_| has_colors.update(|on| *on = !*on)>
              {move || i18n.t("text-preview-colors")}
            </button>
          })}
          {is_json.then(|| view! {
            <button
              class="btn btn-small"
              aria-pressed=move || (json_view.get() == JsonView::Formatted).to_string()
              aria-busy=move || (json_view.get() == JsonView::Formatting).to_string()
              on:click=toggle_json
            >
              {move || i18n.t("text-preview-format-json")}
            </button>
          })}
          <button class="icon-btn" aria-label=move || i18n.t("text-preview-copy") data-tip=move || i18n.t("text-preview-copy") on:click=copy>
            <Icon name=IconName::Copy />
          </button>
        </div>
      </div>
      {move || {
        let notes = notes();
        (!notes.is_empty()).then(|| view! {
          <p class="preview-note tv-notes" role="status">{notes.join(" ")}</p>
        })
      }}
      <div
        class="tv-scroller"
        class:is-wrapped=move || is_wrapped.get()
        node_ref=scroller
        tabindex="0"
        role="document"
        aria-label=label
        translate="no"
        on:scroll=move |ev| scroll_top.set(f64::from(event_target::<web_sys::HtmlElement>(&ev).scroll_top()))
      >
        <div
          class="tv-canvas"
          style:height=move || format!("{}px", canvas_px())
          style:--tv-digits=move || digits.get().to_string()
          style:--tv-columns=move || layout.with(|rows| rows.max_columns).to_string()
        >
          <div class="tv-window" style:transform=move || format!("translateY({}px)", window_now.with(|shown| shown.top_px))>
            <For
              each=move || {
                let stamp = Stamp {
                  doc: shown.with(|doc| Rc::as_ptr(doc) as usize),
                  layout: layout.with(|rows| Rc::as_ptr(rows) as usize),
                  found: found.with(|result| Rc::as_ptr(result) as usize),
                  maybe_current: maybe_current.get(),
                  has_colors: has_colors.get(),
                  maybe_target: maybe_target.get(),
                };
                window_now.with(|shown| shown.rows.clone().map(|index| (stamp, index)).collect::<Vec<_>>())
              }
              key=|item| *item
              children=move |(stamp, index)| row_view(view_state, stamp, index)
            />
          </div>
          <span class="tv-probe" node_ref=probe aria-hidden="true">{"0".repeat(PROBE_CHARS)}</span>
        </div>
        {move || (line_count() == 0).then(|| view! { <p class="tv-empty">{move || i18n.t("text-preview-empty")}</p> })}
      </div>
    </div>
  }
  .into_any()
}

/// Characters in the probe that measures a character's width.
const PROBE_CHARS: usize = 32;

/// The signals the viewer's actions work on.
#[derive(Clone, Copy)]
struct ViewState {
  shown: RwSignal<Rc<TextDoc>, LocalStorage>,
  layout: RwSignal<Rc<Layout>, LocalStorage>,
  heights: RwSignal<Rc<Heights>, LocalStorage>,
  found: RwSignal<Rc<Found>, LocalStorage>,
  metrics: RwSignal<Metrics>,
  maybe_current: RwSignal<Option<usize>>,
  maybe_target: RwSignal<Option<u32>>,
  scroller: NodeRef<html::Div>,
}

impl ViewState {
  /// The next (or previous) match, around the ends.
  fn step(self, is_forward: bool) {
    let count = self.found.with_untracked(|result| result.matches.len());
    if count == 0 {
      return;
    }
    let next = match (self.maybe_current.get_untracked(), is_forward) {
      (None, true) => 0,
      (None, false) => count - 1,
      (Some(index), true) => (index + 1) % count,
      (Some(index), false) => (index + count - 1) % count,
    };
    self.show_match(next);
  }

  /// Makes match `index` the current one and scrolls it into view.
  fn show_match(self, index: usize) {
    let Some(found) = self
      .found
      .with_untracked(|result| result.matches.get(index).copied())
    else {
      return;
    };
    self.maybe_current.set(Some(index));
    let maybe_place = self.shown.with_untracked(|doc| {
      self.layout.with_untracked(|rows| {
        layout::row_of_match(&rows.rows, &found).map(|row| {
          let start = rows.rows[row].span.start as usize;
          let column = doc
            .text
            .get(start..found.span.start as usize)
            .map_or(0, layout::columns_of);
          (row, column)
        })
      })
    });
    if let Some((row, column)) = maybe_place {
      self.reveal(row, column);
    }
  }

  /// Scrolls to the first shown line from `line` on and marks it.
  fn go_to_line(self, line: u32) {
    let maybe_row = self.layout.with_untracked(|rows| {
      layout::row_of_line(&rows.rows, line)
        .or_else(|| rows.rows.len().checked_sub(1))
        .map(|row| (row, rows.rows[row].line))
    });
    if let Some((row, shown_line)) = maybe_row {
      self.maybe_target.set(Some(shown_line));
      self.reveal(row, 0);
    }
  }

  /// Brings `column` of `row` into view: vertically only when it is not in view already (centred
  /// then), horizontally when lines don't wrap.
  fn reveal(self, row: usize, column: u32) {
    let Some(element) = self.scroller.get_untracked() else {
      return;
    };
    let sizes = self.metrics.get_untracked();
    if sizes.row_px <= 0.0 {
      return;
    }
    let maybe_wrap = self
      .heights
      .with_untracked(|heights| matches!(**heights, Heights::Wrapped(_)));
    let wrap_columns = self.wrap_columns(sizes).filter(|_| maybe_wrap);
    self.heights.with_untracked(|heights| {
      let map = ScrollMap::new(heights, sizes.row_px, sizes.viewport_px);
      let extra_rows = wrap_columns.map_or(0, |wrap| column / wrap.max(1));
      let target_px = f64::from(heights.start_of(row).saturating_add(extra_rows)) * sizes.row_px;
      let content_top = map.content_top(f64::from(element.scroll_top()));
      let is_in_view = target_px >= content_top + sizes.row_px
        && target_px + 2.0 * sizes.row_px <= content_top + sizes.viewport_px;
      if !is_in_view {
        let centred = (target_px - (sizes.viewport_px - sizes.row_px) / 2.0).max(0.0);
        element.set_scroll_top(px(map.scroll_top_for(centred)));
      }
    });
    if wrap_columns.is_none() {
      let gutter_px = (f64::from(digit_count(self.line_count())) + GUTTER_PADDING_COLUMNS) * sizes.char_px;
      let x = f64::from(column) * sizes.char_px;
      let left = f64::from(element.scroll_left());
      let visible = sizes.width_px - gutter_px;
      if x < left || x + 4.0 * sizes.char_px > left + visible {
        element.set_scroll_left(px((x - visible / 3.0).max(0.0)));
      }
    }
  }

  /// The row at the top of the view.
  fn top_row(self) -> usize {
    let sizes = self.metrics.get_untracked();
    let Some(element) = self.scroller.get_untracked() else {
      return 0;
    };
    self.heights.with_untracked(|heights| {
      let map = ScrollMap::new(heights, sizes.row_px, sizes.viewport_px);
      let content_top = map.content_top(f64::from(element.scroll_top()));
      let screen_row = if sizes.row_px > 0.0 {
        content_top / sizes.row_px
      } else {
        0.0
      };
      heights.row_at(screen_row.max(0.0) as u32)
    })
  }

  /// Puts `row` at the top of the view. Runs in an animation frame: the view may be gone.
  fn scroll_to_row_top(self, row: usize) {
    let (Some(Some(element)), Some(sizes)) = (
      self.scroller.try_get_untracked(),
      self.metrics.try_get_untracked(),
    ) else {
      return;
    };
    let _ = self.heights.try_with_untracked(|heights| {
      let map = ScrollMap::new(heights, sizes.row_px, sizes.viewport_px);
      let content_px = f64::from(heights.start_of(row)) * sizes.row_px;
      element.set_scroll_top(px(map.scroll_top_for(content_px)));
    });
  }

  fn scroll_to_top(self) {
    if let Some(element) = self.scroller.get_untracked() {
      element.set_scroll_top(0);
      element.set_scroll_left(0);
    }
  }

  /// [`Self::scroll_to_top`] after an `.await`, when the view may be gone.
  fn try_scroll_to_top(self) {
    if let Some(Some(element)) = self.scroller.try_get_untracked() {
      element.set_scroll_top(0);
      element.set_scroll_left(0);
    }
  }

  fn wrap_columns(self, sizes: Metrics) -> Option<u32> {
    (sizes.char_px > 0.0).then(|| {
      let digits = f64::from(digit_count(self.line_count()));
      (sizes.width_px / sizes.char_px - digits - ROW_CHROME_COLUMNS)
        .floor()
        .max(1.0) as u32
    })
  }

  fn line_count(self) -> usize {
    self.shown.with_untracked(|doc| doc.lines.len())
  }
}

impl Layout {
  fn of(doc: &TextDoc, min_level: LogLevel) -> Self {
    let rows = layout::rows(&doc.text, &doc.lines, doc.maybe_levels.as_deref(), min_level);
    let columns = layout::columns(&doc.text, &rows);
    let max_columns = columns.iter().copied().max().unwrap_or(0);
    let has_split_lines = rows.iter().any(|row| !row.is_line_start);
    Self {
      rows,
      columns,
      max_columns,
      has_split_lines,
    }
  }
}

/// One row: its line number (drawn by the stylesheet from `data-n`, so it is never selected or
/// copied with the text) and its pieces.
fn row_view(view_state: ViewState, stamp: Stamp, index: usize) -> AnyView {
  view_state.shown.with_untracked(|doc| {
    view_state.layout.with_untracked(|rows| {
      view_state.found.with_untracked(|result| {
        let Some(row) = rows.rows.get(index).copied() else {
          return ().into_any();
        };
        let decos = decorations(doc, result, row, stamp);
        let pieces = layout::pieces(row.span, &decos);
        let maybe_level = doc
          .maybe_levels
          .as_ref()
          .and_then(|levels| levels.get(row.line as usize))
          .map(|level| level.slug());
        let mut class = String::from("tv-row");
        if let Some(level) = maybe_level {
          class.push_str(" level-");
          class.push_str(level);
        }
        if stamp.maybe_target == Some(row.line) {
          class.push_str(" is-target");
        }
        let maybe_number = row.is_line_start.then(|| (row.line + 1).to_string());
        let content = pieces
          .into_iter()
          .map(|piece| piece_view(&doc.text, piece))
          .collect_view();
        view! { <div class=class data-n=maybe_number><span class="tv-text">{content}</span></div> }.into_any()
      })
    })
  })
}

/// The decorations over a row: colours, a log line's time and level, and matches.
fn decorations(doc: &TextDoc, result: &Found, row: Row, stamp: Stamp) -> Vec<Deco> {
  let mut decos = Vec::new();
  if stamp.has_colors {
    decos.extend(layout::runs_over(&doc.runs, row.span).map(|run| Deco {
      span: run.span,
      mark: Mark::Ansi(run.style),
    }));
  }
  if doc.is_log() && row.is_line_start {
    let line_start = doc.lines.get(row.line as usize).map_or(0, |span| span.start);
    let line = doc.line(row.line as usize);
    if let Some(at) = timestamp_of(line) {
      decos.push(Deco {
        span: Span::new(at.start, at.end).shifted(line_start),
        mark: Mark::Time,
      });
    }
    if let Some((_, at)) = level_of(line) {
      decos.push(Deco {
        span: Span::new(at.start, at.end).shifted(line_start),
        mark: Mark::Level,
      });
    }
  }
  decos.extend(
    layout::matches_over(&result.matches, row.line, row.span).map(|(index, found)| Deco {
      span: found.span,
      mark: if stamp.maybe_current == Some(index) {
        Mark::CurrentMatch
      } else {
        Mark::Match
      },
    }),
  );
  decos
}

fn piece_view(text: &str, piece: Piece) -> AnyView {
  let content = text.get(piece.span.range()).unwrap_or_default().to_owned();
  if piece.marks.is_empty() {
    return content.into_any();
  }
  let is_match = piece
    .marks
    .iter()
    .any(|mark| matches!(mark, Mark::Match | Mark::CurrentMatch));
  let class = piece
    .marks
    .iter()
    .map(|mark| mark_class(*mark))
    .collect::<Vec<_>>()
    .join(" ");
  if is_match {
    view! { <mark class=class>{content}</mark> }.into_any()
  } else {
    view! { <span class=class>{content}</span> }.into_any()
  }
}

fn mark_class(mark: Mark) -> String {
  match mark {
    Mark::Ansi(style) => ansi_class(style),
    Mark::Level => "tv-level".to_owned(),
    Mark::Time => "tv-time".to_owned(),
    Mark::Match => "tv-match".to_owned(),
    Mark::CurrentMatch => "tv-match is-current".to_owned(),
  }
}

/// The classes of a terminal style: one of the sixteen colours for text and background, swapped
/// for inverse, and the attributes.
fn ansi_class(style: AnsiStyle) -> String {
  let is_inverse = style.attributes.contains(Attributes::INVERSE);
  let (maybe_fg, maybe_bg) = if is_inverse {
    (style.maybe_bg, style.maybe_fg)
  } else {
    (style.maybe_fg, style.maybe_bg)
  };
  let mut classes = Vec::new();
  match (maybe_fg, is_inverse) {
    (Some(color), _) => classes.push(format!("ansi-fg-{color}")),
    (None, true) => classes.push("ansi-fg-inverse".to_owned()),
    (None, false) => {},
  }
  match (maybe_bg, is_inverse) {
    (Some(color), _) => classes.push(format!("ansi-bg-{color}")),
    (None, true) => classes.push("ansi-bg-inverse".to_owned()),
    (None, false) => {},
  }
  for (attribute, class) in [
    (Attributes::BOLD, "ansi-bold"),
    (Attributes::DIM, "ansi-dim"),
    (Attributes::ITALIC, "ansi-italic"),
    (Attributes::UNDERLINE, "ansi-underline"),
    (Attributes::STRIKE, "ansi-strike"),
  ] {
    if style.attributes.contains(attribute) {
      classes.push(class.to_owned());
    }
  }
  classes.join(" ")
}

/// The matches of `query` in the lines `rows` shows.
fn find_in(doc: &TextDoc, rows: &Layout, query: &str) -> Found {
  let lines = rows
    .rows
    .iter()
    .filter(|row| row.is_line_start)
    .filter_map(|row| doc.lines.get(row.line as usize).map(|span| (row.line, *span)));
  search::find_all(&doc.text, lines, query.trim(), search::MAX_MATCHES)
}

/// The text selected in the viewer, if any.
fn selected_text(scroller: NodeRef<html::Div>) -> Option<String> {
  let element = scroller.get_untracked()?;
  let selection = web_sys::window()?.get_selection().ok()??;
  let anchor = selection.anchor_node()?;
  if selection.is_collapsed() || !element.contains(Some(&anchor)) {
    return None;
  }
  Some(String::from(selection.to_string())).filter(|text| !text.is_empty())
}

/// The lines shown (all of them, or those the level filter keeps), one per line.
fn shown_text(
  shown: RwSignal<Rc<TextDoc>, LocalStorage>,
  layout: RwSignal<Rc<Layout>, LocalStorage>,
) -> String {
  shown.with_untracked(|doc| {
    layout.with_untracked(|rows| {
      if rows.rows.iter().filter(|row| row.is_line_start).count() == doc.lines.len() {
        return doc.text.clone();
      }
      rows
        .rows
        .iter()
        .filter(|row| row.is_line_start)
        .map(|row| doc.line(row.line as usize))
        .collect::<Vec<_>>()
        .join("\n")
    })
  })
}

/// While the viewer is open, Ctrl/Cmd+F finds in the file (the browser's own find sees only the
/// rows in the page) and Ctrl/Cmd+G or F3 go to the next match, with Shift to the previous. The
/// keys are taken on the window's capture phase, before the app's shortcuts see them (Ctrl/Cmd+F
/// opens the top bar's search elsewhere, which would close the preview).
fn listen_for_find_keys(find_input: NodeRef<html::Input>, view_state: ViewState) {
  let callback = Closure::<dyn Fn(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
    let is_command = (ev.ctrl_key() || ev.meta_key()) && !ev.alt_key();
    let key = ev.key();
    if is_command && !ev.shift_key() && key.eq_ignore_ascii_case("f") {
      ev.prevent_default();
      ev.stop_propagation();
      if let Some(Some(field)) = find_input.try_get_untracked() {
        drop(field.focus());
        field.select();
      }
    } else if key == "F3" || (is_command && key.eq_ignore_ascii_case("g")) {
      ev.prevent_default();
      ev.stop_propagation();
      view_state.step(!ev.shift_key());
    }
  });
  let Some(window) = web_sys::window() else {
    return;
  };
  let is_added = window
    .add_event_listener_with_callback_and_bool("keydown", callback.as_ref().unchecked_ref(), true)
    .is_ok();
  if !is_added {
    tracing::debug!("the find keys could not be taken");
    return;
  }
  let handle = StoredValue::new_local(callback);
  on_cleanup(move || {
    handle.try_with_value(|callback| {
      if let Some(window) = web_sys::window() {
        drop(window.remove_event_listener_with_callback_and_bool(
          "keydown",
          callback.as_ref().unchecked_ref(),
          true,
        ));
      }
    });
  });
}

/// Measures a text row, a character and the scroller now and whenever the scroller's size changes.
fn observe_size(scroller: NodeRef<html::Div>, probe: NodeRef<html::Span>, metrics: RwSignal<Metrics>) {
  let measure = move || {
    let (Some(Some(element)), Some(Some(probe))) = (scroller.try_get_untracked(), probe.try_get_untracked())
    else {
      return;
    };
    let glyphs = probe.get_bounding_client_rect();
    let next = Metrics {
      row_px: glyphs.height(),
      char_px: glyphs.width() / PROBE_CHARS as f64,
      viewport_px: f64::from(element.client_height()),
      width_px: f64::from(element.client_width()),
    };
    if metrics.try_get_untracked() != Some(next) {
      let _ = metrics.try_set(next);
    }
  };
  let callback = Closure::<dyn FnMut()>::new(measure);
  let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) else {
    tracing::debug!("ResizeObserver unavailable");
    return;
  };
  let handle = StoredValue::new_local((observer, callback));
  Effect::new(move |_| {
    if let Some(element) = scroller.get() {
      handle.with_value(|(observer, _)| observer.observe(&element));
      measure();
    }
  });
  on_cleanup(move || {
    handle.try_with_value(|(observer, _)| observer.disconnect());
  });
}

/// Digits of the largest line number (at least two, so short files line up too).
fn digit_count(lines: usize) -> u32 {
  lines.max(1).ilog10().saturating_add(1).max(2)
}

/// A scroll position in whole pixels, as the DOM takes it.
fn px(value: f64) -> i32 {
  // Saturating: scroll positions are far below i32::MAX (MAX_CANVAS_PX).
  value.round() as i32
}
