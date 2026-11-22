//! Sticker packs of the workspace in the UI (issue #2132): their pictures as `blob:` URLs (the
//! page's CSP lets images come from `blob:`), one per picture for the session, the sticker as a
//! message shows it, and the fallback text a sent sticker carries for older apps and push previews.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use leptos::prelude::*;
use teamcraft_proto::{CustomEmojiHash, StickerRef};

use crate::components::file_views::blob_url;
use crate::i18n::I18n;
use crate::space_images::revoke;
use crate::state::{AppState, use_app_state};

/// URLs kept for the session; past this they are dropped together. A pack's stickers are shown a
/// pack at a time, so this holds a few packs.
const MAX_URLS: usize = 512;

/// The UI asks the backend for the packs at most this often; the backend paces its reads itself.
const WANT_EVERY_MS: i64 = 30_000;

thread_local! {
  static URLS: RefCell<HashMap<CustomEmojiHash, String>> = RefCell::new(HashMap::new());
  static LAST_WANTED_MS: Cell<Option<i64>> = const { Cell::new(None) };
}

/// The packs are needed here (the sticker picker, the admin console, a picture this device lacks):
/// the backend reads them unless it did lately and nothing changed.
pub fn want_packs(state: AppState) {
  let now = crate::clock::now_ms();
  let is_due = LAST_WANTED_MS.with(|last| {
    let is_due = last.get().is_none_or(|wanted| now - wanted >= WANT_EVERY_MS);
    if is_due {
      last.set(Some(now));
    }
    is_due
  });
  if is_due {
    state.read(|backend| backend.want_sticker_packs());
  }
}

/// The picture's URL, once this device holds it (checked against its hash by the core). Reactive.
pub fn picture_url(sha256: CustomEmojiHash) -> Option<String> {
  if let Some(url) = URLS.with(|urls| urls.borrow().get(&sha256).cloned()) {
    return Some(url);
  }
  let bytes = use_app_state().read(|backend| backend.sticker_picture(sha256))?;
  let url = blob_url(&bytes, media_type(&bytes))?;
  URLS.with(|urls| {
    let mut urls = urls.borrow_mut();
    if urls.len() >= MAX_URLS {
      for (_, old) in urls.drain() {
        revoke(&old);
      }
    }
    urls.insert(sha256, url.clone());
  });
  Some(url)
}

/// What a sent sticker's message says as text: what older apps, push previews and devices without
/// the picture show.
pub fn fallback_text(i18n: I18n, alt: &str) -> String {
  i18n.t_with("sticker-fallback", &[("alt", alt)])
}

/// A sticker as a message shows it: the picture, large and without a bubble, its alt text as the
/// accessible name; the message's own text while this device has no picture (not yet downloaded,
/// or removed before it was).
#[component]
pub fn StickerMessage(sticker: StickerRef, fallback: String) -> impl IntoView {
  let state = use_app_state();
  let sha256 = sticker.sha256;
  let alt = sticker.alt;
  move || match picture_url(sha256) {
    Some(url) => view! {
      <img class="msg-sticker" src=url alt=alt.clone() title=alt.clone() draggable="false" />
    }
    .into_any(),
    None => {
      // The device lacks it: the packs may have changed since it last read them.
      want_packs(state);
      view! { <span class="msg-sticker-fallback">{fallback.clone()}</span> }.into_any()
    },
  }
}

/// One sticker's picture at `class`'s size, or its alt text while it isn't there yet.
#[component]
pub fn StickerPicture(sha256: CustomEmojiHash, alt: String, #[prop(into)] class: String) -> impl IntoView {
  move || match picture_url(sha256) {
    Some(url) => view! { <img class=class.clone() src=url alt="" draggable="false" /> }.into_any(),
    None => view! { <span class=format!("{class} sticker-missing")>{alt.clone()}</span> }.into_any(),
  }
}

/// PNG unless the bytes say WebP: the only formats the core keeps.
fn media_type(bytes: &[u8]) -> &'static str {
  if bytes.len() >= 12 && bytes[..4] == *b"RIFF" && bytes[8..12] == *b"WEBP" {
    "image/webp"
  } else {
    "image/png"
  }
}
