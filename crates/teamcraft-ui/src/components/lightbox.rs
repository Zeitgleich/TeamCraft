//! The media lightbox: one image, animated image or video at full resolution over the app.
//!
//! - Top left: who sent it, when, in which conversation, and the file name.
//! - Bottom left: rotate and zoom (−, slider, +, fit); pictures also zoom and pan with the wheel,
//!   pinches, double-click or double-tap. An animated image can be paused.
//! - Bottom right: download, the thread panel, open in a new window, and a "…" menu (view thread,
//!   jump to message, saved, links, copy image, forward, file details).
//! - Sides: previous/next through the album of a message or the conversation; a thumbnail strip
//!   for quick navigation.
//!
//! Every control has a tooltip naming its key and is reachable by keyboard
//! ([`crate::lightbox::Command`]).
//!
//! It opens at once with the thumbnail scaled up and a small progress indicator, and swaps in the
//! full file when it is there. Memory stays bounded: files are opened only up to
//! [`MAX_VIEWER_BYTES`], and their decrypted bytes live in `blob:` URLs in the session's cache
//! ([`crate::media_cache`], bounded by a byte budget), so reopening a file is instant. Nothing
//! decrypted ever goes to the server: "open in a new window" hands the same in-memory URL to a
//! blank window of this page.

use std::cell::Cell;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{FileInfo, MAX_VIEWER_BYTES};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::lightbox_controls::{
  Controls, MediaActions, Notice, ThumbnailStrip, still_frame, tooltip,
};
use crate::components::modal::Modal;
use crate::components::thread_panel::ThreadPanel;
use crate::i18n::use_i18n;
use crate::lightbox::{
  Command, Direction, Gesture, LightboxScope, MediaKey, MediaKind, ModifierStyle, Point, Pointers, Release,
  Size, Viewport, album_len, images_in_scope, neighbor, position, wheel_zoom_factor,
};
use crate::media_cache::{self, Rendition};
use crate::model::{ConversationId, ConversationKind, thread_id_of};
use crate::object_url::ObjectUrl;
use crate::state::use_app_state;

/// Pixels per line of a wheel event that scrolls by lines (Firefox with a mouse wheel).
const WHEEL_LINE_PX: f64 = 16.0;

/// Most messages (with attachments) the lightbox loads around the opened one. The list is read once
/// when the lightbox opens: rebuilding the thumbnails of a whole conversation on every state change
/// would stall a big channel. Older images beyond the window are not paged through (tracked as a
/// v1.1 issue).
const MAX_WINDOW_MESSAGES: usize = 100;

/// Below this window width the thread panel starts hidden: the picture needs the room.
const THREAD_PANEL_MIN_WIDTH_PX: f64 = 1200.0;

thread_local! {
  /// Whether the user last showed or hid the thread panel, for the next lightbox of this session.
  static IS_THREAD_SHOWN: Cell<Option<bool>> = const { Cell::new(None) };
}

/// The full-size bytes of the file on screen.
#[derive(Clone)]
pub enum FullMedia {
  Loading,
  Ready(Rc<ObjectUrl>),
  Failed,
  /// Bigger than the in-memory viewer opens; it can still be downloaded.
  TooLarge,
  /// The file's message was deleted (or the file list no longer has it).
  Gone,
}

/// The lightbox over the conversation, opened on `start`.
#[component]
pub fn Lightbox(conversation: ConversationId, start: MediaKey) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // Read once; a memo without dependencies never recomputes.
  let files = Memo::new(move |_| {
    untrack(|| {
      state.read(|backend| backend.conversation_images(conversation, start.message, MAX_WINDOW_MESSAGES))
    })
  });
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
  let kind = Memo::new(move |_| {
    current_file.with(|file| file.as_ref().and_then(|file| MediaKind::of(&file.media_type)))
  });
  let full = RwSignal::new_local(FullMedia::Loading);
  let thumbnail = RwSignal::new_local(None::<Rc<ObjectUrl>>);
  let viewport = RwSignal::new(Viewport::default());
  let is_paused = RwSignal::new(false);
  let notice = Notice::default();
  let retries = RwSignal::new(0_u32);
  let ticket = StoredValue::new(0_u64);
  let stage = NodeRef::<html::Div>::new();
  let image = NodeRef::<html::Img>::new();
  let video = NodeRef::<html::Video>::new();
  let is_thread_shown = RwSignal::new(IS_THREAD_SHOWN.get().unwrap_or_else(is_wide_window));
  let modifier = modifier_style();

  load_current(LoadInputs {
    conversation,
    current,
    current_file,
    images,
    kind,
    full,
    thumbnail,
    viewport,
    is_paused,
    retries,
    ticket,
  });
  notice.clear_on_change(current);

  let go = move |direction: Direction| {
    if let Some(key) = images.with_untracked(|images| neighbor(images, current.get_untracked(), direction)) {
      current.set(key);
    }
  };
  let toggle_thread = move || {
    let shown = !is_thread_shown.get_untracked();
    is_thread_shown.set(shown);
    IS_THREAD_SHOWN.set(Some(shown));
  };
  let actions = MediaActions {
    state,
    i18n,
    conversation,
    current,
    current_file,
    kind,
    full,
    image,
    video,
    stage,
    viewport,
    is_paused,
    is_thread_shown,
    notice,
  };
  let run = move |command: Command| match command {
    Command::Previous => go(Direction::Previous),
    Command::Next => go(Direction::Next),
    Command::ToggleThread => toggle_thread(),
    other => actions.run(other),
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if is_typing_target(&ev) {
      return;
    }
    let Some(command) = Command::for_key(&ev.key(), ev.ctrl_key() || ev.meta_key(), ev.alt_key()) else {
      return;
    };
    // Ctrl/⌘+C with text selected (the file name, the details) copies the text.
    if command == Command::CopyImage && has_text_selection() {
      return;
    }
    ev.prevent_default();
    ev.stop_propagation();
    run(command);
  };

  let sender = Memo::new(move |_| {
    current_file
      .get()
      .and_then(|file| file.maybe_sender)
      .and_then(|user| state.read(|backend| backend.user(user)))
  });
  let channel = Memo::new(move |_| {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|conv| match conv.kind {
        ConversationKind::Direct | ConversationKind::Group => conv.name,
        ConversationKind::Public | ConversationKind::Private | ConversationKind::Integration => {
          format!("# {}", conv.name)
        },
      })
      .unwrap_or_default()
  });
  let sent_at = move || current_file.get().and_then(|file| file.maybe_sent_at);
  // A clock that runs behind must not make a fresh image "in 3 minutes".
  let relative = move || {
    sent_at()
      .map(|ts| {
        let now = now_ms();
        format_relative(ts.min(now), now, i18n.locale().tag())
      })
      .unwrap_or_default()
  };
  let absolute = move || {
    sent_at()
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
  let backdrop = move || {
    thumbnail
      .get()
      .map(|url| format!("url(\"{}\")", url.as_str()))
      .unwrap_or_default()
  };
  let thread = Memo::new(move |_| thread_id_of(current.get().message));
  let hide_thread = Callback::new(move |()| toggle_thread());
  let previous_tip = move || tooltip(i18n, "lightbox-previous", Command::Previous, modifier);
  let next_tip = move || tooltip(i18n, "lightbox-next", Command::Next, modifier);

  view! {
    <Modal labelled_by="lightbox-title" class="modal-lightbox">
      // tabindex: a click on the picture focuses the lightbox itself, not the dialog around it, so
      // the keys keep working.
      <div class="lightbox" tabindex="-1" class:with-thread=move || is_thread_shown.get() on:keydown=on_keydown>
        <div class="lightbox-backdrop" aria-hidden="true" style:background-image=backdrop></div>
        <div class="lightbox-main">
          <header class="lightbox-top">
            <div class="lightbox-origin">
              {move || sender.get().map(|user| view! { <Avatar user=user size=AvatarSize::Medium /> })}
              <div class="lightbox-origin-text">
                <span class="lightbox-sender" translate="no">
                  {move || sender.get().map(|user| user.display_name).unwrap_or_default()}
                </span>
                <span class="lightbox-context">
                  <time title=absolute>{relative}</time>
                  <span class="lightbox-dot" aria-hidden="true">"·"</span>
                  <span translate="no">{move || channel.get()}</span>
                  <span class="lightbox-dot" aria-hidden="true">"·"</span>
                  <h2 id="lightbox-title" class="lightbox-name" translate="no">
                    {move || current_file.get().map(|file| file.file_name).unwrap_or_default()}
                  </h2>
                </span>
              </div>
            </div>
            <div class="lightbox-top-actions">
              <p class="lightbox-count" aria-live="polite">{counter}</p>
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
              <button
                class="icon-btn"
                data-autofocus
                aria-label=move || i18n.t("close")
                data-tip=move || i18n.t("lightbox-close-tip")
                aria-keyshortcuts="Escape"
                on:click=move |_| state.close_overlay()
              >
                <Icon name=IconName::Close />
              </button>
            </div>
          </header>
          <div class="lightbox-body">
            <Stage
              stage=stage
              image=image
              video=video
              viewport=viewport
              full=full
              thumbnail=thumbnail
              current_file=current_file
              kind=kind
              is_paused=is_paused
              go=go
            />
            <button
              class="lightbox-nav lightbox-previous"
              aria-label=previous_tip
              data-tip=previous_tip
              aria-keyshortcuts="ArrowLeft"
              disabled=move || !at.get().is_some_and(|at| at.has_previous())
              on:click=move |_| go(Direction::Previous)
            >
              <Icon name=IconName::ChevronLeft />
            </button>
            <button
              class="lightbox-nav lightbox-next"
              aria-label=next_tip
              data-tip=next_tip
              aria-keyshortcuts="ArrowRight"
              disabled=move || !at.get().is_some_and(|at| at.has_next())
              on:click=move |_| go(Direction::Next)
            >
              <Icon name=IconName::ChevronRight />
            </button>
            <Status full=full retries=retries actions=actions />
          </div>
          <ThumbnailStrip images=images current=current />
          <Controls actions=actions modifier=modifier on_toggle_thread=hide_thread />
        </div>
        <Show when=move || is_thread_shown.get() fallback=|| ()>
          <div class="lightbox-thread">
            {move || {
              let thread = thread.get();
              view! { <ThreadPanel conversation=conversation thread=thread embedded=true maybe_on_close=hide_thread /> }
            }}
          </div>
        </Show>
      </div>
    </Modal>
  }
}

/// What [`load_current`] reads and writes.
struct LoadInputs {
  conversation: ConversationId,
  current: RwSignal<MediaKey>,
  current_file: Memo<Option<FileInfo>>,
  images: Memo<Vec<FileInfo>>,
  kind: Memo<Option<MediaKind>>,
  full: RwSignal<FullMedia, LocalStorage>,
  thumbnail: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
  viewport: RwSignal<Viewport>,
  is_paused: RwSignal<bool>,
  retries: RwSignal<u32>,
  ticket: StoredValue<u64>,
}

/// Fetches the file on screen whenever it changes (or the user retries), and releases the previous
/// one at once.
fn load_current(inputs: LoadInputs) {
  let LoadInputs {
    conversation,
    current,
    current_file,
    images,
    kind,
    full,
    thumbnail,
    viewport,
    is_paused,
    retries,
    ticket,
  } = inputs;
  // The media type is known once the file list arrived; it changes only with the file.
  let media_type =
    Memo::new(move |_| current_file.with(|file| file.as_ref().map(|file| file.media_type.clone())));
  Effect::new(move |_| {
    let key = current.get();
    retries.track();
    viewport.set(Viewport::default());
    let next_ticket = ticket.get_value() + 1;
    ticket.set_value(next_ticket);
    let Some(media_type) = media_type.get() else {
      // An empty list is still loading; a list without the file lost it.
      let is_gone = !images.with_untracked(Vec::is_empty);
      thumbnail.set(None);
      full.set(if is_gone {
        FullMedia::Gone
      } else {
        FullMedia::Loading
      });
      return;
    };
    is_paused.set(
      kind
        .get_untracked()
        .is_some_and(|kind| kind.starts_paused(prefers_reduced_motion())),
    );
    // Replacing the old values drops their URLs, which releases the previous file right away.
    // The sharper inline rendition when the conversation already loaded it, else the thumbnail.
    thumbnail.set(
      media_cache::cached(key, Rendition::Inline)
        .or_else(|| current_file.with_untracked(|file| file.as_ref().and_then(thumbnail_url))),
    );
    let size_bytes = current_file.with_untracked(|file| file.as_ref().map_or(0, |file| file.size_bytes));
    if size_bytes > MAX_VIEWER_BYTES {
      full.set(FullMedia::TooLarge);
      return;
    }
    // Seen before in this session (inline or in the lightbox): shown at once.
    if let Some(url) = media_cache::cached(key, Rendition::Full) {
      full.set(FullMedia::Ready(url));
      return;
    }
    full.set(FullMedia::Loading);
    spawn_local(async move {
      let result = media_cache::load(conversation, key, &media_type, Rendition::Full, || true).await;
      if ticket.try_get_value() != Some(next_ticket) {
        // The user moved on or closed the lightbox; the cache keeps the file for next time.
        return;
      }
      let shown = match result {
        Ok(Some(url)) => FullMedia::Ready(url),
        Ok(None) => FullMedia::Failed,
        Err(error) => {
          tracing::warn!(%error, conversation_id = %conversation, "could not open a file");
          FullMedia::Failed
        },
      };
      drop(full.try_set(shown));
    });
  });
  on_cleanup(move || {
    ticket.try_update_value(|ticket| *ticket += 1);
    drop(full.try_set(FullMedia::Loading));
    drop(thumbnail.try_set(None));
  });
}

/// Where the file is shown and the pointer and wheel gestures happen. A tap beside the picture
/// closes the lightbox, like a click outside a dialog. Videos keep their own controls: the stage
/// leaves their pointer events alone.
#[component]
fn Stage(
  stage: NodeRef<html::Div>,
  image: NodeRef<html::Img>,
  video: NodeRef<html::Video>,
  viewport: RwSignal<Viewport>,
  full: RwSignal<FullMedia, LocalStorage>,
  thumbnail: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
  current_file: Memo<Option<FileInfo>>,
  kind: Memo<Option<MediaKind>>,
  is_paused: RwSignal<bool>,
  go: impl Fn(Direction) + Copy + 'static,
) -> impl IntoView {
  let state = use_app_state();
  let pointers = StoredValue::new(Pointers::default());
  // A double-tap is recognised from pointer events; the browser's own dblclick is for mice only.
  let is_mouse = StoredValue::new(true);
  // Captured pointers report the stage as their target, so where a press began is noted on the way
  // down.
  let is_press_beside_image = StoredValue::new(false);
  // Bumped when the layout changes (the image loaded, the window resized), so a turned image that
  // needs shrinking to fit is measured again.
  let layout = RwSignal::new(0_u32);
  let resize = window_event_listener(leptos::ev::resize, move |_| layout.update(|tick| *tick += 1));
  on_cleanup(move || resize.remove());
  let can_transform = move || kind.get_untracked().is_some_and(MediaKind::can_transform);
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
    if !can_transform() {
      return;
    }
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
    let is_on_stage = ev.target() == ev.current_target();
    // A video's own controls get their presses; capturing them would break seeking.
    if ev.button() > 0 || (!can_transform() && !is_on_stage) {
      return;
    }
    is_mouse.set_value(ev.pointer_type() == "mouse");
    is_press_beside_image.set_value(is_on_stage);
    if let Some(stage) = stage.get_untracked() {
      // Without capture a drag that leaves the stage would never end.
      drop(stage.set_pointer_capture(ev.pointer_id()));
    }
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    pointers.update_value(|pointers| pointers.down(ev.pointer_id(), at));
  };
  let on_pointermove = move |ev: web_sys::PointerEvent| {
    if !can_transform() {
      return;
    }
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
      Release::DoubleTap(anchor) if !is_mouse.get_value() && can_transform() => toggle_at(anchor),
      // A mouse drag on a fitted image is no swipe; it selects nothing and pages nowhere.
      Release::Swipe(direction) if !is_mouse.get_value() => go(direction),
      _ => {},
    }
  };
  let on_dblclick = move |ev: web_sys::MouseEvent| {
    if is_mouse.get_value() && can_transform() {
      toggle_at(anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y())));
    }
  };
  let transform = move || {
    layout.track();
    let viewport = viewport.get();
    let offset = viewport.offset();
    let (image_size, stage_size) = sizes(image, stage);
    format!(
      "translate({:.1}px, {:.1}px) rotate({}deg) scale({:.4})",
      offset.x,
      offset.y,
      viewport.rotation().degrees(),
      viewport.display_scale(image_size, stage_size),
    )
  };
  let name = move || current_file.get().map(|file| file.file_name).unwrap_or_default();
  let still = still_frame_of(full, kind, is_paused);

  view! {
    <div
      class="lightbox-stage"
      class:zoomed=move || viewport.get().is_zoomed()
      class:is-video=move || kind.get() == Some(MediaKind::Video)
      node_ref=stage
      on:pointerdown=on_pointerdown
      on:pointermove=on_pointermove
      on:pointerup=on_pointerup
      on:pointercancel=move |ev: web_sys::PointerEvent| pointers.update_value(|pointers| pointers.cancel(ev.pointer_id()))
      on:dblclick=on_dblclick
    >
      {move || match (full.get(), kind.get()) {
        (FullMedia::Ready(url), Some(MediaKind::Video)) => view! {
          <video
            class="lightbox-video"
            src=url.as_str().to_owned()
            controls
            playsinline
            preload="metadata"
            aria-label=name
            node_ref=video
            on:error=move |_| full.set(FullMedia::Failed)
          ></video>
        }
        .into_any(),
        (FullMedia::Ready(url), _) => {
          // Paused: the still frame once it is made, the thumbnail until then.
          let file_src = url.as_str().to_owned();
          let src = move || {
            if !is_paused.get() {
              return file_src.clone();
            }
            still
              .get()
              .or_else(|| thumbnail.get())
              .map(|frame| frame.as_str().to_owned())
              .unwrap_or_default()
          };
          view! {
            <img
              class="lightbox-image"
              src=src
              alt=name
              decoding="async"
              draggable="false"
              node_ref=image
              style:transform=transform
              on:load=move |_| layout.update(|tick| *tick += 1)
              on:error=move |_| {
                // The still frame is made from the file; only the file itself failing counts.
                if !is_paused.get_untracked() {
                  full.set(FullMedia::Failed);
                }
              }
            />
          }
          .into_any()
        },
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

/// The still frame of a paused animated image, made from the loaded file (the first frame).
fn still_frame_of(
  full: RwSignal<FullMedia, LocalStorage>,
  kind: Memo<Option<MediaKind>>,
  is_paused: RwSignal<bool>,
) -> RwSignal<Option<Rc<ObjectUrl>>, LocalStorage> {
  let still = RwSignal::new_local(None::<Rc<ObjectUrl>>);
  Effect::new(move |_| {
    let maybe_url = match (full.get(), is_paused.get(), kind.get()) {
      (FullMedia::Ready(url), true, Some(MediaKind::Animated)) => Some(url),
      _ => None,
    };
    still.set(None);
    let Some(url) = maybe_url else { return };
    spawn_local(async move {
      let frame = still_frame(url.as_str()).await;
      // Only if the same file is still shown and still paused.
      let is_current =
        matches!(full.try_get_untracked(), Some(FullMedia::Ready(shown)) if Rc::ptr_eq(&shown, &url));
      if is_current && is_paused.try_get_untracked() == Some(true) {
        drop(still.try_set(frame.map(Rc::new)));
      }
    });
  });
  still
}

/// Loading, failure and "too large" over the stage.
#[component]
fn Status(
  full: RwSignal<FullMedia, LocalStorage>,
  retries: RwSignal<u32>,
  actions: MediaActions,
) -> impl IntoView {
  let i18n = use_i18n();
  move || match full.get() {
    // The thumbnail is already on screen: only a small spinner says the full file is coming.
    FullMedia::Loading => view! {
      <div class="lightbox-loading" role="status" aria-label=move || i18n.t("lightbox-loading")>
        <span class="spinner" aria-hidden="true"></span>
      </div>
    }
    .into_any(),
    FullMedia::Failed => view! {
      <div class="lightbox-status lightbox-error" role="alert">
        <span>{move || i18n.t("lightbox-failed")}</span>
        <button class="btn btn-small" on:click=move |_| retries.update(|count| *count += 1)>
          {move || i18n.t("file-retry")}
        </button>
      </div>
    }
    .into_any(),
    FullMedia::TooLarge => view! {
      <div class="lightbox-status" role="status">
        <span>{move || i18n.t("lightbox-too-large")}</span>
        <button class="btn btn-small" on:click=move |_| actions.run(Command::Download)>
          {move || i18n.t("lightbox-save")}
        </button>
      </div>
    }
    .into_any(),
    FullMedia::Gone => view! {
      <div class="lightbox-status" role="status">
        <span>{move || i18n.t("lightbox-gone")}</span>
      </div>
    }
    .into_any(),
    FullMedia::Ready(_) => ().into_any(),
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

/// A thumbnail as a placeholder while the full file loads, and for the tinted backdrop.
fn thumbnail_url(file: &FileInfo) -> Option<Rc<ObjectUrl>> {
  let png = file.maybe_thumbnail_png.as_deref()?;
  ObjectUrl::from_thumbnail(png).map(Rc::new)
}

/// The image's laid-out size (before zoom and rotation) and the stage's size.
pub fn sizes(image: NodeRef<html::Img>, stage: NodeRef<html::Div>) -> (Size, Size) {
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

/// Keys typed into the thread's composer, a text field or the zoom slider are theirs, the "…" menu
/// handles its own, and a focused video seeks with the arrow keys.
fn is_typing_target(ev: &web_sys::KeyboardEvent) -> bool {
  let Some(element) = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
  else {
    return false;
  };
  let tag = element.tag_name().to_ascii_lowercase();
  matches!(tag.as_str(), "input" | "textarea" | "select" | "video")
    || element
      .closest("[contenteditable], .lightbox-thread, [role=menu]")
      .ok()
      .flatten()
      .is_some()
}

fn has_text_selection() -> bool {
  web_sys::window()
    .and_then(|window| window.get_selection().ok().flatten())
    .is_some_and(|selection| !selection.is_collapsed())
}

pub(crate) fn prefers_reduced_motion() -> bool {
  web_sys::window()
    .and_then(|window| {
      window
        .match_media("(prefers-reduced-motion: reduce)")
        .ok()
        .flatten()
    })
    .is_some_and(|query| query.matches())
}

fn is_wide_window() -> bool {
  web_sys::window()
    .and_then(|window| window.inner_width().ok())
    .and_then(|width| width.as_f64())
    .is_some_and(|width| width >= THREAD_PANEL_MIN_WIDTH_PX)
}

fn modifier_style() -> ModifierStyle {
  let platform = web_sys::window()
    .and_then(|window| window.navigator().platform().ok())
    .unwrap_or_default();
  ModifierStyle::for_platform(&platform)
}
