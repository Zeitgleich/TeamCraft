//! The crop dialog for pictures (issue #323): drag to move, wheel, pinch, slider or keys to zoom,
//! turn by 90°, with a round or square mask and live previews at the sizes the picture is shown.
//! Avatars and space pictures share it; the caller uploads the cropped PNG it hands back.
//!
//! It opens on top of the dialog that picked the file (a portal into `<body>`, so the opener's
//! focus trap and Escape handling don't see it) and only closes through the caller.

use futures_util::future::LocalBoxFuture;
use leptos::html;
use leptos::portal::Portal;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::components::modal::trap_tab;
use crate::feedback::{ActionFailure, PendingAction, failure_message};
use crate::i18n::use_i18n;
use crate::image_crop::{CropView, ImagePickError, crop_png, prepare_editor_image};
use crate::object_url::ObjectUrl;
use crate::reactive::SetIfAlive;

/// Room around the crop frame where the rest of the picture shows dimmed.
const STAGE_INSET_PX: f64 = 24.0;
/// What the frame measures before the stage is laid out (the stage's CSS width, minus the inset).
const FALLBACK_FRAME_PX: f64 = 280.0;
/// An arrow key moves the picture this far; with Shift, `KEY_STEP_LARGE_PX`.
const KEY_STEP_PX: f64 = 8.0;
const KEY_STEP_LARGE_PX: f64 = 40.0;
/// One `+`/`-` press or zoom button click.
const ZOOM_STEP: f64 = 1.2;
/// Wheel zoom per scrolled pixel (a mouse notch is about 100 px; trackpad pinches send small steps).
const WHEEL_ZOOM_PER_PX: f64 = 0.002;
/// Pixels per wheel "line" (`deltaMode` 1, Firefox with a mouse).
const WHEEL_LINE_PX: f64 = 16.0;

/// The caller's upload of a cropped picture (see [`ImageCropDialog`]).
pub type CropUpload = LocalBoxFuture<'static, Result<(), ActionFailure>>;

/// What the picture becomes: decides the mask, the previews and the uploaded size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropTarget {
  Avatar,
  SpacePicture,
  ChannelPicture,
}

/// Where the dialog stands with the picked file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CropPhase {
  /// Reading and decoding the file.
  Preparing,
  Ready,
  /// Cutting the chosen square (a moment, on the page's thread); the picture stays as it is.
  Cropping,
  /// Cutting failed; the picture stays and "Apply" can be tried again.
  CropFailed(ImagePickError),
  /// The file could not be read: there is no picture to crop, only another file to pick.
  LoadFailed(ImagePickError),
}

/// One live preview: the size it is shown at, its corner radius and its caption.
struct CropPreview {
  size_px: f64,
  radius: &'static str,
  caption_key: &'static str,
}

/// The editor's copy of the picture: its PNG (cropped from on apply) and the URL showing it.
struct EditorPicture {
  png: Vec<u8>,
  url: ObjectUrl,
  width: u32,
  height: u32,
}

/// A pointer on the stage, for drags and two-finger pinches.
#[derive(Clone, Copy, Debug)]
struct ActivePointer {
  id: i32,
  x: f64,
  y: f64,
}

impl CropTarget {
  fn is_round(self) -> bool {
    matches!(self, Self::Avatar)
  }

  fn title_key(self) -> &'static str {
    match self {
      Self::Avatar => "crop-title-avatar",
      Self::SpacePicture => "crop-title-space",
      Self::ChannelPicture => "crop-title-channel",
    }
  }

  /// Side of the uploaded square (teamcraft-core's sizes; the server re-encodes it again).
  fn output_px(self) -> u32 {
    match self {
      Self::Avatar => 512,
      Self::SpacePicture | Self::ChannelPicture => 256,
    }
  }

  /// Where the picture shows, at the CSS sizes used there (main.css: `.avatar-*`, `.space-btn`,
  /// `.space-tile`).
  fn previews(self) -> [CropPreview; 3] {
    match self {
      Self::Avatar => [
        preview(64.0, "50%", "crop-preview-profile"),
        preview(38.0, "50%", "crop-preview-messages"),
        preview(20.0, "50%", "crop-preview-lists"),
      ],
      Self::SpacePicture => [
        preview(64.0, "var(--radius-lg)", "crop-preview-settings"),
        preview(40.0, "var(--radius-md)", "crop-preview-rail"),
        preview(20.0, "var(--radius-sm)", "crop-preview-sidebar"),
      ],
      Self::ChannelPicture => [
        preview(64.0, "var(--radius-lg)", "crop-preview-settings"),
        preview(32.0, "var(--radius-md)", "crop-preview-header"),
        preview(20.0, "var(--radius-sm)", "crop-preview-sidebar"),
      ],
    }
  }
}

/// The crop dialog for `file`. `on_apply` gets the cropped PNG and returns the upload; the dialog
/// shows it as pending, and a failure as a toast and in the dialog, where "Apply" can be retried.
/// The upload closes the dialog (through the caller's state) when it succeeds.
/// `on_cancel` keeps the old picture.
#[component]
pub fn ImageCropDialog(
  file: web_sys::File,
  target: CropTarget,
  on_apply: Callback<Vec<u8>, CropUpload>,
  on_cancel: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let phase = RwSignal::new(CropPhase::Preparing);
  let picture = StoredValue::new_local(None::<EditorPicture>);
  let view = RwSignal::new(CropView::new(1, 1));
  let frame = RwSignal::new(FALLBACK_FRAME_PX);
  // Keyboard and button changes glide; drags, pinches and the slider follow the hand directly.
  let is_animated = RwSignal::new(false);
  let is_dragging = RwSignal::new(false);
  let pointers = StoredValue::new(Vec::<ActivePointer>::new());
  let dialog = NodeRef::<html::Div>::new();
  let stage = NodeRef::<html::Div>::new();
  let upload = PendingAction::new();
  let busy = move || upload.is_pending();
  let is_pending = move || busy() || phase.get() == CropPhase::Cropping;
  // Interactive: the picture is there and nothing runs.
  let is_ready = move || matches!(phase.get(), CropPhase::Ready | CropPhase::CropFailed(_));
  // The picture is shown (also while cropping and uploading, so nothing flickers).
  let has_picture = Memo::new(move |_| {
    matches!(
      phase.get(),
      CropPhase::Ready | CropPhase::Cropping | CropPhase::CropFailed(_)
    )
  });

  remember_focus();
  load_picture(file, phase, picture, view);

  let measure = move || {
    if let Some(element) = stage.get_untracked() {
      let width = f64::from(element.offset_width());
      if width > 2.0 * STAGE_INSET_PX {
        frame.set(width - 2.0 * STAGE_INSET_PX);
      }
    }
  };
  // Measure once the stage is laid out, and again when the window changes size.
  Effect::new(move |_| {
    if stage.get().is_some() {
      measure();
    }
  });
  let resize = window_event_listener(leptos::ev::resize, move |_| measure());
  on_cleanup(move || resize.remove());
  // The dialog takes focus when it opens, the stage once the picture is there; cropping and
  // uploading leave focus where it is.
  Effect::new(move |_| {
    let target = if has_picture.get() {
      stage
        .get()
        .map(|element| element.unchecked_into::<web_sys::HtmlElement>())
    } else {
      dialog
        .get()
        .map(|element| element.unchecked_into::<web_sys::HtmlElement>())
    };
    if let Some(element) = target {
      drop(element.focus());
    }
  });

  let change = move |animated: bool, edit: &dyn Fn(&mut CropView, f64)| {
    if !is_ready() || untrack(|| upload.is_pending()) {
      return;
    }
    is_animated.set(animated);
    let size = frame.get_untracked();
    view.update(|current| edit(current, size));
  };
  let zoom_by = move |factor: f64| {
    change(true, &|current, size| {
      current.zoom_to(current.zoom() * factor, None, size)
    })
  };
  let rotate = move || change(true, &|current, _| current.rotate_clockwise());
  let reset = move || change(true, &|current, _| current.reset());

  let apply = move || {
    if !is_ready() || untrack(|| upload.is_pending()) {
      return;
    }
    phase.set(CropPhase::Cropping);
    let rect = view.get_untracked().crop();
    spawn_local(async move {
      // Let the button show its spinner before the page's thread is busy cropping.
      next_frame().await;
      let Some(result) = picture.try_with_value(|maybe| {
        maybe.as_ref().map_or(Err(ImagePickError::Unreadable), |editor| {
          crop_png(&editor.png, rect, target.output_px())
        })
      }) else {
        return;
      };
      match result {
        Ok(png) => {
          phase.set_if_alive(CropPhase::Ready);
          if let Some(work) = on_apply.try_run(png) {
            upload.spawn(work);
          }
        },
        Err(error) => {
          phase.set_if_alive(CropPhase::CropFailed(error));
        },
      }
    });
  };
  let cancel = move || {
    if !untrack(|| upload.is_pending()) {
      on_cancel.run(());
    }
  };

  let on_dialog_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Escape" => {
      ev.stop_propagation();
      ev.prevent_default();
      cancel();
    },
    "Tab" => trap_tab(&ev, dialog),
    _ => {},
  };
  let on_stage_keydown = move |ev: web_sys::KeyboardEvent| {
    let step = if ev.shift_key() {
      KEY_STEP_LARGE_PX
    } else {
      KEY_STEP_PX
    };
    // Arrows move the picture the way the key points.
    let pan = |dx: f64, dy: f64| change(true, &move |current, size| current.pan_by(dx, dy, size));
    match ev.key().as_str() {
      "ArrowLeft" => pan(-step, 0.0),
      "ArrowRight" => pan(step, 0.0),
      "ArrowUp" => pan(0.0, -step),
      "ArrowDown" => pan(0.0, step),
      "+" | "=" => zoom_by(ZOOM_STEP),
      "-" | "_" => zoom_by(1.0 / ZOOM_STEP),
      "r" | "R" => rotate(),
      "0" => reset(),
      "Enter" => apply(),
      _ => return,
    }
    // Handled here: neither the page scrolls nor an app shortcut sees the key.
    ev.prevent_default();
    ev.stop_propagation();
  };

  let on_pointer_down = move |ev: web_sys::PointerEvent| {
    if !is_ready() || untrack(|| upload.is_pending()) || (ev.pointer_type() == "mouse" && ev.button() != 0) {
      return;
    }
    ev.prevent_default();
    if let Some(element) = stage.get_untracked() {
      drop(element.focus());
      drop(element.set_pointer_capture(ev.pointer_id()));
    }
    pointers.update_value(|list| {
      list.retain(|pointer| pointer.id != ev.pointer_id());
      list.push(ActivePointer {
        id: ev.pointer_id(),
        x: f64::from(ev.client_x()),
        y: f64::from(ev.client_y()),
      });
    });
    is_animated.set(false);
    is_dragging.set(true);
  };
  let on_pointer_move = move |ev: web_sys::PointerEvent| {
    let moved = ActivePointer {
      id: ev.pointer_id(),
      x: f64::from(ev.client_x()),
      y: f64::from(ev.client_y()),
    };
    let Some((before, maybe_other)) = pointers.with_value(|list| {
      let before = list.iter().find(|pointer| pointer.id == moved.id).copied()?;
      let maybe_other = list.iter().find(|pointer| pointer.id != moved.id).copied();
      Some((before, maybe_other))
    }) else {
      return;
    };
    pointers.update_value(|list| {
      if let Some(pointer) = list.iter_mut().find(|pointer| pointer.id == moved.id) {
        *pointer = moved;
      }
    });
    let size = frame.get_untracked();
    match maybe_other {
      None => view.update(|current| current.pan_by(moved.x - before.x, moved.y - before.y, size)),
      Some(other) => {
        // Two fingers: the midpoint pans, the spread zooms around it.
        let spread_before = distance(before, other);
        let spread_after = distance(moved, other);
        let mid_before = midpoint(before, other);
        let mid_after = midpoint(moved, other);
        let anchor = stage_point(stage, mid_after);
        view.update(|current| {
          current.pan_by(mid_after.0 - mid_before.0, mid_after.1 - mid_before.1, size);
          if spread_before > 0.0 {
            current.zoom_to(current.zoom() * spread_after / spread_before, anchor, size);
          }
        });
      },
    }
  };
  let on_pointer_end = move |ev: web_sys::PointerEvent| {
    pointers.update_value(|list| list.retain(|pointer| pointer.id != ev.pointer_id()));
    if pointers.with_value(Vec::is_empty) {
      is_dragging.set(false);
    }
  };
  let on_wheel = move |ev: web_sys::WheelEvent| {
    if !is_ready() || untrack(|| upload.is_pending()) {
      return;
    }
    ev.prevent_default();
    let unit = match ev.delta_mode() {
      1 => WHEEL_LINE_PX,
      2 => frame.get_untracked(),
      _ => 1.0,
    };
    let factor = (-ev.delta_y() * unit * WHEEL_ZOOM_PER_PX).exp();
    let anchor = stage_point(stage, (f64::from(ev.client_x()), f64::from(ev.client_y())));
    is_animated.set(false);
    let size = frame.get_untracked();
    view.update(|current| current.zoom_to(current.zoom() * factor, anchor, size));
  };

  let image = move || {
    picture.with_value(|maybe| {
      maybe.as_ref().map(|editor| {
        let url = editor.url.as_str().to_owned();
        let (width, height) = (editor.width, editor.height);
        view! {
          <img
            class="crop-image"
            src=url
            alt=""
            draggable="false"
            style:width=format!("{width}px")
            style:height=format!("{height}px")
            style:transform=move || view.get().css_transform(frame.get(), STAGE_INSET_PX)
          />
        }
      })
    })
  };
  let previews = move || {
    target
      .previews()
      .into_iter()
      .map(|preview| {
        let image = move || {
          picture.with_value(|maybe| {
            maybe.as_ref().map(|editor| {
              let url = editor.url.as_str().to_owned();
              let (width, height) = (editor.width, editor.height);
              view! {
                <img
                  class="crop-image"
                  src=url
                  alt=""
                  style:width=format!("{width}px")
                  style:height=format!("{height}px")
                  style:transform=move || view.get().css_transform(preview.size_px, 0.0)
                />
              }
            })
          })
        };
        view! {
          <figure class="crop-preview">
            <span
              class="crop-preview-frame"
              style:width=format!("{}px", preview.size_px)
              style:height=format!("{}px", preview.size_px)
              style:border-radius=preview.radius
            >
              {move || has_picture.get().then(image)}
            </span>
            <figcaption>{move || i18n.t(preview.caption_key)}</figcaption>
          </figure>
        }
      })
      .collect_view()
  };
  let zoom_percent = move || format!("{:.0}", view.get().zoom() * 100.0);
  let message = move || match phase.get() {
    CropPhase::CropFailed(error) | CropPhase::LoadFailed(error) => Some(i18n.t(error.i18n_key())),
    _ => upload
      .maybe_failure()
      .map(|failure| failure_message(i18n, &failure)),
  };

  view! {
    <Portal>
      <div class="crop-backdrop">
        <div
          class="modal crop-dialog"
          role="dialog"
          aria-modal="true"
          aria-labelledby="crop-title"
          aria-describedby="crop-help"
          tabindex="-1"
          node_ref=dialog
          on:keydown=on_dialog_keydown
        >
          <header class="modal-head">
            <h2 id="crop-title">{move || i18n.t(target.title_key())}</h2>
            <button type="button" class="icon-btn" aria-label=move || i18n.t("cancel") prop:disabled=busy on:click=move |_| cancel()>
              <Icon name=IconName::Close />
            </button>
          </header>
          <div class="modal-body crop-body" aria-busy=move || is_pending().to_string()>
            <div
              class="crop-stage"
              class:is-round=target.is_round()
              class:is-dragging=is_dragging
              class:is-animated=is_animated
              tabindex=move || if has_picture.get() { "0" } else { "-1" }
              role="group"
              aria-label=move || i18n.t("crop-stage")
              aria-describedby="crop-help"
              node_ref=stage
              on:pointerdown=on_pointer_down
              on:pointermove=on_pointer_move
              on:pointerup=on_pointer_end
              on:pointercancel=on_pointer_end
              on:wheel=on_wheel
              on:keydown=on_stage_keydown
            >
              {move || has_picture.get().then(image)}
              <div class="crop-mask" style:inset=format!("{STAGE_INSET_PX}px") aria-hidden="true"></div>
              <Show when=move || phase.get() == CropPhase::Preparing fallback=|| ()>
                <div class="crop-loading" role="status">
                  <span class="spinner" aria-hidden="true"></span>
                  <span>{move || i18n.t("crop-preparing")}</span>
                </div>
              </Show>
            </div>
            <p id="crop-help" class="hint crop-help">{move || i18n.t("crop-help")}</p>
            <div class="crop-controls">
              <button
                type="button"
                class="icon-btn"
                aria-label=move || i18n.t("crop-zoom-out")
                prop:disabled=move || { !is_ready() || view.get().zoom() <= 1.0 }
                on:click=move |_| zoom_by(1.0 / ZOOM_STEP)
              >
                <Icon name=IconName::ZoomOut />
              </button>
              <input
                type="range"
                class="crop-zoom"
                min="1"
                step="0.01"
                prop:max=move || view.get().max_zoom().to_string()
                prop:value=move || view.get().zoom().to_string()
                prop:disabled=move || { !is_ready() || view.get().max_zoom() <= 1.0 }
                aria-label=move || i18n.t("crop-zoom")
                aria-valuetext=move || i18n.t_with("crop-zoom-value", &[("percent", &zoom_percent())])
                on:input=move |ev| {
                  if let Ok(zoom) = event_target_value(&ev).parse::<f64>() {
                    change(false, &move |current, size| current.zoom_to(zoom, None, size));
                  }
                }
              />
              <button
                type="button"
                class="icon-btn"
                aria-label=move || i18n.t("crop-zoom-in")
                prop:disabled=move || { !is_ready() || view.get().zoom() >= view.get().max_zoom() }
                on:click=move |_| zoom_by(ZOOM_STEP)
              >
                <Icon name=IconName::ZoomIn />
              </button>
              <span class="crop-controls-gap"></span>
              <button
                type="button"
                class="icon-btn"
                aria-label=move || i18n.t("crop-rotate")
                title=move || i18n.t("crop-rotate")
                prop:disabled=move || !is_ready()
                on:click=move |_| rotate()
              >
                <Icon name=IconName::Rotate />
              </button>
              <button type="button" class="btn btn-small" prop:disabled=move || !is_ready() on:click=move |_| reset()>
                {move || i18n.t("crop-reset")}
              </button>
            </div>
            <div class="crop-previews" role="group" aria-label=move || i18n.t("crop-preview")>
              {previews}
            </div>
            {move || message().map(|text| view! { <p class="field-error" role="alert">{text}</p> })}
          </div>
          <footer class="modal-foot">
            <span class="grow"></span>
            <button type="button" class="btn" prop:disabled=busy on:click=move |_| cancel()>
              {move || i18n.t("cancel")}
            </button>
            <button
              type="button"
              class="btn btn-primary crop-apply"
              prop:disabled=move || !is_ready() || is_pending()
              aria-busy=move || is_pending().to_string()
              on:click=move |_| apply()
            >
              <Show when=is_pending fallback=|| ()>
                <span class="spinner" aria-hidden="true"></span>
              </Show>
              {move || {
                let has_failed = matches!(phase.get(), CropPhase::CropFailed(_)) || upload.maybe_failure().is_some();
                if busy() {
                  i18n.t("crop-uploading")
                } else if has_failed {
                  i18n.t("crop-retry")
                } else {
                  i18n.t("crop-apply")
                }
              }}
            </button>
          </footer>
        </div>
      </div>
    </Portal>
  }
}

/// `dragover` handler of a drop zone for pictures: lets a dragged file be dropped there.
pub fn allow_image_drop(ev: web_sys::DragEvent) {
  let carries_files = ev
    .data_transfer()
    .is_some_and(|transfer| transfer.types().includes(&"Files".into(), 0));
  if carries_files {
    ev.prevent_default();
  }
}

/// The first file of a `drop` on a picture's drop zone (checked by the caller like a picked one).
pub fn dropped_file(ev: &web_sys::DragEvent) -> Option<web_sys::File> {
  let file = ev.data_transfer()?.files()?.item(0)?;
  ev.prevent_default();
  Some(file)
}

fn preview(size_px: f64, radius: &'static str, caption_key: &'static str) -> CropPreview {
  CropPreview {
    size_px,
    radius,
    caption_key,
  }
}

/// Puts focus back on what had it (the file picker) when the dialog goes away.
fn remember_focus() {
  let previously_focused = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  let previously_focused = StoredValue::new_local(previously_focused);
  on_cleanup(move || {
    previously_focused.with_value(|maybe_element| {
      if let Some(element) = maybe_element.as_ref().filter(|element| element.is_connected()) {
        drop(element.focus());
      }
    });
  });
}

/// Reads and decodes `file` into the editor's copy; a failure stays in the dialog as a message.
fn load_picture(
  file: web_sys::File,
  phase: RwSignal<CropPhase>,
  picture: StoredValue<Option<EditorPicture>, LocalStorage>,
  view: RwSignal<CropView>,
) {
  spawn_local(async move {
    let Ok(buffer) = JsFuture::from(file.array_buffer()).await else {
      phase.set_if_alive(CropPhase::LoadFailed(ImagePickError::Unreadable));
      return;
    };
    // The spinner paints before decoding takes the page's thread for a moment.
    next_frame().await;
    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
    let prepared = prepare_editor_image(&bytes).and_then(|(png, width, height)| {
      let url = ObjectUrl::from_bytes(&png, "image/png").ok_or(ImagePickError::Unreadable)?;
      Ok(EditorPicture {
        png,
        url,
        width,
        height,
      })
    });
    match prepared {
      Ok(editor) => {
        let (width, height) = (editor.width, editor.height);
        if picture.try_update_value(|slot| *slot = Some(editor)).is_some() {
          view.set_if_alive(CropView::new(width, height));
          phase.set_if_alive(CropPhase::Ready);
        }
      },
      Err(error) => {
        phase.set_if_alive(CropPhase::LoadFailed(error));
      },
    }
  });
}

/// Waits until the browser has painted (two animation frames would be exact; a short timeout is
/// enough and also runs in a background tab).
async fn next_frame() {
  let timer = js_sys::Promise::new(&mut |resolve, _reject| {
    if let Some(window) = web_sys::window() {
      drop(window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 16));
    }
  });
  // A failed timer resolves nothing to wait for.
  drop(JsFuture::from(timer).await);
}

/// A client position in the frame's coordinates (its top-left corner is 0, 0).
fn stage_point(stage: NodeRef<html::Div>, (x, y): (f64, f64)) -> Option<(f64, f64)> {
  let rect = stage.get_untracked()?.get_bounding_client_rect();
  Some((x - rect.left() - STAGE_INSET_PX, y - rect.top() - STAGE_INSET_PX))
}

fn distance(a: ActivePointer, b: ActivePointer) -> f64 {
  (a.x - b.x).hypot(a.y - b.y)
}

fn midpoint(a: ActivePointer, b: ActivePointer) -> (f64, f64) {
  ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0)
}
