use std::cell::Cell;

use leptos::ev;
use leptos::prelude::*;
use wasm_bindgen::{JsCast, JsValue};

use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::state::{AppState, use_app_state};

thread_local! {
  /// A drag that started in this page (text, a link, a picture of the conversation) is under way:
  /// its `text/uri-list` is not a file from outside.
  static IS_PAGE_DRAG: Cell<bool> = const { Cell::new(false) };
}

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
  state: AppState,
  i18n: I18n,
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
      state: use_app_state(),
      i18n: use_i18n(),
    }
  }

  /// Files are being dragged over the zone.
  pub fn is_dragging(self) -> bool {
    self.can_post && self.depth.get() > 0
  }

  pub fn on_dragenter(self, ev: &web_sys::DragEvent) {
    if self.can_post && carries_files(ev) {
      self.depth.update(|depth| *depth += 1);
    }
  }

  pub fn on_dragleave(self) {
    self.depth.update(|depth| *depth = (*depth - 1).max(0));
  }

  /// Lets files be dropped here (a drop target must cancel `dragover`).
  pub fn on_dragover(self, ev: &web_sys::DragEvent) {
    if self.can_post && carries_files(ev) {
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
    let Some(transfer) = ev.data_transfer() else {
      return false;
    };
    let files = transfer_files(&transfer);
    if files.is_empty() {
      if is_unreadable_file_drop(&transfer) {
        ev.prevent_default();
        report_unreadable_drop(self.state, self.i18n);
      }
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

/// Keeps files (and links) dropped outside every drop zone from opening in the window, which would
/// replace the app (#651): the browser navigates to the file, and so does the desktop webview, as
/// Tauri's own handler is off. Only text fields keep their own drop (dropped text goes in).
pub fn block_stray_file_drops() {
  let on_start = window_event_listener(ev::dragstart, |_| IS_PAGE_DRAG.set(true));
  let on_end = window_event_listener(ev::dragend, |_| IS_PAGE_DRAG.set(false));
  let on_over = window_event_listener(ev::dragover, |ev| {
    if !ev.default_prevented() && carries_files(&ev) {
      ev.prevent_default();
      if let Some(transfer) = ev.data_transfer() {
        transfer.set_drop_effect("none");
      }
    }
  });
  let on_drop = window_event_listener(ev::drop, |ev| {
    IS_PAGE_DRAG.set(false);
    if !ev.default_prevented() && !is_text_field(ev.target()) {
      ev.prevent_default();
    }
  });
  on_cleanup(move || {
    on_start.remove();
    on_end.remove();
    on_over.remove();
    on_drop.remove();
  });
}

/// Files were dropped, but the webview handed over only their addresses, not their contents.
pub fn is_unreadable_file_drop(transfer: &web_sys::DataTransfer) -> bool {
  transfer
    .get_data("text/uri-list")
    .is_ok_and(|uris| lists_local_files(&uris))
}

/// Says that dropped files couldn't be read, instead of attaching nothing (or their address as
/// text), and how to attach them anyway.
pub fn report_unreadable_drop(state: AppState, i18n: I18n) {
  tracing::warn!("dropped files arrived as file addresses without their contents");
  state.show_error(
    i18n.t("drop-files-unreadable"),
    "the drop carried text/uri-list file addresses but no files".to_owned(),
  );
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

/// The drag may carry files from outside: it says so, or (WebKitGTK while dragging from a file
/// manager) it lists only addresses and didn't start in this page. Text or a link dragged within the
/// page doesn't count.
fn carries_files(ev: &web_sys::DragEvent) -> bool {
  let Some(transfer) = ev.data_transfer() else {
    return false;
  };
  let types = transfer.types();
  let has = |kind: &str| types.includes(&JsValue::from_str(kind), 0);
  has("Files") || (has("text/uri-list") && !IS_PAGE_DRAG.get())
}

/// A `text/uri-list` (lines of URIs, `#` comments) that names local files.
fn lists_local_files(uri_list: &str) -> bool {
  uri_list
    .lines()
    .map(str::trim)
    .filter(|line| !line.is_empty() && !line.starts_with('#'))
    .any(|uri| {
      uri
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
    })
}

/// Text fields take dropped text themselves.
fn is_text_field(maybe_target: Option<web_sys::EventTarget>) -> bool {
  maybe_target
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .and_then(|element| {
      element
        .closest("input, textarea, [contenteditable]")
        .ok()
        .flatten()
    })
    .is_some()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_file_addresses_count_as_dropped_files() {
    assert!(lists_local_files("file:///home/user/beach.jpg"));
    assert!(lists_local_files(
      "# dragged from Files\r\nfile:///tmp/a.png\r\nfile:///tmp/b.png\r\n"
    ));
    assert!(lists_local_files("FILE:///C:/Users/a.png"));
    assert!(!lists_local_files("https://example.com/picture.jpg"));
    assert!(!lists_local_files("# file:///not/a/uri"));
    assert!(!lists_local_files(""));
  }
}
