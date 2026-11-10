//! Custom emoji of the workspace in the UI (issue #207): their pictures as `blob:` URLs (the page's
//! CSP lets images come from `blob:`), one per picture for the session, and the glyph that shows a
//! picture or, without one, the `:shortcode:` text older apps show too.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use leptos::prelude::*;
use teamcraft_client_api::custom_emoji::token_shortcode;
use teamcraft_proto::{CustomEmojiHash, CustomEmojiRef};

use crate::components::file_views::blob_url;
use crate::space_images::revoke;
use crate::state::use_app_state;

/// URLs kept for the session; past this they are dropped together. Each is one small picture.
const MAX_URLS: usize = 512;

/// The UI asks the backend for the set at most this often; the backend paces its reads itself.
const WANT_EVERY_MS: i64 = 30_000;

thread_local! {
  static URLS: RefCell<HashMap<CustomEmojiHash, String>> = RefCell::new(HashMap::new());
  static LAST_WANTED_MS: Cell<Option<i64>> = const { Cell::new(None) };
}

/// The set is needed here (the picker, the `:` suggestions, a picture this device lacks): the
/// backend reads it unless it did lately and nothing changed. Asking changes nothing to read.
pub fn want_set() {
  let now = crate::clock::now_ms();
  let is_due = LAST_WANTED_MS.with(|last| {
    let is_due = last.get().is_none_or(|wanted| now - wanted >= WANT_EVERY_MS);
    if is_due {
      last.set(Some(now));
    }
    is_due
  });
  if is_due {
    use_app_state().read(|backend| backend.want_custom_emoji());
  }
}

/// The picture's URL, once this device holds it (checked against its hash by the core). Reactive.
pub fn picture_url(sha256: CustomEmojiHash) -> Option<String> {
  if let Some(url) = URLS.with(|urls| urls.borrow().get(&sha256).cloned()) {
    return Some(url);
  }
  let bytes = use_app_state().read(|backend| backend.custom_emoji_picture(sha256))?;
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

/// The workspace emoji a picked or reacted `:shortcode:` names now, if it is one.
pub fn current_ref(token: &str) -> Option<CustomEmojiRef> {
  let shortcode = token_shortcode(token)?;
  use_app_state().read(|backend| {
    backend
      .custom_emoji()
      .by_shortcode(shortcode)
      .map(teamcraft_client_api::custom_emoji::CustomEmoji::reference)
  })
}

/// One custom emoji: its picture at text size with the `:shortcode:` as its name, or that text
/// while this device has no picture (not yet downloaded, or deleted before it was).
#[component]
pub fn CustomEmojiGlyph(reference: CustomEmojiRef) -> impl IntoView {
  let text = reference.text();
  let sha256 = reference.sha256;
  move || {
    match picture_url(sha256) {
    Some(url) => view! {
      <img class="custom-emoji" src=url alt=text.clone() title=text.clone() draggable="false" translate="no" />
    }
    .into_any(),
    None => view! { <span class="custom-emoji-text" translate="no">{text.clone()}</span> }.into_any(),
  }
  }
}

/// An emoji as picked or reacted: a Unicode emoji as text, a workspace emoji's `:shortcode:` as its
/// picture (`maybe_reference` from the message, else the current set's).
#[component]
pub fn EmojiGlyph(
  emoji: String,
  #[prop(default = None)] maybe_reference: Option<CustomEmojiRef>,
) -> impl IntoView {
  match maybe_reference.or_else(|| current_ref(&emoji)) {
    Some(reference) => view! { <CustomEmojiGlyph reference=reference /> }.into_any(),
    None => emoji.into_any(),
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
