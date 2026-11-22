//! The sender's own pictures while they go up (issue #2269): the `blob:` URL of the file the user
//! picked, shown in the message row from the frame of the send until the sent picture is there,
//! instead of a small thumbnail.
//!
//! The composer hands over the URL of each file's chip as it sends ([`offer`]); the send binds it to
//! the message and the file's place among the message's files ([`bind`]), which is how the row
//! asks for it ([`url_of`]). A URL of a picked file references the file, it holds no copy of the
//! bytes. At most [`MAX_LOCAL_PREVIEWS`] are kept; the oldest is revoked when a newer one comes, and
//! a row without one shows the thumbnail as before.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::model::MessageId;

/// How many pictures are kept: enough for a few albums still going up.
pub const MAX_LOCAL_PREVIEWS: usize = 24;

thread_local! {
  static PREVIEWS: RefCell<LocalPreviews> = RefCell::new(LocalPreviews::default());
}

/// Keeps `url`, the chip's picture of the draft's file `draft_id`, for the row of the message it is
/// sent with. Takes it over: it is revoked here, not by the composer.
pub fn offer(draft_id: &str, url: String) {
  PREVIEWS.with(|previews| previews.borrow_mut().offer(draft_id, url));
}

/// Binds the offered pictures of a message's files: `files` are the draft IDs of its files, in the
/// order the message lists them, with their names. A file without one shows its thumbnail.
pub fn bind(message: MessageId, files: &[(Option<&str>, &str)]) {
  PREVIEWS.with(|previews| previews.borrow_mut().bind(message, files));
}

/// The message `from`, shown until the server took it, is `to` from now on: its pictures go along.
pub fn moved(from: MessageId, to: MessageId) {
  PREVIEWS.with(|previews| previews.borrow_mut().moved(from, to));
}

/// The picture of `message`'s file at `place` (among all its files), if it is named `file_name`.
pub fn url_of(message: MessageId, place: usize, file_name: &str) -> Option<String> {
  PREVIEWS.with(|previews| previews.borrow().url_of(message, place, file_name))
}

/// The pictures kept, oldest first.
#[derive(Default)]
struct LocalPreviews {
  entries: VecDeque<LocalPreview>,
}

/// One picture: offered for a draft's file, then bound to its message.
struct LocalPreview {
  draft_id: String,
  maybe_bound: Option<BoundTo>,
  url: BlobUrl,
}

/// Where a picture shows: the message's file at this place.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BoundTo {
  message: MessageId,
  place: usize,
  file_name: String,
}

/// A `blob:` URL this module owns, revoked when dropped.
struct BlobUrl(String);

impl LocalPreviews {
  fn offer(&mut self, draft_id: &str, url: String) {
    self.entries.retain(|entry| entry.draft_id != draft_id);
    while self.entries.len() >= MAX_LOCAL_PREVIEWS {
      self.entries.pop_front();
    }
    self.entries.push_back(LocalPreview {
      draft_id: draft_id.to_owned(),
      maybe_bound: None,
      url: BlobUrl(url),
    });
  }

  fn bind(&mut self, message: MessageId, files: &[(Option<&str>, &str)]) {
    for (place, (maybe_draft_id, file_name)) in files.iter().enumerate() {
      let Some(draft_id) = maybe_draft_id else {
        continue;
      };
      if let Some(entry) = self
        .entries
        .iter_mut()
        .find(|entry| entry.draft_id == *draft_id && entry.maybe_bound.is_none())
      {
        entry.maybe_bound = Some(BoundTo {
          message,
          place,
          file_name: (*file_name).to_owned(),
        });
      }
    }
  }

  fn moved(&mut self, from: MessageId, to: MessageId) {
    for bound in self
      .entries
      .iter_mut()
      .filter_map(|entry| entry.maybe_bound.as_mut())
    {
      if bound.message == from {
        bound.message = to;
      }
    }
  }

  fn url_of(&self, message: MessageId, place: usize, file_name: &str) -> Option<String> {
    self
      .entries
      .iter()
      .find(|entry| {
        entry.maybe_bound.as_ref().is_some_and(|bound| {
          bound.message == message && bound.place == place && bound.file_name == file_name
        })
      })
      .map(|entry| entry.url.0.clone())
  }
}

impl Drop for BlobUrl {
  fn drop(&mut self) {
    #[cfg(target_arch = "wasm32")]
    drop(web_sys::Url::revoke_object_url(&self.0));
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn a_bound_picture_is_found_at_its_place() {
    let mut previews = LocalPreviews::default();
    previews.offer("a", "blob:a".to_owned());
    previews.offer("b", "blob:b".to_owned());
    previews.bind(
      message(1),
      &[(Some("b"), "two.jpg"), (None, "doc.pdf"), (Some("a"), "one.jpg")],
    );
    assert_eq!(
      previews.url_of(message(1), 0, "two.jpg").as_deref(),
      Some("blob:b")
    );
    assert_eq!(
      previews.url_of(message(1), 2, "one.jpg").as_deref(),
      Some("blob:a")
    );
    assert_eq!(previews.url_of(message(1), 1, "doc.pdf"), None);
  }

  #[test]
  fn another_file_at_the_place_gets_no_picture() {
    // The core left a file out (an expired draft file): the places moved.
    let mut previews = LocalPreviews::default();
    previews.offer("a", "blob:a".to_owned());
    previews.bind(message(1), &[(Some("a"), "one.jpg")]);
    assert_eq!(previews.url_of(message(1), 0, "other.jpg"), None);
    assert_eq!(previews.url_of(message(2), 0, "one.jpg"), None);
  }

  #[test]
  fn the_pictures_follow_the_message_the_server_took() {
    let mut previews = LocalPreviews::default();
    previews.offer("a", "blob:a".to_owned());
    previews.bind(message(1), &[(Some("a"), "one.jpg")]);
    previews.moved(message(1), message(7));
    assert_eq!(
      previews.url_of(message(7), 0, "one.jpg").as_deref(),
      Some("blob:a")
    );
    assert_eq!(previews.url_of(message(1), 0, "one.jpg"), None);
  }

  #[test]
  fn an_unbound_picture_is_not_shown() {
    let mut previews = LocalPreviews::default();
    previews.offer("a", "blob:a".to_owned());
    assert_eq!(previews.url_of(message(1), 0, "one.jpg"), None);
  }

  #[test]
  fn the_oldest_go_first() {
    let mut previews = LocalPreviews::default();
    for index in 0..=MAX_LOCAL_PREVIEWS {
      previews.offer(&index.to_string(), format!("blob:{index}"));
    }
    assert_eq!(previews.entries.len(), MAX_LOCAL_PREVIEWS);
    assert!(previews.entries.iter().all(|entry| entry.draft_id != "0"));
  }

  fn message(raw: u64) -> MessageId {
    id_from_raw(raw)
  }
}
