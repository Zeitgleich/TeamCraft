//! Editing a picture before it is sent (issue #1354), as on Android and iOS: turn it a quarter at a
//! time and crop it with a frame dragged by its corners or moved whole. The browser draws the
//! result (`raster::edited_picture`); the composer puts it in place of the original.

use leptos::prelude::*;
use teamcraft_client_api::picture_edit::{CropCorner, CropFrame};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::raster;
use crate::reactive::SetIfAlive;

/// Longest side of the preview while editing: sharp on a large screen, quick to turn.
const PREVIEW_SIDE: u32 = 1600;
/// Longest side of the edited picture: sharp in the lightbox, within a phone's memory.
const EDITED_SIDE: u32 = 3072;
/// How far an arrow key moves or resizes the frame, as a share of the picture.
const KEY_STEP: f64 = 0.02;

/// What a pointer that went down on the frame moves.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drag {
  Corner(CropCorner),
  Frame,
}

/// The editor over the whole window. `on_done` gets the edited picture's bytes and media type;
/// `on_cancel` leaves the original.
#[component]
pub(crate) fn PictureEditor(
  bytes: Vec<u8>,
  media_type: String,
  on_done: Callback<(Vec<u8>, String)>,
  on_cancel: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let source = StoredValue::new(bytes);
  let media_type = StoredValue::new(media_type);
  let turns = RwSignal::new(0_u8);
  let frame = RwSignal::new(CropFrame::default());
  let preview = RwSignal::new(None::<String>);
  let is_saving = RwSignal::new(false);
  let is_failed = RwSignal::new(false);
  let picture = NodeRef::<leptos::html::Div>::new();
  let drag = StoredValue::new(None::<(Drag, f64, f64)>);

  Effect::new(move |_| {
    let quarter_turns = turns.get();
    let bytes = source.with_value(|bytes| js_sys::Uint8Array::from(bytes.as_slice()));
    let kind = media_type.get_value();
    leptos::task::spawn_local(async move {
      let maybe_blob =
        raster::edited_picture(&bytes, &kind, quarter_turns, CropFrame::default(), PREVIEW_SIDE).await;
      let maybe_url = maybe_blob.and_then(|blob| web_sys::Url::create_object_url_with_blob(&blob).ok());
      if maybe_url.is_none() {
        is_failed.set_if_alive(true);
      }
      let maybe_old = preview.try_get_untracked().flatten();
      // `try_set` hands the value back when the editor is gone: then the new URL goes, else the old.
      let maybe_unused = match preview.try_set(maybe_url) {
        Some(unshown) => unshown,
        None => maybe_old,
      };
      if let Some(url) = maybe_unused {
        drop(web_sys::Url::revoke_object_url(&url));
      }
    });
  });
  on_cleanup(move || {
    if let Some(url) = preview.try_get_untracked().flatten() {
      drop(web_sys::Url::revoke_object_url(&url));
    }
  });

  let shares_of = move |dx: f64, dy: f64| -> Option<(f64, f64)> {
    let rect = picture.get_untracked()?.get_bounding_client_rect();
    (rect.width() > 0.0 && rect.height() > 0.0).then(|| (dx / rect.width(), dy / rect.height()))
  };
  let start = move |ev: web_sys::PointerEvent, what: Drag| {
    ev.stop_propagation();
    ev.prevent_default();
    if let Some(target) = ev
      .current_target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    {
      drop(target.set_pointer_capture(ev.pointer_id()));
    }
    drag.set_value(Some((what, f64::from(ev.client_x()), f64::from(ev.client_y()))));
  };
  let on_move = move |ev: web_sys::PointerEvent| {
    let Some((what, x, y)) = drag.get_value() else {
      return;
    };
    let (now_x, now_y) = (f64::from(ev.client_x()), f64::from(ev.client_y()));
    let Some((dx, dy)) = shares_of(now_x - x, now_y - y) else {
      return;
    };
    drag.set_value(Some((what, now_x, now_y)));
    frame.update(|current| {
      *current = match what {
        Drag::Corner(corner) => current.dragged(corner, dx, dy),
        Drag::Frame => current.moved(dx, dy),
      };
    });
  };
  let on_up = move |_: web_sys::PointerEvent| drag.set_value(None);
  let on_key = move |ev: web_sys::KeyboardEvent| {
    let (dx, dy) = match ev.key().as_str() {
      "ArrowLeft" => (-KEY_STEP, 0.0),
      "ArrowRight" => (KEY_STEP, 0.0),
      "ArrowUp" => (0.0, -KEY_STEP),
      "ArrowDown" => (0.0, KEY_STEP),
      "Escape" => {
        ev.stop_propagation();
        on_cancel.run(());
        return;
      },
      _ => return,
    };
    ev.prevent_default();
    // Arrows move the frame; with Shift they move its bottom-right corner (smaller, larger).
    let is_resizing = ev.shift_key();
    frame.update(|current| {
      *current = match is_resizing {
        true => current.dragged(CropCorner::BottomRight, dx, dy),
        false => current.moved(dx, dy),
      };
    });
  };
  let save = move |_| {
    if turns.get_untracked() == 0 && frame.get_untracked().is_whole() {
      on_cancel.run(());
      return;
    }
    is_saving.set(true);
    let bytes = source.with_value(|bytes| js_sys::Uint8Array::from(bytes.as_slice()));
    let kind = media_type.get_value();
    let (quarter_turns, crop) = (turns.get_untracked(), frame.get_untracked());
    leptos::task::spawn_local(async move {
      let maybe_bytes = match raster::edited_picture(&bytes, &kind, quarter_turns, crop, EDITED_SIDE).await {
        Some(blob) => JsFuture::from(blob.array_buffer())
          .await
          .ok()
          .map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec()),
        None => None,
      };
      is_saving.set_if_alive(false);
      match maybe_bytes {
        Some(edited) => {
          let edited_type = if kind == "image/png" {
            kind
          } else {
            "image/jpeg".to_owned()
          };
          on_done.run((edited, edited_type));
        },
        None => is_failed.set_if_alive(true),
      }
    });
  };
  let corner = move |corner: CropCorner, class: &'static str| {
    view! {
      <span
        class=format!("picture-editor-handle {class}")
        aria-hidden="true"
        on:pointerdown=move |ev| start(ev, Drag::Corner(corner))
      ></span>
    }
  };

  view! {
    <div class="picture-editor" role="dialog" aria-modal="true" aria-label=move || i18n.t("picture-edit-title")>
      <div class="picture-editor-stage">
        {move || match (is_failed.get(), preview.get()) {
          (true, _) => view! { <p class="picture-editor-failed">{i18n.t("picture-edit-failed")}</p> }.into_any(),
          (false, None) => view! { <span class="spinner" role="status" aria-label=i18n.t("action-running")></span> }.into_any(),
          (false, Some(url)) => view! {
            <div class="picture-editor-picture" node_ref=picture on:pointermove=on_move on:pointerup=on_up on:pointercancel=on_up>
              <img class="picture-editor-image" src=url alt="" draggable="false" />
              <div
                class="picture-editor-frame"
                role="group"
                tabindex="0"
                aria-label=move || i18n.t("picture-crop-area")
                style:left=move || format!("{:.3}%", frame.get().left * 100.0)
                style:top=move || format!("{:.3}%", frame.get().top * 100.0)
                style:width=move || format!("{:.3}%", frame.get().width() * 100.0)
                style:height=move || format!("{:.3}%", frame.get().height() * 100.0)
                on:pointerdown=move |ev| start(ev, Drag::Frame)
                on:keydown=on_key
                data-autofocus=""
              >
                {corner(CropCorner::TopLeft, "is-top-left")}
                {corner(CropCorner::TopRight, "is-top-right")}
                {corner(CropCorner::BottomLeft, "is-bottom-left")}
                {corner(CropCorner::BottomRight, "is-bottom-right")}
              </div>
            </div>
          }.into_any(),
        }}
      </div>
      <div class="picture-editor-bar">
        <button type="button" class="btn" on:click=move |_| on_cancel.run(())>{move || i18n.t("cancel")}</button>
        <div class="picture-editor-tools">
          <button
            type="button"
            class="icon-btn"
            aria-label=move || i18n.t("picture-rotate")
            title=move || i18n.t("picture-rotate")
            disabled=move || is_saving.get() || preview.with(Option::is_none)
            on:click=move |_| {
              turns.update(|value| *value = (*value + 1) % 4);
              frame.update(|current| *current = current.turned_left());
            }
          >
            <Icon name=IconName::RotateLeft />
          </button>
          <button
            type="button"
            class="btn"
            disabled=move || turns.get() == 0 && frame.get().is_whole()
            on:click=move |_| {
              turns.set(0);
              frame.set(CropFrame::default());
            }
          >
            {move || i18n.t("picture-reset")}
          </button>
        </div>
        <button
          type="button"
          class="btn btn-primary"
          disabled=move || is_saving.get() || preview.with(Option::is_none)
          on:click=save
        >
          {move || i18n.t("picture-edit-done")}
        </button>
      </div>
    </div>
  }
}
