//! The menu of a right click on a file in the desktop app (#719): open, save, copy the picture,
//! forward, the link to the message. It replaces the webview's own menu, whose "Save image as" did
//! nothing and whose "Open image in new window" and "Copy image address" only knew a `blob:` URL.
//! On a touch screen a long-press keeps showing the file's actions instead (file_views.rs).

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::actions::FileInfo;

use crate::auth::tauri;
use crate::components::file_views::FileActions;
use crate::components::icons::{Icon, IconName};
use crate::components::link_menu::{focus_first_item, move_focus, placed_with};
use crate::components::sheet::PopoverBackdrop;
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::preview::{self, PreviewKind};
use crate::state::{Overlay, use_app_state};

/// Room the menu needs below the pointer before it opens upwards (five entries and a separator).
const MENU_HEIGHT_PX: i32 = 236;

/// A right-clicked file and where the pointer was.
#[derive(Clone, Debug)]
pub struct AttachmentMenuTarget {
  pub file: FileInfo,
  pub x: i32,
  pub y: i32,
}

/// Where a file opens from the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenWith {
  /// Pictures and videos: the lightbox.
  Lightbox(MediaKey),
  /// Markdown, HTML, PDF: the document preview.
  Preview(MediaKey),
}

impl OpenWith {
  fn of(file: &FileInfo) -> Option<Self> {
    if let Some(key) = MediaKey::of(file) {
      return Some(Self::Lightbox(key));
    }
    let is_previewed = PreviewKind::of(&file.media_type, &file.file_name).is_some()
      && preview::is_within_limit(file.size_bytes);
    MediaKey::for_file(file)
      .filter(|_| is_previewed)
      .map(Self::Preview)
  }
}

/// The menu's state, for [`open_on_right_click`]; taken while a component is set up (event
/// handlers have no context). `None` outside the signed-in shell.
pub fn use_attachment_menu() -> Option<RwSignal<Option<AttachmentMenuTarget>>> {
  use_context::<RwSignal<Option<AttachmentMenuTarget>>>()
}

/// Shows the app's menu for a right click on `file` in the desktop app: cancels the webview's own.
/// Does nothing in the browser (its menu saves and copies pictures itself), on a touch screen, for a
/// file still going up, or outside the signed-in shell. `true` if the menu opened.
pub fn open_on_right_click(
  maybe_menu: Option<RwSignal<Option<AttachmentMenuTarget>>>,
  ev: &web_sys::MouseEvent,
  file: &FileInfo,
  is_touch: bool,
) -> bool {
  let is_on_server = file.file_id.parse::<u64>().is_ok();
  let Some(menu) = maybe_menu.filter(|_| tauri::is_available() && !is_touch && is_on_server) else {
    return false;
  };
  ev.prevent_default();
  ev.stop_propagation();
  menu.set(Some(AttachmentMenuTarget {
    file: file.clone(),
    x: ev.client_x(),
    y: ev.client_y(),
  }));
  true
}

/// Shows the menu while a file was right-clicked ([`open_on_right_click`] fills the context).
#[component]
pub fn AttachmentMenu() -> impl IntoView {
  let Some(menu) = use_context::<RwSignal<Option<AttachmentMenuTarget>>>() else {
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
fn MenuFor(target: AttachmentMenuTarget, menu: RwSignal<Option<AttachmentMenuTarget>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let node = NodeRef::<html::Div>::new();
  let actions = FileActions::new(state, &target.file);
  let maybe_open = OpenWith::of(&target.file);
  let can_copy_image = actions.can_copy_image();
  let conversation = target.file.conversation;
  let (x, y) = placed_with(target.x, target.y, MENU_HEIGHT_PX);
  // Starts fetching the permalink so "Copy link to message" has it when it is clicked.
  actions.prefetch_permalink();
  let close = move || menu.set(None);
  Effect::new(move |_| focus_first_item(node));
  let on_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Escape" | "Tab" => {
      ev.prevent_default();
      close();
    },
    "ArrowDown" | "ArrowUp" => {
      ev.prevent_default();
      move_focus(node, ev.key() == "ArrowDown");
    },
    _ => {},
  };
  let item = move |run: fn(FileActions)| {
    move |_| {
      // Runs within the click (copying a picture is only allowed then), before the menu and the
      // actions' values go.
      run(actions);
      close();
    }
  };
  view! {
    <PopoverBackdrop on_close=Callback::new(move |()| close()) popover=node />
    <div
      class="popover link-menu attachment-menu"
      role="menu"
      aria-label=move || i18n.t("attachment-menu-label")
      node_ref=node
      style:--link-menu-x=format!("{x}px")
      style:--link-menu-y=format!("{y}px")
      on:keydown=on_keydown
    >
      {maybe_open.map(|open| view! {
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| {
          close();
          match open {
            OpenWith::Lightbox(key) => state.overlay.set(Overlay::Lightbox(conversation, key)),
            OpenWith::Preview(key) => state.overlay.set(Overlay::FilePreview(conversation, key)),
          }
        }>
          <Icon name=IconName::Eye />
          <span class="menu-label">{move || i18n.t("attachment-menu-open")}</span>
        </button>
      })}
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=item(FileActions::download)>
        <Icon name=IconName::Download />
        <span class="menu-label">{move || i18n.t("attachment-menu-save")}</span>
      </button>
      {can_copy_image.then(|| view! {
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=item(FileActions::copy_image)>
          <Icon name=IconName::Copy />
          <span class="menu-label">{move || i18n.t("attachment-menu-copy-image")}</span>
        </button>
      })}
      <div class="menu-sep" role="separator"></div>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=item(FileActions::forward)>
        <Icon name=IconName::Forward />
        <span class="menu-label">{move || i18n.t("lightbox-forward")}</span>
      </button>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=item(FileActions::copy_message_link)>
        <Icon name=IconName::Link />
        <span class="menu-label">{move || i18n.t("lightbox-copy-link")}</span>
      </button>
    </div>
  }
}
