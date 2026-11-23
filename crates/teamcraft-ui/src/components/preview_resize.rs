//! The file preview as a floating window (#2341): dragged by its header it moves, dragged at an
//! edge or corner it resizes, both live under the pointer. The dialog is centred by the backdrop,
//! so a move is an offset (`translate`) from the centre and a resize grows both sides: an edge
//! stays under the pointer when the size changes by twice the pointer's way. Position and size are
//! kept on the device for the next preview; a double click on the header goes back to the default.
//! The header always stays in the window. On compact layouts the preview is a full-screen sheet
//! and none of this applies (the stylesheet hides the edges; moving is off there).

use leptos::html;
use leptos::prelude::*;
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;

use crate::settings;
use crate::state::AppState;

/// Where the preview's place is kept.
const FRAME_KEY: &str = "teamcraft.preview_frame";
/// The smallest preview, and the room it keeps to the window's edges.
const MIN_WIDTH_PX: f64 = 360.0;
const MIN_HEIGHT_PX: f64 = 280.0;
const WINDOW_MARGIN_PX: f64 = 32.0;
/// How much of the header stays in the window, so it can always be dragged back.
const KEEP_VISIBLE_PX: f64 = 96.0;
/// Header elements a press on which is no move: its buttons and fields.
const NOT_A_HANDLE: &str = "button, input, select, a, [role='button'], [contenteditable]";

/// The edges and corners that resize, as (class, horizontal, vertical) direction.
const HANDLES: [(&str, f64, f64); 8] = [
  ("is-n", 0.0, -1.0),
  ("is-s", 0.0, 1.0),
  ("is-e", 1.0, 0.0),
  ("is-w", -1.0, 0.0),
  ("is-ne", 1.0, -1.0),
  ("is-nw", -1.0, -1.0),
  ("is-se", 1.0, 1.0),
  ("is-sw", -1.0, 1.0),
];

/// The moving and resizing of one preview dialog.
#[derive(Clone, Copy)]
pub struct PreviewFrame {
  state: AppState,
  gesture: StoredValue<Option<Gesture>>,
  frame: StoredValue<Frame>,
}

/// The dialog's place: its size (none: the stylesheet's) and its offset from the centre.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Frame {
  #[serde(default)]
  maybe_size: Option<Size>,
  #[serde(default)]
  x: f64,
  #[serde(default)]
  y: f64,
}

/// A size of the preview, in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Size {
  width: f64,
  height: f64,
}

/// A drag in progress: what it changes, where the pointer started and the frame then.
#[derive(Clone, Copy, Debug)]
struct Gesture {
  kind: GestureKind,
  start_x: f64,
  start_y: f64,
  start: Frame,
  start_size: Size,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum GestureKind {
  Move,
  /// Resizing in a direction: -1, 0 or 1 per axis.
  Resize {
    x: f64,
    y: f64,
  },
}

impl PreviewFrame {
  pub fn new(state: AppState) -> Self {
    Self {
      state,
      gesture: StoredValue::new(None),
      frame: StoredValue::new(settings::load_json(FRAME_KEY).unwrap_or_default()),
    }
  }

  /// The edges and corners; placed in the dialog, they also apply the kept place once it shows.
  pub fn handles(self) -> impl IntoView {
    let anchor = NodeRef::<html::Span>::new();
    Effect::new(move |_| {
      // A full-screen sheet on compact layouts: no kept place there (again when the window grows).
      let is_compact = self.state.is_compact();
      if let Some(dialog) = anchor.get().and_then(|span| dialog_of(&span)) {
        let frame = if is_compact {
          Frame::default()
        } else {
          self.frame.get_value()
        };
        apply(&dialog, frame);
      }
    });
    let handle = move |(class, x, y): (&'static str, f64, f64)| {
      view! {
        <span
          class=format!("preview-resize {class}")
          aria-hidden="true"
          on:pointerdown=move |ev| self.start(ev, GestureKind::Resize { x, y })
          on:pointermove=move |ev| self.follow(ev)
          on:pointerup=move |ev| self.end(ev)
          on:pointercancel=move |ev| self.end(ev)
        ></span>
      }
    };
    view! {
      <span class="preview-resize-anchor" node_ref=anchor aria-hidden="true"></span>
      {HANDLES.into_iter().map(handle).collect_view()}
    }
  }

  /// A press on the header: starts a move, unless it is on a button or field there.
  pub fn on_head_down(self, ev: web_sys::PointerEvent) {
    let is_control = target_of(&ev)
      .and_then(|target| target.closest(NOT_A_HANDLE).ok().flatten())
      .is_some();
    if ev.button() == 0 && !is_control && !self.state.is_compact() {
      self.start(ev, GestureKind::Move);
    }
  }

  pub fn on_head_move(self, ev: web_sys::PointerEvent) {
    self.follow(ev);
  }

  pub fn on_head_up(self, ev: web_sys::PointerEvent) {
    self.end(ev);
  }

  /// A double click on the header: back to the default size, in the middle.
  pub fn on_head_double_click(self, ev: web_sys::MouseEvent) {
    let is_control = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|target| target.closest(NOT_A_HANDLE).ok().flatten())
      .is_some();
    if is_control {
      return;
    }
    let maybe_dialog = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|target| dialog_of(&target));
    self.frame.set_value(Frame::default());
    if let Some(dialog) = maybe_dialog {
      apply(&dialog, Frame::default());
    }
    settings::remove(FRAME_KEY);
  }

  fn start(self, ev: web_sys::PointerEvent, kind: GestureKind) {
    let Some(target) = target_of(&ev) else {
      return;
    };
    let Some(dialog) = dialog_of(&target) else {
      return;
    };
    ev.prevent_default();
    drop(target.set_pointer_capture(ev.pointer_id()));
    let rect = dialog.get_bounding_client_rect();
    self.gesture.set_value(Some(Gesture {
      kind,
      start_x: f64::from(ev.client_x()),
      start_y: f64::from(ev.client_y()),
      start: self.frame.get_value(),
      start_size: Size {
        width: rect.width(),
        height: rect.height(),
      },
    }));
  }

  fn follow(self, ev: web_sys::PointerEvent) {
    let Some(gesture) = self.gesture.get_value() else {
      return;
    };
    let Some(dialog) = target_of(&ev).and_then(|target| dialog_of(&target)) else {
      return;
    };
    let next = gesture.frame_at(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let next = fit(next, gesture.start_size, window_size());
    self.frame.set_value(next);
    apply(&dialog, next);
  }

  fn end(self, _ev: web_sys::PointerEvent) {
    if self.gesture.get_value().is_none() {
      return;
    }
    self.gesture.set_value(None);
    settings::store_json(FRAME_KEY, &self.frame.get_value());
  }
}

impl Gesture {
  /// The frame with the pointer at (`x`, `y`).
  fn frame_at(self, x: f64, y: f64) -> Frame {
    let (dx, dy) = (x - self.start_x, y - self.start_y);
    match self.kind {
      GestureKind::Move => Frame {
        x: self.start.x + dx,
        y: self.start.y + dy,
        ..self.start
      },
      // Centred: the size changes by twice the pointer's way, so the dragged edge follows it.
      GestureKind::Resize {
        x: along_x,
        y: along_y,
      } => Frame {
        maybe_size: Some(Size {
          width: self.start_size.width + 2.0 * along_x * dx,
          height: self.start_size.height + 2.0 * along_y * dy,
        }),
        ..self.start
      },
    }
  }
}

/// `frame` within the window: the size between the minimum and the window, the header in view.
/// `fallback` is the size when the frame keeps the stylesheet's.
fn fit(frame: Frame, fallback: Size, window: Size) -> Frame {
  let max_width = (window.width - WINDOW_MARGIN_PX).max(MIN_WIDTH_PX);
  let max_height = (window.height - WINDOW_MARGIN_PX).max(MIN_HEIGHT_PX);
  let maybe_size = frame.maybe_size.map(|size| Size {
    width: size.width.clamp(MIN_WIDTH_PX, max_width),
    height: size.height.clamp(MIN_HEIGHT_PX, max_height),
  });
  let size = maybe_size.unwrap_or(fallback);
  // The dialog's top is at window/2 - height/2 + y; it stays between 0 and the window's foot less
  // a header's room, and some of it stays in view sideways.
  let min_y = (size.height - window.height) / 2.0;
  let max_y = (window.height + size.height) / 2.0 - KEEP_VISIBLE_PX;
  let reach_x = (window.width + size.width) / 2.0 - KEEP_VISIBLE_PX;
  Frame {
    maybe_size,
    x: frame.x.clamp(-reach_x.max(0.0), reach_x.max(0.0)),
    y: frame.y.clamp(min_y.min(0.0), max_y.max(0.0)),
  }
}

/// Puts the dialog in its place.
fn apply(dialog: &web_sys::HtmlElement, frame: Frame) {
  let style = dialog.style();
  let frame = match frame.maybe_size {
    Some(_) => fit(
      frame,
      Size {
        width: 0.0,
        height: 0.0,
      },
      window_size(),
    ),
    None => frame,
  };
  match frame.maybe_size {
    Some(size) => {
      drop(style.set_property("width", &format!("{:.0}px", size.width)));
      drop(style.set_property("height", &format!("{:.0}px", size.height)));
    },
    None => {
      drop(style.remove_property("width"));
      drop(style.remove_property("height"));
    },
  }
  if frame.x == 0.0 && frame.y == 0.0 {
    drop(style.remove_property("translate"));
  } else {
    drop(style.set_property("translate", &format!("{:.0}px {:.0}px", frame.x, frame.y)));
  }
}

fn window_size() -> Size {
  let read = |value: Option<Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>>| {
    value
      .and_then(Result::ok)
      .and_then(|value| value.as_f64())
      .unwrap_or(f64::MAX / 4.0)
  };
  let window = web_sys::window();
  Size {
    width: read(window.as_ref().map(web_sys::Window::inner_width)),
    height: read(window.as_ref().map(web_sys::Window::inner_height)),
  }
}

fn target_of(ev: &web_sys::PointerEvent) -> Option<web_sys::Element> {
  ev.target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
}

/// The preview dialog around `element`.
fn dialog_of(element: &web_sys::Element) -> Option<web_sys::HtmlElement> {
  element
    .closest(".modal.file-preview")
    .ok()
    .flatten()
    .and_then(|dialog| dialog.dyn_into::<web_sys::HtmlElement>().ok())
}

#[cfg(test)]
mod tests {
  use super::*;

  const WINDOW: Size = Size {
    width: 1600.0,
    height: 1000.0,
  };
  const START: Size = Size {
    width: 800.0,
    height: 600.0,
  };

  #[test]
  fn a_centred_dialog_grows_by_twice_the_pointers_way() {
    let frame = gesture(GestureKind::Resize { x: 1.0, y: -1.0 }).frame_at(130.0, 80.0);
    assert_eq!(
      frame.maybe_size,
      Some(Size {
        width: 860.0,
        height: 640.0
      })
    );
    let side = gesture(GestureKind::Resize { x: -1.0, y: 0.0 }).frame_at(70.0, 500.0);
    assert_eq!(
      side.maybe_size,
      Some(Size {
        width: 860.0,
        height: 600.0
      })
    );
  }

  #[test]
  fn a_move_shifts_the_offset_only() {
    let frame = gesture(GestureKind::Move).frame_at(150.0, 60.0);
    assert_eq!((frame.x, frame.y, frame.maybe_size), (50.0, -40.0, None));
  }

  #[test]
  fn the_size_stays_between_the_minimum_and_the_window() {
    let tiny = fit(sized(100.0, 50.0), START, WINDOW);
    assert_eq!(
      tiny.maybe_size,
      Some(Size {
        width: MIN_WIDTH_PX,
        height: MIN_HEIGHT_PX
      })
    );
    let huge = fit(sized(9_000.0, 9_000.0), START, WINDOW);
    assert_eq!(
      huge.maybe_size,
      Some(Size {
        width: WINDOW.width - WINDOW_MARGIN_PX,
        height: WINDOW.height - WINDOW_MARGIN_PX
      })
    );
  }

  #[test]
  fn the_header_stays_in_the_window() {
    let far = Frame {
      maybe_size: None,
      x: 5_000.0,
      y: -5_000.0,
    };
    let fitted = fit(far, START, WINDOW);
    // The top edge at the window's top, a part of the dialog still in view on the right.
    assert_eq!(fitted.y, -200.0);
    assert_eq!(fitted.x, 1_200.0 - KEEP_VISIBLE_PX);
    let low = fit(Frame { y: 5_000.0, ..far }, START, WINDOW);
    assert_eq!(low.y, 800.0 - KEEP_VISIBLE_PX);
  }

  #[test]
  fn the_kept_frame_reads_back_and_tolerates_missing_fields() {
    let frame = Frame {
      maybe_size: Some(START),
      x: 12.0,
      y: -4.0,
    };
    let json = serde_json::to_string(&frame).expect("encodes");
    assert_eq!(serde_json::from_str::<Frame>(&json).expect("decodes"), frame);
    assert_eq!(
      serde_json::from_str::<Frame>("{}").expect("decodes"),
      Frame::default()
    );
  }

  fn gesture(kind: GestureKind) -> Gesture {
    Gesture {
      kind,
      start_x: 100.0,
      start_y: 100.0,
      start: Frame::default(),
      start_size: START,
    }
  }

  fn sized(width: f64, height: f64) -> Frame {
    Frame {
      maybe_size: Some(Size { width, height }),
      ..Frame::default()
    }
  }
}
