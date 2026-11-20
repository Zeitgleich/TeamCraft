//! Live Photos and Motion Photos on web and desktop (issue #2181, e2ee.md §4.5): a picture with a
//! motion part shows a "Live" badge; hovering the picture plays the motion muted and looping over
//! it, and the badge (or the lightbox's toggle) plays it on request. With reduced motion only the
//! request plays it. The motion comes down only once it is to play, through the media cache the
//! lightbox shares; a clip the browser can't decode leaves the picture still, never an error.

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::actions::FileInfo;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::lightbox::MediaKey;
use crate::media_cache::{self, Rendition};
use crate::model::ConversationId;
use crate::object_url::ObjectUrl;

/// The motion part of one shown picture and whether it plays.
#[derive(Clone, Copy)]
pub struct LiveMotion {
  conversation: ConversationId,
  key: MediaKey,
  media_type: StoredValue<String>,
  /// Played on request: the badge or the lightbox's toggle.
  pub is_requested: RwSignal<bool>,
  /// The decrypted clip, once loaded.
  url: RwSignal<Option<Rc<ObjectUrl>>, LocalStorage>,
  is_loading: RwSignal<bool>,
  /// The download failed: not tried again until the next request.
  has_failed: RwSignal<bool>,
  /// The browser can't play it: the picture stays still, without a badge.
  pub is_unplayable: RwSignal<bool>,
  /// Set by [`Self::play_while`].
  playing: StoredValue<Option<Memo<bool>>>,
}

impl LiveMotion {
  /// The motion of `file`, when it is a picture with a playable motion part.
  pub fn of(file: &FileInfo) -> Option<Self> {
    let motion = file.maybe_motion.as_ref()?;
    if !file.media_type.starts_with("image/") || !motion.media_type.starts_with("video/") {
      return None;
    }
    let key = MediaKey {
      message: file.message,
      attachment: motion.file_id.parse().ok()?,
    };
    Some(Self {
      conversation: file.conversation,
      key,
      media_type: StoredValue::new(motion.media_type.clone()),
      is_requested: RwSignal::new(false),
      url: RwSignal::new_local(None),
      is_loading: RwSignal::new(false),
      has_failed: RwSignal::new(false),
      is_unplayable: RwSignal::new(media_cache::is_unplayable(key)),
      playing: StoredValue::new(None),
    })
  }

  /// Plays the motion while `is_wanted` (hover, a request) holds, loading it the first time.
  pub fn play_while(self, is_wanted: impl Fn() -> bool + Send + Sync + 'static) {
    let is_wanted = Signal::derive(is_wanted);
    Effect::new(move |_| {
      let wants = is_wanted.get() && !self.is_unplayable.get();
      if wants && self.url.with(Option::is_none) && !self.is_loading.get() && !self.has_failed.get() {
        self.load(is_wanted);
      }
    });
    let is_playing =
      Memo::new(move |_| is_wanted.get() && !self.is_unplayable.get() && self.url.with(Option::is_some));
    self.playing.set_value(Some(is_playing));
  }

  /// Whether the clip is on screen.
  pub fn is_playing(self) -> bool {
    self.playing.get_value().is_some_and(|memo| memo.get())
  }

  /// Whether it is coming down to play.
  pub fn is_coming(self) -> bool {
    self.is_loading.get()
  }

  /// The badge's press: plays or stops the motion; a failed download is tried again.
  pub fn toggle(self) {
    self.has_failed.set(false);
    self.is_requested.update(|requested| *requested = !*requested);
  }

  fn load(self, is_wanted: Signal<bool>) {
    let Self {
      conversation,
      key,
      url,
      is_loading,
      has_failed,
      ..
    } = self;
    let media_type = self.media_type.get_value();
    is_loading.set(true);
    spawn_local(async move {
      let still_wanted = move || is_wanted.try_get_untracked().unwrap_or(false);
      match media_cache::load(
        conversation,
        key,
        &media_type,
        Rendition::Full,
        None,
        still_wanted,
      )
      .await
      {
        Ok(Some(loaded)) => drop(url.try_set(Some(loaded))),
        Ok(None) => {},
        Err(error) => {
          // The still picture stays; the badge tries again.
          tracing::debug!(%error, conversation_id = %conversation, "live photo motion not loaded");
          has_failed.try_set(true);
        },
      }
      is_loading.try_set(false);
    });
  }

  pub fn src(self) -> Option<String> {
    self
      .url
      .with(|url| url.as_ref().map(|url| url.as_str().to_owned()))
  }

  pub fn mark_unplayable(self) {
    media_cache::mark_unplayable(self.key);
    self.is_unplayable.set(true);
  }
}

/// The clip over its picture while it plays: muted, looping, without controls.
#[component]
pub fn LiveVideo(motion: LiveMotion) -> impl IntoView {
  move || {
    motion.is_playing().then(|| {
      motion.src().map(|src| {
        view! {
          <video
            class="media-live"
            src=src
            muted=true
            prop:muted=true
            autoplay=true
            r#loop=true
            playsinline=true
            disablepictureinpicture=true
            aria-hidden="true"
            on:error=move |_| motion.mark_unplayable()
          ></video>
        }
      })
    })
  }
}

/// The "Live" badge: says the picture moves, and plays it on press.
#[component]
pub fn LiveBadge(motion: LiveMotion) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    (!motion.is_unplayable.get()).then(|| {
      view! {
        <button
          type="button"
          class="media-live-badge"
          class:is-playing=move || motion.is_playing()
          aria-pressed=move || motion.is_requested.get().to_string()
          aria-label=move || i18n.t("media-live-play")
          on:click=move |ev| {
            // The picture under the badge opens the lightbox; the badge only plays.
            ev.stop_propagation();
            motion.toggle();
          }
        >
          {move || match motion.is_coming() {
            true => view! { <span class="spinner" aria-hidden="true"></span> }.into_any(),
            false => view! { <Icon name=IconName::Live /> }.into_any(),
          }}
          <span aria-hidden="true">{move || i18n.t("media-live-badge")}</span>
        </button>
      }
    })
  }
}
