//! The table view of file previews (#2341): CSV, TSV, semicolon- and pipe-separated files as a
//! table with a sticky header row and row numbers, find, go to row, column widths from the content
//! (dragged wider or narrower at the header's edge), numbers aligned at the end, wrapped cells on
//! demand, and copying a cell or a row.
//!
//! The file is untrusted and its cells are only ever text. The table is parsed in steps
//! (`text_preview::table`) with the browser getting a turn in between, and only the rows in view
//! are in the page (`text_preview::layout`), so a file of a million rows costs as much to show as
//! one of a hundred. A file that turns out to be no table falls back to the text view with a note.
//! The choices (view, separator, header, wrapping) are remembered per file type on the device.

use std::rc::Rc;

use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_text;
use crate::components::sliding_indicator::SlidingIndicator;
use crate::components::text_viewer::{
  DEBOUNCE_FROM_BYTES, FIND_PAUSE, MAX_QUERY_CHARS, listen_for_find_keys, yield_to_browser,
};
use crate::i18n::I18n;
use crate::reactive::SetIfAlive;
use crate::settings;
use crate::state::AppState;
use crate::text_preview::TextDoc;
use crate::text_preview::decode::TextEncoding;
use crate::text_preview::layout::{self, Heights, ScrollMap};
use crate::text_preview::search;
use crate::text_preview::table::{
  self, CellMatch, ColumnProfile, CustomDelimiter, Delimiter, Dialect, Quote, Sniffed, Table, TableError,
  TableParser,
};

/// Rows parsed per step before the browser gets a turn.
const ROWS_PER_STEP: usize = 20_000;
/// Rows drawn above and below the viewport, so scrolling shows no blank rows.
const OVERSCAN_ROWS: u32 = 10;
/// Characters of a cell drawn at most; the rest is in the copy and the download.
const MAX_CELL_CHARS: usize = 1_000;
/// Lines a wrapped cell takes at most; a longer one is clipped.
const MAX_WRAPPED_LINES: u32 = 8;
/// A cell's horizontal padding, both sides together (the stylesheet's `.tt td`).
const CELL_PADDING_PX: f64 = 20.0;
/// Starting widths count characters this much wider than the measured average: capitals and
/// digits, which IDs and numbers are made of, are wider than the average letter.
const WIDE_CHAR_SHARE: f64 = 1.25;
/// How narrow and how wide a column can be dragged.
const MIN_COLUMN_PX: f64 = 40.0;
const MAX_COLUMN_PX: f64 = 1_600.0;
/// Wrapping counts on characters a little wider than the average, so a row is rarely too short.
const WRAP_WIDTH_SHARE: f64 = 0.9;
/// Where the choices for a file type are kept, followed by the type (`csv`, `txt`).
const PREFS_KEY_PREFIX: &str = "teamcraft.table_preview.";
/// Text that measures a character of the table's font: letters, digits and capitals mixed.
/// The picker's entry for a typed separator.
const CUSTOM_SLUG: &str = "custom";
const PROBE_TEXT: &str = "Location 0123456789 sample ABCDEFGHIJ";

/// The table pane of a file: parses `doc` in steps (again when the separator changes), then shows
/// it, or falls back to the text view with a note.
pub fn table_pane(
  state: AppState,
  i18n: I18n,
  doc: Rc<TextDoc>,
  encoding: TextEncoding,
  maybe_sniffed: Option<Sniffed>,
  options: TableOptions,
  name: String,
) -> AnyView {
  let phase: RwSignal<Option<Rc<Parsed>>, LocalStorage> = RwSignal::new_local(None);
  let generation = StoredValue::new(0u32);
  let source = StoredValue::new_local(Rc::clone(&doc));
  Effect::watch(
    move || options.delimiter.get(),
    move |choice, _, _| {
      let choice = *choice;
      generation.update_value(|count| *count = count.wrapping_add(1));
      let mine = generation.get_value();
      phase.set(None);
      let doc = source.get_value();
      spawn_local(async move {
        let is_current = move || generation.try_get_value() == Some(mine);
        match parse(doc, choice, maybe_sniffed, is_current).await {
          Some(Ok(parsed)) => phase.set_if_alive(Some(Rc::new(parsed))),
          Some(Err(error)) if is_current() => options.refuse(error),
          // Closed, or the separator changed meanwhile.
          _ => {},
        }
      });
    },
    true,
  );
  let name = StoredValue::new(name);
  view! {
    {move || match phase.get() {
      None => view! {
        <div class="preview-status" role="status" aria-busy="true">
          <span class="spinner" aria-hidden="true"></span>
          <p>{move || i18n.t("table-preview-parsing")}</p>
        </div>
      }
      .into_any(),
      Some(parsed) => ready_view(state, i18n, source.get_value(), encoding, parsed, options, name.get_value()),
    }}
  }
  .into_any()
}

/// Whether a file is shown as a table or as text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
  #[default]
  Table,
  Text,
}

/// The separator sniffed, or the one the user picked or typed. Kept as `auto`, a separator's
/// slug or `custom:` and the typed text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "String", from = "String")]
pub enum DelimiterChoice {
  #[default]
  Auto,
  Fixed(Delimiter),
}

/// Whether the first row names the columns: guessed, or set by the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderChoice {
  #[default]
  Auto,
  On,
  Off,
}

/// The choices of the table view, kept per file type on the device.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct TablePrefs {
  #[serde(default)]
  view: ViewMode,
  #[serde(default)]
  delimiter: DelimiterChoice,
  #[serde(default)]
  header: HeaderChoice,
  #[serde(default)]
  is_wrapped: bool,
}

/// The table view's choices while a preview is open, shared by its table and text panes.
#[derive(Clone, Copy)]
pub struct TableOptions {
  pub view: RwSignal<ViewMode>,
  delimiter: RwSignal<DelimiterChoice>,
  header: RwSignal<HeaderChoice>,
  is_wrapped: RwSignal<bool>,
  /// Why the table was given up, shown in the text view.
  refusal: RwSignal<Option<TableError>>,
  file_type: StoredValue<String>,
}

/// A parsed table and what the view needs to know about it.
struct Parsed {
  table: Table,
  /// The separator the sniffer found, which "Automatic" names.
  maybe_detected: Option<Delimiter>,
  looks_like_header: bool,
  /// The columns with the first row as data, and as a header.
  profiles: [Vec<ColumnProfile>; 2],
}

/// Sizes measured in the page.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
  /// A row of one line, padding included.
  row_px: f64,
  /// One line of text in a cell.
  line_px: f64,
  /// An average character of the table's font.
  char_px: f64,
  /// The header row.
  header_px: f64,
  viewport_px: f64,
  width_px: f64,
}

/// The rows' heights, and the unit they are counted in (a row when cells don't wrap, a pixel when
/// they do).
#[derive(Debug)]
struct RowHeights {
  heights: Heights,
  unit_px: f64,
}

/// The current selection: one cell or one row (rows of the table, header included).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Selected {
  Cell { row: usize, column: usize },
  Row(usize),
}

/// The matches of the find field and whether there were more.
#[derive(Debug, Default)]
struct TableFound {
  matches: Vec<CellMatch>,
  is_capped: bool,
}

/// What drawn rows depend on besides their index: when it changes, they are drawn anew.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Stamp {
  has_header: bool,
  is_wrapped: bool,
  found: usize,
  heights: usize,
  maybe_current: Option<usize>,
  maybe_selected: Option<Selected>,
}

/// A column being dragged wider or narrower.
#[derive(Clone, Copy, Debug)]
struct Drag {
  column: usize,
  start_x: f64,
  start_px: f64,
}

/// The signals the table's actions work on.
#[derive(Clone, Copy)]
struct TableView {
  doc: StoredValue<Rc<TextDoc>, LocalStorage>,
  parsed: StoredValue<Rc<Parsed>, LocalStorage>,
  has_header: Memo<bool>,
  heights: RwSignal<Rc<RowHeights>, LocalStorage>,
  widths: RwSignal<Vec<f64>>,
  metrics: RwSignal<Metrics>,
  found: RwSignal<Rc<TableFound>, LocalStorage>,
  maybe_current: RwSignal<Option<usize>>,
  maybe_selected: RwSignal<Option<Selected>>,
  scroller: NodeRef<html::Div>,
}

impl DelimiterChoice {
  /// The picker's entries: automatic, the usual separators, and one to type.
  const PICKER: [&'static str; 7] = ["auto", "comma", "semicolon", "tab", "pipe", "colon", CUSTOM_SLUG];

  fn fixed(self) -> Option<Delimiter> {
    match self {
      Self::Auto => None,
      Self::Fixed(delimiter) => Some(delimiter),
    }
  }

  /// The picker entry this choice shows as.
  fn picker_slug(self) -> &'static str {
    match self {
      Self::Auto => "auto",
      Self::Fixed(delimiter) => delimiter.slug().unwrap_or(CUSTOM_SLUG),
    }
  }

  fn maybe_custom_text(self) -> Option<String> {
    match self {
      Self::Fixed(Delimiter::Custom(custom)) => Some(custom.as_str().to_owned()),
      Self::Auto | Self::Fixed(_) => None,
    }
  }
}

impl From<DelimiterChoice> for String {
  fn from(choice: DelimiterChoice) -> Self {
    match choice {
      DelimiterChoice::Fixed(Delimiter::Custom(custom)) => format!("{CUSTOM_SLUG}:{}", custom.as_str()),
      _ => choice.picker_slug().to_owned(),
    }
  }
}

impl From<String> for DelimiterChoice {
  /// Anything unknown (an older or damaged value) is automatic.
  fn from(kept: String) -> Self {
    if let Some(text) = kept
      .strip_prefix(CUSTOM_SLUG)
      .and_then(|rest| rest.strip_prefix(':'))
    {
      return CustomDelimiter::new(text).map_or(Self::Auto, |custom| Self::Fixed(Delimiter::Custom(custom)));
    }
    Delimiter::from_slug(&kept).map_or(Self::Auto, Self::Fixed)
  }
}

impl TablePrefs {
  fn load(file_type: &str) -> Self {
    settings::load_json(&format!("{PREFS_KEY_PREFIX}{file_type}")).unwrap_or_default()
  }

  fn save(self, file_type: &str) {
    settings::store_json(&format!("{PREFS_KEY_PREFIX}{file_type}"), &self);
  }
}

impl TableOptions {
  /// The choices kept for the type of file `name`.
  pub fn load(name: &str) -> Self {
    let file_type = file_type_of(name);
    let prefs = TablePrefs::load(&file_type);
    Self {
      view: RwSignal::new(prefs.view),
      delimiter: RwSignal::new(prefs.delimiter),
      header: RwSignal::new(prefs.header),
      is_wrapped: RwSignal::new(prefs.is_wrapped),
      refusal: RwSignal::new(None),
      file_type: StoredValue::new(file_type),
    }
  }

  /// The Table / Text switch for a pane's toolbar.
  pub fn view_switch(self, i18n: I18n) -> AnyView {
    let button = move |mode: ViewMode, key: &'static str| {
      view! {
        <button type="button" aria-pressed=move || (self.view.get() == mode).to_string() on:click=move |_| self.choose_view(mode)>
          {move || i18n.t(key)}
        </button>
      }
    };
    view! {
      <div class="segmented tt-view-switch" role="group" aria-label=move || i18n.t("table-preview-view")>
        <SlidingIndicator />
        {button(ViewMode::Table, "table-preview-table")}
        {button(ViewMode::Text, "table-preview-text")}
      </div>
    }
    .into_any()
  }

  /// Why the table was given up, worded for the text view's notes.
  pub fn refusal_note(self, i18n: I18n) -> Option<String> {
    let error = self.refusal.get()?;
    Some(match error {
      TableError::NotDelimited => i18n.t("table-preview-refused-not-delimited"),
      TableError::UnterminatedQuote { line } => {
        i18n.t_with("table-preview-refused-quote", &[("line", &line.to_string())])
      },
      TableError::Ragged { differing, rows } => i18n.t_count_with(
        "table-preview-refused-ragged",
        differing,
        &[("rows", &rows.to_string())],
      ),
    })
  }

  fn choose_view(self, mode: ViewMode) {
    if self.view.get_untracked() == mode {
      return;
    }
    self.refusal.set(None);
    self.view.set(mode);
    self.save();
  }

  /// Gives up the table: shows the text with a note. A separator the user picked goes back to
  /// automatic, so the table can be tried again.
  fn refuse(self, error: TableError) {
    let _ = self.refusal.try_set(Some(error));
    let _ = self.view.try_set(ViewMode::Text);
    if self.delimiter.try_get_untracked() != Some(DelimiterChoice::Auto) {
      let _ = self.delimiter.try_set(DelimiterChoice::Auto);
    }
  }

  fn save(self) {
    let prefs = TablePrefs {
      view: self.view.get_untracked(),
      delimiter: self.delimiter.get_untracked(),
      header: self.header.get_untracked(),
      is_wrapped: self.is_wrapped.get_untracked(),
    };
    self.file_type.with_value(|file_type| prefs.save(file_type));
  }
}

/// Parses the text in steps with the chosen or sniffed dialect; `None` when the pane went away or
/// the choice changed meanwhile.
async fn parse(
  doc: Rc<TextDoc>,
  choice: DelimiterChoice,
  maybe_sniffed: Option<Sniffed>,
  is_current: impl Fn() -> bool,
) -> Option<Result<Parsed, TableError>> {
  let maybe_dialect = match choice.fixed() {
    Some(delimiter) => Some(dialect_for(&doc.text, delimiter)),
    None => maybe_sniffed.map(|found| found.dialect),
  };
  let Some(dialect) = maybe_dialect else {
    return Some(Err(TableError::NotDelimited));
  };
  let mut parser = TableParser::new(dialect);
  loop {
    yield_to_browser().await;
    if !is_current() {
      return None;
    }
    match parser.step(&doc.text, ROWS_PER_STEP) {
      Ok(true) => break,
      Ok(false) => {},
      Err(error) => return Some(Err(error)),
    }
  }
  let table = match parser.finish() {
    Ok(table) => table,
    Err(error) => return Some(Err(error)),
  };
  let looks_like_header = table.looks_like_header(&doc.text);
  let profiles = [table.profile(&doc.text, false), table.profile(&doc.text, true)];
  Some(Ok(Parsed {
    table,
    maybe_detected: maybe_sniffed.map(|found| found.dialect.delimiter),
    looks_like_header,
    profiles,
  }))
}

/// A picked separator, with the quote character that reads the text best.
fn dialect_for(text: &str, delimiter: Delimiter) -> Dialect {
  let mut best = Dialect::new(delimiter);
  let mut best_percent = 0;
  for quote in Quote::ALL {
    let dialect = Dialect { delimiter, quote };
    if let Some(found) = table::sniff_dialect(text, dialect)
      && found.consistent_percent > best_percent
    {
      best = dialect;
      best_percent = found.consistent_percent;
    }
  }
  best
}

/// The toolbar, the notes and the table of a parsed file.
fn ready_view(
  state: AppState,
  i18n: I18n,
  doc: Rc<TextDoc>,
  encoding: TextEncoding,
  parsed: Rc<Parsed>,
  options: TableOptions,
  name: String,
) -> AnyView {
  let row_count = parsed.table.rows();
  let column_count = parsed.table.columns;
  let looks_like_header = parsed.looks_like_header;
  let is_truncated = parsed.table.is_truncated;
  let has_dropped_columns = parsed.table.has_dropped_columns;
  let maybe_detected = parsed.maybe_detected;
  let text_bytes = doc.text.len();
  let doc = StoredValue::new_local(doc);
  let parsed = StoredValue::new_local(parsed);

  let has_header = Memo::new(move |_| match options.header.get() {
    HeaderChoice::On => true,
    HeaderChoice::Off => false,
    HeaderChoice::Auto => looks_like_header,
  });
  let body_rows = Memo::new(move |_| row_count.saturating_sub(usize::from(has_header.get())));
  let metrics = RwSignal::new(Metrics::default());
  let widths = RwSignal::new(Vec::<f64>::new());
  // The widths the wrapped rows' heights follow: set when a drag ends, not at every move.
  let settled_widths = RwSignal::new(Vec::<f64>::new());
  let heights: RwSignal<Rc<RowHeights>, LocalStorage> = RwSignal::new_local(Rc::new(RowHeights {
    heights: Heights::Single(0),
    unit_px: 0.0,
  }));
  let scroll_top = RwSignal::new(0.0f64);
  let query = RwSignal::new(String::new());
  let found: RwSignal<Rc<TableFound>, LocalStorage> = RwSignal::new_local(Rc::new(TableFound::default()));
  let maybe_current = RwSignal::new(None::<usize>);
  let maybe_selected = RwSignal::new(None::<Selected>);
  let pending_query: StoredValue<Option<TimeoutHandle>, LocalStorage> = StoredValue::new_local(None);
  let drag: StoredValue<Option<Drag>> = StoredValue::new(None);
  let is_custom_open = RwSignal::new(
    options
      .delimiter
      .with_untracked(|choice| choice.maybe_custom_text().is_some()),
  );
  let has_custom_error = RwSignal::new(false);
  let custom_input = NodeRef::<html::Input>::new();
  let scroller = NodeRef::<html::Div>::new();
  let probe = NodeRef::<html::Span>::new();
  let head = NodeRef::<html::Thead>::new();
  let find_input = NodeRef::<html::Input>::new();

  let view_state = TableView {
    doc,
    parsed,
    has_header,
    heights,
    widths,
    metrics,
    found,
    maybe_current,
    maybe_selected,
    scroller,
  };

  observe_size(scroller, probe, head, metrics);
  // The columns' starting widths, once a character can be measured.
  Effect::new(move |_| {
    let char_px = metrics.with(|sizes| sizes.char_px);
    if char_px <= 0.0 || widths.with_untracked(|all| all.len() == column_count) {
      return;
    }
    let start: Vec<f64> = parsed.with_value(|parsed| {
      parsed.profiles[1]
        .iter()
        .map(|column| column_px(column.width_chars, char_px))
        .collect()
    });
    widths.set(start.clone());
    settled_widths.set(start);
  });
  Effect::watch(
    move || {
      (
        body_rows.get(),
        options.is_wrapped.get(),
        settled_widths.get(),
        metrics.get(),
      )
    },
    move |(rows, is_wrapped, settled, sizes), _, _| {
      let next = view_state.row_heights(*rows, *is_wrapped, settled, *sizes);
      heights.set(Rc::new(next));
    },
    true,
  );
  Effect::watch(
    move || query.get(),
    move |query, _, _| {
      let text = query.trim();
      let (matches, is_capped) = doc.with_value(|doc| {
        parsed.with_value(|parsed| parsed.table.find(&doc.text, text, search::MAX_MATCHES))
      });
      found.set(Rc::new(TableFound { matches, is_capped }));
      maybe_current.set(None);
    },
    false,
  );

  let window_now = Memo::new(move |_| {
    let sizes = metrics.get();
    heights.with(|rows| {
      let map = ScrollMap::new(&rows.heights, rows.unit_px, body_viewport_px(sizes));
      let overscan = if rows.unit_px > 0.0 && sizes.row_px > 0.0 {
        (f64::from(OVERSCAN_ROWS) * sizes.row_px / rows.unit_px).ceil() as u32
      } else {
        OVERSCAN_ROWS
      };
      let window = layout::window(&rows.heights, &map, rows.unit_px, scroll_top.get(), overscan);
      let drawn_px = f64::from(
        rows
          .heights
          .start_of(window.rows.end)
          .saturating_sub(rows.heights.start_of(window.rows.start)),
      ) * rows.unit_px;
      let below_px = (map.canvas_px - window.top_px - drawn_px).max(0.0);
      (window.rows, window.top_px, below_px)
    })
  });

  // The first match from the top of the view on, once a query found something.
  Effect::watch(
    move || found.get(),
    move |result, _, _| {
      if result.matches.is_empty() {
        return;
      }
      let first_row =
        window_now.with_untracked(|(rows, _, _)| rows.start) + usize::from(has_header.get_untracked());
      let index = result
        .matches
        .iter()
        .position(|found| found.row as usize >= first_row)
        .unwrap_or(0);
      view_state.show_match(index);
    },
    false,
  );

  listen_for_find_keys(find_input, move |is_forward| view_state.step(is_forward));
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
    if text_bytes < DEBOUNCE_FROM_BYTES {
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
  let on_go_to_row = move |ev: web_sys::Event| {
    if let Ok(number) = event_target_value(&ev).trim().parse::<usize>() {
      let body_row = number
        .saturating_sub(1)
        .min(body_rows.get_untracked().saturating_sub(1));
      let row = body_row + usize::from(has_header.get_untracked());
      maybe_selected.set(Some(Selected::Row(row)));
      view_state.reveal(row, None);
    }
  };
  let on_delimiter = move |ev: web_sys::Event| {
    let slug = event_target_value(&ev);
    if slug == CUSTOM_SLUG {
      is_custom_open.set(true);
      request_animation_frame(move || {
        if let Some(Some(field)) = custom_input.try_get_untracked() {
          drop(field.focus());
        }
      });
      return;
    }
    is_custom_open.set(false);
    has_custom_error.set(false);
    options
      .delimiter
      .set(Delimiter::from_slug(&slug).map_or(DelimiterChoice::Auto, DelimiterChoice::Fixed));
    options.save();
  };
  let on_custom = move |ev: web_sys::Event| {
    let text = event_target_value(&ev);
    if text.is_empty() {
      has_custom_error.set(false);
      return;
    }
    match CustomDelimiter::new(&text) {
      Some(custom) => {
        has_custom_error.set(false);
        options
          .delimiter
          .set(DelimiterChoice::Fixed(Delimiter::Custom(custom)));
        options.save();
      },
      None => has_custom_error.set(true),
    }
  };
  let toggle_header = move |_| {
    let next = if has_header.get_untracked() {
      HeaderChoice::Off
    } else {
      HeaderChoice::On
    };
    options.header.set(next);
    maybe_selected.set(None);
    options.save();
  };
  let toggle_wrap = move |_| {
    options.is_wrapped.update(|on| *on = !*on);
    options.save();
  };
  let copy = move |_| view_state.copy(state, i18n);

  let on_table_click = move |ev: web_sys::MouseEvent| {
    let Some(target) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    else {
      return;
    };
    let Ok(Some(cell)) = target.closest("[data-r]") else {
      return;
    };
    let read = |name: &str| {
      cell
        .get_attribute(name)
        .and_then(|value| value.parse::<usize>().ok())
    };
    let selection = match (read("data-r"), read("data-c")) {
      (Some(row), Some(column)) => Some(Selected::Cell { row, column }),
      (Some(row), None) => Some(Selected::Row(row)),
      _ => None,
    };
    maybe_selected.set(selection);
  };
  let on_table_keydown = move |ev: web_sys::KeyboardEvent| view_state.on_key(ev, state, i18n);

  let on_resize_start = move |column: usize, ev: web_sys::PointerEvent| {
    ev.prevent_default();
    ev.stop_propagation();
    if let Some(element) = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    {
      drop(element.set_pointer_capture(ev.pointer_id()));
    }
    let start_px = widths.with_untracked(|all| all.get(column).copied().unwrap_or(MIN_COLUMN_PX));
    drag.set_value(Some(Drag {
      column,
      start_x: f64::from(ev.client_x()),
      start_px,
    }));
  };
  let on_resize_move = move |ev: web_sys::PointerEvent| {
    let Some(current) = drag.get_value() else {
      return;
    };
    let next =
      (current.start_px + f64::from(ev.client_x()) - current.start_x).clamp(MIN_COLUMN_PX, MAX_COLUMN_PX);
    widths.update(|all| {
      if let Some(width) = all.get_mut(current.column) {
        *width = next;
      }
    });
  };
  let on_resize_end = move |_ev: web_sys::PointerEvent| {
    if drag.get_value().is_some() {
      drag.set_value(None);
      settled_widths.set(widths.get_untracked());
    }
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
  let notes = move || {
    let mut notes: Vec<String> = Vec::new();
    match encoding {
      TextEncoding::Utf8 => {},
      TextEncoding::Utf8Repaired => notes.push(i18n.t("text-preview-encoding-repaired")),
      TextEncoding::Utf16 => notes.push(i18n.t("text-preview-encoding-utf16")),
      TextEncoding::Latin1 => notes.push(i18n.t("text-preview-encoding-latin1")),
    }
    if is_truncated {
      notes.push(i18n.t_with("table-preview-truncated", &[("count", &row_count.to_string())]));
    }
    if has_dropped_columns {
      notes.push(i18n.t_with(
        "table-preview-columns-dropped",
        &[("count", &table::MAX_COLUMNS.to_string())],
      ));
    }
    notes
  };
  let picked_slug = move || {
    if is_custom_open.get() {
      CUSTOM_SLUG
    } else {
      options.delimiter.get().picker_slug()
    }
  };
  let delimiter_option = move |slug: &'static str| {
    let label = move || match slug {
      "auto" => match maybe_detected {
        Some(detected) => i18n.t_with(
          "table-preview-delimiter-auto",
          &[("detected", &delimiter_name(i18n, detected))],
        ),
        None => i18n.t("table-preview-delimiter-auto-none"),
      },
      CUSTOM_SLUG => i18n.t("table-preview-delimiter-custom"),
      _ => Delimiter::from_slug(slug)
        .map(|fixed| delimiter_name(i18n, fixed))
        .unwrap_or_default(),
    };
    view! { <option value=slug selected=move || picked_slug() == slug>{label}</option> }
  };
  let copy_label = move || match maybe_selected.get() {
    Some(Selected::Cell { .. }) => i18n.t("table-preview-copy-cell"),
    Some(Selected::Row(_)) => i18n.t("table-preview-copy-row"),
    None => i18n.t("table-preview-copy-all"),
  };
  let number_px = move || {
    let sizes = metrics.get();
    number_column_px(body_rows.get(), sizes.char_px)
  };
  let table_px = move || number_px() + widths.with(|all| all.iter().sum::<f64>());
  let label = i18n.t_with("table-preview-content", &[("name", &name)]);

  let header_cells = move || {
    // Drawn anew when the header switches or the matches change (they are marked in it too).
    has_header.track();
    found.track();
    maybe_current.track();
    (0..column_count)
      .map(|column| {
        let content = if has_header.get_untracked() {
          view_state.cell_content(0, column)
        } else {
          column_letter(column).into_any()
        };
        let is_numeric = view_state.is_numeric(column);
        view! {
          <th scope="col" class="tt-head" class:is-numeric=is_numeric data-r=has_header.get_untracked().then_some("0") data-c=column.to_string()>
            <span class="tt-head-text" title=has_header.get_untracked().then(|| hover_text(&view_state.cell_text(0, column)))>{content}</span>
            <span
              class="tt-resize"
              aria-hidden="true"
              on:pointerdown=move |ev| on_resize_start(column, ev)
              on:pointermove=on_resize_move
              on:pointerup=on_resize_end
              on:pointercancel=on_resize_end
              on:click=|ev: web_sys::MouseEvent| ev.stop_propagation()
            ></span>
          </th>
        }
      })
      .collect_view()
  };

  view! {
    <div class="tv tt-pane">
      <div class="tv-tools" role="toolbar" aria-label=move || i18n.t("text-preview-tools")>
        {options.view_switch(i18n)}
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
            title=move || i18n.t("text-preview-find-previous")
            prop:disabled=move || !has_matches()
            on:click=move |_| view_state.step(false)
          >
            <Icon name=IconName::ChevronDown class="is-flipped" />
          </button>
          <button
            type="button"
            class="icon-btn"
            aria-label=move || i18n.t("text-preview-find-next")
            title=move || i18n.t("text-preview-find-next")
            prop:disabled=move || !has_matches()
            on:click=move |_| view_state.step(true)
          >
            <Icon name=IconName::ChevronDown />
          </button>
        </div>
        <label class="tv-go-to">
          <span class="visually-hidden">{move || i18n.t("table-preview-go-to-row")}</span>
          <input
            class="input tv-go-to-input"
            type="number"
            inputmode="numeric"
            min="1"
            prop:max=move || body_rows.get().to_string()
            placeholder=move || i18n.t("table-preview-row")
            on:change=on_go_to_row
          />
          <span class="tv-line-count" aria-hidden="true">{move || format!("/ {}", body_rows.get())}</span>
        </label>
        <label class="tt-delimiter">
          <span class="tt-option-label">{move || i18n.t("table-preview-delimiter")}</span>
          <select class="input" on:change=on_delimiter>
            {DelimiterChoice::PICKER.into_iter().map(delimiter_option).collect_view()}
          </select>
        </label>
        {move || is_custom_open.get().then(|| view! {
          <div class="tt-custom">
            <input
              class="input tt-custom-input"
              type="text"
              node_ref=custom_input
              maxlength=table::MAX_CUSTOM_DELIMITER_BYTES.to_string()
              autocomplete="off"
              spellcheck="false"
              placeholder="::"
              prop:value=options.delimiter.get_untracked().maybe_custom_text().unwrap_or_default()
              aria-label=move || i18n.t("table-preview-delimiter-custom-label")
              aria-invalid=move || has_custom_error.get().then_some("true")
              aria-describedby=move || has_custom_error.get().then_some("tt-custom-error")
              on:change=on_custom
            />
            {move || has_custom_error.get().then(|| view! {
              <span id="tt-custom-error" class="field-error tt-custom-error">{move || i18n.t("table-preview-delimiter-custom-invalid")}</span>
            })}
          </div>
        })}
        <label class="tt-switch">
          <input type="checkbox" role="switch" prop:checked=move || has_header.get() on:change=toggle_header />
          <span>{move || i18n.t("table-preview-header")}</span>
        </label>
        <div class="tv-toggles">
          <button class="btn btn-small" aria-pressed=move || options.is_wrapped.get().to_string() on:click=toggle_wrap>
            {move || i18n.t("table-preview-wrap")}
          </button>
          <button class="icon-btn" aria-label=copy_label title=copy_label on:click=copy>
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
        class="tt-scroller"
        class:is-wrapped=move || options.is_wrapped.get()
        node_ref=scroller
        tabindex="0"
        role="region"
        aria-label=label
        translate="no"
        on:scroll=move |ev| scroll_top.set(f64::from(event_target::<web_sys::HtmlElement>(&ev).scroll_top()))
        on:keydown=on_table_keydown
      >
        <table
          class="tt"
          aria-rowcount=move || (body_rows.get() + 1).to_string()
          style:width=move || format!("{}px", table_px())
          style:--tt-line=move || format!("{}px", metrics.with(|sizes| sizes.line_px))
          on:click=on_table_click
        >
          <colgroup>
            <col style:width=move || format!("{}px", number_px()) />
            {move || widths.with(|all| all.iter().map(|width| view! { <col style:width=format!("{width}px") /> }).collect_view())}
          </colgroup>
          <thead node_ref=head>
            <tr aria-rowindex="1">
              <th scope="col" class="tt-n tt-corner">
                <span class="visually-hidden">{move || i18n.t("table-preview-row-number")}</span>
              </th>
              {header_cells}
            </tr>
          </thead>
          <tbody>
            <tr class="tt-spacer" aria-hidden="true" style:height=move || format!("{}px", window_now.with(|(_, top, _)| *top))></tr>
            <For
              each=move || {
                let stamp = Stamp {
                  has_header: has_header.get(),
                  is_wrapped: options.is_wrapped.get(),
                  found: found.with(|result| Rc::as_ptr(result) as usize),
                  heights: heights.with(|rows| Rc::as_ptr(rows) as usize),
                  maybe_current: maybe_current.get(),
                  maybe_selected: maybe_selected.get(),
                };
                window_now.with(|(rows, _, _)| rows.clone().map(|index| (stamp, index)).collect::<Vec<_>>())
              }
              key=|item| *item
              children=move |(stamp, index)| view_state.row_view(stamp, index, column_count)
            />
            <tr class="tt-spacer" aria-hidden="true" style:height=move || format!("{}px", window_now.with(|(_, _, below)| *below))></tr>
          </tbody>
        </table>
        <span class="tt-probe" node_ref=probe aria-hidden="true">{PROBE_TEXT}</span>
      </div>
    </div>
  }
  .into_any()
}

impl TableView {
  /// One body row: its number and its cells.
  fn row_view(self, stamp: Stamp, body_row: usize, columns: usize) -> AnyView {
    let row = body_row + usize::from(stamp.has_header);
    let (row_len, height) = self.parsed.with_value(|parsed| {
      let height = self.heights.with_untracked(|rows| {
        let start = rows.heights.start_of(body_row);
        let end = rows.heights.start_of(body_row + 1);
        f64::from(end.saturating_sub(start)) * rows.unit_px
      });
      (parsed.table.row_len(row), height)
    });
    let is_row_selected = stamp.maybe_selected == Some(Selected::Row(row));
    let cells = (0..columns)
      .map(|column| {
        let is_missing = column >= row_len;
        let is_selected = stamp.maybe_selected == Some(Selected::Cell { row, column });
        let is_empty = !is_missing && self.cell_text(row, column).trim().is_empty();
        let class = cell_class(self.is_numeric(column), is_empty, is_missing, is_selected);
        let content = if is_missing {
          ().into_any()
        } else {
          self.cell_content(row, column)
        };
        // The whole text on hover, for a cell cut off with an ellipsis.
        let maybe_title = (!is_missing && !is_empty).then(|| hover_text(&self.cell_text(row, column)));
        view! { <td class=class title=maybe_title data-r=row.to_string() data-c=column.to_string()><div class="tt-cell">{content}</div></td> }
      })
      .collect_view();
    let lines = if stamp.is_wrapped && self.metrics.get_untracked().line_px > 0.0 {
      let sizes = self.metrics.get_untracked();
      ((height - (sizes.row_px - sizes.line_px)) / sizes.line_px)
        .round()
        .max(1.0)
    } else {
      1.0
    };
    view! {
      <tr
        class:is-selected=is_row_selected
        aria-rowindex=(body_row + 2).to_string()
        style:height=stamp.is_wrapped.then(|| format!("{height}px"))
        style:--tt-lines=lines.to_string()
      >
        <th scope="row" class="tt-n" data-r=row.to_string()>{(body_row + 1).to_string()}</th>
        {cells}
      </tr>
    }
    .into_any()
  }

  /// A cell's text as drawn: at most [`MAX_CELL_CHARS`] characters, with the matches marked.
  fn cell_content(self, row: usize, column: usize) -> AnyView {
    let text = self.cell_text(row, column);
    let shown_end = text
      .char_indices()
      .nth(MAX_CELL_CHARS)
      .map_or(text.len(), |(at, _)| at);
    let current = self.maybe_current.get_untracked();
    let pieces = self.found.with_untracked(|result| {
      let first = result
        .matches
        .partition_point(|found| (found.row as usize, found.column as usize) < (row, column));
      let mut pieces: Vec<AnyView> = Vec::new();
      let mut at = 0usize;
      for (offset, found) in result.matches[first..].iter().enumerate() {
        if (found.row as usize, found.column as usize) != (row, column) {
          break;
        }
        let span = found.span.range();
        if span.end > shown_end {
          break;
        }
        pieces.push(text.get(at..span.start).unwrap_or_default().to_owned().into_any());
        let class = if current == Some(first + offset) {
          "tv-match is-current"
        } else {
          "tv-match"
        };
        let marked = text.get(span.clone()).unwrap_or_default().to_owned();
        pieces.push(view! { <mark class=class>{marked}</mark> }.into_any());
        at = span.end;
      }
      pieces.push(text.get(at..shown_end).unwrap_or_default().to_owned().into_any());
      if shown_end < text.len() {
        pieces.push("…".into_any());
      }
      pieces
    });
    pieces.collect_view().into_any()
  }

  fn cell_text(self, row: usize, column: usize) -> String {
    self.doc.with_value(|doc| {
      self.parsed.with_value(|parsed| {
        parsed
          .table
          .cell(&doc.text, row, column)
          .unwrap_or_default()
          .to_owned()
      })
    })
  }

  fn is_numeric(self, column: usize) -> bool {
    let has_header = usize::from(self.has_header.get_untracked());
    self.parsed.with_value(|parsed| {
      parsed.profiles[has_header]
        .get(column)
        .is_some_and(|profile| profile.is_numeric)
    })
  }

  /// The rows' heights: one row each, or in pixels from the lines their cells wrap to.
  fn row_heights(self, rows: usize, is_wrapped: bool, widths: &[f64], sizes: Metrics) -> RowHeights {
    if !is_wrapped || sizes.char_px <= 0.0 || widths.is_empty() {
      return RowHeights {
        heights: Heights::Single(rows),
        unit_px: sizes.row_px,
      };
    }
    let column_chars: Vec<u32> = widths
      .iter()
      .map(|width| {
        (((width - CELL_PADDING_PX) * WRAP_WIDTH_SHARE / sizes.char_px)
          .floor()
          .max(1.0)) as u32
      })
      .collect();
    let header = usize::from(self.has_header.get_untracked());
    let extra_px = sizes.row_px - sizes.line_px;
    let heights = self.doc.with_value(|doc| {
      self.parsed.with_value(|parsed| {
        Heights::from_sizes((0..rows).map(|body_row| {
          let lines =
            parsed
              .table
              .wrapped_lines(&doc.text, body_row + header, &column_chars, MAX_WRAPPED_LINES);
          (f64::from(lines) * sizes.line_px + extra_px).round() as u32
        }))
      })
    });
    RowHeights {
      heights,
      unit_px: 1.0,
    }
  }

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

  /// Makes match `index` the current one and scrolls its cell into view.
  fn show_match(self, index: usize) {
    let Some(found) = self
      .found
      .with_untracked(|result| result.matches.get(index).copied())
    else {
      return;
    };
    self.maybe_current.set(Some(index));
    self.reveal(found.row as usize, Some(found.column as usize));
  }

  /// Scrolls row `row` (of the table) into view, centred when it isn't, and `column` sideways.
  fn reveal(self, row: usize, maybe_column: Option<usize>) {
    let Some(element) = self.scroller.get_untracked() else {
      return;
    };
    let sizes = self.metrics.get_untracked();
    let header = usize::from(self.has_header.get_untracked());
    if let Some(body_row) = row.checked_sub(header) {
      self.heights.with_untracked(|rows| {
        if rows.unit_px <= 0.0 {
          return;
        }
        let viewport_px = body_viewport_px(sizes);
        let map = ScrollMap::new(&rows.heights, rows.unit_px, viewport_px);
        let target_px = f64::from(rows.heights.start_of(body_row)) * rows.unit_px;
        let end_px = f64::from(rows.heights.start_of(body_row + 1)) * rows.unit_px;
        let content_top = map.content_top(f64::from(element.scroll_top()));
        let is_in_view = target_px >= content_top && end_px <= content_top + viewport_px;
        if !is_in_view {
          let centred = (target_px - (viewport_px - (end_px - target_px)) / 2.0).max(0.0);
          element.set_scroll_top(px(map.scroll_top_for(centred)));
        }
      });
    }
    if let Some(column) = maybe_column {
      let (left_px, width_px) = self.widths.with_untracked(|all| {
        (
          all.iter().take(column).sum::<f64>(),
          all.get(column).copied().unwrap_or(0.0),
        )
      });
      let number_px = number_column_px(self.body_rows(), sizes.char_px);
      let visible = sizes.width_px - number_px;
      let scroll_left = f64::from(element.scroll_left());
      if left_px < scroll_left || left_px + width_px.min(visible) > scroll_left + visible {
        element.set_scroll_left(px((left_px - visible / 4.0).max(0.0)));
      }
    }
  }

  /// Arrow keys move the selected cell, Ctrl/Cmd+C copies it, Escape lets it go.
  fn on_key(self, ev: web_sys::KeyboardEvent, state: AppState, i18n: I18n) {
    let Some(selected) = self.maybe_selected.get_untracked() else {
      return;
    };
    let is_command = (ev.ctrl_key() || ev.meta_key()) && !ev.alt_key();
    let key = ev.key();
    if is_command && key.eq_ignore_ascii_case("c") {
      if selected_text(self.scroller).is_none() {
        ev.prevent_default();
        self.copy(state, i18n);
      }
      return;
    }
    let (rows, columns) = self
      .parsed
      .with_value(|parsed| (parsed.table.rows(), parsed.table.columns));
    let first_row = usize::from(self.has_header.get_untracked());
    let (row, column) = match selected {
      Selected::Cell { row, column } => (row, Some(column)),
      Selected::Row(row) => (row, None),
    };
    let next = match key.as_str() {
      "ArrowDown" => Selected::Cell {
        row: (row + 1).min(rows.saturating_sub(1)),
        column: column.unwrap_or(0),
      },
      "ArrowUp" => Selected::Cell {
        row: row.saturating_sub(1).max(first_row),
        column: column.unwrap_or(0),
      },
      "ArrowRight" => Selected::Cell {
        row,
        column: column.map_or(0, |column| (column + 1).min(columns.saturating_sub(1))),
      },
      "ArrowLeft" => Selected::Cell {
        row,
        column: column.map_or(0, |column| column.saturating_sub(1)),
      },
      "Escape" => {
        // Lets the selection go first; a second Escape closes the preview.
        ev.stop_propagation();
        self.maybe_selected.set(None);
        return;
      },
      _ => return,
    };
    ev.prevent_default();
    self.maybe_selected.set(Some(next));
    if let Selected::Cell { row, column } = next {
      self.reveal(row, Some(column));
    }
  }

  /// Copies the selected cell or row; else the text selected in the table, or the whole file.
  fn copy(self, state: AppState, i18n: I18n) {
    let text = match self.maybe_selected.get_untracked() {
      Some(Selected::Cell { row, column }) => self.cell_text(row, column),
      Some(Selected::Row(row)) => self.doc.with_value(|doc| {
        self
          .parsed
          .with_value(|parsed| parsed.table.row_text(&doc.text, row))
      }),
      None => selected_text(self.scroller).unwrap_or_else(|| self.doc.with_value(|doc| doc.text.clone())),
    };
    let copying = copy_text(&text);
    let (copied, failed) = (i18n.t("action-copied"), i18n.t("action-copy-failed"));
    spawn_local(async move {
      // `show_error` does nothing once the shell is gone (issue #322); the toast checks first.
      if copying.await {
        if state.is_alive() {
          state.show_brief_toast(copied);
        }
      } else {
        state.show_error(failed, String::new());
      }
    });
  }

  fn body_rows(self) -> usize {
    let header = usize::from(self.has_header.get_untracked());
    self
      .parsed
      .with_value(|parsed| parsed.table.rows().saturating_sub(header))
  }
}

/// A cell's text for its tooltip: at most [`MAX_CELL_CHARS`] characters.
fn hover_text(cell: &str) -> String {
  match cell.char_indices().nth(MAX_CELL_CHARS) {
    Some((end, _)) => format!("{}…", &cell[..end]),
    None => cell.to_owned(),
  }
}

/// The classes of a body cell.
fn cell_class(is_numeric: bool, is_empty: bool, is_missing: bool, is_selected: bool) -> String {
  let mut class = String::from("tt-td");
  for (is_on, name) in [
    (is_numeric, " is-numeric"),
    (is_empty, " is-empty"),
    (is_missing, " is-missing"),
    (is_selected, " is-selected"),
  ] {
    if is_on {
      class.push_str(name);
    }
  }
  class
}

/// A separator's name: its i18n name, or a typed one in quotes.
fn delimiter_name(i18n: I18n, delimiter: Delimiter) -> String {
  let key = match delimiter {
    Delimiter::Comma => "table-preview-delimiter-comma",
    Delimiter::Semicolon => "table-preview-delimiter-semicolon",
    Delimiter::Tab => "table-preview-delimiter-tab",
    Delimiter::Pipe => "table-preview-delimiter-pipe",
    Delimiter::Colon => "table-preview-delimiter-colon",
    Delimiter::Custom(custom) => return format!("“{}”", custom.as_str()),
  };
  i18n.t(key)
}

/// A file's type for the kept choices: its extension in lower case, `txt` without one.
pub(crate) fn file_type_of(name: &str) -> String {
  name
    .rsplit_once('.')
    .map(|(_, extension)| extension.to_ascii_lowercase())
    .filter(|extension| {
      !extension.is_empty()
        && extension.len() <= 8
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
    .unwrap_or_else(|| "txt".to_owned())
}

/// A spreadsheet's name for column `index`: A to Z, then AA, AB and so on.
fn column_letter(index: usize) -> String {
  let mut name = Vec::new();
  let mut rest = index + 1;
  while rest > 0 {
    let digit = (rest - 1) % 26;
    // Invariant: `digit` is below 26, so the sum is an ASCII capital.
    name.push(char::from(b'A' + digit as u8));
    rest = (rest - 1) / 26;
  }
  name.iter().rev().collect()
}

/// The text selected in the table, if any.
fn selected_text(scroller: NodeRef<html::Div>) -> Option<String> {
  let element = scroller.get_untracked()?;
  let selection = web_sys::window()?.get_selection().ok()??;
  let anchor = selection.anchor_node()?;
  if selection.is_collapsed() || !element.contains(Some(&anchor)) {
    return None;
  }
  Some(String::from(selection.to_string())).filter(|text| !text.is_empty())
}

/// Measures a row, a line, a character, the header and the scroller now and whenever the
/// scroller's size changes.
fn observe_size(
  scroller: NodeRef<html::Div>,
  probe: NodeRef<html::Span>,
  head: NodeRef<html::Thead>,
  metrics: RwSignal<Metrics>,
) {
  let measure = move || {
    let (Some(Some(element)), Some(Some(probe)), Some(Some(head))) = (
      scroller.try_get_untracked(),
      probe.try_get_untracked(),
      head.try_get_untracked(),
    ) else {
      return;
    };
    let glyphs = probe.get_bounding_client_rect();
    let style = web_sys::window().and_then(|window| window.get_computed_style(&probe).ok().flatten());
    let padding_px = style
      .map(|style| {
        let read = |name: &str| {
          style
            .get_property_value(name)
            .ok()
            .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
            .unwrap_or(0.0)
        };
        read("padding-top") + read("padding-bottom")
      })
      .unwrap_or(0.0);
    let next = Metrics {
      row_px: glyphs.height(),
      line_px: glyphs.height() - padding_px,
      char_px: (glyphs.width() - CELL_PADDING_PX) / PROBE_TEXT.chars().count() as f64,
      header_px: head.get_bounding_client_rect().height(),
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

/// A column's starting width for `chars` characters, padding and border included.
fn column_px(chars: u32, char_px: f64) -> f64 {
  (f64::from(chars) * char_px * WIDE_CHAR_SHARE + CELL_PADDING_PX + 1.0).ceil()
}

/// The width of the row numbers' column for `rows` rows.
fn number_column_px(rows: usize, char_px: f64) -> f64 {
  column_px(digit_count(rows) + 1, char_px)
}

/// The height rows have below the sticky header.
fn body_viewport_px(sizes: Metrics) -> f64 {
  (sizes.viewport_px - sizes.header_px).max(0.0)
}

/// Digits of the largest row number (at least two, so short tables line up too).
fn digit_count(rows: usize) -> u32 {
  rows.max(1).ilog10().saturating_add(1).max(2)
}

/// A scroll position in whole pixels, as the DOM takes it.
fn px(value: f64) -> i32 {
  // Saturating: scroll positions are far below i32::MAX (MAX_CANVAS_PX).
  value.round() as i32
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn columns_are_named_like_a_spreadsheet() {
    let names: Vec<String> = [0, 1, 25, 26, 27, 51, 52, 701, 702].map(column_letter).to_vec();
    assert_eq!(names, ["A", "B", "Z", "AA", "AB", "AZ", "BA", "ZZ", "AAA"]);
  }

  #[test]
  fn the_file_type_is_the_extension() {
    assert_eq!(file_type_of("Export.CSV"), "csv");
    assert_eq!(file_type_of("list.tab"), "tab");
    assert_eq!(file_type_of("noextension"), "txt");
    assert_eq!(file_type_of("odd.ext with spaces"), "txt");
  }

  #[test]
  fn kept_choices_read_back_and_tolerate_missing_fields() {
    let prefs = TablePrefs {
      view: ViewMode::Text,
      delimiter: DelimiterChoice::Fixed(Delimiter::Semicolon),
      header: HeaderChoice::Off,
      is_wrapped: true,
    };
    let json = serde_json::to_string(&prefs).expect("encodes");
    assert_eq!(
      json,
      r#"{"view":"text","delimiter":"semicolon","header":"off","is_wrapped":true}"#
    );
    assert_eq!(serde_json::from_str::<TablePrefs>(&json).expect("decodes"), prefs);
    assert_eq!(
      serde_json::from_str::<TablePrefs>("{}").expect("decodes"),
      TablePrefs::default()
    );
  }

  #[test]
  fn separator_choices_are_kept_as_text() {
    let custom = CustomDelimiter::new(":: ").expect("valid");
    for choice in [
      DelimiterChoice::Auto,
      DelimiterChoice::Fixed(Delimiter::Tab),
      DelimiterChoice::Fixed(Delimiter::Colon),
      DelimiterChoice::Fixed(Delimiter::Custom(custom)),
    ] {
      assert_eq!(DelimiterChoice::from(String::from(choice)), choice);
    }
    assert_eq!(
      String::from(DelimiterChoice::Fixed(Delimiter::Custom(custom))),
      "custom::: "
    );
    for damaged in ["nonsense", "custom", "custom:", "custom:\"", ""] {
      assert_eq!(
        DelimiterChoice::from(damaged.to_owned()),
        DelimiterChoice::Auto,
        "{damaged:?}"
      );
    }
    for slug in DelimiterChoice::PICKER {
      if slug != CUSTOM_SLUG && slug != "auto" {
        assert!(Delimiter::from_slug(slug).is_some(), "{slug}");
      }
    }
  }

  #[test]
  fn a_picked_separator_takes_the_quote_that_reads_best() {
    let text = "a;b\n'x; y';1\n'z; w';2\n";
    assert_eq!(dialect_for(text, Delimiter::Semicolon).quote, Quote::Single);
    assert_eq!(
      dialect_for("a;b\n1;2\n", Delimiter::Semicolon).quote,
      Quote::Double
    );
  }
}
