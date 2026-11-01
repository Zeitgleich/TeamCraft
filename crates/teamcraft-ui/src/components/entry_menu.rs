//! The menu of a sidebar entry (#903): a channel or chat in the sidebar, a space on the rail or in
//! the sidebar's head. It opens on a right click, a long-press on a touch screen, and the menu key
//! or Shift+F10, as a popover at the pointer on wider layouts and as the bottom sheet on phones
//! ([`Sheet`]), so every layout has the same menu. Arrows move through it, Escape closes it and the
//! focus goes back to the entry. What it offers follows [`crate::entry_actions`].

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::composer_commands::left_text;
use crate::components::conversation_notify::{NotifyChoices, NotifyTarget, entry_of, update_entry};
use crate::components::icons::{Icon, IconName};
use crate::components::link_menu::placed_with;
use crate::components::long_press::LongPress;
use crate::components::message_actions::copy_and_confirm;
use crate::components::sheet::{PopoverBackdrop, Sheet};
use crate::components::space_invites::web_origin;
use crate::entry_actions::{ConversationMenu, LeaveChoice, MenuEntry, SpaceMenu};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, SpaceId, SpaceRole};
use crate::prefs::MuteFor;
use crate::state::{AppState, Overlay, use_app_state};

/// Marks the entry the open menu belongs to, so the focus can go back to it.
const OPENER_ATTRIBUTE: &str = "data-entry-menu-opener";
/// Every item of the menu, whatever its kind.
const ITEM_SELECTOR: &str = "[role='menuitem'], [role='menuitemradio']";

/// The entry whose menu is open and where it opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryMenuTarget {
  pub entry: MenuEntry,
  pub x: i32,
  pub y: i32,
}

/// The menu's state; provided by the signed-in shell.
pub fn use_entry_menu() -> Option<RwSignal<Option<EntryMenuTarget>>> {
  use_context::<RwSignal<Option<EntryMenuTarget>>>()
}

/// What the menu shows: its actions, the notification choices, or the question before leaving a
/// space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuPage {
  Actions,
  Notifications,
  ConfirmLeave,
}

/// What a key pressed in the menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuKey {
  Next,
  Previous,
  First,
  Last,
  /// Into the notification choices (→ on an item that leads there).
  Enter,
  /// Back to the actions (← in the notification choices).
  Back,
  Close,
  Other,
}

impl MenuKey {
  fn of(key: &str) -> Self {
    match key {
      "ArrowDown" => Self::Next,
      "ArrowUp" => Self::Previous,
      "Home" => Self::First,
      "End" => Self::Last,
      "ArrowRight" => Self::Enter,
      "ArrowLeft" => Self::Back,
      "Escape" | "Tab" => Self::Close,
      _ => Self::Other,
    }
  }
}

/// Whether a key on an entry opens its menu: the menu key, or Shift+F10.
fn opens_menu(key: &str, is_shift: bool) -> bool {
  key == "ContextMenu" || (is_shift && key == "F10")
}

/// The events of an entry that open its menu. One per entry button; its handlers go on the
/// button's `pointerdown`, `pointermove`, `pointerup`, `pointercancel`, `contextmenu`, `keydown`
/// and `click` (capture).
#[derive(Clone, Copy)]
pub struct EntryMenuTrigger {
  entry: MenuEntry,
  maybe_menu: Option<RwSignal<Option<EntryMenuTarget>>>,
  long_press: LongPress,
  /// The element the finger rests on, for the focus to come back to.
  pressed: StoredValue<Option<web_sys::Element>, LocalStorage>,
}

impl EntryMenuTrigger {
  pub fn new(entry: MenuEntry) -> Self {
    let maybe_menu = use_entry_menu();
    let pressed = StoredValue::new_local(None::<web_sys::Element>);
    let long_press = LongPress::new(move |x, y| {
      if let Some(element) = pressed.try_get_value().flatten() {
        open(maybe_menu, entry, &element, x, y);
      }
    });
    Self {
      entry,
      maybe_menu,
      long_press,
      pressed,
    }
  }

  /// Whether a finger rests on the entry (it tints before the menu comes).
  pub fn is_pressing(&self) -> bool {
    self.long_press.is_pressing.get()
  }

  pub fn on_pointerdown(&self, ev: &web_sys::PointerEvent) {
    self.pressed.set_value(element_of(ev.current_target()));
    self.long_press.start(ev, self.maybe_menu.is_some());
  }

  pub fn on_pointermove(&self, ev: &web_sys::PointerEvent) {
    self.long_press.track(ev);
  }

  pub fn on_pointerup(&self) {
    self.long_press.cancel();
  }

  /// A right click opens the menu at the pointer. A finger's `contextmenu` (Android sends one on a
  /// long-press) is left to the long-press, so the menu opens once.
  pub fn on_contextmenu(&self, ev: &web_sys::MouseEvent) {
    let Some(menu) = self.maybe_menu else {
      return;
    };
    ev.prevent_default();
    if self.long_press.was_touch() || self.long_press.has_fired() {
      return;
    }
    if let Some(element) = element_of(ev.current_target()) {
      open(Some(menu), self.entry, &element, ev.client_x(), ev.client_y());
    }
  }

  /// The menu key or Shift+F10 opens the menu under the entry.
  pub fn on_keydown(&self, ev: &web_sys::KeyboardEvent) {
    if self.maybe_menu.is_none() || !opens_menu(&ev.key(), ev.shift_key()) {
      return;
    }
    ev.prevent_default();
    ev.stop_propagation();
    if let Some(element) = element_of(ev.current_target()) {
      let rect = element.get_bounding_client_rect();
      open(
        self.maybe_menu,
        self.entry,
        &element,
        (rect.left() + rect.width().min(24.0)) as i32,
        rect.bottom() as i32,
      );
    }
  }

  /// The lift that ends a long-press is no tap on the entry.
  pub fn on_click_capture(&self, ev: &web_sys::MouseEvent) {
    if self.long_press.has_fired() {
      ev.prevent_default();
      ev.stop_propagation();
      self.long_press.cancel();
    }
  }
}

/// Shows the menu while an entry asked for it ([`EntryMenuTrigger`] fills the context).
#[component]
pub fn EntryMenu() -> impl IntoView {
  let Some(menu) = use_entry_menu() else {
    return ().into_any();
  };
  view! {
    <Show when=move || menu.with(Option::is_some) fallback=|| ()>
      {move || menu.get().map(|target| view! { <MenuFor target=target menu=menu /> })}
    </Show>
  }
  .into_any()
}

#[component]
fn MenuFor(target: EntryMenuTarget, menu: RwSignal<Option<EntryMenuTarget>>) -> impl IntoView {
  let state = use_app_state();
  let node = NodeRef::<html::Div>::new();
  let page = RwSignal::new(MenuPage::Actions);
  let close = Callback::new(move |()| {
    menu.set(None);
    return_focus();
  });
  let label = Signal::derive(move || entry_name(state, target.entry));
  // Each page starts at its first item; the popover is moved inside the window once its height
  // is known.
  let position = RwSignal::new((target.x, target.y));
  Effect::new(move |_| {
    page.track();
    if node.get().is_none() {
      return;
    }
    request_animation_frame(move || {
      focus_item(node, ItemAt::First);
      if let Some(element) = node.try_get_untracked().flatten() {
        let height = element.get_bounding_client_rect().height() as i32;
        position.try_set(placed_with(target.x, target.y, height));
      }
    });
  });
  let is_compact = state.layout.get_untracked().is_compact();
  let on_keydown = Callback::new(move |ev: web_sys::KeyboardEvent| {
    // The sheet keeps Tab inside itself; only the popover closes on it.
    if is_compact && ev.key() == "Tab" {
      return;
    }
    let key = MenuKey::of(&ev.key());
    if key != MenuKey::Other {
      ev.prevent_default();
    }
    match key {
      MenuKey::Next => focus_item(node, ItemAt::Next),
      MenuKey::Previous => focus_item(node, ItemAt::Previous),
      MenuKey::First => focus_item(node, ItemAt::First),
      MenuKey::Last => focus_item(node, ItemAt::Last),
      MenuKey::Enter if is_on_submenu_item() => page.set(MenuPage::Notifications),
      MenuKey::Back if page.get_untracked() != MenuPage::Actions => page.set(MenuPage::Actions),
      MenuKey::Close => close.run(()),
      MenuKey::Enter | MenuKey::Back | MenuKey::Other => {},
    }
  });
  let content = move || match target.entry {
    MenuEntry::Conversation(conversation) => {
      view! { <ConversationItems conversation=conversation page=page close=close /> }.into_any()
    },
    MenuEntry::Space(space) => view! { <SpaceItems space=space page=page close=close /> }.into_any(),
  };

  if is_compact {
    return view! {
      <Sheet class="entry-menu" role="menu" label=label on_close=close node_ref=node on_keydown=on_keydown>
        <p class="menu-heading entry-menu-title" aria-hidden="true" translate="no">{move || label.get()}</p>
        {content}
      </Sheet>
    }
    .into_any();
  }
  view! {
    <PopoverBackdrop on_close=close popover=node />
    <div
      class="popover link-menu entry-menu"
      role="menu"
      aria-label=move || label.get()
      node_ref=node
      style:--link-menu-x=move || format!("{}px", position.get().0)
      style:--link-menu-y=move || format!("{}px", position.get().1)
      on:keydown=move |ev| on_keydown.run(ev)
    >
      {content}
    </div>
  }
  .into_any()
}

/// The actions of a channel or chat.
#[component]
fn ConversationItems(
  conversation: ConversationId,
  page: RwSignal<MenuPage>,
  close: Callback<()>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let maybe_origin = web_origin();
  let info = Memo::new(move |_| ConversationMenu::of(state, conversation, maybe_origin.as_deref()));
  // A conversation that is gone (left elsewhere, deleted) has no menu any more.
  Effect::new(move |_| {
    if info.with(Option::is_none) {
      close.run(());
    }
  });
  let target = NotifyTarget::Conversation(conversation);
  let has_unread = move || info.with(|menu| menu.as_ref().is_some_and(|menu| menu.has_unread));
  let leave = move || info.with(|menu| menu.as_ref().map_or(LeaveChoice::Hidden, |menu| menu.leave));
  let maybe_link = move || info.with(|menu| menu.as_ref().and_then(|menu| menu.maybe_link.clone()));
  let hint_id = format!("entry-menu-leave-{conversation}");
  view! {
    <Show
      when=move || page.get() == MenuPage::Actions
      fallback=move || view! { <NotificationsPage target=target page=page /> }
    >
      <NotifyItems target=target page=page close=close />
      <button type="button" class="menu-item" role="menuitem" disabled=move || !has_unread() on:click=move |_| {
        state.write(|backend| backend.mark_read(conversation));
        close.run(());
      }>
        <Icon name=IconName::CheckDouble />
        <span class="menu-label">{move || i18n.t("entry-menu-mark-read")}</span>
      </button>
      {move || maybe_link().map(|link| view! {
        <button type="button" class="menu-item" role="menuitem" on:click=move |_| {
          copy_and_confirm(link.clone());
          close.run(());
        }>
          <Icon name=IconName::Link />
          <span class="menu-label">{move || i18n.t("action-copy-link")}</span>
        </button>
      })}
      {
        let hint_id = hint_id.clone();
        move || {
          let hint_id = hint_id.clone();
          match leave() {
            LeaveChoice::Hidden => ().into_any(),
            LeaveChoice::Allowed => view! {
              <div class="menu-sep" role="separator"></div>
              <button type="button" class="menu-item is-destructive" role="menuitem" on:click=move |_| {
                close.run(());
                state.leave_with_undo(conversation, left_text(state, i18n, conversation));
              }>
                <Icon name=IconName::Leave />
                <span class="menu-label">{move || i18n.t("entry-menu-leave-channel")}</span>
              </button>
            }
            .into_any(),
            LeaveChoice::LastAdmin => view! {
              <div class="menu-sep" role="separator"></div>
              <LeaveRefused label_key="entry-menu-leave-channel" hint_key="entry-menu-leave-last-channel-admin" hint_id=hint_id />
            }
            .into_any(),
          }
        }
      }
      <div class="menu-sep" role="separator"></div>
      <button type="button" class="menu-item" role="menuitem" on:click=move |_| {
        close.run(());
        state.overlay.set(Overlay::ChannelSettings(conversation));
      }>
        <Icon name=IconName::Sliders />
        <span class="menu-label">{move || i18n.t("entry-menu-settings")}</span>
      </button>
    </Show>
  }
}

/// The actions of a space.
#[component]
fn SpaceItems(space: SpaceId, page: RwSignal<MenuPage>, close: Callback<()>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let info = Memo::new(move |_| SpaceMenu::of(state, space));
  Effect::new(move |_| {
    if info.with(Option::is_none) {
      close.run(());
    }
  });
  // Whether an admin may leave depends on the other admins: the member list says.
  let is_admin = state
    .read(|backend| backend.spaces())
    .iter()
    .any(|found| found.id == space && found.maybe_my_role == Some(SpaceRole::SpaceAdmin));
  if is_admin {
    untrack(|| state.write(|backend| backend.refresh_space_roster(space)));
  }
  let target = NotifyTarget::Space(space);
  let has_unread = move || info.with(|menu| menu.as_ref().is_some_and(|menu| menu.has_unread));
  let leave = move || info.with(|menu| menu.as_ref().map_or(LeaveChoice::Hidden, |menu| menu.leave));
  let name = move || info.with(|menu| menu.as_ref().map(|menu| menu.name.clone()).unwrap_or_default());
  let hint_id = format!("entry-menu-leave-space-{space}");
  view! {
    {move || match page.get() {
      MenuPage::Notifications => view! { <NotificationsPage target=target page=page /> }.into_any(),
      MenuPage::ConfirmLeave => view! {
        <p class="menu-confirm" id=format!("entry-menu-confirm-{space}")>
          {move || i18n.t_with("entry-menu-leave-space-confirm", &[("name", &name())])}
        </p>
        <button
          type="button"
          class="menu-item is-destructive"
          role="menuitem"
          aria-describedby=format!("entry-menu-confirm-{space}")
          on:click=move |_| {
            close.run(());
            state.leave_space(space);
          }
        >
          <Icon name=IconName::Leave />
          <span class="menu-label">{move || i18n.t("entry-menu-leave-space")}</span>
        </button>
        <button type="button" class="menu-item" role="menuitem" on:click=move |_| page.set(MenuPage::Actions)>
          <span class="menu-label">{move || i18n.t("cancel")}</span>
        </button>
      }
      .into_any(),
      MenuPage::Actions => {
        let hint_id = hint_id.clone();
        view! {
          <NotifyItems target=target page=page close=close />
          <button type="button" class="menu-item" role="menuitem" disabled=move || !has_unread() on:click=move |_| {
            state.mark_space_read(space);
            close.run(());
          }>
            <Icon name=IconName::CheckDouble />
            <span class="menu-label">{move || i18n.t("sidebar-mark-all-read")}</span>
          </button>
          {move || {
            let hint_id = hint_id.clone();
            match leave() {
              LeaveChoice::Hidden => ().into_any(),
              LeaveChoice::Allowed => view! {
                <div class="menu-sep" role="separator"></div>
                <button type="button" class="menu-item is-destructive" role="menuitem" on:click=move |_| page.set(MenuPage::ConfirmLeave)>
                  <Icon name=IconName::Leave />
                  <span class="menu-label">{move || i18n.t("entry-menu-leave-space-ellipsis")}</span>
                </button>
              }
              .into_any(),
              LeaveChoice::LastAdmin => view! {
                <div class="menu-sep" role="separator"></div>
                <LeaveRefused label_key="entry-menu-leave-space-ellipsis" hint_key="entry-menu-leave-last-space-admin" hint_id=hint_id />
              }
              .into_any(),
            }
          }}
          <div class="menu-sep" role="separator"></div>
          <button type="button" class="menu-item" role="menuitem" on:click=move |_| {
            close.run(());
            state.overlay.set(Overlay::SpaceSettings(space));
          }>
            <Icon name=IconName::Sliders />
            <span class="menu-label">{move || i18n.t("entry-menu-settings")}</span>
          </button>
        }
        .into_any()
      },
    }}
  }
}

/// "Notifications ›", and "Unmute" while the entry's own setting mutes it. A channel muted by its
/// space is unmuted with its own level in the notification choices (its own setting wins).
#[component]
fn NotifyItems(target: NotifyTarget, page: RwSignal<MenuPage>, close: Callback<()>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_muted = Memo::new(move |_| entry_of(state, target).mute != MuteFor::Off);
  view! {
    <button
      type="button"
      class="menu-item"
      role="menuitem"
      aria-haspopup="menu"
      data-submenu="true"
      on:click=move |_| page.set(MenuPage::Notifications)
    >
      <Icon name=IconName::Bell />
      <span class="menu-label">{move || i18n.t("entry-menu-notifications")}</span>
      <Icon name=IconName::ChevronRight />
    </button>
    <Show when=move || is_muted.get() fallback=|| ()>
      <button type="button" class="menu-item" role="menuitem" on:click=move |_| {
        update_entry(state, target, |entry| entry.mute = MuteFor::Off);
        close.run(());
      }>
        <Icon name=IconName::BellOff />
        <span class="menu-label">{move || i18n.t("entry-menu-unmute")}</span>
      </button>
    </Show>
  }
}

/// The notification level and mute time, with the way back to the actions.
#[component]
fn NotificationsPage(target: NotifyTarget, page: RwSignal<MenuPage>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button type="button" class="menu-item menu-back" role="menuitem" on:click=move |_| page.set(MenuPage::Actions)>
      <Icon name=IconName::ChevronLeft />
      <span class="menu-label">{move || i18n.t("back")}</span>
    </button>
    <NotifyChoices target=target />
  }
}

/// "Leave" where the server would refuse it: shown, not taken, with what to do first.
#[component]
fn LeaveRefused(label_key: &'static str, hint_key: &'static str, hint_id: String) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <button type="button" class="menu-item" role="menuitem" aria-disabled="true" aria-describedby=hint_id.clone()>
      <Icon name=IconName::Leave />
      <span class="menu-label">{move || i18n.t(label_key)}</span>
    </button>
    <p class="menu-hint" id=hint_id>{move || i18n.t(hint_key)}</p>
  }
}

/// Opens the menu of `entry` at (`x`, `y`), marking `element` for the focus to come back to.
fn open(
  maybe_menu: Option<RwSignal<Option<EntryMenuTarget>>>,
  entry: MenuEntry,
  element: &web_sys::Element,
  x: i32,
  y: i32,
) {
  let Some(menu) = maybe_menu else {
    return;
  };
  clear_opener();
  // Without the mark the focus just stays where it is when the menu closes.
  drop(element.set_attribute(OPENER_ATTRIBUTE, ""));
  menu.set(Some(EntryMenuTarget { entry, x, y }));
}

/// Puts the focus back on the entry the menu was opened from, if it is still there.
fn return_focus() {
  if let Some(opener) = opener() {
    drop(opener.remove_attribute(OPENER_ATTRIBUTE));
    if let Ok(element) = opener.dyn_into::<web_sys::HtmlElement>() {
      drop(element.focus());
    }
  }
}

fn clear_opener() {
  if let Some(opener) = opener() {
    drop(opener.remove_attribute(OPENER_ATTRIBUTE));
  }
}

fn opener() -> Option<web_sys::Element> {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| {
      document
        .query_selector(&format!("[{OPENER_ATTRIBUTE}]"))
        .ok()
        .flatten()
    })
}

fn element_of(maybe_target: Option<web_sys::EventTarget>) -> Option<web_sys::Element> {
  maybe_target.and_then(|target| target.dyn_into::<web_sys::Element>().ok())
}

/// The name the menu is labelled with.
fn entry_name(state: AppState, entry: MenuEntry) -> String {
  match entry {
    MenuEntry::Conversation(conversation) => state
      .read(|backend| backend.conversation(conversation))
      .map(|found| found.name)
      .unwrap_or_default(),
    MenuEntry::Space(space) => state
      .read(|backend| backend.spaces())
      .into_iter()
      .find(|found| found.id == space)
      .map(|found| found.name)
      .unwrap_or_default(),
  }
}

/// Whether the focused item leads to the notification choices.
fn is_on_submenu_item() -> bool {
  web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .is_some_and(|element| element.has_attribute("data-submenu"))
}

/// Which item [`focus_item`] moves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemAt {
  First,
  Last,
  Next,
  Previous,
}

/// Moves the focus within the menu's items that can take it (not the disabled ones), round the
/// ends.
fn focus_item(menu: NodeRef<html::Div>, at: ItemAt) {
  let Some(element) = menu.try_get_untracked().flatten() else {
    return;
  };
  let Ok(found) = element.query_selector_all(ITEM_SELECTOR) else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..found.length())
    .filter_map(|index| found.item(index))
    .filter_map(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
    .filter(|item| !item.has_attribute("disabled"))
    .collect();
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let maybe_current = items.iter().position(|item| {
    active
      .as_ref()
      .is_some_and(|active| item.is_same_node(Some(active)))
  });
  if let Some(index) = next_index(maybe_current, items.len(), at) {
    drop(items[index].focus());
  }
}

/// The index [`focus_item`] moves to among `count` items, from `maybe_current`.
fn next_index(maybe_current: Option<usize>, count: usize, at: ItemAt) -> Option<usize> {
  if count == 0 {
    return None;
  }
  Some(match (at, maybe_current) {
    (ItemAt::First, _) | (ItemAt::Next, None) => 0,
    (ItemAt::Last, _) | (ItemAt::Previous, None) => count - 1,
    (ItemAt::Next, Some(current)) => (current + 1) % count,
    (ItemAt::Previous, Some(current)) => (current + count - 1) % count,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  mod keyboard {
    use super::*;

    #[test]
    fn the_menu_key_and_shift_f10_open_the_menu() {
      assert!(opens_menu("ContextMenu", false));
      assert!(opens_menu("F10", true));
      assert!(!opens_menu("F10", false));
      assert!(!opens_menu("Enter", true));
    }

    #[test]
    fn keys_in_the_menu() {
      assert_eq!(MenuKey::of("ArrowDown"), MenuKey::Next);
      assert_eq!(MenuKey::of("ArrowUp"), MenuKey::Previous);
      assert_eq!(MenuKey::of("Home"), MenuKey::First);
      assert_eq!(MenuKey::of("End"), MenuKey::Last);
      assert_eq!(MenuKey::of("ArrowRight"), MenuKey::Enter);
      assert_eq!(MenuKey::of("ArrowLeft"), MenuKey::Back);
      assert_eq!(MenuKey::of("Escape"), MenuKey::Close);
      assert_eq!(MenuKey::of("Tab"), MenuKey::Close);
      // Enter and Space press the focused item themselves.
      assert_eq!(MenuKey::of("Enter"), MenuKey::Other);
    }

    #[test]
    fn arrows_go_round_the_ends() {
      assert_eq!(next_index(None, 4, ItemAt::Next), Some(0));
      assert_eq!(next_index(None, 4, ItemAt::Previous), Some(3));
      assert_eq!(next_index(Some(3), 4, ItemAt::Next), Some(0));
      assert_eq!(next_index(Some(0), 4, ItemAt::Previous), Some(3));
      assert_eq!(next_index(Some(1), 4, ItemAt::Last), Some(3));
      assert_eq!(next_index(Some(2), 4, ItemAt::First), Some(0));
      assert_eq!(next_index(None, 0, ItemAt::Next), None);
    }
  }
}
