//! The composer's formatting toolbar and its keyboard shortcuts, and the send options menu.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_client_api::actions::MessagePriority;

use crate::components::icons::{Icon, IconName};
use crate::editor_doc::{BlockFormat, InlineStyle};
use crate::i18n::{I18n, use_i18n};
use crate::keys::{Key, Modifier, Platform, Shortcut, shortcut_text};

/// What a toolbar button or formatting shortcut does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatAction {
  Inline(InlineStyle),
  Block(BlockFormat),
  Link,
}

/// Formatting shortcuts (Slack's where it has one: Ctrl+K stays the quick switcher).
pub const FORMAT_SHORTCUTS: [(FormatAction, Shortcut); 14] = [
  (FormatAction::Inline(InlineStyle::Bold), Shortcut::primary("B")),
  (FormatAction::Inline(InlineStyle::Italic), Shortcut::primary("I")),
  (
    FormatAction::Inline(InlineStyle::Underline),
    Shortcut::primary("U"),
  ),
  (FormatAction::Inline(InlineStyle::Strike), with_shift("X")),
  (FormatAction::Link, with_shift("U")),
  (FormatAction::Block(BlockFormat::Numbered), with_shift("7")),
  (FormatAction::Block(BlockFormat::Bulleted), with_shift("8")),
  (FormatAction::Block(BlockFormat::Quote), with_shift("9")),
  (FormatAction::Inline(InlineStyle::Code), Shortcut::primary("E")),
  (FormatAction::Block(BlockFormat::CodeBlock), with_alt("C")),
  (FormatAction::Block(BlockFormat::Paragraph), with_alt("0")),
  (FormatAction::Block(BlockFormat::Heading(1)), with_alt("1")),
  (FormatAction::Block(BlockFormat::Heading(2)), with_alt("2")),
  (FormatAction::Block(BlockFormat::Heading(3)), with_alt("3")),
];

/// Text styles in the style menu.
const TEXT_STYLES: [BlockFormat; 4] = [
  BlockFormat::Paragraph,
  BlockFormat::Heading(1),
  BlockFormat::Heading(2),
  BlockFormat::Heading(3),
];

/// The formatting at the caret, for the buttons' pressed state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveFormats {
  pub inline: [bool; 5],
  pub block: BlockFormat,
}

impl Default for ActiveFormats {
  fn default() -> Self {
    Self {
      inline: [false; 5],
      block: BlockFormat::Paragraph,
    }
  }
}

impl ActiveFormats {
  pub fn has(&self, style: InlineStyle) -> bool {
    self.inline[inline_index(style)]
  }

  pub fn set(&mut self, style: InlineStyle, is_active: bool) {
    self.inline[inline_index(style)] = is_active;
  }
}

/// The formatting action a key press is a shortcut for.
pub fn format_action_for(ev: &web_sys::KeyboardEvent) -> Option<FormatAction> {
  let platform = Platform::current();
  FORMAT_SHORTCUTS
    .iter()
    .find(|(_, shortcut)| shortcut.matches(ev, platform))
    .map(|(action, _)| *action)
}

/// Bold, italic, … above the input; buttons keep the editor's focus and selection.
#[component]
pub fn FormatToolbar(
  #[prop(into)] active: Signal<ActiveFormats>,
  on_action: Callback<FormatAction>,
) -> impl IntoView {
  let i18n = use_i18n();
  let toolbar = NodeRef::<html::Div>::new();
  let is_style_menu_open = RwSignal::new(false);
  let button =
    move |action: FormatAction, glyph: Glyph| format_button(i18n, active, on_action, action, glyph);

  view! {
    <div
      class="format-toolbar"
      role="toolbar"
      aria-label=move || i18n.t("composer-format")
      node_ref=toolbar
      on:keydown=move |ev| move_focus(&ev, toolbar)
    >
      {button(FormatAction::Inline(InlineStyle::Bold), Glyph::Text("B", "glyph-bold"))}
      {button(FormatAction::Inline(InlineStyle::Italic), Glyph::Text("I", "glyph-italic"))}
      {button(FormatAction::Inline(InlineStyle::Underline), Glyph::Text("U", "glyph-underline"))}
      {button(FormatAction::Inline(InlineStyle::Strike), Glyph::Text("S", "glyph-strike"))}
      <span class="format-sep" aria-hidden="true"></span>
      <div class="format-style">
        <button
          type="button"
          class="format-btn format-style-btn"
          tabindex="-1"
          aria-haspopup="menu"
          aria-expanded=move || is_style_menu_open.get().to_string()
          aria-label=move || format!("{}: {}", i18n.t("format-style"), i18n.t(style_label(active.get().block)))
          title=move || i18n.t("format-style")
          on:mousedown=|ev| ev.prevent_default()
          on:click=move |_| is_style_menu_open.update(|open| *open = !*open)
        >
          <span>{move || i18n.t(style_label(active.get().block))}</span>
          <Icon name=IconName::ChevronDown />
        </button>
        <Show when=move || is_style_menu_open.get() fallback=|| ()>
          <div class="popover-backdrop" on:mousedown=move |_| is_style_menu_open.set(false)></div>
          <div
            class="popover format-style-menu opens-up align-start"
            role="menu"
            aria-label=move || i18n.t("format-style")
            on:keydown=move |ev: web_sys::KeyboardEvent| {
              if ev.key() == "Escape" {
                ev.prevent_default();
                is_style_menu_open.set(false);
              }
            }
          >
            {TEXT_STYLES
              .into_iter()
              .map(|format| {
                let shortcut = shortcut_for(FormatAction::Block(format));
                view! {
                  <button
                    type="button"
                    class=format!("menu-item format-style-{}", style_class(format))
                    role="menuitemradio"
                    aria-checked=move || (active.get().block == format).to_string()
                    on:mousedown=|ev| ev.prevent_default()
                    on:click=move |_| {
                      is_style_menu_open.set(false);
                      on_action.run(FormatAction::Block(format));
                    }
                  >
                    <span class="menu-label">{move || i18n.t(style_label(format))}</span>
                    <small class="menu-shortcut">{move || shortcut.map(|shortcut| shortcut_text(&shortcut, i18n.locale()))}</small>
                  </button>
                }
              })
              .collect_view()}
          </div>
        </Show>
      </div>
      <span class="format-sep" aria-hidden="true"></span>
      {button(FormatAction::Link, Glyph::Icon(IconName::Link))}
      {button(FormatAction::Block(BlockFormat::Numbered), Glyph::Icon(IconName::ListNumbered))}
      {button(FormatAction::Block(BlockFormat::Bulleted), Glyph::Icon(IconName::List))}
      {button(FormatAction::Block(BlockFormat::Quote), Glyph::Icon(IconName::Quote))}
      <span class="format-sep" aria-hidden="true"></span>
      {button(FormatAction::Inline(InlineStyle::Code), Glyph::Icon(IconName::Code))}
      {button(FormatAction::Block(BlockFormat::CodeBlock), Glyph::Icon(IconName::CodeBlock))}
    </div>
  }
}

/// Send options behind the arrow next to the send button. They apply to the next message.
#[component]
pub fn SendOptionsMenu(
  priority: RwSignal<MessagePriority>,
  is_silent: RwSignal<bool>,
  wants_acknowledgement: RwSignal<bool>,
  on_close: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let menu = NodeRef::<html::Div>::new();
  Effect::new(move |_| {
    if let Some(first) = menu
      .get()
      .and_then(|menu| menu.query_selector("[role^=menuitem]").ok().flatten())
      .and_then(|first| first.dyn_into::<web_sys::HtmlElement>().ok())
    {
      drop(first.focus());
    }
  });
  let priority_item = move |value: MessagePriority, label: &'static str| {
    view! {
      <button
        type="button"
        class="menu-item"
        role="menuitemradio"
        aria-checked=move || (priority.get() == value).to_string()
        on:click=move |_| priority.set(value)
      >
        <span class="menu-label">{move || i18n.t(label)}</span>
        {move || (priority.get() == value).then(|| view! { <Icon name=IconName::Check /> })}
      </button>
    }
  };
  let toggle_item = move |flag: RwSignal<bool>, label: &'static str, detail: &'static str| {
    view! {
      <button
        type="button"
        class="menu-item"
        role="menuitemcheckbox"
        aria-checked=move || flag.get().to_string()
        on:click=move |_| flag.update(|on| *on = !*on)
      >
        <span class="menu-label">{move || i18n.t(label)}<small>{move || i18n.t(detail)}</small></span>
        {move || flag.get().then(|| view! { <Icon name=IconName::Check /> })}
      </button>
    }
  };

  view! {
    <div class="popover-backdrop" on:mousedown=move |_| on_close.run(())></div>
    <div
      class="popover send-options opens-up"
      role="menu"
      aria-label=move || i18n.t("composer-send-options")
      node_ref=menu
      on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
        "Escape" => {
          ev.prevent_default();
          on_close.run(());
        },
        "ArrowDown" | "ArrowUp" => {
          ev.prevent_default();
          move_focus_vertical(&ev, menu);
        },
        _ => {},
      }
    >
      <p class="menu-heading">{move || i18n.t("composer-priority")}</p>
      {priority_item(MessagePriority::Normal, "priority-normal")}
      {priority_item(MessagePriority::Important, "priority-important")}
      {priority_item(MessagePriority::Urgent, "priority-urgent")}
      <div class="menu-sep" role="separator"></div>
      {toggle_item(is_silent, "composer-silent", "composer-silent-detail")}
      {toggle_item(wants_acknowledgement, "composer-ack", "composer-ack-detail")}
    </div>
  }
}

/// What a toolbar button shows.
#[derive(Clone, Copy)]
enum Glyph {
  /// A letter in the style it applies (`B` in bold).
  Text(&'static str, &'static str),
  Icon(IconName),
}

const fn with_shift(key: &'static str) -> Shortcut {
  Shortcut::new(&[Modifier::Primary, Modifier::Shift], Key::Char(key))
}

const fn with_alt(key: &'static str) -> Shortcut {
  Shortcut::new(&[Modifier::Primary, Modifier::Alt], Key::Char(key))
}

fn inline_index(style: InlineStyle) -> usize {
  match style {
    InlineStyle::Bold => 0,
    InlineStyle::Italic => 1,
    InlineStyle::Underline => 2,
    InlineStyle::Strike => 3,
    InlineStyle::Code => 4,
  }
}

fn shortcut_for(action: FormatAction) -> Option<Shortcut> {
  FORMAT_SHORTCUTS
    .iter()
    .find(|(candidate, _)| *candidate == action)
    .map(|(_, shortcut)| *shortcut)
}

fn format_button(
  i18n: I18n,
  active: Signal<ActiveFormats>,
  on_action: Callback<FormatAction>,
  action: FormatAction,
  glyph: Glyph,
) -> impl IntoView {
  let label = action_label(action);
  let maybe_shortcut = shortcut_for(action);
  let is_first = action == FormatAction::Inline(InlineStyle::Bold);
  let is_pressed = move || match action {
    FormatAction::Inline(style) => active.get().has(style),
    FormatAction::Block(format) => active.get().block == format,
    FormatAction::Link => false,
  };
  let tooltip = move || match maybe_shortcut {
    Some(shortcut) => format!("{} ({})", i18n.t(label), shortcut_text(&shortcut, i18n.locale())),
    None => i18n.t(label),
  };
  let content = match glyph {
    Glyph::Text(letter, class) => view! { <span class=class aria-hidden="true">{letter}</span> }.into_any(),
    Glyph::Icon(name) => view! { <Icon name=name /> }.into_any(),
  };
  view! {
    <button
      type="button"
      class="format-btn"
      tabindex=if is_first { "0" } else { "-1" }
      aria-label=move || i18n.t(label)
      aria-pressed=move || (action != FormatAction::Link).then(|| is_pressed().to_string())
      aria-keyshortcuts=maybe_shortcut.map(|shortcut| shortcut.aria(Platform::current()))
      title=tooltip
      on:mousedown=|ev| ev.prevent_default()
      on:click=move |_| on_action.run(action)
    >
      {content}
    </button>
  }
}

/// i18n key of an action's name.
pub fn action_label(action: FormatAction) -> &'static str {
  match action {
    FormatAction::Inline(InlineStyle::Bold) => "format-bold",
    FormatAction::Inline(InlineStyle::Italic) => "format-italic",
    FormatAction::Inline(InlineStyle::Underline) => "format-underline",
    FormatAction::Inline(InlineStyle::Strike) => "format-strike",
    FormatAction::Inline(InlineStyle::Code) => "format-code",
    FormatAction::Link => "format-link",
    FormatAction::Block(BlockFormat::Numbered) => "format-numbered-list",
    FormatAction::Block(BlockFormat::Bulleted) => "format-bulleted-list",
    FormatAction::Block(BlockFormat::Quote) => "format-quote",
    FormatAction::Block(BlockFormat::CodeBlock) => "format-code-block",
    FormatAction::Block(format) => style_label(format),
  }
}

fn style_label(format: BlockFormat) -> &'static str {
  match format {
    BlockFormat::Heading(1) => "format-heading-1",
    BlockFormat::Heading(2) => "format-heading-2",
    BlockFormat::Heading(_) => "format-heading-3",
    _ => "format-paragraph",
  }
}

fn style_class(format: BlockFormat) -> &'static str {
  match format {
    BlockFormat::Heading(1) => "h1",
    BlockFormat::Heading(2) => "h2",
    BlockFormat::Heading(_) => "h3",
    _ => "p",
  }
}

/// ← and → (Home, End) move between the toolbar's buttons, so Tab passes the toolbar in one step.
fn move_focus(ev: &web_sys::KeyboardEvent, toolbar: NodeRef<html::Div>) {
  let step: i32 = match ev.key().as_str() {
    "ArrowRight" => 1,
    "ArrowLeft" => -1,
    "Home" => i32::MIN,
    "End" => i32::MAX,
    _ => return,
  };
  // Only between the toolbar's own buttons, not inside the open style menu.
  if ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .is_some_and(|target| target.closest("[role=menu]").ok().flatten().is_some())
  {
    return;
  }
  ev.prevent_default();
  focus_sibling(ev, toolbar, ":scope > button, :scope > div > button", step);
}

/// ↓ and ↑ move between a menu's items.
pub fn move_focus_vertical(ev: &web_sys::KeyboardEvent, menu: NodeRef<html::Div>) {
  let step = if ev.key() == "ArrowDown" { 1 } else { -1 };
  focus_sibling(ev, menu, "[role^=menuitem]", step);
}

fn focus_sibling(ev: &web_sys::KeyboardEvent, container: NodeRef<html::Div>, selector: &str, step: i32) {
  let Some(container) = container.get_untracked() else {
    return;
  };
  let Ok(list) = container.query_selector_all(selector) else {
    return;
  };
  let buttons: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|index| list.item(index))
    .filter_map(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
    .collect();
  if buttons.is_empty() {
    return;
  }
  let maybe_current = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::HtmlElement>().ok())
    .and_then(|target| buttons.iter().position(|button| *button == target));
  let last = buttons.len() as i32 - 1;
  let next = match (step, maybe_current) {
    (i32::MIN, _) => 0,
    (i32::MAX, _) => last,
    (step, Some(current)) => (current as i32 + step).rem_euclid(last + 1),
    (_, None) => 0,
  };
  if let Some(button) = buttons.get(next as usize) {
    drop(button.focus());
  }
}
