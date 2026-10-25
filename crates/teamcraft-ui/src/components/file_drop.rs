use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::JsValue;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;

/// Files dropped on a conversation or thread, handed to its composer (provided by [`FileDrop`]).
#[derive(Clone, Copy)]
pub struct DroppedFiles(pub RwSignal<Vec<web_sys::File>, LocalStorage>);

/// Drop zone for files over a conversation or a thread panel (#484): while files are dragged over
/// it the [`DropStrip`] shows, and dropped files go to the composer inside, which attaches them
/// like the + button. Nothing is sent on its own.
#[derive(Clone, Copy)]
pub struct FileDrop {
  /// `dragenter` minus `dragleave`: every move onto a child element enters it before leaving the
  /// previous one, so the zone is left only when this falls back to zero.
  depth: RwSignal<i32>,
  dropped: DroppedFiles,
  can_post: bool,
}

impl FileDrop {
  /// A drop zone whose files reach the composer below it; `can_post` false ignores drags.
  pub fn provide(can_post: bool) -> Self {
    let dropped = DroppedFiles(RwSignal::new_local(Vec::new()));
    provide_context(dropped);
    let depth = RwSignal::new(0_i32);
    // A drag cancelled with Esc or ended elsewhere leaves no `dragleave` on some engines.
    let on_key = window_event_listener(ev::keydown, move |ev| {
      if ev.key() == "Escape" {
        depth.set(0);
      }
    });
    let on_end = window_event_listener(ev::dragend, move |_| depth.set(0));
    let on_window_drop = window_event_listener(ev::drop, move |_| depth.set(0));
    // Removed with the component: a late event must not reach its disposed signals.
    on_cleanup(move || {
      on_key.remove();
      on_end.remove();
      on_window_drop.remove();
    });
    Self {
      depth,
      dropped,
      can_post,
    }
  }

  /// Files are being dragged over the zone.
  pub fn is_dragging(self) -> bool {
    self.can_post && self.depth.get() > 0
  }

  pub fn on_dragenter(self, ev: &web_sys::DragEvent) {
    if self.can_post && has_files(ev) {
      self.depth.update(|depth| *depth += 1);
    }
  }

  pub fn on_dragleave(self) {
    self.depth.update(|depth| *depth = (*depth - 1).max(0));
  }

  /// Lets files be dropped here (a drop target must cancel `dragover`).
  pub fn on_dragover(self, ev: &web_sys::DragEvent) {
    if self.can_post && has_files(ev) {
      ev.prevent_default();
    }
  }

  /// Hands dropped files to the composer; true if there were any. A drop the composer handled
  /// itself (it cancels the event) is left alone, so nothing is attached twice.
  pub fn on_drop(self, ev: &web_sys::DragEvent) -> bool {
    self.depth.set(0);
    if !self.can_post || ev.default_prevented() {
      return false;
    }
    let files = ev
      .data_transfer()
      .map(|transfer| transfer_files(&transfer))
      .unwrap_or_default();
    if files.is_empty() {
      return false;
    }
    ev.prevent_default();
    self.dropped.0.set(files);
    true
  }
}

/// "Drop to attach" strip that slides up above the composer while files are dragged over the
/// [`FileDrop`] zone, where the dropped files will appear. Always rendered, so it can also slide
/// away; it takes no pointer events, so the drop reaches the element below.
#[component]
pub fn DropStrip(zone: FileDrop) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="drop-strip-anchor" aria-hidden="true">
      <div class="drop-strip" class:is-visible=move || zone.is_dragging()>
        <Icon name=IconName::Paperclip />
        <span>{move || i18n.t("drop-files-here")}</span>
      </div>
    </div>
  }
}

/// Keeps files dropped outside every drop zone from opening in the window, which would leave the
/// app (the browser navigates to the file; the desktop webview too, as Tauri's own handler is off).
pub fn block_stray_file_drops() {
  let on_over = window_event_listener(ev::dragover, |ev| {
    if !ev.default_prevented() && has_files(&ev) {
      ev.prevent_default();
      if let Some(transfer) = ev.data_transfer() {
        transfer.set_drop_effect("none");
      }
    }
  });
  let on_drop = window_event_listener(ev::drop, |ev| {
    if !ev.default_prevented() && has_files(&ev) {
      ev.prevent_default();
    }
  });
  on_cleanup(move || {
    on_over.remove();
    on_drop.remove();
  });
}

/// The files a drag or a paste carries. Some engines (WebKitGTK pasting an image) list them only as
/// items, not in `files`.
pub fn transfer_files(transfer: &web_sys::DataTransfer) -> Vec<web_sys::File> {
  if let Some(files) = transfer.files()
    && files.length() > 0
  {
    return (0..files.length())
      .filter_map(|index| files.item(index))
      .collect();
  }
  let items = transfer.items();
  (0..items.length())
    .filter_map(|index| items.get(index))
    .filter(|item| item.kind() == "file")
    .filter_map(|item| item.get_as_file().ok().flatten())
    .collect()
}

/// The drag carries files (not text or a link dragged within the page).
fn has_files(ev: &web_sys::DragEvent) -> bool {
  ev.data_transfer()
    .is_some_and(|transfer| transfer.types().includes(&JsValue::from_str("Files"), 0))
}
