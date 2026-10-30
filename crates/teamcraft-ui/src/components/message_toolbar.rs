//! The floating action bar of a message list (#320): one instance per list (the feed, a thread),
//! moved onto the message under the pointer or the keyboard focus, so long lists stay light. It
//! carries quick reactions, add reaction, reply in thread, forward, save and a "more" menu; on
//! touch screens a long press opens the same actions as a sheet.
//!
//! Each message row tells the list's [`ToolbarHost`] when it is hovered, focused or long-pressed;
//! the toolbar reads the message from there and finds the row again by its `data-msg-id` (rows are
//! recreated when their message changes, for example after a reaction).

use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::channel_access::ChannelAccess;
use crate::components::emoji_picker::EmojiPicker;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::{
  ActionContext, ActionSurface, MessageAction, message_actions, run_message_action, with_shortcut,
};
use crate::components::sheet::{PopoverBackdrop, Sheet};
use crate::i18n::use_i18n;
use crate::model::{Message, MessageId, thread_id_of};
use crate::motion::PopoverSide;
use crate::state::use_app_state;

/// Quick reactions until the user has used emoji of their own.
pub const DEFAULT_QUICK_REACTIONS: [&str; 3] = ["✅", "👀", "🙌"];
/// How many quick reactions the toolbar shows.
const QUICK_REACTION_COUNT: usize = 3;
/// How long the toolbar stays after the pointer left its message, so it can move onto the bar.
const HIDE_GRACE: Duration = Duration::from_millis(120);
/// How far the bar reaches above its message's top edge.
const OVERLAP_PX: f64 = 16.0;
/// Space kept between the bar and the top of the visible list.
const EDGE_PX: f64 = 4.0;
/// Room the "more" menu is assumed to need before it is measured, in rem (it grows with the text
/// size).
const MENU_HEIGHT_REM: f64 = 18.75;
/// Space between the "more" button and its menu.
const MENU_GAP_PX: f64 = 4.0;
/// How long a finger rests on a message before its action sheet opens.
pub const LONG_PRESS: Duration = Duration::from_millis(500);

/// The message the list's toolbar is on, and the menus it has open. Provided by each message list.
#[derive(Clone, Copy)]
pub struct ToolbarHost {
  /// The scroll container the toolbar lives in (positioned relative to it).
  container: NodeRef<html::Div>,
  active: RwSignal<Option<ActiveMessage>>,
  /// Top of the toolbar in the container's scroll coordinates.
  top: RwSignal<f64>,
  open: RwSignal<OpenMenu>,
  hide_generation: StoredValue<u32>,
  /// The touch action sheet, for this message.
  sheet: RwSignal<Option<ActiveMessage>>,
}

/// The message the toolbar acts on.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveMessage {
  pub message: Message,
  pub in_thread: bool,
  pub is_mine: bool,
}

/// What the toolbar has open; while anything is, it stays on its message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenMenu {
  Nothing,
  More,
  EmojiPicker,
}

impl ToolbarHost {
  /// A host for the list whose scroll container is `container`; also provided as context for the
  /// rows below.
  pub fn provide(container: NodeRef<html::Div>) -> Self {
    let host = Self {
      container,
      active: RwSignal::new(None),
      top: RwSignal::new(0.0),
      open: RwSignal::new(OpenMenu::Nothing),
      hide_generation: StoredValue::new(0),
      sheet: RwSignal::new(None),
    };
    provide_context(host);
    host
  }

  pub fn use_nearest() -> Option<Self> {
    use_context::<Self>()
  }

  /// The keyboard focus moved onto a message: an open menu of another message closes.
  pub fn focus(&self, active: ActiveMessage) {
    let is_other = self.active.with_untracked(|current| {
      current
        .as_ref()
        .is_none_or(|current| current.message.id != active.message.id)
    });
    if is_other {
      self.close_menu();
    }
    self.show(active);
  }

  /// The pointer or the keyboard focus is on a message.
  pub fn show(&self, active: ActiveMessage) {
    self.cancel_hide();
    if self.open.get_untracked() != OpenMenu::Nothing {
      return;
    }
    let id = active.message.id;
    if self
      .active
      .with_untracked(|current| current.as_ref() != Some(&active))
    {
      self.active.set(Some(active));
    }
    self.reposition_to(id);
  }

  /// A row of the active message was recreated with fresh data (an edit, a reaction).
  pub fn refresh(&self, active: ActiveMessage) {
    let is_same = self.active.with_untracked(|current| {
      current
        .as_ref()
        .is_some_and(|current| current.message.id == active.message.id)
    });
    if is_same {
      let id = active.message.id;
      self.active.set(Some(active));
      let host = *self;
      request_animation_frame(move || {
        // The list may be gone by the next frame.
        if host.active.try_with_untracked(|_| ()).is_some() {
          host.reposition_to(id);
        }
      });
    }
  }

  /// The pointer or focus left a message: hide unless it reaches the toolbar (or another message)
  /// first.
  pub fn hide_soon(&self) {
    let host = *self;
    let generation = self.cancel_hide();
    set_timeout(
      move || {
        if host.hide_generation.try_get_value() == Some(generation)
          && host.open.try_get_untracked() == Some(OpenMenu::Nothing)
          && !host.is_in_use()
        {
          host.active.try_set(None);
        }
      },
      HIDE_GRACE,
    );
  }

  /// Whether the toolbar is on this message (the row keeps its highlight while the pointer is on
  /// the bar).
  pub fn is_active(&self, message: MessageId) -> bool {
    self
      .active
      .with(|active| active.as_ref().is_some_and(|active| active.message.id == message))
  }

  /// Keyboard `E` on a focused message: open the emoji picker for it.
  pub fn open_picker(&self, active: ActiveMessage) {
    self.show(active);
    self.open.set(OpenMenu::EmojiPicker);
  }

  /// `Shift+F10` or the context-menu key on a focused message: open the "more" menu.
  pub fn open_more(&self, active: ActiveMessage) {
    self.show(active);
    self.open.set(OpenMenu::More);
  }

  /// A long press on a touch screen.
  pub fn open_sheet(&self, active: ActiveMessage) {
    self.active.set(None);
    self.sheet.set(Some(active));
  }

  /// The rows above changed height: keep the toolbar on its message.
  pub fn reposition(&self) {
    if let Some(id) = self
      .active
      .with_untracked(|active| active.as_ref().map(|active| active.message.id))
    {
      self.reposition_to(id);
    }
  }

  fn reposition_to(&self, id: MessageId) {
    let Some(container) = self.container.get_untracked() else {
      return;
    };
    let Ok(Some(row)) = container.query_selector(&format!("[data-msg-id=\"{id}\"]")) else {
      // Scrolled out of the rendered window.
      if self.open.get_untracked() == OpenMenu::Nothing {
        self.active.set(None);
      }
      return;
    };
    let container_top = container.get_bounding_client_rect().top();
    let scroll_top = f64::from(container.scroll_top());
    let row_top = row.get_bounding_client_rect().top() - container_top + scroll_top;
    // Over the row's top edge, but never above the visible part of the list.
    let top = (row_top - OVERLAP_PX).max(scroll_top + EDGE_PX);
    if (self.top.get_untracked() - top).abs() > 0.5 {
      self.top.set(top);
    }
  }

  fn cancel_hide(&self) -> u32 {
    let mut next = 0;
    self.hide_generation.try_update_value(|generation| {
      *generation = generation.wrapping_add(1);
      next = *generation;
    });
    next
  }

  /// A closed menu took the pointer with it without a `mouseleave`: hide the toolbar unless the
  /// pointer or the focus is still on it or its message.
  fn close_menu(&self) {
    self.open.set(OpenMenu::Nothing);
    self.hide_soon();
  }

  /// Whether the pointer or the keyboard focus is on the toolbar or on its message.
  fn is_in_use(&self) -> bool {
    let (Some(container), Some(id)) = (
      self.container.try_get_untracked().flatten(),
      self
        .active
        .try_with_untracked(|active| active.as_ref().map(|active| active.message.id))
        .flatten(),
    ) else {
      return false;
    };
    let selector = format!(
      ".msg-toolbar:hover, .msg-toolbar:focus-within, [data-msg-id=\"{id}\"]:hover, [data-msg-id=\"{id}\"]:focus-within"
    );
    container.query_selector(&selector).ok().flatten().is_some()
  }
}

/// The one toolbar of a list, rendered inside its scroll container.
#[component]
pub fn MessageToolbar(host: ToolbarHost) -> impl IntoView {
  view! {
    {move || host.active.get().map(|active| view! { <ToolbarBar host=host active=active /> })}
    {move || host.sheet.get().map(|active| view! { <ActionSheet host=host active=active /> })}
  }
}

#[component]
fn ToolbarBar(host: ToolbarHost, active: ActiveMessage) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let message = StoredValue::new(active.message.clone());
  let (conversation, id) = (active.message.conversation, active.message.id);
  let thread_root = active.message.maybe_thread.unwrap_or_else(|| thread_id_of(id));
  let (in_thread, is_mine) = (active.in_thread, active.is_mine);
  let extras = Memo::new(move |_| {
    state
      .read(|backend| backend.message_extras(conversation, &[id]))
      .into_iter()
      .find(|extras| extras.message == id)
  });
  // Ask for the permalink now so that "Copy link" has it by the time it is used.
  state.read(|backend| drop(backend.permalink(conversation, id)));
  let access = ChannelAccess::new(state, conversation);
  let permissions = Memo::new(move |_| access.message_permissions());
  let can_react = move || permissions.get().can_react;
  let context = move || ActionContext::new(is_mine, in_thread, extras.get().as_ref(), permissions.get());
  let is_saved = move || extras.get().is_some_and(|extras| extras.is_saved);
  let react_button = NodeRef::<html::Button>::new();
  let more_button = NodeRef::<html::Button>::new();
  let menu = NodeRef::<html::Div>::new();
  let placement = RwSignal::new(MenuPlacement::default());
  // Placed when it opens with an assumed height, then again with the height it really has.
  Effect::new(move |_| {
    if host.open.get() != OpenMenu::More {
      return;
    }
    let Some(button) = more_button.get_untracked() else {
      return;
    };
    let maybe_measured = menu
      .get()
      .map(|menu| f64::from(menu.scroll_height()))
      .filter(|height| *height > 0.0);
    let menu_height = maybe_measured.unwrap_or_else(|| MENU_HEIGHT_REM * crate::viewport::rem_px());
    let maybe_bounds = host.container.get_untracked().map(|list| list.get_bounding_client_rect());
    placement.set(menu_placement(&button, maybe_bounds.as_ref(), menu_height));
  });
  let run = move |action: MessageAction| {
    host.close_menu();
    message.with_value(|message| run_message_action(action, message, extras.get_untracked().as_ref()));
  };
  let react = move |emoji: &str| {
    state.settings.remember_emoji(emoji);
    state.write(|backend| backend.toggle_reaction(conversation, id, emoji));
  };
  let quick_reactions = move || {
    if !can_react() {
      return Vec::new();
    }
    let mut list = state.settings.recent_emoji.get();
    for emoji in DEFAULT_QUICK_REACTIONS {
      if !list.iter().any(|recent| recent == emoji) {
        list.push(emoji.to_owned());
      }
    }
    list.truncate(QUICK_REACTION_COUNT);
    list
  };
  let label = move |key: &'static str, maybe_shortcut: Option<&'static str>| {
    move || with_shortcut(i18n.t(key), maybe_shortcut)
  };
  let save_key = move || {
    if is_saved() {
      "action-unsave"
    } else {
      "action-save"
    }
  };
  // The keyboard goes into the menu when it opens.
  Effect::new(move |_| {
    if let Some(element) = menu.get()
      && let Ok(Some(first)) = element.query_selector(".menu-item")
      && let Some(first) = first.dyn_ref::<web_sys::HtmlElement>()
    {
      drop(first.focus());
    }
  });
  let close_and_refocus = move || {
    let button = match host.open.get_untracked() {
      OpenMenu::More => more_button.get_untracked(),
      OpenMenu::EmojiPicker => react_button.get_untracked(),
      OpenMenu::Nothing => return,
    };
    host.close_menu();
    if let Some(button) = button {
      drop(button.focus());
    }
  };
  view! {
    <div
      class="msg-toolbar"
      class:has-open-menu=move || host.open.get() != OpenMenu::Nothing
      role="toolbar"
      aria-label=move || i18n.t("message-actions")
      style:top=move || format!("{}px", host.top.get())
      on:mouseenter=move |_| {
        host.cancel_hide();
      }
      on:mouseleave=move |_| host.hide_soon()
      on:focusin=move |_| {
        host.cancel_hide();
      }
      on:focusout=move |_| host.hide_soon()
      on:keydown=move |ev| {
        if ev.key() == "Escape" {
          ev.stop_propagation();
          close_and_refocus();
        }
      }
    >
      <For each=quick_reactions key=|emoji| emoji.clone() let:emoji>
        {
          let emoji_for_label = emoji.clone();
          let label = move || i18n.t_with("action-react-with", &[("emoji", &emoji_for_label)]);
          let chosen = emoji.clone();
          view! {
            <button class="icon-btn quick-reaction" aria-label=label.clone() title=label on:click=move |_| react(&chosen)>
              {emoji}
            </button>
          }
        }
      </For>
      <Show when=can_react fallback=|| ()>
        <button
          class="icon-btn"
          node_ref=react_button
          aria-label=label("action-react", Some("E"))
          title=label("action-react", Some("E"))
          aria-haspopup="dialog"
          aria-expanded=move || (host.open.get() == OpenMenu::EmojiPicker).to_string()
          on:click=move |_| host.open.set(OpenMenu::EmojiPicker)
        >
          <Icon name=IconName::Smile />
        </button>
        <span class="msg-toolbar-sep" aria-hidden="true"></span>
      </Show>
      {(!in_thread).then(|| view! {
        <button
          class="icon-btn"
          aria-label=label("action-reply", Some("T"))
          title=label("action-reply", Some("T"))
          on:click=move |_| state.open_thread(conversation, thread_root)
        >
          <Icon name=IconName::Threads />
        </button>
      })}
      <button
        class="icon-btn"
        aria-label=label("action-forward", None)
        title=label("action-forward", None)
        on:click=move |_| run(MessageAction::Forward)
      >
        <Icon name=IconName::Forward />
      </button>
      <button
        class="icon-btn"
        class:is-on=is_saved
        aria-pressed=move || is_saved().to_string()
        aria-label=move || i18n.t(save_key())
        title=move || i18n.t(save_key())
        on:click=move |_| run(if is_saved() { MessageAction::Unsave } else { MessageAction::Save })
      >
        <Icon name=IconName::Bookmark />
      </button>
      <button
        class="icon-btn"
        node_ref=more_button
        aria-haspopup="menu"
        aria-expanded=move || (host.open.get() == OpenMenu::More).to_string()
        aria-label=move || i18n.t("message-more")
        title=move || i18n.t("message-more")
        on:click=move |_| {
          let next = if host.open.get_untracked() == OpenMenu::More { OpenMenu::Nothing } else { OpenMenu::More };
          host.open.set(next);
        }
      >
        <Icon name=IconName::More />
      </button>
      <Show when=move || host.open.get() == OpenMenu::More fallback=|| ()>
        <PopoverBackdrop on_close=Callback::new(move |()| host.close_menu()) popover=menu />
        <div
          class=move || format!("popover msg-menu {}", placement.with(|placement| placement.side.class()))
          style:position="fixed"
          style:right=move || placement.with(|placement| placement.right.clone())
          style:top=move || placement.with(|placement| placement.top.clone())
          style:bottom=move || placement.with(|placement| placement.bottom.clone())
          style:max-height=move || placement.with(|placement| placement.max_height.clone())
          role="menu"
          aria-label=move || i18n.t("message-more")
          node_ref=menu
          on:keydown=move |ev| move_in_menu(&ev, menu)
        >
          {move || {
            message_actions(context(), ActionSurface::MoreMenu)
              .into_iter()
              .map(|action| view! { <ActionItem action=action on_run=Callback::new(run) /> })
              .collect_view()
          }}
        </div>
      </Show>
      {move || {
        (host.open.get() == OpenMenu::EmojiPicker)
          .then(|| react_button.get())
          .flatten()
          .map(|button| {
            let anchor: web_sys::Element = button.unchecked_into();
            view! {
              <EmojiPicker
                anchor=anchor
                on_pick=Callback::new(move |emoji: String| {
                  host.close_menu();
                  react(&emoji);
                })
                on_close=Callback::new(move |()| host.close_menu())
              />
            }
          })
      }}
    </div>
  }
}

/// Where the "more" menu goes: fixed to the window (the list would clip it), below its button or
/// above it, whichever side of the visible list has room for `menu_height`; with room on neither,
/// the roomier side, the menu scrolling. `maybe_bounds` is the list's box: its bottom edge is the
/// composer's top, which the menu must not cover.
fn menu_placement(
  button: &web_sys::Element,
  maybe_bounds: Option<&web_sys::DomRect>,
  menu_height: f64,
) -> MenuPlacement {
  let rect = button.get_bounding_client_rect();
  let (width, height) = web_sys::window()
    .map(|window| {
      let read = |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0)
      };
      (read(window.inner_width()), read(window.inner_height()))
    })
    .unwrap_or((1024.0, 768.0));
  let (bounds_top, bounds_bottom) =
    maybe_bounds.map_or((0.0, height), |bounds| (bounds.top().max(0.0), bounds.bottom().min(height)));
  let right = format!("{}px", (width - rect.right()).max(0.0));
  let room_below = (bounds_bottom - rect.bottom() - 2.0 * MENU_GAP_PX).max(0.0);
  let room_above = (rect.top() - bounds_top - 2.0 * MENU_GAP_PX).max(0.0);
  if room_below >= menu_height || (room_above < menu_height && room_below >= room_above) {
    MenuPlacement {
      right,
      top: format!("{}px", rect.bottom() + MENU_GAP_PX),
      bottom: "auto".into(),
      max_height: format!("{room_below}px"),
      side: PopoverSide::Below,
    }
  } else {
    MenuPlacement {
      right,
      top: "auto".into(),
      bottom: format!("{}px", height - rect.top() + MENU_GAP_PX),
      max_height: format!("{room_above}px"),
      side: PopoverSide::Above,
    }
  }
}

/// Where the "More" menu sits, as CSSOM values: the CSP (`style-src 'self'`) blocks `style`
/// attributes.
#[derive(Clone, Default)]
struct MenuPlacement {
  right: String,
  top: String,
  bottom: String,
  /// What fits on that side; a longer menu scrolls.
  max_height: String,
  /// The side it opens on, so it grows out of the button (#359).
  side: PopoverSide,
}

/// Arrow keys, Home and End move between the entries of a menu (and not between the messages
/// of the list around it).
fn move_in_menu(ev: &web_sys::KeyboardEvent, menu: NodeRef<html::Div>) {
  let key = ev.key();
  if !matches!(key.as_str(), "ArrowUp" | "ArrowDown" | "Home" | "End") {
    return;
  }
  ev.prevent_default();
  ev.stop_propagation();
  let Some(element) = menu.get_untracked() else {
    return;
  };
  let Ok(list) = element.query_selector_all(".menu-item") else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..list.length())
    .filter_map(|index| list.item(index))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  if items.is_empty() {
    return;
  }
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let maybe_current = active.and_then(|active| items.iter().position(|item| active.is_same_node(Some(item))));
  let last = items.len() - 1;
  let target = match (key.as_str(), maybe_current) {
    ("Home", _) | ("ArrowDown", None) => 0,
    ("ArrowDown", Some(current)) => (current + 1) % items.len(),
    ("ArrowUp", Some(current)) => current.checked_sub(1).unwrap_or(last),
    _ => last,
  };
  if let Some(item) = items.get(target) {
    drop(item.focus());
  }
}

/// One entry of the "more" menu or the action sheet.
#[component]
fn ActionItem(action: MessageAction, on_run: Callback<MessageAction>) -> impl IntoView {
  let i18n = use_i18n();
  let title = move || with_shortcut(i18n.t(action.label_key()), action.maybe_shortcut());
  view! {
    {action.starts_group().then(|| view! { <div class="menu-sep" role="separator"></div> })}
    <button
      class="menu-item"
      class:menu-danger=action.is_danger()
      role="menuitem"
      title=title
      on:click=move |_| on_run.run(action)
    >
      <Icon name=action.icon() />
      <span class="menu-label">{move || i18n.t(action.label_key())}</span>
      {action.maybe_shortcut().map(|key| view! { <kbd class="menu-shortcut" aria-hidden="true">{key}</kbd> })}
    </button>
  }
}

/// Touch screens: the actions of a long-pressed message as a sheet from the bottom.
#[component]
fn ActionSheet(host: ToolbarHost, active: ActiveMessage) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let message = StoredValue::new(active.message.clone());
  let (conversation, id) = (active.message.conversation, active.message.id);
  let extras = Memo::new(move |_| {
    state
      .read(|backend| backend.message_extras(conversation, &[id]))
      .into_iter()
      .find(|extras| extras.message == id)
  });
  state.read(|backend| drop(backend.permalink(conversation, id)));
  let close = move || host.sheet.set(None);
  let run = move |action: MessageAction| {
    close();
    message.with_value(|message| run_message_action(action, message, extras.get_untracked().as_ref()));
  };
  let react = move |emoji: &str| {
    close();
    state.settings.remember_emoji(emoji);
    state.write(|backend| backend.toggle_reaction(conversation, id, emoji));
  };
  let mut quick = state.settings.recent_emoji.get_untracked();
  for emoji in DEFAULT_QUICK_REACTIONS {
    if !quick.iter().any(|recent| recent == emoji) {
      quick.push(emoji.to_owned());
    }
  }
  quick.truncate(QUICK_REACTION_COUNT + 2);
  let (is_mine, in_thread) = (active.is_mine, active.in_thread);
  let access = ChannelAccess::new(state, conversation);
  let permissions = Memo::new(move |_| access.message_permissions());
  // Reactions only where the user may react (`add_reaction`, which channels can take away).
  let can_react = untrack(|| permissions.get().can_react);
  let actions = NodeRef::<html::Div>::new();
  let picker_button = NodeRef::<html::Button>::new();
  let is_picking = RwSignal::new(false);
  view! {
    <Sheet class="action-sheet" label=Signal::derive(move || i18n.t("message-action-sheet")) on_close=Callback::new(move |()| close())>
      {can_react.then(|| view! {
        <div class="sheet-reactions">
          {quick
            .into_iter()
            .map(|emoji| {
              let label = i18n.t_with("action-react-with", &[("emoji", &emoji)]);
              let chosen = emoji.clone();
              view! { <button class="sheet-reaction" aria-label=label on:click=move |_| react(&chosen)>{emoji}</button> }
            })
            .collect_view()}
          <button
            class="sheet-reaction"
            node_ref=picker_button
            aria-label=move || i18n.t("action-react")
            aria-haspopup="dialog"
            on:click=move |_| is_picking.set(true)
          >
            <Icon name=IconName::Smile />
          </button>
        </div>
      })}
      {move || {
        is_picking
          .get()
          .then(|| picker_button.get())
          .flatten()
          .map(|button| {
            let anchor: web_sys::Element = button.unchecked_into();
            view! {
              <EmojiPicker
                anchor=anchor
                on_pick=Callback::new(move |emoji: String| react(&emoji))
                on_close=Callback::new(move |()| is_picking.set(false))
              />
            }
          })
      }}
      <div class="sheet-actions" role="menu" node_ref=actions on:keydown=move |ev| move_in_menu(&ev, actions)>
        {move || {
          message_actions(ActionContext::new(is_mine, in_thread, extras.get().as_ref(), permissions.get()), ActionSurface::Sheet)
            .into_iter()
            .map(|action| view! { <ActionItem action=action on_run=Callback::new(run) /> })
            .collect_view()
        }}
      </div>
    </Sheet>
  }
}
