//! The pictures integration sources show on their messages (docs/bridge.md, "Source identity"), as
//! `blob:` URLs (the page's CSP lets images come from `blob:` and `data:`). The core holds the
//! bytes, by hash and already checked; this keeps one URL per picture, so a channel full of a
//! source's messages makes one blob.

use std::cell::RefCell;
use std::collections::HashMap;

use teamcraft_proto::SourcePictureHash;

use crate::components::file_views::blob_url;
use crate::model::ConversationId;
use crate::space_images::revoke;
use crate::state::use_app_state;

/// URLs kept for the session; past this they are dropped together. A source picture is one small
/// file, and only sources with messages on screen ask.
const MAX_URLS: usize = 256;

thread_local! {
  static URLS: RefCell<HashMap<(ConversationId, SourcePictureHash), String>> = RefCell::new(HashMap::new());
}

/// The picture's URL, once the backend has answered and this device holds it. Reactive.
pub fn picture_url(conversation: ConversationId, sha256: SourcePictureHash) -> Option<String> {
  let key = (conversation, sha256);
  if let Some(url) = URLS.with(|urls| urls.borrow().get(&key).cloned()) {
    return Some(url);
  }
  let bytes = use_app_state().read(|backend| backend.integration_source_picture(conversation, sha256))?;
  let url = blob_url(&bytes, "image/png")?;
  URLS.with(|urls| {
    let mut urls = urls.borrow_mut();
    if urls.len() >= MAX_URLS {
      for (_, old) in urls.drain() {
        revoke(&old);
      }
    }
    urls.insert(key, url.clone());
  });
  Some(url)
}
