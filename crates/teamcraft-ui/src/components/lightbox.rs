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
//! Pictures follow the finger ([`crate::components::lightbox_motion`]): a sideways drag (or a
//! trackpad's two-finger swipe) pages, a drag up or down closes, and a zoomed picture pans and
//! pinches with soft edges. The picture grows out of the tile it was opened from and closes back
//! into it.
//!
//! It opens at once with the thumbnail scaled up and a small progress indicator, and cross-fades to
//! the full file once that is decoded ([`Arrival`]). Memory stays bounded: files are opened only up to
//! [`MAX_VIEWER_BYTES`], and their decrypted bytes live in `blob:` URLs in the session's cache
//! ([`crate::media_cache`], bounded by a byte budget), so reopening a file is instant. Nothing
//! decrypted ever goes to the server: "open in a new window" hands the same in-memory URL to a
//! blank window of this page.

use std::cell::Cell;
use std::rc::Rc;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{FileInfo, MAX_VIEWER_BYTES, MediaAnchor};
// How many messages a page has and how close to an end the next one is read (#274).
use teamcraft_client_api::media_viewer::{PAGE_MARGIN, PAGE_MESSAGES};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::lightbox_controls::{
  Controls, MediaActions, Notice, ThumbnailStrip, still_frame, tooltip,
};
use crate::components::lightbox_motion::{MotionHooks, MotionNodes, StageMotion};
use crate::components::message_row::shown_author;
use crate::components::modal::Modal;
use crate::components::pending_button::PendingButton;
use crate::components::thread_panel::ThreadPanel;
use crate::components::video_player::VideoPlayer;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::lightbox::{
  Arrival, Command, Direction, Gesture, LightboxScope, MediaKey, MediaKind, MediaPages, ModifierStyle, Point,
  Pointers, Release, Size, Viewport, album_len, images_in_scope, neighbor, position, wheel_zoom_factor,
};
use crate::media_cache::{self, Rendition};
use crate::model::{ConversationId, ConversationKind, thread_id_of};
use crate::object_url::ObjectUrl;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// Pixels per line of a wheel event that scrolls by lines (Firefox with a mouse wheel).
const WHEEL_LINE_PX: f64 = 16.0;

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
  /// A video this webview can't play (#804): it can still be saved or opened in the system's player.
  Unplayable,
  /// Bigger than the in-memory viewer opens; it can still be downloaded.
  TooLarge,
  /// The file's message was deleted (or the file list no longer has it).
  Gone,
}

/// The current file's thumbnail (or inline rendition), shown until its full picture is there.
#[derive(Clone)]
struct Placeholder {
  key: MediaKey,
  url: Rc<ObjectUrl>,
}

/// The lightbox over the conversation, opened on `start`.
#[component]
pub fn Lightbox(conversation: ConversationId, start: MediaKey) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The first page around the opened file. A backend that asks the core has no answer the first
  // time (#637): `None` until the core answered, then read again on the change notification. Read
  // once it is there: rebuilding the list on every state change would stall a big channel.
  let read_first = move || {
    state.read(|backend| {
      backend.conversation_media(conversation, MediaAnchor::Around(start.message), PAGE_MESSAGES)
    })
  };
  let pages = RwSignal::new(untrack(read_first).map(MediaPages::first));
  Effect::new(move |_| {
    if pages.with_untracked(Option::is_some) {
      // Read: nothing tracked, so this doesn't run again.
      return;
    }
    if let Some(first) = read_first() {
      pages.set(Some(MediaPages::first(first)));
    }
  });
  let is_listed = Memo::new(move |_| pages.with(Option::is_some));
  // `None` until the user picks one: then the album decides, once the files are known.
  let chosen_scope = RwSignal::new(None::<LightboxScope>);
  let album_size =
    Memo::new(move |_| pages.with(|pages| pages.as_ref().map_or(0, |pages| album_len(pages.files(), start))));
  let scope = Memo::new(move |_| {
    chosen_scope
      .get()
      .unwrap_or_else(|| LightboxScope::for_album_of(album_size.get()))
  });
  let images = Memo::new(move |_| {
    let scope = scope.get();
    pages.with(|pages| {
      pages
        .as_ref()
        .map(|pages| images_in_scope(pages.files(), scope, start))
        .unwrap_or_default()
    })
  });
  let current = RwSignal::new(start);
  let current_file = Memo::new(move |_| {
    let key = current.get();
    images.with(|images| images.iter().find(|file| key.matches(file)).cloned())
  });
  let at = Memo::new(move |_| images.with(|images| position(images, current.get())));
  // Near an end of what is loaded (in the whole conversation, not one message's album), the next
  // page comes in: the neighbours keep their keys, so the view stays where it is. Runs again after
  // each page (the other end may be wanted too) and, while the core hasn't answered, on the change
  // notification.
  Effect::new(move |_| {
    let Some(position) = at.get() else {
      return;
    };
    if scope.get_untracked() != LightboxScope::Conversation {
      return;
    }
    let Some(anchor) = pages.with(|pages| {
      pages
        .as_ref()
        .and_then(|pages| pages.wanted(position, PAGE_MARGIN))
    }) else {
      return;
    };
    if let Some(page) = state.read(|backend| backend.conversation_media(conversation, anchor, PAGE_MESSAGES))
    {
      pages.update(|pages| {
        if let Some(pages) = pages {
          pages.add(anchor, page);
        }
      });
    }
  });
  let full = RwSignal::new_local(FullMedia::Loading);
  // A WebP or PNG turns out to move once it is loaded (animated WebP, APNG): it can be paused then.
  let kind = Memo::new(move |_| {
    let declared = current_file.with(|file| file.as_ref().and_then(|file| MediaKind::of(&file.media_type)));
    let moves = full.with(|full| matches!(full, FullMedia::Ready(url) if url.is_animated()));
    match declared {
      Some(MediaKind::Image) if moves => Some(MediaKind::Animated),
      other => other,
    }
  });
  let thumbnail = RwSignal::new_local(None::<Placeholder>);
  let arrival = RwSignal::new(Arrival::default());
  let viewport = RwSignal::new(Viewport::default());
  let is_paused = RwSignal::new(false);
  // An animation found only once the file arrived starts paused under reduced motion too.
  Effect::new(move |previous: Option<Option<MediaKind>>| {
    let now = kind.get();
    if previous == Some(Some(MediaKind::Image))
      && now == Some(MediaKind::Animated)
      && prefers_reduced_motion()
    {
      is_paused.set(true);
    }
    now
  });
  let notice = Notice::default();
  let retries = RwSignal::new(0_u32);
  let ticket = StoredValue::new(0_u64);
  let root = NodeRef::<html::Div>::new();
  let backdrop_node = NodeRef::<html::Div>::new();
  let stage = NodeRef::<html::Div>::new();
  let track = NodeRef::<html::Div>::new();
  let image = NodeRef::<html::Img>::new();
  let video = NodeRef::<html::Video>::new();
  let is_thread_shown = RwSignal::new(IS_THREAD_SHOWN.get().unwrap_or_else(is_wide_window));
  let modifier = modifier_style();

  load_current(LoadInputs {
    conversation,
    current,
    current_file,
    is_listed,
    kind,
    full,
    thumbnail,
    arrival,
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
  let motion = StageMotion::new(
    MotionNodes {
      root,
      backdrop: backdrop_node,
      stage,
      track,
      image,
    },
    MotionHooks {
      go: Callback::new(go),
      close: Callback::new(move |()| state.close_overlay()),
      neighbours: Signal::derive(move || {
        at.get()
          .map_or((false, false), |at| (at.has_previous(), at.has_next()))
      }),
      current: current.into(),
    },
    viewport,
  );
  Effect::new(move |_| motion.enter());
  // A button, a key or the strip changed the picture: whatever moved is done.
  Effect::new(move |previous: Option<MediaKey>| {
    let key = current.get();
    if previous.is_some_and(|previous| previous != key) {
      motion.reset();
    }
    key
  });
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
    motion,
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
    if ev.key() == "Escape" {
      // Before the dialog closes itself: the picture goes back into its tile.
      ev.prevent_default();
      ev.stop_propagation();
      motion.close();
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

  // The author the message list shows: the original one of an imported message (#1088).
  let sender = Memo::new(move |_| {
    current_file
      .get()
      .and_then(|file| shown_author(file.maybe_sender, file.maybe_imported_author.as_ref()))
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
      .map(|placeholder| format!("url(\"{}\")", placeholder.url.as_str()))
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
      <div
        class="lightbox"
        tabindex="-1"
        class:with-thread=move || is_thread_shown.get()
        node_ref=root
        on:keydown=on_keydown
      >
        <div class="lightbox-backdrop" aria-hidden="true" node_ref=backdrop_node style:background-image=backdrop></div>
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
                on:click=move |_| motion.close()
              >
                <Icon name=IconName::Close />
              </button>
            </div>
          </header>
          <div class="lightbox-body">
            <Stage
              stage=stage
              track=track
              image=image
              video=video
              viewport=viewport
              full=full
              thumbnail=thumbnail
              arrival=arrival
              current_file=current_file
              images=images
              current=current
              kind=kind
              is_paused=is_paused
              motion=motion
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
  /// Whether the first page of files is there.
  is_listed: Memo<bool>,
  kind: Memo<Option<MediaKind>>,
  full: RwSignal<FullMedia, LocalStorage>,
  thumbnail: RwSignal<Option<Placeholder>, LocalStorage>,
  arrival: RwSignal<Arrival>,
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
    is_listed,
    kind,
    full,
    thumbnail,
    arrival,
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
    arrival.set(Arrival::start(key));
    let next_ticket = ticket.get_value() + 1;
    ticket.set_value(next_ticket);
    let Some(media_type) = media_type.get() else {
      // The list is still loading (it runs again when it is there), or it doesn't have the file.
      let is_gone = is_listed.get();
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
        .or_else(|| current_file.with_untracked(|file| file.as_ref().and_then(thumbnail_url)))
        .map(|url| Placeholder { key, url }),
    );
    let size_bytes = current_file.with_untracked(|file| file.as_ref().map_or(0, |file| file.size_bytes));
    if size_bytes > MAX_VIEWER_BYTES {
      full.set(FullMedia::TooLarge);
      return;
    }
    let is_video = kind.get_untracked() == Some(MediaKind::Video);
    // Seen before in this session (inline or in the lightbox): shown at once.
    if let Some(url) = media_cache::cached(key, Rendition::Full) {
      full.set(loaded(key, is_video, url));
      return;
    }
    full.set(FullMedia::Loading);
    spawn_local(async move {
      let result = media_cache::load(conversation, key, &media_type, Rendition::Full, None, || true).await;
      if ticket.try_get_value() != Some(next_ticket) {
        // The user moved on or closed the lightbox; the cache keeps the file for next time.
        return;
      }
      let shown = match result {
        Ok(Some(url)) => loaded(key, is_video, url),
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

/// A loaded file, or that it can't be played: a video that already failed to play in this session
/// (in its message row, say) isn't tried again (#804).
fn loaded(key: MediaKey, is_video: bool, url: Rc<ObjectUrl>) -> FullMedia {
  match is_video && media_cache::is_unplayable(key) {
    true => FullMedia::Unplayable,
    false => FullMedia::Ready(url),
  }
}

/// Where the file is shown and the pointer and wheel gestures happen. A tap beside the picture
/// closes the lightbox, like a click outside a dialog. Videos keep their own controls: the stage
/// leaves their pointer events alone.
///
/// The stage holds a track of three slots, the previous, current and next file, so a page drag
/// shows the neighbour coming in. The neighbours show what is already in memory (the full file seen
/// before, the inline rendition, or the thumbnail), never a new download.
#[component]
fn Stage(
  stage: NodeRef<html::Div>,
  track: NodeRef<html::Div>,
  image: NodeRef<html::Img>,
  video: NodeRef<html::Video>,
  viewport: RwSignal<Viewport>,
  full: RwSignal<FullMedia, LocalStorage>,
  thumbnail: RwSignal<Option<Placeholder>, LocalStorage>,
  arrival: RwSignal<Arrival>,
  current_file: Memo<Option<FileInfo>>,
  images: Memo<Vec<FileInfo>>,
  current: RwSignal<MediaKey>,
  kind: Memo<Option<MediaKind>>,
  is_paused: RwSignal<bool>,
  motion: StageMotion,
) -> impl IntoView {
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
  let previous_peer = peer_of(images, current, Direction::Previous);
  let next_peer = peer_of(images, current, Direction::Next);
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
    motion.stop_view();
    viewport.update(|viewport| viewport.zoom_by(factor, anchor, image_size, stage_size));
  };
  let toggle_at = move |anchor: Point| {
    let (image_size, stage_size) = sizes(image, stage);
    motion.stop_view();
    viewport.update(|viewport| viewport.toggle_zoom(anchor, image_size, stage_size));
  };
  let on_wheel = move |ev: web_sys::WheelEvent| {
    if !can_transform() {
      return;
    }
    ev.prevent_default();
    let to_px = |delta: f64| match ev.delta_mode() {
      web_sys::WheelEvent::DOM_DELTA_LINE => delta * WHEEL_LINE_PX,
      web_sys::WheelEvent::DOM_DELTA_PAGE => delta * sizes(image, stage).1.height,
      _ => delta,
    };
    let delta = Point::new(to_px(ev.delta_x()), to_px(ev.delta_y()));
    // A trackpad's two-finger swipe sideways pages; trackpad pinches arrive with the control key.
    if !ev.ctrl_key() && motion.wheel(delta, ev.time_stamp()) {
      return;
    }
    zoom_at(
      wheel_zoom_factor(delta.y, ev.ctrl_key()),
      anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y())),
    );
  };
  listen_to_wheel(stage, on_wheel);
  let on_pointerdown = move |ev: web_sys::PointerEvent| {
    let is_on_picture = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .and_then(|target| target.closest(".lightbox-image, .lightbox-video").ok().flatten())
      .is_some();
    // A video's own controls get their presses; capturing them would break seeking.
    if ev.button() > 0 || (!can_transform() && is_on_picture) {
      return;
    }
    is_mouse.set_value(ev.pointer_type() == "mouse");
    is_press_beside_image.set_value(!is_on_picture);
    if let Some(stage) = stage.get_untracked() {
      // Without capture a drag that leaves the stage would never end.
      drop(stage.set_pointer_capture(ev.pointer_id()));
    }
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let mut count = 0;
    pointers.update_value(|pointers| {
      pointers.down(ev.pointer_id(), at);
      count = pointers.count();
    });
    if can_transform() {
      motion.press(at, ev.time_stamp(), count);
    }
  };
  let on_pointermove = move |ev: web_sys::PointerEvent| {
    if !can_transform() {
      return;
    }
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let mut gesture = Gesture::None;
    pointers.update_value(|pointers| gesture = pointers.moved(ev.pointer_id(), at));
    if gesture != Gesture::None {
      motion.drag(gesture, at, ev.time_stamp());
    }
  };
  let on_pointerup = move |ev: web_sys::PointerEvent| {
    let at = anchor_of(f64::from(ev.client_x()), f64::from(ev.client_y()));
    let mut release = Release::None;
    let mut count = 0;
    pointers.update_value(|pointers| {
      release = pointers.up(ev.pointer_id(), at, ev.time_stamp());
      count = pointers.count();
    });
    let was_gesture = can_transform() && motion.release(ev.time_stamp(), count);
    match release {
      _ if was_gesture => {},
      Release::Tap(_) if is_press_beside_image.get_value() => motion.close(),
      Release::DoubleTap(anchor) if !is_mouse.get_value() && can_transform() => toggle_at(anchor),
      _ => {},
    }
  };
  let on_pointercancel = move |ev: web_sys::PointerEvent| {
    pointers.update_value(|pointers| pointers.cancel(ev.pointer_id()));
    if can_transform() {
      motion.cancel();
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
      class:is-live=motion.is_live()
      node_ref=stage
      on:pointerdown=on_pointerdown
      on:pointermove=on_pointermove
      on:pointerup=on_pointerup
      on:pointercancel=on_pointercancel
      on:dblclick=on_dblclick
    >
      // Hidden until the first picture is there, which then grows out of its tile.
      <div class="lightbox-track is-pending" node_ref=track>
        <div class="lightbox-slot is-previous" aria-hidden="true">
          <Peer peer=previous_peer />
        </div>
        // The full file over its own placeholder: the placeholder stays until the file is decoded,
        // then the two cross-fade.
        <div class="lightbox-slot is-current">
          {move || match (full.get(), kind.get()) {
            (FullMedia::Ready(url), Some(MediaKind::Video)) => {
              let key = arrival.with_untracked(|arrival| arrival.key());
              view! {
                <VideoPlayer
                  src=url.as_str().to_owned()
                  video=video
                  label=Signal::derive(name)
                  class="lightbox-video"
                  maybe_on_loaded=Callback::new(move |()| {
                    mark_shown(arrival, key);
                    motion.shown();
                  })
                  on_unplayable=Callback::new(move |()| {
                    if let Some(key) = key {
                      media_cache::mark_unplayable(key);
                    }
                    drop(full.try_set(FullMedia::Unplayable));
                  })
                />
              }
              .into_any()
            },
            (FullMedia::Ready(url), _) => {
              // Paused: the still frame once it is made, the placeholder until then.
              let file_src = url.as_str().to_owned();
              let src = move || {
                if !is_paused.get() {
                  return file_src.clone();
                }
                still
                  .get()
                  .or_else(|| thumbnail.get().map(|placeholder| placeholder.url))
                  .map(|frame| frame.as_str().to_owned())
                  .unwrap_or_default()
              };
              // Captured with the element: a late `load` of a file paged away from is ignored.
              let key = arrival.with_untracked(|arrival| arrival.key());
              let on_shown = move |element: &web_sys::HtmlImageElement| {
                // Measured right away by the motion, before the class binding catches up.
                drop(element.class_list().remove_1("is-arriving"));
                mark_shown(arrival, key);
                layout.update(|tick| *tick += 1);
                motion.shown();
              };
              // A file decoded before the listener was there (cached) fires no `load` of its own.
              // The node ref may still hold the previous file's image: only one showing what this
              // one shows counts (the file, or while paused its still frame or placeholder).
              let expected_src = src.clone();
              Effect::new(move |_| {
                if let Some(element) = image.get()
                  && element.src() == untrack(&expected_src)
                  && element.complete()
                  && element.natural_width() > 0
                {
                  on_shown(&element);
                }
              });
              view! {
                <img
                  class="lightbox-image"
                  class:is-arriving=move || !key.is_some_and(|key| arrival.with(|arrival| arrival.is_full_shown(key)))
                  src=src
                  alt=name
                  decoding="async"
                  draggable="false"
                  node_ref=image
                  style:transform=transform
                  on:load=move |ev| on_shown(&event_target::<web_sys::HtmlImageElement>(&ev))
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
            _ => ().into_any(),
          }}
          {move || {
            let key = current.get();
            thumbnail
              .get()
              .filter(|placeholder| placeholder.key == key)
              .map(|placeholder| {
                view! {
                  <img
                    class="lightbox-image lightbox-placeholder"
                    class:is-hidden=move || arrival.with(|arrival| arrival.is_full_shown(key))
                    src=placeholder.url.as_str().to_owned()
                    alt=""
                    aria-hidden="true"
                    draggable="false"
                    on:load=move |_| motion.shown()
                  />
                }
              })
          }}
        </div>
        <div class="lightbox-slot is-next" aria-hidden="true">
          <Peer peer=next_peer />
        </div>
      </div>
    </div>
  }
}

/// The full picture of `maybe_key` is on screen: its placeholder fades out.
fn mark_shown(arrival: RwSignal<Arrival>, maybe_key: Option<MediaKey>) {
  if let Some(key) = maybe_key {
    // The lightbox may have closed in the meantime.
    arrival.try_update(|arrival| arrival.full_shown(key));
  }
}

/// What a neighbour's slot shows: the best rendition already in memory.
#[derive(Clone)]
struct PeerImage {
  url: Rc<ObjectUrl>,
  /// The full file, laid out like the current picture; else a placeholder like the current one
  /// before its file arrives, so the hand-over at the end of a page turn doesn't jump.
  is_full: bool,
}

/// The neighbour in `direction` of the current file, kept up to date as the lightbox pages.
fn peer_of(
  images: Memo<Vec<FileInfo>>,
  current: RwSignal<MediaKey>,
  direction: Direction,
) -> RwSignal<Option<PeerImage>, LocalStorage> {
  let peer = RwSignal::new_local(None::<PeerImage>);
  Effect::new(move |_| {
    let maybe_file = images.with(|images| {
      let key = neighbor(images, current.get(), direction)?;
      images.iter().find(|file| key.matches(file)).cloned()
    });
    peer.set(maybe_file.and_then(|file| peer_image(&file)));
  });
  peer
}

fn peer_image(file: &FileInfo) -> Option<PeerImage> {
  let key = MediaKey::of(file)?;
  let is_picture = MediaKind::of(&file.media_type).is_some_and(MediaKind::can_transform);
  if is_picture && let Some(url) = media_cache::cached(key, Rendition::Full) {
    return Some(PeerImage { url, is_full: true });
  }
  media_cache::cached(key, Rendition::Inline)
    .or_else(|| thumbnail_url(file))
    .map(|url| PeerImage { url, is_full: false })
}

#[component]
fn Peer(peer: RwSignal<Option<PeerImage>, LocalStorage>) -> impl IntoView {
  move || {
    peer.get().map(|peer| {
      view! {
        <img
          class="lightbox-image"
          class:lightbox-placeholder=!peer.is_full
          src=peer.url.as_str().to_owned()
          alt=""
          decoding="async"
          draggable="false"
        />
      }
    })
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
    FullMedia::Unplayable => view! { <Unplayable actions=actions /> }.into_any(),
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

/// A video this webview can't play (#804): what to do instead, and on Linux the packages that bring
/// the missing decoder. "Open in video player" only in the desktop app, which can hand the file over.
#[component]
fn Unplayable(actions: MediaActions) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_hint = RwSignal::new(None::<String>);
  spawn_local(async move {
    let Some(playback) = crate::auth::tauri::video_playback().await else {
      return;
    };
    let packages = playback.packages_to_install();
    let hint = if !packages.is_empty() {
      i18n.t_with("video-unplayable-install", &[("packages", &packages.join(", "))])
    } else if !playback.missing().is_empty() {
      i18n.t("video-unplayable-codecs")
    } else {
      return;
    };
    maybe_hint.set_if_alive(Some(hint));
  });
  let opening = PendingAction::new();
  let open_externally = move |_| {
    let Some(file) = actions.current_file.get_untracked() else {
      return;
    };
    opening.spawn(async move {
      crate::auth::tauri::open_video_externally(&file)
        .await
        .map_err(|error| ActionFailure::new(FailureKind::Other, error))
    });
  };
  view! {
    <div class="lightbox-unplayable" role="alert">
      <p class="lightbox-unplayable-title">{move || i18n.t("video-unplayable")}</p>
      {move || maybe_hint.get().map(|hint| view! { <p class="lightbox-unplayable-hint">{hint}</p> })}
      <div class="lightbox-unplayable-actions">
        <button class="btn btn-small" on:click=move |_| actions.run(Command::Download)>
          {move || i18n.t("lightbox-save")}
        </button>
        {crate::auth::tauri::is_available().then(|| view! {
          <PendingButton pending=opening class="btn btn-small btn-primary" on_click=open_externally>
            {move || i18n.t("video-open-externally")}
          </PendingButton>
        })}
      </div>
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

/// Keys typed into the thread's composer, a text field or the zoom slider are theirs, and the "…"
/// menu handles its own.
fn is_typing_target(ev: &web_sys::KeyboardEvent) -> bool {
  let Some(element) = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
  else {
    return false;
  };
  // The video player takes the keys it uses itself; the rest (Escape, T, J) are the lightbox's.
  if element.closest(".video-player").ok().flatten().is_some() {
    return false;
  }
  let tag = element.tag_name().to_ascii_lowercase();
  matches!(tag.as_str(), "input" | "textarea" | "select")
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
