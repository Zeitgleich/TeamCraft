//! The image lightbox: one image at full resolution over the app, with zoom and pan (wheel, pinch,
//! double-click or double-tap, `+` `-` `0`), previous/next through the album of a message or the
//! conversation's images, and save and forward.
//!
//! Memory stays bounded: only the image on screen is held at full size. Its decrypted bytes live in
//! one `blob:` URL that is revoked when the lightbox moves to another image or closes; a download
//! that finishes after the user moved on is dropped (and revoked) unseen.

use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{FileInfo, FileTransfer};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::clock::format_date_time;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::lightbox::{
  Direction, Gesture, ImageKey, KEY_ZOOM_STEP, LightboxScope, Point, Pointers, Release, Size, Viewport,
  album_len, images_in_scope, neighbor, position, wheel_zoom_factor,
};
use crate::model::ConversationId;
use crate::object_url::{ObjectUrl, fetch_attachment};
use crate::state::{Overlay, use_app_state};

/// Pixels per line of a wheel event that scrolls by lines (Firefox with a mouse wheel).
const WHEEL_LINE_PX: f64 = 16.0;

/// The full-size bytes of the image on screen.
#[derive(Clone)]
enum FullImage {
  Loading,
  Ready(Rc<ObjectUrl>),
  Failed,
  /// The image's message was deleted (or the file list no longer has it).
  Gone,
}

/// The lightbox over the conversation, opened on `start`.
#[component]
pub fn Lightbox(conversation: ConversationId, start: ImageKey) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let files = Memo::new(move |_| state.read(|backend| backend.conversation_files(conversation)));
  // `None` until the user picks one: then the album decides, once the files are known.
  let chosen_scope = RwSignal::new(None::<LightboxScope>);
  let album_size = Memo::new(move |_| files.with(|files| album_len(files, start)));
  let scope = Memo::new(move |_| {
    chosen_scope
      .get()
      .unwrap_or_else(|| LightboxScope::for_album_of(album_size.get()))
  });
  let images = Memo::new(move |_| files.with(|files| images_in_scope(files, scope.get(), start)));
  let current = RwSignal::new(start);
  let current_file = Memo::new(move |_| {
    let key = current.get();
    images.with(|images| images.iter().find(|file| key.matches(file)).cloned())
  });
  let at = Memo::new(move |_| images.with(|images| position(images, current.get())));
  let full = RwSignal::new_local(FullImage::Loading);
  let thumbnail = RwSignal::new_local(None::<Rc<ObjectUrl>>);
  let viewport = RwSignal::new(Viewport::default());
  let retries = RwSignal::new(0_u32);
  let ticket = StoredValue::new(0_u64);
  let stage = NodeRef::<html::Div>::new();
  let image = NodeRef::<html::Img>::new();

  // The media type is known once the file list arrived; it changes only with the image.
  let media_type =
    Memo::new(move |_| current_file.with(|file| file.as_ref().map(|file| file.media_type.clone())));
  Effect::new(move |_| {
    let key = current.get();
    retries.track();
    viewport.set(Viewport::default());
    let next_ticket = ticket.get_value() + 1;
    ticket.set_value(next_ticket);
    let Some(media_type) = media_type.get() else {
      // An empty list is still loading; a list without the image lost it.
      let is_gone = !images.with_untracked(Vec::is_empty);
      thumbnail.set(None);
      full.set(if is_gone {
        FullImage::Gone
      } else {
        FullImage::Loading
      });
      return;
    };
    // Replacing the old values drops their URLs, which releases the previous image right away.
    thumbnail.set(current_file.with_untracked(|file| file.as_ref().and_then(thumbnail_url)));
    full.set(FullImage::Loading);
    spawn_local(async move {
      let result = fetch_attachment(conversation, key.message, key.attachment.to_string(), &media_type).await;
      if ticket.try_get_value() != Some(next_ticket) {
        // The user moved on or closed the lightbox; dropping the URL revokes it.
        return;
      }
      let shown = match result {
        Ok(url) => FullImage::Ready(Rc::new(url)),
        Err(error) => {
          tracing::warn!(%error, conversation_id = %conversation, "could not open an image");
          FullImage::Failed
        },
      };
      drop(full.try_set(shown));
    });
  });
  on_cleanup(move || {
    ticket.try_update_value(|ticket| *ticket += 1);
    drop(full.try_set(FullImage::Loading));
    drop(thumbnail.try_set(None));
  });

  let go = move |direction: Direction| {
    if let Some(key) = images.with_untracked(|images| neighbor(images, current.get_untracked(), direction)) {
      current.set(key);
    }
  };
  let zoom_from_center = move |factor: f64| {
    let (image_size, stage_size) = sizes(image, stage);
    viewport.update(|viewport| viewport.zoom_by(factor, Point::ORIGIN, image_size, stage_size));
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.ctrl_key() || ev.meta_key() || ev.alt_key() {
      return;
    }
    match ev.key().as_str() {
      "ArrowLeft" => go(Direction::Previous),
      "ArrowRight" => go(Direction::Next),
      "+" | "=" => zoom_from_center(KEY_ZOOM_STEP),
      "-" | "_" => zoom_from_center(1.0 / KEY_ZOOM_STEP),
      "0" => viewport.update(Viewport::reset),
      _ => return,
    }
    ev.prevent_default();
  };

  let sender = Memo::new(move |_| {
    current_file
      .get()
      .and_then(|file| file.maybe_sender)
      .and_then(|user| state.read(|backend| backend.user(user)))
      .map(|user| user.display_name)
      .unwrap_or_default()
  });
  let sent_at = move || {
    current_file
      .get()
      .and_then(|file| file.maybe_sent_at)
      .map(|ts| format_date_time(ts, i18n.locale().tag()))
      .unwrap_or_default()
  };
  let counter = move || {
    at.get()
      .map(|at| {
        let (index, len) = ((at.index + 1).to_string(), at.len.to_string());
        i18n.t_with("lightbox-position", &[("index", &index), ("count", &len)])
      })
      .unwrap_or_default()
  };
  let zoom_label = move || format!("{:.0} %", viewport.get().scale() * 100.0);

  view! {
    <Modal labelled_by="lightbox-title" class="modal-lightbox">
      <div class="lightbox" on:keydown=on_keydown>
        <header class="lightbox-bar">
          <div class="lightbox-meta">
            <h2 id="lightbox-title" class="lightbox-name" translate="no">
              {move || current_file.get().map(|file| file.file_name).unwrap_or_default()}
            </h2>
            <p class="lightbox-origin">
              <span translate="no">{move || sender.get()}</span>
              <time>{sent_at}</time>
            </p>
          </div>
          <p class="lightbox-count" aria-live="polite">{counter}</p>
          <div class="lightbox-actions" role="toolbar" aria-label=move || i18n.t("lightbox-toolbar")>
            <button
              class="icon-btn"
              aria-label=move || i18n.t("lightbox-zoom-out")
              disabled=move || !viewport.get().is_zoomed()
              on:click=move |_| zoom_from_center(1.0 / KEY_ZOOM_STEP)
            >
              <Icon name=IconName::ZoomOut />
            </button>
            <button
              class="btn btn-small lightbox-zoom"
              aria-label=move || i18n.t_with("lightbox-zoom-reset", &[("zoom", &zoom_label())])
              on:click=move |_| viewport.update(Viewport::reset)
            >
              {zoom_label}
            </button>
            <button
              class="icon-btn"
              aria-label=move || i18n.t("lightbox-zoom-in")
              disabled=move || !viewport.get().can_zoom_in()
              on:click=move |_| zoom_from_center(KEY_ZOOM_STEP)
            >
              <Icon name=IconName::ZoomIn />
            </button>
            <Show when=move || { album_size.get() > 1 } fallback=|| ()>
              <button
                class="btn btn-small"
                aria-pressed=move || (scope.get() == LightboxScope::Conversation).to_string()
                on:click=move |_| {
                  let next = match scope.get_untracked() {
                    LightboxScope::Album => LightboxScope::Conversation,
                    LightboxScope::Conversation => LightboxScope::Album,
                  };
                  chosen_scope.set(Some(next));
                  // Back in the album from another message's image: show the album's first one.
                  if images.with_untracked(|images| position(images, current.get_untracked()).is_none()) {
                    current.set(start);
                  }
                }
              >
                {move || i18n.t("lightbox-show-all")}
              </button>
            </Show>
            <SaveButton current=current current_file=current_file full=full />
            <button
              class="icon-btn"
              aria-label=move || i18n.t("lightbox-forward")
              on:click=move |_| state.overlay.set(Overlay::Forward(conversation, current.get_untracked().message))
            >
              <Icon name=IconName::Forward />
            </button>
            <button class="icon-btn" data-autofocus aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
              <Icon name=IconName::Close />
            </button>
          </div>
        </header>
        <div class="lightbox-body">
          <button
            class="icon-btn lightbox-nav lightbox-previous"
            aria-label=move || i18n.t("lightbox-previous")
            disabled=move || !at.get().is_some_and(|at| at.has_previous())
            on:click=move |_| go(Direction::Previous)
          >
            <Icon name=IconName::ChevronLeft />
          </button>
          <Stage stage=stage image=image viewport=viewport full=full thumbnail=thumbnail current_file=current_file go=go />
          <button
            class="icon-btn lightbox-nav lightbox-next"
            aria-label=move || i18n.t("lightbox-next")
            disabled=move || !at.get().is_some_and(|at| at.has_next())
            on:click=move |_| go(Direction::Next)
          >
            <Icon name=IconName::ChevronRight />
          </button>
        </div>
        {move || match full.get() {
          FullImage::Loading => view! {
            <div class="lightbox-status" role="status">
              <progress class="lightbox-progress" aria-label=move || i18n.t("lightbox-loading")></progress>
              <span>{move || i18n.t("lightbox-loading")}</span>
            </div>
          }
          .into_any(),
          FullImage::Failed => view! {
            <div class="lightbox-status lightbox-error" role="alert">
              <span>{move || i18n.t("lightbox-failed")}</span>
              <button class="btn btn-small" on:click=move |_| retries.update(|count| *count += 1)>
                {move || i18n.t("file-retry")}
              </button>
            </div>
          }
          .into_any(),
          FullImage::Gone => view! {
            <div class="lightbox-status" role="status">
              <span>{move || i18n.t("lightbox-gone")}</span>
            </div>
          }
          .into_any(),
          FullImage::Ready(_) => ().into_any(),
        }}
      </div>
    </Modal>
  }
}

/// Where the image is shown and the pointer and wheel gestures happen. A tap beside the image closes
/// the lightbox, like a click outside a dialog.
#[component]
fn Stage(
  stage: NodeRef<html::Div>,
  image: NodeRef<html::Img>,
  viewport: RwSignal<Viewport>,
  full: RwSignal<FullImage, LocalStorage>,
  thumbnail: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
  current_file: Memo<Option<FileInfo>>,
  go: impl Fn(Direction) + Copy + 'static,
) -> impl IntoView {
  let state = use_app_state();
  let pointers = StoredValue::new(Pointers::default());
  // A double-tap is recognised from pointer events; the browser's own dblclick is for mice only.
  let is_mouse = StoredValue::new(true);
  // Captured pointers report the stage as their target, so where a press began is noted on the way
  // down.
  let is_press_beside_image = StoredValue::new(false);
  let anchor_of = move |client_x: f64, client_y: f64| {
    stage
      .get_untracked()
      .map(|stage| {
        let rect = stage.get_bounding_client_rect();
        Point::new(
          client_x - rect.left() - rect.width() / 2.0,
          client_y - rect.top() - rect.height() / 2.0,
        )
      })
      .unwrap_or(Point::ORIGIN)
  };
  let zoom_at = move |factor: f64, anchor: Point| {
    let (image_size, stage_size) = sizes(image, stage);
    viewport.update(|viewport| viewport.zoom_by(factor, anchor, image_size, stage_size));
  };
  let toggle_at = move |anchor: Point| {
    let (image_size, stage_size) = sizes(image, stage);
    viewport.update(|viewport| viewport.toggle_zoom(anchor, image_size, stage_size));
  };
  let on_wheel = move |ev: web_sys::WheelEvent| {
    ev.prevent_default();
    let delta = match ev.delta_mode() {
      web_sys::WheelEvent::DOM_DELTA_LINE => ev.delta_y() * WHEEL_LINE_PX,
      web_sys::WheelEvent::DOM_DELTA_PAGE => ev.delta_y() * sizes(image, stage).1.height,
      _ => ev.delta_y(),
    };
    // Trackpad pinches arrive as wheel events with the control key set.
    zoom_at(
      wheel_zoom_factor(delta, ev.ctrl_key()),
      anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y())),
    );
  };
  listen_to_wheel(stage, on_wheel);
  let on_pointerdown = move |ev: web_sys::PointerEvent| {
    if ev.button() > 0 {
      return;
    }
    is_mouse.set_value(ev.pointer_type() == "mouse");
    is_press_beside_image.set_value(ev.target() == ev.current_target());
    if let Some(stage) = stage.get_untracked() {
      // Without capture a drag that leaves the stage would never end.
      drop(stage.set_pointer_capture(ev.pointer_id()));
    }
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    pointers.update_value(|pointers| pointers.down(ev.pointer_id(), at));
  };
  let on_pointermove = move |ev: web_sys::PointerEvent| {
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let mut gesture = Gesture::None;
    pointers.update_value(|pointers| gesture = pointers.moved(ev.pointer_id(), at));
    let (image_size, stage_size) = sizes(image, stage);
    match gesture {
      Gesture::None => {},
      Gesture::Pan(delta) => viewport.update(|viewport| viewport.pan_by(delta, image_size, stage_size)),
      Gesture::Pinch { factor, center } => {
        viewport.update(|viewport| viewport.zoom_by(factor, center, image_size, stage_size));
      },
    }
  };
  let on_pointerup = move |ev: web_sys::PointerEvent| {
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let is_zoomed = viewport.get_untracked().is_zoomed();
    let mut release = Release::None;
    pointers.update_value(|pointers| {
      release = pointers.up(ev.pointer_id(), at, ev.time_stamp(), is_zoomed);
    });
    match release {
      Release::Tap(_) if is_press_beside_image.get_value() => state.close_overlay(),
      Release::DoubleTap(anchor) if !is_mouse.get_value() => toggle_at(anchor),
      // A mouse drag on a fitted image is no swipe; it selects nothing and pages nowhere.
      Release::Swipe(direction) if !is_mouse.get_value() => go(direction),
      _ => {},
    }
  };
  let on_dblclick = move |ev: web_sys::MouseEvent| {
    if is_mouse.get_value() {
      toggle_at(anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y())));
    }
  };
  let transform = move || {
    let viewport = viewport.get();
    let offset = viewport.offset();
    format!(
      "translate({:.1}px, {:.1}px) scale({:.4})",
      offset.x,
      offset.y,
      viewport.scale()
    )
  };
  let name = move || current_file.get().map(|file| file.file_name).unwrap_or_default();

  view! {
    <div
      class="lightbox-stage"
      class:zoomed=move || viewport.get().is_zoomed()
      node_ref=stage
      on:pointerdown=on_pointerdown
      on:pointermove=on_pointermove
      on:pointerup=on_pointerup
      on:pointercancel=move |ev: web_sys::PointerEvent| pointers.update_value(|pointers| pointers.cancel(ev.pointer_id()))
      on:dblclick=on_dblclick
    >
      {move || match full.get() {
        FullImage::Ready(url) => view! {
          <img
            class="lightbox-image"
            src=url.as_str().to_owned()
            alt=name
            decoding="async"
            draggable="false"
            node_ref=image
            style:transform=transform
          />
        }
        .into_any(),
        _ => thumbnail
          .get()
          .map(|url| {
            view! { <img class="lightbox-image lightbox-placeholder" src=url.as_str().to_owned() alt="" draggable="false" /> }
          })
          .into_any(),
      }}
    </div>
  }
}

/// Wheel events on the stage, with a listener that may cancel them: a delegated or passive one
/// could not stop ctrl+wheel (a trackpad pinch) from zooming the whole page instead of the image.
fn listen_to_wheel(stage: NodeRef<html::Div>, on_wheel: impl FnMut(web_sys::WheelEvent) + 'static) {
  let listener = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(on_wheel);
  let handle = StoredValue::new_local(listener);
  Effect::new(move |_| {
    let Some(element) = stage.get() else { return };
    let options = web_sys::AddEventListenerOptions::new();
    options.set_passive(false);
    handle.with_value(|listener| {
      let added = element.add_event_listener_with_callback_and_add_event_listener_options(
        "wheel",
        listener.as_ref().unchecked_ref(),
        &options,
      );
      if added.is_err() {
        tracing::debug!("could not listen to the wheel");
      }
    });
  });
  on_cleanup(move || {
    let Some(element) = stage.get_untracked() else {
      return;
    };
    handle.try_with_value(|listener| {
      // Removing a listener that was never added is a no-op; nothing to report.
      drop(element.remove_event_listener_with_callback("wheel", listener.as_ref().unchecked_ref()));
    });
  });
}

/// Save: in the browser the decrypted image already in memory is offered as a download; the desktop
/// app has the core write it into the download folder and reports where it stands.
#[component]
fn SaveButton(
  current: RwSignal<ImageKey>,
  current_file: Memo<Option<FileInfo>>,
  full: RwSignal<FullImage, LocalStorage>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_desktop = crate::auth::tauri::is_available();
  let save = move |_| {
    let Some(file) = current_file.get_untracked() else {
      return;
    };
    if is_desktop {
      let key = current.get_untracked();
      state.write(|backend| backend.download_attachment(file.conversation, key.message, &file.file_id));
      return;
    }
    if let FullImage::Ready(url) = full.get_untracked() {
      offer_download(url.as_str(), &file.file_name);
    }
  };
  let progress = move || {
    let key = match current_file.get().map(|file| file.transfer) {
      Some(FileTransfer::Downloading { .. }) => "lightbox-saving",
      Some(FileTransfer::Local { .. }) => "file-saved",
      Some(FileTransfer::Failed { .. }) => "file-failed",
      _ => return None,
    };
    is_desktop.then(|| i18n.t(key))
  };
  view! {
    <button
      class="icon-btn"
      aria-label=move || i18n.t("lightbox-save")
      disabled=move || !is_desktop && !matches!(full.get(), FullImage::Ready(_))
      on:click=save
    >
      <Icon name=IconName::Download />
    </button>
    <span class="lightbox-save-state" role="status">{progress}</span>
  }
}

/// A thumbnail as a placeholder while the full image loads.
fn thumbnail_url(file: &FileInfo) -> Option<Rc<ObjectUrl>> {
  let png = file.maybe_thumbnail_png.as_deref()?;
  ObjectUrl::from_bytes(png, "image/png").map(Rc::new)
}

/// The image's laid-out size (before zoom) and the stage's size.
fn sizes(image: NodeRef<html::Img>, stage: NodeRef<html::Div>) -> (Size, Size) {
  let image = image
    .get_untracked()
    .map(|image| Size::new(f64::from(image.offset_width()), f64::from(image.offset_height())))
    .unwrap_or_default();
  let stage = stage
    .get_untracked()
    .map(|stage| Size::new(f64::from(stage.client_width()), f64::from(stage.client_height())))
    .unwrap_or_default();
  (image, stage)
}

/// Saves a `blob:` URL under the file's name through a temporary download link.
fn offer_download(url: &str, file_name: &str) {
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let Some(link) = document
    .create_element("a")
    .ok()
    .and_then(|element| element.dyn_into::<web_sys::HtmlAnchorElement>().ok())
  else {
    return;
  };
  link.set_href(url);
  link.set_download(file_name);
  link.click();
}
