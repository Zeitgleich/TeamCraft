//! Emoji picker for reactions and the composer: all of Unicode in its groups with a tab per group,
//! search, frequently used first, skin tones and keyboard navigation. The grid is virtualised: only
//! the rows in view (and a few around them) are in the page.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::areas::{self, Place, lazy_area};
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::{PopoverBackdrop, Sheet};
use crate::emoji_catalog::{self, CatalogEmoji, EmojiGroup, EmojiVersion, SkinTone};
use crate::emoji_support::{can_draw, can_draw_skin_tones, newest_drawable};
use crate::i18n::use_i18n;
use crate::motion::PopoverSide;
use crate::state::use_app_state;

/// Emoji per row of the grid; ↑/↓ move by rows.
const COLUMNS: usize = 9;
/// The picker's size in rem (`.emoji-picker`, style/main/550-emoji-picker.css): it grows with the
/// text size.
const PICKER_WIDTH_REM: f64 = 22.5;
const PICKER_HEIGHT_REM: f64 = 26.0;
/// Heights of a grid row and a section title in rem (`.emoji-row`, `.emoji-section`).
const ROW_REM: f64 = 2.375;
const SECTION_REM: f64 = 1.75;
/// Rendered beyond the visible part of the grid, so a fast scroll shows no empty rows.
const OVERSCAN_PX: f64 = 320.0;
/// The scroller's height until it is measured.
const FALLBACK_VIEWPORT_PX: f64 = 320.0;
/// Rows of "frequently used": recently used emoji, filled up with the team chat favorites.
const FREQUENT_ROWS: usize = 2;
/// Space kept between the picker and the window edge.
const EDGE_PX: f64 = 8.0;

/// A floating emoji picker next to `anchor` (the button that opened it). `on_pick` gets the chosen
/// emoji, `on_close` runs on Escape or a click outside; neither closes the picker by itself.
///
/// It loads the first time it opens (`areas::emoji_picker`).
#[component]
pub fn EmojiPicker(
  anchor: web_sys::Element,
  on_pick: Callback<String>,
  on_close: Callback<()>,
  /// Give the keyboard focus back to `anchor` when the picker goes away (the composer focuses its
  /// input itself instead).
  #[prop(default = true)]
  returns_focus: bool,
) -> impl IntoView {
  lazy_area(Place::Popover, move || {
    areas::emoji_picker(anchor.clone(), on_pick, on_close, returns_focus)
  })
}

/// The picker of [`EmojiPicker`].
#[component]
pub fn EmojiPickerPanel(
  anchor: web_sys::Element,
  on_pick: Callback<String>,
  on_close: Callback<()>,
  returns_focus: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let active = RwSignal::new(0_usize);
  let scroll_top = RwSignal::new(0.0_f64);
  let viewport_px = RwSignal::new(FALLBACK_VIEWPORT_PX);
  let is_choosing_tone = RwSignal::new(false);
  let input = NodeRef::<html::Input>::new();
  let picker = NodeRef::<html::Div>::new();
  let scroller = NodeRef::<html::Div>::new();
  let (top, left, side) = placement(&anchor);
  let maybe_newest = newest_drawable();
  let rem = crate::viewport::rem_px();
  let metrics = Metrics {
    row_px: ROW_REM * rem,
    section_px: SECTION_REM * rem,
  };
  if returns_focus {
    let anchor = StoredValue::new_local(anchor.clone());
    on_cleanup(move || {
      anchor.try_with_value(|anchor| {
        if let Some(element) = anchor.dyn_ref::<web_sys::HtmlElement>() {
          drop(element.focus());
        }
      });
    });
  }

  let layout = Memo::new(move |_| {
    let tone = state.settings.emoji_skin_tone.get();
    query.with(|query| {
      state
        .settings
        .recent_emoji
        .with(|recent| Layout::build(query, recent, tone, maybe_newest, metrics))
    })
  });
  let visible_rows = Memo::new(move |_| {
    let (from, to) = (
      scroll_top.get() - OVERSCAN_PX,
      scroll_top.get() + viewport_px.get() + OVERSCAN_PX,
    );
    layout.with(|layout| layout.rows_between(from, to))
  });
  let current_section = Memo::new(move |_| layout.with(|layout| layout.section_at(scroll_top.get())));
  let active_emoji = Memo::new(move |_| layout.with(|layout| layout.cells.get(active.get()).cloned()));

  let scroll_to = move |position: f64| {
    if let Some(element) = scroller.get_untracked() {
      element.set_scroll_top(position.max(0.0) as i32);
    }
    scroll_top.set(position.max(0.0));
  };
  Effect::new(move |_| {
    query.track();
    active.set(0);
    scroll_to(0.0);
  });
  // On a phone the sheet opens without the on-screen keyboard; the search is a tap away.
  let is_sheet = state.layout.get_untracked().is_compact();
  Effect::new(move |_| {
    if let Some(element) = input.get()
      && !is_sheet
    {
      drop(element.focus());
    }
  });
  Effect::new(move |_| {
    if let Some(element) = scroller.get() {
      viewport_px.set(f64::from(element.client_height()));
    }
  });

  let pick = move |emoji: String| {
    state.settings.remember_emoji(&emoji);
    on_pick.run(emoji);
  };
  let reveal = move |index: usize| {
    let Some((row_top, row_bottom)) = layout.with_untracked(|layout| layout.cell_bounds(index)) else {
      return;
    };
    let (top, height) = (scroll_top.get_untracked(), viewport_px.get_untracked());
    if row_top < top {
      scroll_to(row_top);
    } else if row_bottom > top + height {
      scroll_to(row_bottom - height);
    }
  };
  let jump_to = move |section: Section| {
    if !query.get_untracked().is_empty() {
      query.set(String::new());
    }
    if let Some((position, first_cell)) = layout.with_untracked(|layout| layout.section_start(section)) {
      scroll_to(position);
      active.set(first_cell);
    }
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let at = active.get_untracked();
    let maybe_next = layout.with_untracked(|layout| match ev.key().as_str() {
      "ArrowRight" => Some((at + 1).min(layout.cells.len().saturating_sub(1))),
      "ArrowLeft" => Some(at.saturating_sub(1)),
      "ArrowDown" => layout.vertical_neighbour(at, Direction::Down),
      "ArrowUp" => layout.vertical_neighbour(at, Direction::Up),
      _ => None,
    });
    match ev.key().as_str() {
      "Enter" => {
        ev.prevent_default();
        if let Some(emoji) = active_emoji.get_untracked() {
          pick(emoji);
        }
      },
      "Escape" => {
        ev.prevent_default();
        ev.stop_propagation();
        if is_choosing_tone.get_untracked() {
          is_choosing_tone.set(false);
        } else {
          on_close.run(());
        }
      },
      _ => {},
    }
    if let Some(next) = maybe_next {
      ev.prevent_default();
      active.set(next);
      reveal(next);
    }
  };
  let on_scroll = move |_| {
    if let Some(element) = scroller.get_untracked() {
      scroll_top.set(f64::from(element.scroll_top()));
      viewport_px.set(f64::from(element.client_height()));
    }
  };

  let tabs = Section::TABS
    .into_iter()
    .map(|section| {
      let is_current = move || query.with(String::is_empty) && current_section.get() == Some(section);
      view! {
        <button
          type="button"
          class="emoji-tab"
          class:current=is_current
          aria-current=move || is_current().then_some("true")
          aria-label=move || i18n.t(section.label_key())
          title=move || i18n.t(section.label_key())
          on:mousedown=move |ev| ev.prevent_default()
          on:click=move |_| jump_to(section)
        >
          <Icon name=section.icon() />
        </button>
      }
    })
    .collect_view();

  let rows = move || {
    let list = visible_rows.get();
    if layout.with(|layout| layout.cells.is_empty()) {
      return view! { <li class="emoji-empty" role="presentation">{move || i18n.t("emoji-none")}</li> }
        .into_any();
    }
    list
      .into_iter()
      .map(|(top, row)| match row {
        Row::Title(section) => view! {
          <li class="emoji-section" role="presentation" style:transform=format!("translateY({top}px)")>
            {move || i18n.t(section.label_key())}
          </li>
        }
        .into_any(),
        Row::Emoji { first, emoji } => view! {
          <li class="emoji-row" role="presentation" style:transform=format!("translateY({top}px)")>
            {emoji
              .into_iter()
              .enumerate()
              .map(|(column, emoji)| {
                let index = first + column;
                let label = emoji_catalog::shortcode(&emoji).map_or_else(|| emoji.clone(), |code| format!(":{code}:"));
                let chosen = emoji.clone();
                view! {
                  <span
                    id=format!("emoji-opt-{index}")
                    class="emoji-cell"
                    role="option"
                    aria-label=label
                    aria-selected=move || (active.get() == index).to_string()
                    class:active=move || active.get() == index
                    on:mousemove=move |_| active.set(index)
                    on:mousedown=move |ev| ev.prevent_default()
                    on:click=move |_| pick(chosen.clone())
                  >
                    {emoji}
                  </span>
                }
              })
              .collect_view()}
          </li>
        }
        .into_any(),
      })
      .collect_view()
      .into_any()
  };

  let tone_chooser = move || {
    SkinTone::ALL
      .into_iter()
      .map(|tone| {
        let is_chosen = move || state.settings.emoji_skin_tone.get() == tone;
        view! {
          <button
            type="button"
            role="radio"
            class=format!("emoji-tone-swatch tone-{}", tone.slug())
            aria-checked=move || is_chosen().to_string()
            aria-label=move || i18n.t(&format!("emoji-tone-{}", tone.slug()))
            title=move || i18n.t(&format!("emoji-tone-{}", tone.slug()))
            on:mousedown=move |ev| ev.prevent_default()
            on:click=move |_| {
              state.settings.emoji_skin_tone.set(tone);
              is_choosing_tone.set(false);
            }
          />
        }
      })
      .collect_view()
  };

  let content = view! {
      <div class="emoji-tabs" role="toolbar" aria-label=move || i18n.t("emoji-categories")>{tabs}</div>
      <input
        class="input emoji-search"
        type="text"
        role="combobox"
        aria-expanded="true"
        aria-controls="emoji-grid"
        aria-activedescendant=move || format!("emoji-opt-{}", active.get())
        aria-label=move || i18n.t("emoji-search")
        placeholder=move || i18n.t("emoji-search")
        autocomplete="off"
        spellcheck="false"
        node_ref=input
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
        on:keydown=on_keydown
      />
      <div class="emoji-scroller" node_ref=scroller on:scroll=on_scroll>
        <ul
          id="emoji-grid"
          class="emoji-grid"
          role="listbox"
          aria-label=move || i18n.t("emoji-picker-title")
          style:height=move || format!("{}px", layout.with(|layout| layout.total_px))
        >
          {rows}
        </ul>
      </div>
      <div class="emoji-footer">
        <Show
          when=move || is_choosing_tone.get()
          fallback=move || view! {
            <span class="emoji-preview" aria-hidden="true">{move || active_emoji.get()}</span>
            <span class="emoji-preview-code">
              {move || active_emoji.get().and_then(|emoji| emoji_catalog::shortcode(&emoji)).map(|code| format!(":{code}:"))}
            </span>
          }
        >
          <div class="emoji-tones" role="radiogroup" aria-label=move || i18n.t("emoji-skin-tone")>{tone_chooser}</div>
        </Show>
        <button
          type="button"
          class=move || format!("emoji-tone-button emoji-tone-swatch tone-{}", state.settings.emoji_skin_tone.get().slug())
          aria-expanded=move || is_choosing_tone.get().to_string()
          aria-label=move || i18n.t("emoji-skin-tone")
          title=move || i18n.t("emoji-skin-tone")
          on:mousedown=move |ev| ev.prevent_default()
          on:click=move |_| is_choosing_tone.update(|is_open| *is_open = !*is_open)
        />
      </div>
  };
  if is_sheet {
    return view! {
      <Sheet class="emoji-sheet" label=Signal::derive(move || i18n.t("emoji-picker-title")) on_close=on_close>
        {content}
      </Sheet>
    }
    .into_any();
  }
  view! {
    <PopoverBackdrop on_close=on_close popover=picker />
    <div
      class=format!("emoji-picker {}", side.class())
      node_ref=picker
      role="dialog"
      aria-label=move || i18n.t("emoji-picker-title")
      style:top=format!("{top}px")
      style:left=format!("{left}px")
    >
      {content}
    </div>
  }
  .into_any()
}

/// A part of the grid with a title, and the picker's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
  Frequent,
  Group(EmojiGroup),
}

/// A line of the virtualised grid.
#[derive(Clone, Debug, PartialEq)]
enum Row {
  Title(Section),
  /// Up to [`COLUMNS`] emoji; `first` is the first one's index among all cells.
  Emoji {
    first: usize,
    emoji: Vec<String>,
  },
}

/// What the grid shows, row by row, with each row's position.
#[derive(Clone, Debug, Default, PartialEq)]
struct Layout {
  rows: Vec<Row>,
  /// Top of each row in px.
  tops: Vec<f64>,
  total_px: f64,
  /// Every emoji in order, as picked (with the skin tone).
  cells: Vec<String>,
  /// The row of each cell.
  cell_rows: Vec<usize>,
  metrics: Metrics,
}

/// Row heights in px for the current text size.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
  row_px: f64,
  section_px: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
  Up,
  Down,
}

impl Section {
  const TABS: [Self; 10] = [
    Self::Frequent,
    Self::Group(EmojiGroup::SmileysAndEmotion),
    Self::Group(EmojiGroup::PeopleAndBody),
    Self::Group(EmojiGroup::AnimalsAndNature),
    Self::Group(EmojiGroup::FoodAndDrink),
    Self::Group(EmojiGroup::TravelAndPlaces),
    Self::Group(EmojiGroup::Activities),
    Self::Group(EmojiGroup::Objects),
    Self::Group(EmojiGroup::Symbols),
    Self::Group(EmojiGroup::Flags),
  ];

  fn label_key(self) -> &'static str {
    match self {
      Self::Frequent => "emoji-frequent",
      Self::Group(EmojiGroup::SmileysAndEmotion) => "emoji-group-smileys",
      Self::Group(EmojiGroup::PeopleAndBody) => "emoji-group-people",
      Self::Group(EmojiGroup::AnimalsAndNature) => "emoji-group-nature",
      Self::Group(EmojiGroup::FoodAndDrink) => "emoji-group-food",
      Self::Group(EmojiGroup::TravelAndPlaces) => "emoji-group-travel",
      Self::Group(EmojiGroup::Activities) => "emoji-group-activities",
      Self::Group(EmojiGroup::Objects) => "emoji-group-objects",
      Self::Group(EmojiGroup::Symbols) => "emoji-group-symbols",
      Self::Group(EmojiGroup::Flags) => "emoji-group-flags",
    }
  }

  fn icon(self) -> IconName {
    match self {
      Self::Frequent => IconName::Clock,
      Self::Group(EmojiGroup::SmileysAndEmotion) => IconName::Smile,
      Self::Group(EmojiGroup::PeopleAndBody) => IconName::User,
      Self::Group(EmojiGroup::AnimalsAndNature) => IconName::Leaf,
      Self::Group(EmojiGroup::FoodAndDrink) => IconName::Coffee,
      Self::Group(EmojiGroup::TravelAndPlaces) => IconName::Car,
      Self::Group(EmojiGroup::Activities) => IconName::Ball,
      Self::Group(EmojiGroup::Objects) => IconName::Bulb,
      Self::Group(EmojiGroup::Symbols) => IconName::Heart,
      Self::Group(EmojiGroup::Flags) => IconName::Flag,
    }
  }
}

impl Layout {
  /// The grid for `query`: search results without titles, or "frequently used" and every group.
  /// Emoji the font can't draw are left out; `tone` goes on those that have skin tones.
  fn build(
    query: &str,
    recent: &[String],
    tone: SkinTone,
    maybe_newest: Option<EmojiVersion>,
    metrics: Metrics,
  ) -> Self {
    let mut layout = Self {
      metrics,
      ..Self::default()
    };
    let shown = |entry: &&CatalogEmoji| can_draw(entry, maybe_newest);
    let toned = |entry: &CatalogEmoji| {
      if can_draw_skin_tones(entry, maybe_newest) {
        entry.with_skin_tone(tone).to_owned()
      } else {
        entry.emoji.to_owned()
      }
    };
    if !query.trim().is_empty() {
      let found = emoji_catalog::search(query).into_iter().filter(shown).map(toned);
      layout.push_section(None, found.collect());
      return layout;
    }
    layout.push_section(Some(Section::Frequent), frequent(recent, maybe_newest, toned));
    for group in EmojiGroup::ALL {
      let emoji = emoji_catalog::group(group).filter(shown).map(toned).collect();
      layout.push_section(Some(Section::Group(group)), emoji);
    }
    layout
  }

  /// The rows that overlap `from..to` (px), with their tops.
  fn rows_between(&self, from: f64, to: f64) -> Vec<(f64, Row)> {
    let start = self.tops.partition_point(|top| *top + self.metrics.row_px < from);
    self
      .tops
      .iter()
      .zip(&self.rows)
      .skip(start)
      .take_while(|(top, _)| **top < to)
      .map(|(top, row)| (*top, row.clone()))
      .collect()
  }

  /// The section whose part of the grid is at `position` (px) of the scroller.
  fn section_at(&self, position: f64) -> Option<Section> {
    self
      .tops
      .iter()
      .zip(&self.rows)
      .take_while(|(top, _)| **top <= position + 1.0)
      .filter_map(|(_, row)| match row {
        Row::Title(section) => Some(*section),
        Row::Emoji { .. } => None,
      })
      .last()
  }

  /// Where `section` starts (px) and its first cell.
  fn section_start(&self, section: Section) -> Option<(f64, usize)> {
    let index = self.rows.iter().position(|row| *row == Row::Title(section))?;
    let first_cell = self.rows[index..].iter().find_map(|row| match row {
      Row::Emoji { first, .. } => Some(*first),
      Row::Title(_) => None,
    })?;
    Some((self.tops[index], first_cell))
  }

  /// Top and bottom (px) of the row of cell `index`; with the title above for a section's first row.
  fn cell_bounds(&self, index: usize) -> Option<(f64, f64)> {
    let row = *self.cell_rows.get(index)?;
    let top = match row.checked_sub(1).map(|above| &self.rows[above]) {
      Some(Row::Title(_)) => self.tops[row - 1],
      _ => self.tops[row],
    };
    Some((top, self.tops[row] + self.metrics.row_px))
  }

  /// The cell above or below `index` in the same column (or the last one of a shorter row).
  fn vertical_neighbour(&self, index: usize, direction: Direction) -> Option<usize> {
    let row = *self.cell_rows.get(index)?;
    let Row::Emoji { first, .. } = &self.rows[row] else {
      return None;
    };
    let column = index - first;
    let mut candidates: Box<dyn Iterator<Item = &Row>> = match direction {
      Direction::Down => Box::new(self.rows.iter().skip(row + 1)),
      Direction::Up => Box::new(self.rows[..row].iter().rev()),
    };
    candidates.find_map(|candidate| match candidate {
      Row::Emoji { first, emoji } => Some(first + column.min(emoji.len().saturating_sub(1))),
      Row::Title(_) => None,
    })
  }

  fn push_section(&mut self, maybe_section: Option<Section>, emoji: Vec<String>) {
    if emoji.is_empty() {
      return;
    }
    if let Some(section) = maybe_section {
      self.push_row(Row::Title(section), self.metrics.section_px);
    }
    for chunk in emoji.chunks(COLUMNS) {
      let first = self.cells.len();
      let row = self.rows.len();
      self.cells.extend(chunk.iter().cloned());
      self.cell_rows.extend(std::iter::repeat_n(row, chunk.len()));
      self.push_row(
        Row::Emoji {
          first,
          emoji: chunk.to_vec(),
        },
        self.metrics.row_px,
      );
    }
  }

  fn push_row(&mut self, row: Row, height: f64) {
    self.tops.push(self.total_px);
    self.rows.push(row);
    self.total_px += height;
  }
}

/// "Frequently used": the recently used emoji as they were picked, then the team chat favorites
/// in `toned` form, without repeating an emoji in another skin tone.
fn frequent(
  recent: &[String],
  maybe_newest: Option<EmojiVersion>,
  toned: impl Fn(&CatalogEmoji) -> String,
) -> Vec<String> {
  let limit = FREQUENT_ROWS * COLUMNS;
  let base =
    |emoji: &str| emoji_catalog::get(emoji).map_or_else(|| emoji.to_owned(), |entry| entry.emoji.to_owned());
  let mut list: Vec<String> = recent.iter().take(limit).cloned().collect();
  let mut seen: Vec<String> = list.iter().map(|emoji| base(emoji)).collect();
  for entry in emoji_catalog::favorites().filter(|entry| can_draw(entry, maybe_newest)) {
    if list.len() >= limit {
      break;
    }
    if !seen.iter().any(|emoji| emoji == entry.emoji) {
      seen.push(entry.emoji.to_owned());
      list.push(toned(entry));
    }
  }
  list
}

/// Fixed position (top, left) that keeps the picker inside the window: above the anchor when there is
/// room, otherwise below it; right-aligned with it.
fn placement(anchor: &web_sys::Element) -> (f64, f64, PopoverSide) {
  let rect = anchor.get_bounding_client_rect();
  let rem = crate::viewport::rem_px();
  let (picker_width, picker_height) = (PICKER_WIDTH_REM * rem, PICKER_HEIGHT_REM * rem);
  let (width, height) = web_sys::window()
    .map(|window| {
      let read = |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0)
      };
      (read(window.inner_width()), read(window.inner_height()))
    })
    .unwrap_or((1024.0, 768.0));
  let (top, side) = if rect.top() - picker_height - EDGE_PX >= 0.0 {
    (rect.top() - picker_height - 4.0, PopoverSide::Above)
  } else {
    (
      (rect.bottom() + 4.0).min(height - picker_height - EDGE_PX),
      PopoverSide::Below,
    )
  };
  let left = (rect.right() - picker_width).clamp(EDGE_PX, (width - picker_width - EDGE_PX).max(EDGE_PX));
  (top.max(EDGE_PX), left, side)
}

#[cfg(test)]
mod tests {
  use super::*;

  const METRICS: Metrics = Metrics {
    row_px: 38.0,
    section_px: 28.0,
  };

  #[test]
  fn every_group_gets_a_titled_section() {
    let layout = Layout::build("", &[], SkinTone::Default, None, METRICS);
    for section in Section::TABS {
      assert!(layout.section_start(section).is_some(), "{section:?}");
    }
    assert_eq!(
      layout.cells.len(),
      FREQUENT_ROWS * COLUMNS + emoji_catalog::all().len()
    );
  }

  #[test]
  fn frequent_starts_with_recent_and_fills_up_with_favorites() {
    let recent = vec!["🦄".to_owned(), "👍🏽".to_owned()];
    let layout = Layout::build("", &recent, SkinTone::Default, None, METRICS);
    assert_eq!(&layout.cells[..3], ["🦄", "👍🏽", "👎"]);
  }

  #[test]
  fn search_has_no_titles_and_uses_the_skin_tone() {
    let layout = Layout::build("wave", &[], SkinTone::Dark, None, METRICS);
    assert!(layout.rows.iter().all(|row| matches!(row, Row::Emoji { .. })));
    assert_eq!(layout.cells.first().map(String::as_str), Some("👋🏿"));
  }

  #[test]
  fn emoji_the_font_cant_draw_are_left_out() {
    let old_font = Some(EmojiVersion::new(13, 0));
    let layout = Layout::build("melting", &[], SkinTone::Default, old_font, METRICS);
    assert!(!layout.cells.iter().any(|emoji| emoji == "🫠"));
  }

  #[test]
  fn only_rows_in_view_are_rendered() {
    let layout = Layout::build("", &[], SkinTone::Default, None, METRICS);
    let rows = layout.rows_between(1_000.0, 1_400.0);
    assert!(rows.len() < 15, "{}", rows.len());
    assert!(
      rows
        .iter()
        .all(|(top, _)| *top + METRICS.row_px >= 1_000.0 && *top < 1_400.0)
    );
    assert_eq!(layout.section_at(0.0), Some(Section::Frequent));
    let (people_top, _) = layout
      .section_start(Section::Group(EmojiGroup::PeopleAndBody))
      .expect("people");
    assert_eq!(
      layout.section_at(people_top),
      Some(Section::Group(EmojiGroup::PeopleAndBody))
    );
  }

  #[test]
  fn arrows_move_by_rows_across_sections() {
    let layout = Layout::build("", &[], SkinTone::Default, None, METRICS);
    assert_eq!(layout.vertical_neighbour(0, Direction::Down), Some(COLUMNS));
    assert_eq!(layout.vertical_neighbour(COLUMNS, Direction::Up), Some(0));
    assert_eq!(layout.vertical_neighbour(0, Direction::Up), None);
    // From the last row of "frequently used" into the first row of smileys.
    let last_frequent = FREQUENT_ROWS * COLUMNS - 1;
    assert_eq!(
      layout.vertical_neighbour(last_frequent, Direction::Down),
      Some(last_frequent + COLUMNS)
    );
  }
}
