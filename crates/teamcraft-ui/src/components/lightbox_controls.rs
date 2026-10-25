//! The lightbox's controls: the toolbar at the bottom (rotate and zoom on the left; download, the
//! thread panel, a new window and the "…" menu on the right), the file details, the thumbnail strip,
//! and the browser parts behind them (clipboard, still frames, the new window).
//!
//! The decrypted file only ever exists as the lightbox's in-memory `blob:` URL; copying draws it into
//! a canvas in this page, and the new window is a blank window of this page that shows the same URL.

use std::rc::Rc;
use std::time::Duration;

use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::{FileInfo, ShareLinkOptions};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::clock::format_date_time;
use crate::components::icons::{Icon, IconName};
use crate::components::lightbox::{FullMedia, sizes};
use crate::components::message_actions::copy_to_clipboard;
use crate::i18n::{I18n, use_i18n};
use crate::lightbox::{
  Command, KEY_ZOOM_STEP, MAX_STRIP_THUMBNAILS, MediaKey, MediaKind, ModifierStyle, Point, Viewport,
  position, strip_range,
};
use crate::model::{ConversationId, thread_id_of};
use crate::object_url::ObjectUrl;
use crate::state::{AppState, Overlay, use_app_state};

/// How long a confirmation ("Link copied") stays in the toolbar.
const NOTICE_DURATION: Duration = Duration::from_secs(4);
/// Steps of the zoom slider between fit and the maximum zoom.
const SLIDER_STEPS: f64 = 100.0;

/// A short confirmation in the toolbar, read out by screen readers.
#[derive(Clone, Copy)]
pub struct Notice {
  text: RwSignal<Option<String>>,
  serial: StoredValue<u64>,
}

/// Everything the lightbox does to the file on screen, shared by the keys, the buttons and the menu.
#[derive(Clone, Copy)]
pub struct MediaActions {
  pub state: AppState,
  pub i18n: I18n,
  pub conversation: ConversationId,
  pub current: RwSignal<MediaKey>,
  pub current_file: Memo<Option<FileInfo>>,
  pub kind: Memo<Option<MediaKind>>,
  pub full: RwSignal<FullMedia, LocalStorage>,
  pub image: NodeRef<html::Img>,
  pub video: NodeRef<html::Video>,
  pub stage: NodeRef<html::Div>,
  pub viewport: RwSignal<Viewport>,
  pub is_paused: RwSignal<bool>,
  pub is_thread_shown: RwSignal<bool>,
  pub notice: Notice,
}

impl Default for Notice {
  fn default() -> Self {
    Self {
      text: RwSignal::new(None),
      serial: StoredValue::new(0),
    }
  }
}

impl Notice {
  /// Shows `text` for a few seconds; a newer notice replaces it.
  pub fn show(self, text: String) {
    let serial = self.serial.get_value() + 1;
    self.serial.set_value(serial);
    self.text.set(Some(text));
    set_timeout(
      move || {
        if self.serial.try_get_value() == Some(serial) {
          drop(self.text.try_set(None));
        }
      },
      NOTICE_DURATION,
    );
  }

  /// A notice is about the file it was shown for.
  pub fn clear_on_change(self, current: RwSignal<MediaKey>) {
    Effect::new(move |_| {
      current.track();
      self.text.set(None);
    });
  }
}

impl MediaActions {
  /// Runs a command that concerns the file on screen; paging and the thread panel belong to the
  /// lightbox itself.
  pub fn run(self, command: Command) {
    let can_transform = self.kind.get_untracked().is_some_and(MediaKind::can_transform);
    match command {
      Command::ZoomIn | Command::ZoomOut if can_transform => {
        let factor = match command {
          Command::ZoomOut => 1.0 / KEY_ZOOM_STEP,
          _ => KEY_ZOOM_STEP,
        };
        let (image, stage) = sizes(self.image, self.stage);
        self
          .viewport
          .update(|viewport| viewport.zoom_by(factor, Point::ORIGIN, image, stage));
      },
      Command::ZoomReset => self.viewport.update(Viewport::reset),
      Command::Rotate if can_transform => self.viewport.update(Viewport::rotate_clockwise),
      Command::TogglePlay => self.toggle_play(),
      Command::JumpToMessage => self.jump_to_message(),
      Command::Download => self.download(),
      Command::CopyImage if can_transform => self.copy_image(),
      _ => {},
    }
  }

  pub fn open_in_window(self) {
    let (FullMedia::Ready(url), Some(kind)) = (self.full.get_untracked(), self.kind.get_untracked()) else {
      return;
    };
    let name = self
      .current_file
      .with_untracked(|file| file.as_ref().map(|file| file.file_name.clone()))
      .unwrap_or_default();
    if !open_in_window(url, &name, kind) {
      self.notice.show(self.i18n.t("lightbox-window-blocked"));
    }
  }

  fn toggle_play(self) {
    match self.kind.get_untracked() {
      Some(MediaKind::Animated) => self.is_paused.update(|paused| *paused = !*paused),
      Some(MediaKind::Video) => {
        let Some(video) = self.video.get_untracked() else {
          return;
        };
        if video.paused() {
          // A refused play (no user gesture yet) leaves the video paused, with its own play button.
          drop(video.play());
        } else {
          drop(video.pause());
        }
      },
      Some(MediaKind::Image) | None => {},
    }
  }

  fn jump_to_message(self) {
    let message = self.current.get_untracked().message;
    self.state.close_overlay();
    self.state.open_message(self.conversation, message, None);
  }

  /// The core downloads and decrypts the file again and saves it (the download folder on the
  /// desktop, the browser's download on the web), with the same size cap as everywhere else.
  fn download(self) {
    let Some(file) = self.current_file.get_untracked() else {
      return;
    };
    self
      .state
      .write(|backend| backend.download_attachment(file.conversation, file.message, &file.file_id));
    // The file list is a snapshot, so progress is not followed here: the file's card shows the result.
    self.notice.show(self.i18n.t("lightbox-saving"));
  }

  fn copy_image(self) {
    let i18n = self.i18n;
    let Some(image) = self.image.get_untracked() else {
      return;
    };
    let notice = self.notice;
    spawn_local(async move {
      let key = match copy_image(&image).await {
        true => "lightbox-image-copied",
        false => "lightbox-copy-failed",
      };
      notice.show(i18n.t(key));
    });
  }
}

/// The toolbar under the image.
#[component]
pub fn Controls(
  actions: MediaActions,
  modifier: ModifierStyle,
  on_toggle_thread: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let viewport = actions.viewport;
  let can_transform = Signal::derive(move || actions.kind.get().is_some_and(MediaKind::can_transform));
  let is_animated = move || actions.kind.get() == Some(MediaKind::Animated);
  let is_ready = Signal::derive(move || matches!(actions.full.get(), FullMedia::Ready(_)));
  let is_details_open = RwSignal::new(false);
  let zoom_label = move || format!("{:.0} %", viewport.get().scale() * 100.0);
  let on_slider = move |ev: web_sys::Event| {
    let Ok(value) = event_target_value(&ev).parse::<f64>() else {
      return;
    };
    let (image, stage) = sizes(actions.image, actions.stage);
    viewport.update(|viewport| viewport.zoom_to_slider(value / SLIDER_STEPS, image, stage));
  };
  let thread_label = Signal::derive(move || match actions.is_thread_shown.get() {
    true => i18n.t("lightbox-thread-hide"),
    false => i18n.t("lightbox-thread-show"),
  });
  let play_label = Signal::derive(move || match actions.is_paused.get() {
    true => i18n.t("lightbox-play"),
    false => i18n.t("lightbox-pause"),
  });

  view! {
    <footer class="lightbox-controls">
      <div class="lightbox-controls-start" role="toolbar" aria-label=move || i18n.t("lightbox-view-tools")>
        <ToolButton
          label=Signal::derive(move || i18n.t("lightbox-rotate"))
          command=Command::Rotate
          modifier=modifier
          icon=IconName::Rotate
          disabled=Signal::derive(move || !can_transform.get())
          on_click=Callback::new(move |()| actions.run(Command::Rotate))
        />
        <div class="lightbox-zoom-group" role="group" aria-label=move || i18n.t("lightbox-zoom")>
          <ToolButton
            label=Signal::derive(move || i18n.t("lightbox-zoom-out"))
            command=Command::ZoomOut
            modifier=modifier
            icon=IconName::ZoomOut
            disabled=Signal::derive(move || !can_transform.get() || !viewport.get().is_zoomed())
            on_click=Callback::new(move |()| actions.run(Command::ZoomOut))
          />
          <input
            type="range"
            class="lightbox-slider"
            min="0"
            max=SLIDER_STEPS.to_string()
            step="1"
            aria-label=move || i18n.t("lightbox-zoom")
            aria-valuetext=zoom_label
            disabled=move || !can_transform.get()
            prop:value=move || (viewport.get().slider_position() * SLIDER_STEPS).round().to_string()
            on:input=on_slider
          />
          <ToolButton
            label=Signal::derive(move || i18n.t("lightbox-zoom-in"))
            command=Command::ZoomIn
            modifier=modifier
            icon=IconName::ZoomIn
            disabled=Signal::derive(move || !can_transform.get() || !viewport.get().can_zoom_in())
            on_click=Callback::new(move |()| actions.run(Command::ZoomIn))
          />
          <button
            class="btn btn-small lightbox-zoom"
            aria-label=move || i18n.t_with("lightbox-zoom-reset", &[("zoom", &zoom_label())])
            data-tip=move || tooltip(i18n, "lightbox-fit", Command::ZoomReset, modifier)
            aria-keyshortcuts="0"
            disabled=move || !can_transform.get()
            on:click=move |_| actions.run(Command::ZoomReset)
          >
            {zoom_label}
          </button>
        </div>
        <Show when=is_animated fallback=|| ()>
          <ToolButton
            label=play_label
            command=Command::TogglePlay
            modifier=modifier
            icon=Signal::derive(move || if actions.is_paused.get() { IconName::Play } else { IconName::Pause })
            on_click=Callback::new(move |()| actions.run(Command::TogglePlay))
          />
        </Show>
      </div>
      <span class="lightbox-notice" role="status">{move || actions.notice.text.get()}</span>
      <div class="lightbox-controls-end" role="toolbar" aria-label=move || i18n.t("lightbox-toolbar")>
        <ToolButton
          label=Signal::derive(move || i18n.t("lightbox-save"))
          command=Command::Download
          modifier=modifier
          icon=IconName::Download
          on_click=Callback::new(move |()| actions.run(Command::Download))
        />
        <ToolButton
          label=thread_label
          command=Command::ToggleThread
          modifier=modifier
          icon=IconName::Threads
          is_active=Signal::derive(move || actions.is_thread_shown.get())
          on_click=on_toggle_thread
        />
        <button
          class="icon-btn"
          aria-label=move || i18n.t("lightbox-open-window")
          data-tip=move || i18n.t("lightbox-open-window")
          disabled=move || !is_ready.get()
          on:click=move |_| actions.open_in_window()
        >
          <Icon name=IconName::External />
        </button>
        <MoreMenu actions=actions modifier=modifier is_details_open=is_details_open />
      </div>
      <Show when=move || is_details_open.get() fallback=|| ()>
        <FileDetails actions=actions is_open=is_details_open />
      </Show>
    </footer>
  }
}

/// An icon button with a tooltip that names its key.
#[component]
fn ToolButton(
  label: Signal<String>,
  command: Command,
  modifier: ModifierStyle,
  #[prop(into)] icon: Signal<IconName>,
  /// Shown as switched on (the thread panel while it is open). The label says what a click does,
  /// so the state is not announced a second time.
  #[prop(optional)]
  is_active: Signal<bool>,
  #[prop(optional)] disabled: Signal<bool>,
  on_click: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let tip = move || {
    i18n.t_with(
      "lightbox-shortcut",
      &[
        ("action", &label.get()),
        ("key", command.shortcut_label(modifier)),
      ],
    )
  };
  view! {
    <button
      class="icon-btn"
      class:is-active=move || is_active.get()
      aria-label=move || label.get()
      aria-keyshortcuts=command.aria_shortcut(modifier)
      data-tip=tip
      disabled=move || disabled.get()
      on:click=move |_| on_click.run(())
    >
      {move || view! { <Icon name=icon.get() /> }}
    </button>
  }
}

/// The "…" menu: what else can be done with the file and its message.
#[component]
fn MoreMenu(
  actions: MediaActions,
  modifier: ModifierStyle,
  is_details_open: RwSignal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_open = RwSignal::new(false);
  let trigger = NodeRef::<html::Button>::new();
  let menu = NodeRef::<html::Div>::new();
  let conversation = actions.conversation;
  let message = move || actions.current.get_untracked().message;
  let is_saved = Memo::new(move |_| {
    let message = actions.current.get().message;
    state
      .read(|backend| backend.message_extras(conversation, &[message]))
      .into_iter()
      .next()
      .is_some_and(|extras| extras.is_saved)
  });
  let share_link = Memo::new(move |_| {
    let file = actions.current_file.get()?;
    state
      .read(|backend| backend.share_links(conversation, file.message, &file.file_id))
      .into_iter()
      .next()
  });
  let can_copy_image = move || {
    actions.kind.get().is_some_and(MediaKind::can_transform)
      && matches!(actions.full.get(), FullMedia::Ready(_))
  };
  let close = move || {
    is_open.set(false);
    if let Some(trigger) = trigger.get_untracked() {
      drop(trigger.focus());
    }
  };
  // Opening the menu starts fetching the permalink so that "Copy link" has it by the time it is used.
  let toggle = move |_| {
    let opening = !is_open.get_untracked();
    is_open.set(opening);
    if opening {
      state.read(|backend| drop(backend.permalink(conversation, message())));
    }
  };
  Effect::new(move |_| {
    if is_open.get()
      && let Some(first) = menu
        .get()
        .and_then(|menu| menu.query_selector(".menu-item").ok().flatten())
      && let Ok(first) = first.dyn_into::<web_sys::HtmlElement>()
    {
      drop(first.focus());
    }
  });
  let on_menu_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Escape" => {
      // Closes the menu, not the lightbox.
      ev.stop_propagation();
      ev.prevent_default();
      close();
    },
    "ArrowDown" | "ArrowUp" => {
      ev.prevent_default();
      move_focus(menu, ev.key() == "ArrowDown");
    },
    "Tab" => is_open.set(false),
    _ => {},
  };
  let view_thread = move |_| {
    let message = message();
    state.close_overlay();
    state.open_message(conversation, message, None);
    state.open_thread(conversation, thread_id_of(message));
  };
  let toggle_saved = move |_| {
    let save = !is_saved.get_untracked();
    state.write(|backend| backend.set_saved(conversation, message(), save));
    let key = match save {
      true => "lightbox-saved-added",
      false => "lightbox-saved-removed",
    };
    actions.notice.show(i18n.t(key));
    close();
  };
  let copy_message_link = move |_| {
    let key = match state.read(|backend| backend.permalink(conversation, message())) {
      Some(link) => {
        copy_to_clipboard(&link);
        "lightbox-link-copied"
      },
      None => "lightbox-link-unavailable",
    };
    actions.notice.show(i18n.t(key));
    close();
  };
  let share = move |_| {
    let key = match share_link.get_untracked() {
      Some(link) => {
        copy_to_clipboard(&link.url);
        "lightbox-share-copied"
      },
      None => {
        let Some(file) = actions.current_file.get_untracked() else {
          return;
        };
        state.write(|backend| {
          backend.create_share_link(
            conversation,
            file.message,
            &file.file_id,
            ShareLinkOptions::default(),
          )
        });
        "lightbox-share-created"
      },
    };
    actions.notice.show(i18n.t(key));
    close();
  };
  let shortcut = move |command: Command| command.shortcut_label(modifier);

  view! {
    <div class="lightbox-menu-wrap">
      <button
        class="icon-btn"
        node_ref=trigger
        aria-haspopup="menu"
        aria-expanded=move || is_open.get().to_string()
        aria-label=move || i18n.t("lightbox-more")
        data-tip=move || i18n.t("lightbox-more")
        on:click=toggle
      >
        <Icon name=IconName::More />
      </button>
      <Show when=move || is_open.get() fallback=|| ()>
        <div class="popover-backdrop" on:mousedown=move |_| is_open.set(false)></div>
        <div class="popover lightbox-menu opens-up" role="menu" node_ref=menu aria-label=move || i18n.t("lightbox-more") on:keydown=on_menu_keydown>
          <button class="menu-item" role="menuitem" on:click=view_thread>
            <Icon name=IconName::Threads />
            <span class="menu-label">{move || i18n.t("lightbox-view-thread")}</span>
          </button>
          <button class="menu-item" role="menuitem" on:click=move |_| actions.run(Command::JumpToMessage)>
            <Icon name=IconName::Reply />
            <span class="menu-label">{move || i18n.t("lightbox-jump")}</span>
            <kbd class="menu-shortcut">{shortcut(Command::JumpToMessage)}</kbd>
          </button>
          <button class="menu-item" role="menuitem" on:click=toggle_saved>
            <Icon name=IconName::Bookmark />
            <span class="menu-label">
              {move || if is_saved.get() { i18n.t("action-unsave") } else { i18n.t("action-save") }}
            </span>
          </button>
          <div class="menu-sep" role="separator"></div>
          <button class="menu-item" role="menuitem" on:click=copy_message_link>
            <Icon name=IconName::Link />
            <span class="menu-label">{move || i18n.t("lightbox-copy-link")}</span>
          </button>
          <button class="menu-item" role="menuitem" on:click=share>
            <Icon name=IconName::Link />
            <span class="menu-label">
              {move || if share_link.get().is_some() { i18n.t("file-copy-share") } else { i18n.t("file-share") }}
            </span>
          </button>
          <Show when=can_copy_image fallback=|| ()>
            <button class="menu-item" role="menuitem" on:click=move |_| { actions.run(Command::CopyImage); close(); }>
              <Icon name=IconName::Copy />
              <span class="menu-label">{move || i18n.t("lightbox-copy-image")}</span>
              <kbd class="menu-shortcut">{shortcut(Command::CopyImage)}</kbd>
            </button>
          </Show>
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| state.overlay.set(Overlay::Forward(conversation, message()))
          >
            <Icon name=IconName::Forward />
            <span class="menu-label">{move || i18n.t("lightbox-forward")}</span>
          </button>
          <div class="menu-sep" role="separator"></div>
          <button
            class="menu-item"
            role="menuitem"
            on:click=move |_| { is_details_open.update(|open| *open = !*open); close(); }
          >
            <Icon name=IconName::Info />
            <span class="menu-label">{move || i18n.t("lightbox-details")}</span>
          </button>
        </div>
      </Show>
    </div>
  }
}

/// Name, type, size, dimensions, sender and time of the file on screen.
#[component]
fn FileDetails(actions: MediaActions, is_open: RwSignal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let file = move || actions.current_file.get();
  let sender = move || {
    file()
      .and_then(|file| file.maybe_sender)
      .and_then(|user| state.read(|backend| backend.user(user)))
      .map(|user| user.display_name)
      .unwrap_or_default()
  };
  let sent = move || {
    file()
      .and_then(|file| file.maybe_sent_at)
      .map(|ts| format_date_time(ts, i18n.locale().tag()))
      .unwrap_or_default()
  };
  // Known once the file is shown; a video reports its size after its metadata loaded.
  let dimensions = move || {
    actions.full.track();
    actions.current.track();
    let (width, height) = match actions.kind.get() {
      Some(MediaKind::Video) => actions
        .video
        .get()
        .map_or((0, 0), |video| (video.video_width(), video.video_height())),
      _ => actions
        .image
        .get()
        .map_or((0, 0), |image| (image.natural_width(), image.natural_height())),
    };
    (width > 0 && height > 0).then(|| {
      i18n.t_with(
        "lightbox-details-dimensions-value",
        &[("width", &width.to_string()), ("height", &height.to_string())],
      )
    })
  };
  // Focus moves into the panel, and back to the "…" button it was opened from when it closes.
  let close_button = NodeRef::<html::Button>::new();
  Effect::new(move |_| {
    if let Some(button) = close_button.get() {
      drop(button.focus());
    }
  });
  let close = move || {
    is_open.set(false);
    focus_selector(".lightbox-menu-wrap > .icon-btn");
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.key() == "Escape" {
      ev.stop_propagation();
      close();
    }
  };

  view! {
    <section class="lightbox-details" aria-labelledby="lightbox-details-title" on:keydown=on_keydown>
      <header class="lightbox-details-head">
        <h3 id="lightbox-details-title">{move || i18n.t("lightbox-details")}</h3>
        <button class="icon-btn" node_ref=close_button aria-label=move || i18n.t("lightbox-details-close") on:click=move |_| close()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <dl class="lightbox-facts">
        <dt>{move || i18n.t("lightbox-details-name")}</dt>
        <dd translate="no">{move || file().map(|file| file.file_name).unwrap_or_default()}</dd>
        <dt>{move || i18n.t("lightbox-details-type")}</dt>
        <dd translate="no">{move || file().map(|file| file.media_type).unwrap_or_default()}</dd>
        <dt>{move || i18n.t("lightbox-details-size")}</dt>
        <dd>{move || file().map(|file| format_bytes(file.size_bytes)).unwrap_or_default()}</dd>
        {move || dimensions().map(|value| view! {
          <dt>{move || i18n.t("lightbox-details-dimensions")}</dt>
          <dd>{value}</dd>
        })}
        <dt>{move || i18n.t("lightbox-details-sender")}</dt>
        <dd translate="no">{sender}</dd>
        <dt>{move || i18n.t("lightbox-details-sent")}</dt>
        <dd>{sent}</dd>
      </dl>
    </section>
  }
}

/// Thumbnails of the files the lightbox pages through, a window around the current one.
#[component]
pub fn ThumbnailStrip(images: Memo<Vec<FileInfo>>, current: RwSignal<MediaKey>) -> impl IntoView {
  let i18n = use_i18n();
  let visible = Memo::new(move |_| {
    images.with(|images| {
      let index = position(images, current.get()).map_or(0, |at| at.index);
      images[strip_range(images.len(), index, MAX_STRIP_THUMBNAILS)].to_vec()
    })
  });
  view! {
    <Show when=move || images.with(|images| images.len() > 1) fallback=|| ()>
      <nav class="lightbox-strip" aria-label=move || i18n.t("lightbox-thumbnails")>
        <For each=move || visible.get() key=|file| (file.message, file.file_id.clone()) let:file>
          <StripTile file=file current=current />
        </For>
      </nav>
    </Show>
  }
}

#[component]
fn StripTile(file: FileInfo, current: RwSignal<MediaKey>) -> impl IntoView {
  let i18n = use_i18n();
  let Some(key) = MediaKey::of(&file) else {
    return ().into_any();
  };
  let thumbnail = file
    .maybe_thumbnail_png
    .as_deref()
    .and_then(ObjectUrl::from_thumbnail);
  let maybe_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
  // Revoked when the tile leaves the strip.
  StoredValue::new_local(thumbnail);
  let is_video = MediaKind::of(&file.media_type) == Some(MediaKind::Video);
  let name = file.file_name;
  let is_current = move || current.get() == key;
  view! {
    <button
      class="lightbox-strip-tile"
      aria-label=move || i18n.t_with("lightbox-thumbnail", &[("name", &name)])
      aria-current=move || is_current().then_some("true")
      tabindex=move || if is_current() { "0" } else { "-1" }
      on:click=move |_| current.set(key)
    >
      {match maybe_src {
        Some(src) => view! { <img src=src alt="" loading="lazy" draggable="false" /> }.into_any(),
        None => view! { <Icon name=if is_video { IconName::Video } else { IconName::Image } /> }.into_any(),
      }}
      {is_video.then(|| view! { <span class="lightbox-strip-badge" aria-hidden="true"><Icon name=IconName::Play /></span> })}
    </button>
  }
  .into_any()
}

/// "Rotate (R)": a control's tooltip with its key.
pub fn tooltip(i18n: I18n, label_key: &str, command: Command, modifier: ModifierStyle) -> String {
  i18n.t_with(
    "lightbox-shortcut",
    &[
      ("action", &i18n.t(label_key)),
      ("key", command.shortcut_label(modifier)),
    ],
  )
}

/// The first frame of an animated image as a still PNG, for a paused GIF.
pub async fn still_frame(url: &str) -> Option<ObjectUrl> {
  let image = web_sys::HtmlImageElement::new().ok()?;
  image.set_src(url);
  JsFuture::from(image.decode()).await.ok()?;
  let blob = JsFuture::from(png_of(&image)?).await.ok()?;
  ObjectUrl::from_blob(&blob.dyn_into().ok()?)
}

/// Puts the image on the clipboard as a PNG (the type every browser accepts there).
async fn copy_image(image: &web_sys::HtmlImageElement) -> bool {
  let Some(write) = clipboard_write(image) else {
    return false;
  };
  JsFuture::from(write).await.is_ok()
}

/// `navigator.clipboard.write([new ClipboardItem({"image/png": png})])`, started at once: Safari
/// only allows it while the click that asked for it is being handled. web-sys keeps the API
/// unstable, so it is reached through `Reflect`.
fn clipboard_write(image: &web_sys::HtmlImageElement) -> Option<js_sys::Promise> {
  let window = web_sys::window()?;
  let png = png_of(image)?;
  let constructor = js_sys::Reflect::get(&window, &"ClipboardItem".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  let parts = js_sys::Object::new();
  js_sys::Reflect::set(&parts, &"image/png".into(), &png).ok()?;
  let item = js_sys::Reflect::construct(&constructor, &js_sys::Array::of1(&parts)).ok()?;
  let clipboard = js_sys::Reflect::get(&window.navigator(), &"clipboard".into()).ok()?;
  let write = js_sys::Reflect::get(&clipboard, &"write".into())
    .ok()?
    .dyn_into::<js_sys::Function>()
    .ok()?;
  write
    .call1(&clipboard, &js_sys::Array::of1(&item))
    .ok()?
    .dyn_into::<js_sys::Promise>()
    .ok()
}

/// The image drawn into a canvas of its natural size, as a promise of a PNG `Blob`.
fn png_of(image: &web_sys::HtmlImageElement) -> Option<js_sys::Promise> {
  let (width, height) = (image.natural_width(), image.natural_height());
  if width == 0 || height == 0 {
    return None;
  }
  let canvas = web_sys::window()?
    .document()?
    .create_element("canvas")
    .ok()?
    .dyn_into::<web_sys::HtmlCanvasElement>()
    .ok()?;
  canvas.set_width(width);
  canvas.set_height(height);
  let context = canvas
    .get_context("2d")
    .ok()??
    .dyn_into::<web_sys::CanvasRenderingContext2d>()
    .ok()?;
  context.draw_image_with_html_image_element(image, 0.0, 0.0).ok()?;
  Some(js_sys::Promise::new(&mut |resolve, reject| {
    let on_reject = reject.clone();
    let done = Closure::once_into_js(move |blob: JsValue| {
      // A canvas too large for the browser yields no blob.
      let result = match blob.is_null() {
        true => reject.call0(&JsValue::NULL),
        false => resolve.call1(&JsValue::NULL, &blob),
      };
      drop(result);
    });
    if canvas.to_blob(done.unchecked_ref()).is_err() {
      drop(on_reject.call0(&JsValue::NULL));
    }
  }))
}

/// Shows the in-memory file in a blank window of this page (a new tab in a browser, a window of its
/// own in the desktop app, which allows `about:blank` and nothing else): the window gets an `<img>`
/// or `<video>` with the same `blob:` URL, and keeps the URL alive until it closes. `false` if the
/// window was refused (a popup blocker).
fn open_in_window(url: Rc<ObjectUrl>, title: &str, kind: MediaKind) -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let Ok(Some(popup)) = window.open_with_url_and_target("about:blank", "_blank") else {
    return false;
  };
  let Some(document) = popup.document() else {
    return false;
  };
  let Some(body) = document.body() else {
    return false;
  };
  document.set_title(title);
  let tag = if kind == MediaKind::Video { "video" } else { "img" };
  let Ok(element) = document.create_element(tag) else {
    return false;
  };
  let attributes = [
    ("src", url.as_str()),
    ("alt", title),
    ("style", "max-width: 100%; max-height: 100vh; object-fit: contain"),
  ];
  for (name, value) in attributes {
    drop(element.set_attribute(name, value));
  }
  if kind == MediaKind::Video {
    drop(element.set_attribute("controls", ""));
  }
  drop(body.set_attribute(
    "style",
    "margin: 0; min-height: 100vh; display: grid; place-items: center; background: #111",
  ));
  if body.append_child(&element).is_err() {
    return false;
  }
  // The window holds a reference to the bytes until it goes away.
  let release = Closure::once_into_js(move || drop(url));
  drop(popup.add_event_listener_with_callback("pagehide", release.unchecked_ref()));
  true
}

fn focus_selector(selector: &str) {
  let maybe_element = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.query_selector(selector).ok().flatten())
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
  if let Some(element) = maybe_element {
    drop(element.focus());
  }
}

/// Arrow keys move through the menu's items, wrapping around.
fn move_focus(menu: NodeRef<html::Div>, is_down: bool) {
  let Some(menu) = menu.get_untracked() else { return };
  let Ok(items) = menu.query_selector_all(".menu-item") else {
    return;
  };
  let items: Vec<web_sys::HtmlElement> = (0..items.length())
    .filter_map(|index| items.get(index))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  if items.is_empty() {
    return;
  }
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let maybe_at = items.iter().position(|item| {
    active
      .as_ref()
      .is_some_and(|active| active == item.unchecked_ref::<web_sys::Element>())
  });
  let next = match (maybe_at, is_down) {
    (None, true) => 0,
    (None, false) => items.len() - 1,
    (Some(at), true) => (at + 1) % items.len(),
    (Some(at), false) => (at + items.len() - 1) % items.len(),
  };
  drop(items[next].focus());
}
