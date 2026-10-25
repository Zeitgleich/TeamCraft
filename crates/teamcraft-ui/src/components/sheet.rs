//! The one bottom sheet of the phone layout (#368): message actions, the account menu, the
//! composer's menus, the emoji picker and the conversation's "…" all open in it, so they look and
//! behave the same. It rises from the bottom edge, can be dragged down and flicked away (it follows
//! the finger and decides with its momentum, `gestures.rs`), closes on Escape and a tap on the
//! backdrop, keeps Tab inside and gives the focus back. It leaves the way it came: a ghost of it
//! slides down from wherever the finger let go (`motion.rs`).
//!
//! [`MenuSurface`] is what menus use: a popover beside its trigger on wider layouts, this sheet on
//! phones.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::modal::trap_tab;
use crate::gestures::{SheetDrag, SheetRelease};
use crate::motion::use_leave_ghost;
use crate::state::use_app_state;

/// A bottom sheet over the page. `node_ref` reaches its content box (for keyboard handling in the
/// menu it holds); `on_keydown` gets every key the sheet doesn't handle itself.
#[component]
pub fn Sheet(
  /// Classes for the content (e.g. `status-menu`), besides `sheet`.
  #[prop(optional)]
  class: &'static str,
  /// The ARIA role: `dialog` by default, `menu` for a plain menu.
  #[prop(default = "dialog")]
  role: &'static str,
  #[prop(into)] label: Signal<String>,
  on_close: Callback<()>,
  #[prop(optional)] node_ref: Option<NodeRef<html::Div>>,
  #[prop(optional)] on_keydown: Option<Callback<web_sys::KeyboardEvent>>,
  children: Children,
) -> impl IntoView {
  let sheet = node_ref.unwrap_or_default();
  let backdrop = NodeRef::<html::Div>::new();
  use_leave_ghost(sheet);
  use_leave_ghost(backdrop);
  return_focus_on_close();
  // The sheet takes the focus (for screen readers and the keyboard), without a ring on itself,
  // unless its content already put it on an item.
  Effect::new(move |_| {
    if let Some(element) = sheet.get() {
      let maybe_focused = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element());
      let is_inside = maybe_focused.is_some_and(|focused| element.contains(Some(&focused)));
      if !is_inside {
        drop(element.focus());
      }
    }
  });
  // A sheet whose content doesn't scroll gives every touch to the drag (`.is-static`): the browser
  // would otherwise take a vertical pull for a scroll and cancel the pointer.
  Effect::new(move |_| {
    if let Some(element) = sheet.get() {
      request_animation_frame(move || {
        if element.scroll_height() <= element.client_height() {
          drop(element.class_list().add_1("is-static"));
        }
      });
    }
  });
  let drag = StoredValue::new(None::<SheetDrag>);
  let was_dragged = StoredValue::new(false);
  hold_touches_while_pulled(sheet, drag);

  let on_pointerdown = move |ev: web_sys::PointerEvent| {
    let Some(element) = sheet.get_untracked() else {
      return;
    };
    // Scrolled content scrolls first; at its top a pull down moves the sheet. The handle always
    // moves it (it stays on top while the content scrolls).
    let is_on_handle = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .is_some_and(|element| element.class_list().contains("sheet-handle"));
    if !ev.is_primary() || (element.scroll_top() > 0 && !is_on_handle) || is_in_text_field(&ev) {
      return;
    }
    was_dragged.set_value(false);
    drag.set_value(Some(SheetDrag::start(
      f64::from(ev.client_x()),
      f64::from(ev.client_y()),
      ev.time_stamp(),
    )));
  };
  let on_pointermove = move |ev: web_sys::PointerEvent| {
    let Some(element) = sheet.get_untracked() else {
      return;
    };
    let maybe_offset = drag
      .try_update_value(|maybe_drag| {
        maybe_drag.as_mut().and_then(|drag| {
          let was_dragging = drag.is_dragging();
          let offset = drag.moved(
            f64::from(ev.client_x()),
            f64::from(ev.client_y()),
            ev.time_stamp(),
          )?;
          Some((offset, was_dragging))
        })
      })
      .flatten();
    let Some((offset, was_dragging)) = maybe_offset else {
      return;
    };
    if !was_dragging {
      // From here on the sheet has the finger, also when it leaves the sheet.
      drop(element.set_pointer_capture(ev.pointer_id()));
      drop(element.class_list().add_1("is-dragging"));
      was_dragged.set_value(true);
    }
    set_offset(&element, Some(offset));
  };
  let on_pointerup = move |ev: web_sys::PointerEvent| {
    let Some(finished) = drag.try_update_value(Option::take).flatten() else {
      return;
    };
    let Some(element) = sheet.get_untracked() else {
      return;
    };
    drop(element.class_list().remove_1("is-dragging"));
    let height = element.get_bounding_client_rect().height();
    match finished.released(f64::from(ev.client_y()), height) {
      SheetRelease::Tap => {},
      // The ghost slides on from where the finger left it.
      SheetRelease::Dismiss => on_close.run(()),
      SheetRelease::Settle => set_offset(&element, None),
    }
  };
  let on_pointercancel = move |_: web_sys::PointerEvent| {
    drag.set_value(None);
    if let Some(element) = sheet.get_untracked() {
      drop(element.class_list().remove_1("is-dragging"));
      set_offset(&element, None);
    }
  };
  let on_key = move |ev: web_sys::KeyboardEvent| {
    match ev.key().as_str() {
      "Escape" => {
        ev.prevent_default();
        ev.stop_propagation();
        on_close.run(());
        return;
      },
      "Tab" => trap_tab(&ev, sheet),
      _ => {},
    }
    if let Some(on_keydown) = on_keydown {
      on_keydown.run(ev);
    }
  };

  view! {
    <div class="sheet-backdrop" node_ref=backdrop on:click=move |_| on_close.run(())></div>
    <div
      class=format!("sheet {class}")
      role=role
      // A dialog keeps the page out of reach; a plain menu is no dialog.
      aria-modal=(role == "dialog").then_some("true")
      aria-label=move || label.get()
      tabindex="-1"
      node_ref=sheet
      on:keydown=on_key
      on:pointerdown=on_pointerdown
      on:pointermove=on_pointermove
      on:pointerup=on_pointerup
      on:pointercancel=on_pointercancel
      // A drag ends with the finger over some button: that is no tap on it.
      on:click:capture=move |ev| {
        if was_dragged.get_value() {
          ev.stop_propagation();
          ev.prevent_default();
          was_dragged.set_value(false);
        }
      }
    >
      <div class="sheet-handle" aria-hidden="true"></div>
      {children()}
    </div>
  }
}

/// A menu or picker: a popover beside its trigger on wider layouts, the [`Sheet`] on phones. The
/// popover keeps the markup it always had (`popover {class}` after a backdrop), so its placement
/// rules still apply; the sheet gets `sheet_class` for what its content needs there.
#[component]
pub fn MenuSurface(
  /// The popover's classes besides `popover` (its kind and placement).
  class: &'static str,
  /// Classes the content keeps in the sheet.
  #[prop(optional)]
  sheet_class: &'static str,
  #[prop(default = "menu")] role: &'static str,
  #[prop(into)] label: Signal<String>,
  on_close: Callback<()>,
  #[prop(optional)] node_ref: Option<NodeRef<html::Div>>,
  #[prop(optional)] on_keydown: Option<Callback<web_sys::KeyboardEvent>>,
  children: Children,
) -> impl IntoView {
  let state = use_app_state();
  let node_ref = node_ref.unwrap_or_default();
  if state.layout.get_untracked().is_compact() {
    return view! {
      <Sheet class=sheet_class role=role label=label on_close=on_close node_ref=node_ref on_keydown=on_keydown.unwrap_or(Callback::new(|_| {}))>
        {children()}
      </Sheet>
    }
    .into_any();
  }
  view! {
    <div class="popover-backdrop" on:mousedown=move |_| on_close.run(())></div>
    <div
      class=format!("popover {class}")
      role=role
      aria-label=move || label.get()
      node_ref=node_ref
      on:keydown=move |ev| {
        if let Some(on_keydown) = on_keydown {
          on_keydown.run(ev);
        }
      }
    >
      {children()}
    </div>
  }
  .into_any()
}

/// A sheet whose content scrolls is dragged from the top of its content: once the finger pulls it
/// down there, its touches must not become a scroll (which would cancel the pointer). Only a
/// non-passive `touchmove` can say so; `touch-action` is fixed when the touch starts.
fn hold_touches_while_pulled(sheet: NodeRef<html::Div>, drag: StoredValue<Option<SheetDrag>>) {
  let listener =
    StoredValue::new_local(None::<wasm_bindgen::closure::Closure<dyn FnMut(web_sys::TouchEvent)>>);
  Effect::new(move |_| {
    let Some(element) = sheet.get() else {
      return;
    };
    let on_touchmove = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::TouchEvent)>::new(
      move |ev: web_sys::TouchEvent| {
        let Some(touch) = ev.touches().get(0) else {
          return;
        };
        let y = f64::from(touch.client_y());
        let holds = drag
          .try_with_value(|maybe_drag| maybe_drag.as_ref().is_some_and(|drag| drag.holds_the_finger(y)))
          .unwrap_or(false);
        if holds && ev.cancelable() {
          ev.prevent_default();
        }
      },
    );
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    if element
      .add_event_listener_with_callback_and_add_event_listener_options(
        "touchmove",
        on_touchmove.as_ref().unchecked_ref(),
        &options,
      )
      .is_ok()
    {
      // Kept with the sheet; the element and its listener go together.
      listener.set_value(Some(on_touchmove));
    }
  });
}

/// Moves the sheet by `offset` pixels while dragged; `None` lets it spring back to its place.
fn set_offset(element: &web_sys::HtmlDivElement, maybe_offset: Option<f64>) {
  let style = web_sys::HtmlElement::style(element);
  // A failed style write only means the sheet doesn't move under the finger.
  drop(match maybe_offset {
    // `translate`, not `transform`: the enter and exit animations own `transform`.
    Some(offset) => style.set_property("translate", &format!("0 {offset}px")),
    None => style.remove_property("translate").map(|_| ()),
  });
}

/// Text fields keep their own touch behaviour (moving the caret, selecting).
fn is_in_text_field(ev: &web_sys::PointerEvent) -> bool {
  ev.target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .and_then(|element| {
      element
        .closest("input, textarea, [contenteditable]")
        .ok()
        .flatten()
    })
    .is_some()
}

/// Gives the focus back to what had it when the sheet opened (the button that opened it), if it is
/// still there.
fn return_focus_on_close() {
  let previously_focused = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  let previously_focused = StoredValue::new_local(previously_focused);
  on_cleanup(move || {
    previously_focused.try_with_value(|maybe_element| {
      if let Some(element) = maybe_element.as_ref().filter(|element| element.is_connected()) {
        drop(element.focus());
      }
    });
  });
}
