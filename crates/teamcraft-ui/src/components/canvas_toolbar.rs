//! The formatting toolbar of the shared notes (issue #1456): the composer's buttons and shortcuts
//! with a checklist and undo and redo added.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::canvas::{BlockKind, InlineMark};

use crate::components::composer_toolbar::{
  FormatAction, action_label, format_action_for, move_toolbar_focus,
};
use crate::components::icons::{Icon, IconName};
use crate::components::sheet::MenuSurface;
use crate::editor_doc::{BlockFormat, InlineStyle};
use crate::i18n::{I18n, use_i18n};
use crate::keys::{Key, Modifier, Platform, Shortcut, shortcut_text};

/// What a toolbar button or a shortcut of the notes does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasAction {
  Format(FormatAction),
  Check,
  Undo,
  Redo,
}

/// Besides the composer's formatting shortcuts.
const CHECK_SHORTCUT: Shortcut = Shortcut::new(&[Modifier::Primary, Modifier::Alt], Key::Char("4"));
const UNDO_SHORTCUT: Shortcut = Shortcut::primary("Z");
const REDO_SHORTCUT: Shortcut = Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Char("Z"));
const REDO_SHORTCUT_OTHER: Shortcut = Shortcut::primary("Y");

/// The styles of the text style menu.
const TEXT_STYLES: [BlockKind; 4] = [
  BlockKind::Paragraph,
  BlockKind::Heading,
  BlockKind::Heading2,
  BlockKind::Heading3,
];

/// What the toolbar shows: the formatting at the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanvasActive {
  /// The kind of the selected block; `None` before any is selected.
  pub maybe_kind: Option<BlockKind>,
  pub marks: [bool; InlineMark::ALL.len()],
  pub can_undo: bool,
  pub can_redo: bool,
}

impl CanvasActive {
  fn has(&self, mark: InlineMark) -> bool {
    InlineMark::ALL
      .iter()
      .position(|candidate| *candidate == mark)
      .is_some_and(|index| self.marks[index])
  }

  fn allows_inline(&self) -> bool {
    self.maybe_kind.is_some_and(|kind| !kind.is_code())
  }
}

/// The notes' action a key press is a shortcut for.
pub fn canvas_action_for(ev: &web_sys::KeyboardEvent) -> Option<CanvasAction> {
  let platform = Platform::current();
  let matches = |shortcut: Shortcut| shortcut.matches(ev, platform);
  if matches(UNDO_SHORTCUT) {
    Some(CanvasAction::Undo)
  } else if matches(REDO_SHORTCUT) || matches(REDO_SHORTCUT_OTHER) {
    Some(CanvasAction::Redo)
  } else if matches(CHECK_SHORTCUT) {
    Some(CanvasAction::Check)
  } else {
    format_action_for(ev).map(CanvasAction::Format)
  }
}

/// The mark of a composer formatting; the notes have no underline.
pub fn mark_of(style: InlineStyle) -> Option<InlineMark> {
  match style {
    InlineStyle::Bold => Some(InlineMark::Bold),
    InlineStyle::Italic => Some(InlineMark::Italic),
    InlineStyle::Strike => Some(InlineMark::Strike),
    InlineStyle::Code => Some(InlineMark::Code),
    InlineStyle::Underline => None,
  }
}

/// The block kind a composer block format is.
pub fn kind_of(format: BlockFormat) -> BlockKind {
  match format {
    BlockFormat::Paragraph => BlockKind::Paragraph,
    BlockFormat::Heading(1) => BlockKind::Heading,
    BlockFormat::Heading(2) => BlockKind::Heading2,
    BlockFormat::Heading(_) => BlockKind::Heading3,
    BlockFormat::Bulleted => BlockKind::Bullet,
    BlockFormat::Numbered => BlockKind::Numbered,
    BlockFormat::Quote => BlockKind::Quote,
    BlockFormat::CodeBlock => BlockKind::Code,
  }
}

/// A toolbar in the ARIA sense: Tab reaches it once, the arrow keys move between its buttons (the
/// composer's); a button that applies to nothing stays focusable and says so.
#[component]
pub fn CanvasToolbar(active: Memo<CanvasActive>, on_action: Callback<CanvasAction>) -> impl IntoView {
  let i18n = use_i18n();
  let toolbar = NodeRef::<html::Div>::new();
  let is_style_menu_open = RwSignal::new(false);
  let mark_button = move |style: InlineStyle, content: AnyView, mark: InlineMark| {
    tool_button(
      i18n,
      CanvasAction::Format(FormatAction::Inline(style)),
      content,
      Signal::derive(move || active.get().has(mark)),
      Signal::derive(move || active.get().allows_inline()),
      on_action,
      style == InlineStyle::Bold,
    )
  };
  let kind_button = move |action: CanvasAction, icon: IconName, kind: BlockKind| {
    tool_button(
      i18n,
      action,
      view! { <Icon name=icon /> }.into_any(),
      Signal::derive(move || active.get().maybe_kind == Some(kind)),
      Signal::derive(move || active.get().maybe_kind.is_some()),
      on_action,
      false,
    )
  };
  let style_kind = move || {
    active
      .get()
      .maybe_kind
      .filter(|kind| TEXT_STYLES.contains(kind))
      .unwrap_or(BlockKind::Paragraph)
  };
  view! {
    <div
      class="format-toolbar canvas-toolbar"
      role="toolbar"
      aria-label=move || i18n.t("canvas-toolbar")
      node_ref=toolbar
      on:keydown=move |ev| move_toolbar_focus(&ev, toolbar)
    >
      {mark_button(InlineStyle::Bold, glyph("B", "glyph-bold"), InlineMark::Bold)}
      {mark_button(InlineStyle::Italic, glyph("I", "glyph-italic"), InlineMark::Italic)}
      {mark_button(InlineStyle::Strike, glyph("S", "glyph-strike"), InlineMark::Strike)}
      {mark_button(InlineStyle::Code, view! { <Icon name=IconName::Code /> }.into_any(), InlineMark::Code)}
      {tool_button(
        i18n,
        CanvasAction::Format(FormatAction::Link),
        view! { <Icon name=IconName::Link /> }.into_any(),
        Signal::derive(|| false),
        Signal::derive(move || active.get().allows_inline()),
        on_action,
        false,
      )}
      <span class="format-sep" aria-hidden="true"></span>
      <div class="format-style">
        <button
          type="button"
          class="format-btn format-style-btn"
          tabindex="-1"
          aria-haspopup="menu"
          aria-expanded=move || is_style_menu_open.get().to_string()
          aria-disabled=move || active.get().maybe_kind.is_none().then_some("true")
          aria-label=move || format!("{}: {}", i18n.t("format-style"), i18n.t(kind_label(style_kind())))
          title=move || i18n.t("format-style")
          on:mousedown=|ev| ev.prevent_default()
          on:click=move |_| {
            if active.get_untracked().maybe_kind.is_some() {
              is_style_menu_open.update(|open| *open = !*open);
            }
          }
        >
          <span>{move || i18n.t(kind_label(style_kind()))}</span>
          <Icon name=IconName::ChevronDown />
        </button>
        <Show when=move || is_style_menu_open.get() fallback=|| ()>
          <MenuSurface
            class="format-style-menu align-start"
            sheet_class="format-style-menu"
            label=Signal::derive(move || i18n.t("format-style"))
            on_close=Callback::new(move |()| is_style_menu_open.set(false))
            on_keydown=Callback::new(move |ev: web_sys::KeyboardEvent| {
              if ev.key() == "Escape" {
                ev.prevent_default();
                is_style_menu_open.set(false);
              }
            })
          >
            {TEXT_STYLES
              .into_iter()
              .map(|kind| {
                view! {
                  <button
                    type="button"
                    class=format!("menu-item format-style-{}", style_class(kind))
                    role="menuitemradio"
                    aria-checked=move || (active.get().maybe_kind == Some(kind)).to_string()
                    on:mousedown=|ev| ev.prevent_default()
                    on:click=move |_| {
                      is_style_menu_open.set(false);
                      on_action.run(CanvasAction::Format(FormatAction::Block(format_of(kind))));
                    }
                  >
                    <span class="menu-label">{move || i18n.t(kind_label(kind))}</span>
                  </button>
                }
              })
              .collect_view()}
          </MenuSurface>
        </Show>
      </div>
      <span class="format-sep" aria-hidden="true"></span>
      {kind_button(CanvasAction::Format(FormatAction::Block(BlockFormat::Bulleted)), IconName::List, BlockKind::Bullet)}
      {kind_button(CanvasAction::Format(FormatAction::Block(BlockFormat::Numbered)), IconName::ListNumbered, BlockKind::Numbered)}
      {kind_button(CanvasAction::Check, IconName::Check, BlockKind::Check)}
      {kind_button(CanvasAction::Format(FormatAction::Block(BlockFormat::Quote)), IconName::Quote, BlockKind::Quote)}
      {kind_button(CanvasAction::Format(FormatAction::Block(BlockFormat::CodeBlock)), IconName::CodeBlock, BlockKind::Code)}
      <span class="format-sep" aria-hidden="true"></span>
      {history_button(i18n, CanvasAction::Undo, IconName::Undo, Signal::derive(move || active.get().can_undo), on_action)}
      {history_button(i18n, CanvasAction::Redo, IconName::Redo, Signal::derive(move || active.get().can_redo), on_action)}
    </div>
  }
}

/// A block's kind as the page's toolbar and fields name it.
pub fn kind_label(kind: BlockKind) -> &'static str {
  match kind {
    BlockKind::Paragraph => "canvas-kind-paragraph",
    BlockKind::Heading => "format-heading-1",
    BlockKind::Heading2 => "format-heading-2",
    BlockKind::Heading3 => "format-heading-3",
    BlockKind::Bullet => "canvas-kind-bullet",
    BlockKind::Numbered => "canvas-kind-numbered",
    BlockKind::Check => "canvas-kind-check",
    BlockKind::Quote => "format-quote",
    BlockKind::Code => "format-code-block",
  }
}

fn glyph(letter: &'static str, class: &'static str) -> AnyView {
  view! { <span class=class aria-hidden="true">{letter}</span> }.into_any()
}

fn format_of(kind: BlockKind) -> BlockFormat {
  match kind {
    BlockKind::Heading => BlockFormat::Heading(1),
    BlockKind::Heading2 => BlockFormat::Heading(2),
    BlockKind::Heading3 => BlockFormat::Heading(3),
    BlockKind::Bullet => BlockFormat::Bulleted,
    BlockKind::Numbered => BlockFormat::Numbered,
    BlockKind::Quote => BlockFormat::Quote,
    BlockKind::Code => BlockFormat::CodeBlock,
    BlockKind::Paragraph | BlockKind::Check => BlockFormat::Paragraph,
  }
}

fn style_class(kind: BlockKind) -> &'static str {
  match kind {
    BlockKind::Heading => "h1",
    BlockKind::Heading2 => "h2",
    BlockKind::Heading3 => "h3",
    _ => "p",
  }
}

fn action_shortcut(action: CanvasAction) -> Option<Shortcut> {
  match action {
    CanvasAction::Undo => Some(UNDO_SHORTCUT),
    CanvasAction::Redo => Some(REDO_SHORTCUT),
    CanvasAction::Check => Some(CHECK_SHORTCUT),
    CanvasAction::Format(format) => crate::components::composer_toolbar::shortcut_for(format),
  }
}

fn label_of(action: CanvasAction) -> &'static str {
  match action {
    CanvasAction::Format(format) => action_label(format),
    CanvasAction::Check => "canvas-kind-check",
    CanvasAction::Undo => "canvas-undo",
    CanvasAction::Redo => "canvas-redo",
  }
}

fn history_button(
  i18n: I18n,
  action: CanvasAction,
  icon: IconName,
  is_enabled: Signal<bool>,
  on_action: Callback<CanvasAction>,
) -> impl IntoView {
  tool_button(
    i18n,
    action,
    view! { <Icon name=icon /> }.into_any(),
    Signal::derive(|| false),
    is_enabled,
    on_action,
    false,
  )
}

fn tool_button(
  i18n: I18n,
  action: CanvasAction,
  content: AnyView,
  is_pressed: Signal<bool>,
  is_enabled: Signal<bool>,
  on_action: Callback<CanvasAction>,
  is_first: bool,
) -> impl IntoView {
  let label = label_of(action);
  let maybe_shortcut = action_shortcut(action);
  let is_toggle = !matches!(
    action,
    CanvasAction::Undo | CanvasAction::Redo | CanvasAction::Format(FormatAction::Link)
  );
  let tooltip = move || match maybe_shortcut {
    Some(shortcut) => format!("{} ({})", i18n.t(label), shortcut_text(&shortcut, i18n.locale())),
    None => i18n.t(label),
  };
  view! {
    <button
      type="button"
      class="format-btn"
      tabindex=if is_first { "0" } else { "-1" }
      aria-label=move || i18n.t(label)
      aria-pressed=move || is_toggle.then(|| is_pressed.get().to_string())
      aria-disabled=move || (!is_enabled.get()).then_some("true")
      aria-keyshortcuts=maybe_shortcut.map(|shortcut| shortcut.aria(Platform::current()))
      title=tooltip
      // Keeps the caret in the block the button changes.
      on:mousedown=|ev| ev.prevent_default()
      on:click=move |_| {
        if is_enabled.get_untracked() {
          on_action.run(action);
        }
      }
    >
      {content}
    </button>
  }
}
