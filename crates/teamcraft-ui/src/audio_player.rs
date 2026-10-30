//! The app's one audio player (#802): a single `<audio>` element outside the page's layout that
//! plays the audio attachment whose card was pressed. One element means one attachment plays at a
//! time; it lives in [`crate::state::AppState`], not in a message row, because a row is drawn anew
//! whenever its message changes (a reaction, a save, the download's progress) and playback must not
//! notice.
//!
//! - The file is end-to-end encrypted: it is downloaded and decrypted on the first press on play,
//!   not when the message scrolls into view, and handed to the element as a `blob:` URL
//!   ([`ObjectUrl`]). The card shows the download's progress (the core's transfer state).
//! - The URL is revoked as soon as the player lets go of the file: another attachment is played,
//!   the conversation is left ([`stop_when_leaving`]), or no card shows it any more (the row left
//!   the list, the message was deleted) — see [`AudioPlayer::show`].
//! - A failed download says so and can be tried again; a format the webview can't play says so and
//!   offers the download (#322). A load that finishes after the user moved on is dropped.
//! - In the desktop app, playback pauses when the window goes to the tray or is minimized.

use std::collections::HashMap;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::AttachmentPriority;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::audio::{AudioFailure, AudioFormat, AudioPhase};
use crate::lightbox::MediaKey;
use crate::model::ConversationId;
use crate::object_url::{FetchError, ObjectUrl, fetch_decrypted, url_for};
use crate::state::{AppState, MainView};
use crate::video_controls::{PlayerCommand, seek_target, volume_target};

/// The volume unmuting goes back to when the volume was turned all the way down.
const UNMUTED_VOLUME: f64 = 0.5;

/// The player and what it holds. Signals, so every card showing the playing attachment follows it.
#[derive(Clone, Copy)]
pub struct AudioPlayer {
  /// The attachment the player holds (loading, loaded or failed), if any.
  track: RwSignal<Option<MediaKey>>,
  phase: RwSignal<AudioPhase>,
  /// Seconds played.
  position: RwSignal<f64>,
  /// Seconds; NaN until the element knows.
  duration: RwSignal<f64>,
  volume: RwSignal<f64>,
  is_muted: RwSignal<bool>,
  /// Bumped by every new attachment and every stop: a load that finishes for an older one is
  /// dropped.
  generation: StoredValue<u64>,
  /// Cards on screen per attachment: the player lets go of a file once none shows it.
  shown: StoredValue<HashMap<MediaKey, usize>>,
  /// Made on the first press on play.
  element: StoredValue<Option<Element>, LocalStorage>,
  /// The decrypted file the element plays; dropping it revokes the URL.
  url: StoredValue<Option<ObjectUrl>, LocalStorage>,
}

/// What a press on play asks for.
#[derive(Clone, Copy, Debug)]
pub struct AudioRequest {
  pub conversation: ConversationId,
  pub key: MediaKey,
  pub format: AudioFormat,
}

/// The `<audio>` element and the listeners it needs kept alive.
struct Element {
  audio: web_sys::HtmlMediaElement,
  _listeners: Vec<Closure<dyn Fn()>>,
}

impl AudioPlayer {
  pub fn new() -> Self {
    Self {
      track: RwSignal::new(None),
      phase: RwSignal::new(AudioPhase::Paused),
      position: RwSignal::new(0.0),
      duration: RwSignal::new(f64::NAN),
      volume: RwSignal::new(1.0),
      is_muted: RwSignal::new(false),
      generation: StoredValue::new(0),
      shown: StoredValue::new(HashMap::new()),
      element: StoredValue::new_local(None),
      url: StoredValue::new_local(None),
    }
  }

  /// Where the player stands with `key`; `None` while it holds another attachment or none.
  pub fn phase_of(self, key: MediaKey) -> Option<AudioPhase> {
    (self.track.get() == Some(key)).then(|| self.phase.get())
  }

  pub fn position(self) -> f64 {
    self.position.get()
  }

  pub fn duration(self) -> f64 {
    self.duration.get()
  }

  pub fn volume(self) -> f64 {
    self.volume.get()
  }

  pub fn is_muted(self) -> bool {
    self.is_muted.get()
  }

  /// Play, or pause, the attachment of a card. A new attachment stops the one before and loads;
  /// a failed download is tried again.
  pub fn press(self, request: AudioRequest) {
    let is_current = self.track.try_get_untracked().flatten() == Some(request.key);
    let phase = self.phase.try_get_untracked().unwrap_or(AudioPhase::Paused);
    match phase {
      AudioPhase::Playing if is_current => self.pause(),
      AudioPhase::Paused if is_current => self.resume(),
      AudioPhase::Loading | AudioPhase::Failed(AudioFailure::Unplayable) if is_current => {},
      _ => self.load(request),
    }
  }

  pub fn pause(self) {
    if let Some(audio) = self.audio() {
      drop(audio.pause());
    }
  }

  /// Lets go of the attachment: playback ends, the URL is revoked, a load under way is dropped.
  pub fn stop(self) {
    self.generation.try_update_value(|generation| *generation += 1);
    if let Some(audio) = self.audio() {
      drop(audio.pause());
      // Without a source the element lets go of the blob; `load` resets it.
      drop(audio.remove_attribute("src"));
      audio.load();
    }
    self.url.try_set_value(None);
    self.track.try_set(None);
    self.phase.try_set(AudioPhase::Paused);
    self.position.try_set(0.0);
    self.duration.try_set(f64::NAN);
  }

  /// A key pressed on a card's player, or one of its buttons.
  pub fn run(self, command: PlayerCommand) {
    let Some(audio) = self.audio() else {
      return;
    };
    match command {
      PlayerCommand::TogglePlay if audio.paused() => self.resume(),
      PlayerCommand::TogglePlay => self.pause(),
      PlayerCommand::SeekBy(seconds) => {
        audio.set_current_time(seek_target(audio.current_time(), seconds, audio.duration()));
      },
      PlayerCommand::SeekToStart => audio.set_current_time(0.0),
      PlayerCommand::SeekToEnd => {
        let duration = audio.duration();
        if duration.is_finite() {
          audio.set_current_time(duration);
        }
      },
      PlayerCommand::VolumeBy(delta) => self.set_volume(volume_target(audio.volume(), delta)),
      // Unmuted at zero volume would still be silent.
      PlayerCommand::ToggleMute if audio.muted() && audio.volume() == 0.0 => self.set_volume(UNMUTED_VOLUME),
      PlayerCommand::ToggleMute => audio.set_muted(!audio.muted()),
      // Nothing to show full screen.
      PlayerCommand::ToggleFullscreen => {},
    }
  }

  pub fn seek_to(self, seconds: f64) {
    if let Some(audio) = self.audio() {
      audio.set_current_time(seconds);
    }
  }

  /// Sets the volume; turning it up unmutes.
  pub fn set_volume(self, level: f64) {
    if let Some(audio) = self.audio() {
      audio.set_volume(level);
      audio.set_muted(level == 0.0);
    }
  }

  /// A card of `key` is on screen until the current scope ends. Once no card shows the attachment
  /// the player holds (its row left the list, the message was deleted, the conversation closed),
  /// the player stops and frees the file. A row drawn anew drops its old card and makes a new one
  /// in the same turn, so the count is checked after it.
  pub fn show(self, key: MediaKey) {
    self
      .shown
      .update_value(|shown| *shown.entry(key).or_default() += 1);
    on_cleanup(move || {
      self.shown.try_update_value(|shown| {
        if let Some(count) = shown.get_mut(&key) {
          *count = count.saturating_sub(1);
          if *count == 0 {
            shown.remove(&key);
          }
        }
      });
      set_timeout(
        move || {
          let is_shown = self
            .shown
            .try_with_value(|shown| shown.contains_key(&key))
            .unwrap_or(false);
          if !is_shown && self.track.try_get_untracked().flatten() == Some(key) {
            self.stop();
          }
        },
        Duration::ZERO,
      );
    });
  }

  /// Downloads and decrypts the attachment, then plays it.
  fn load(self, request: AudioRequest) {
    self.stop();
    let Some(audio) = self.audio_or_make() else {
      self.track.try_set(Some(request.key));
      self.phase.try_set(AudioPhase::Failed(AudioFailure::Unplayable));
      return;
    };
    // Inside the press: WebKit lets an element play later without a new gesture once it was
    // loaded during one.
    audio.load();
    let generation = self.generation.try_get_value().unwrap_or_default();
    self.track.try_set(Some(request.key));
    self.phase.try_set(AudioPhase::Loading);
    let AudioRequest {
      conversation,
      key,
      format,
    } = request;
    spawn_local(async move {
      let result = fetch(conversation, key, format).await;
      // Stopped, or another attachment pressed, in the meantime.
      if self.generation.try_get_value() != Some(generation) {
        return;
      }
      match result {
        Ok(url) => {
          audio.set_src(url.as_str());
          self.url.try_set_value(Some(url));
          self.phase.try_set(AudioPhase::Paused);
          self.resume();
        },
        Err(error) => {
          tracing::debug!(%error, conversation_id = %conversation, "audio not loaded");
          let failure = match error {
            FetchError::Unavailable(_) => AudioFailure::Unavailable,
            FetchError::Undecodable(_) => AudioFailure::Unplayable,
          };
          self.phase.try_set(AudioPhase::Failed(failure));
        },
      }
    });
  }

  /// Starts playing what is loaded. A refused start (no user gesture any more after a long
  /// download) leaves it paused for the next press; a format the webview can't play fails.
  fn resume(self) {
    let Some(audio) = self.audio() else {
      return;
    };
    let Ok(started) = audio.play() else {
      return;
    };
    let generation = self.generation.try_get_value().unwrap_or_default();
    spawn_local(async move {
      let Err(error) = wasm_bindgen_futures::JsFuture::from(started).await else {
        return;
      };
      let name = js_sys::Reflect::get(&error, &"name".into())
        .ok()
        .and_then(|name| name.as_string())
        .unwrap_or_default();
      tracing::debug!(%name, "audio did not start");
      if name == "NotSupportedError" && self.generation.try_get_value() == Some(generation) {
        self.fail_unplayable();
      }
    });
  }

  /// The webview can't play the file: it is let go, the card says so and offers the download.
  fn fail_unplayable(self) {
    if let Some(audio) = self.audio() {
      drop(audio.remove_attribute("src"));
      audio.load();
    }
    self.url.try_set_value(None);
    self.phase.try_set(AudioPhase::Failed(AudioFailure::Unplayable));
  }

  fn audio(self) -> Option<web_sys::HtmlMediaElement> {
    self
      .element
      .try_with_value(|maybe_element| maybe_element.as_ref().map(|element| element.audio.clone()))
      .flatten()
  }

  fn audio_or_make(self) -> Option<web_sys::HtmlMediaElement> {
    if let Some(audio) = self.audio() {
      return Some(audio);
    }
    let element = Element::new(self)?;
    let audio = element.audio.clone();
    self.element.try_set_value(Some(element));
    Some(audio)
  }

  /// What the element reports, into the signals. It may report after its attachment was let go.
  fn on_event(self, event: &str, audio: &web_sys::HtmlMediaElement) {
    let phase = self.phase.try_get_untracked().unwrap_or(AudioPhase::Paused);
    match event {
      "play" if phase == AudioPhase::Paused => drop(self.phase.try_set(AudioPhase::Playing)),
      "pause" | "ended" if phase == AudioPhase::Playing => drop(self.phase.try_set(AudioPhase::Paused)),
      "timeupdate" => drop(self.position.try_set(audio.current_time())),
      "durationchange" | "loadedmetadata" => drop(self.duration.try_set(audio.duration())),
      "volumechange" => {
        self.volume.try_set(audio.volume());
        self.is_muted.try_set(audio.muted());
      },
      // A source the element can't play; an element without one reports nothing.
      "error" if phase.is_loaded() => self.fail_unplayable(),
      _ => {},
    }
  }
}

impl Default for AudioPlayer {
  fn default() -> Self {
    Self::new()
  }
}

impl Element {
  /// The `<audio>` element, not in the page; `None` where the browser refuses.
  fn new(player: AudioPlayer) -> Option<Self> {
    let document = web_sys::window()?.document()?;
    let audio = document
      .create_element("audio")
      .ok()?
      .dyn_into::<web_sys::HtmlMediaElement>()
      .ok()?;
    audio.set_preload("auto");
    let mut listeners = Vec::new();
    let events = [
      "play",
      "pause",
      "ended",
      "timeupdate",
      "durationchange",
      "loadedmetadata",
      "volumechange",
      "error",
    ];
    for event in events {
      let target = audio.clone();
      let listener = Closure::<dyn Fn()>::new(move || player.on_event(event, &target));
      drop(audio.add_event_listener_with_callback(event, listener.as_ref().unchecked_ref()));
      listeners.push(listener);
    }
    // The desktop app hidden to the tray (or minimized) doesn't play on unseen.
    if crate::auth::tauri::is_available() {
      let target = audio.clone();
      let listener = Closure::<dyn Fn()>::new(move || {
        if !crate::reactive::is_document_visible() {
          drop(target.pause());
        }
      });
      drop(document.add_event_listener_with_callback("visibilitychange", listener.as_ref().unchecked_ref()));
      listeners.push(listener);
    }
    Some(Self {
      audio,
      _listeners: listeners,
    })
  }
}

/// Stops the player when the main view changes (another conversation, search, settings pages):
/// what plays belongs to the conversation that was open.
pub fn stop_when_leaving(state: AppState) {
  Effect::new(move |maybe_before: Option<MainView>| {
    let view = state.view.get();
    if maybe_before.is_some_and(|before| before != view) {
      state.audio.stop();
    }
    view
  });
}

/// The decrypted file as a URL of its audio type.
async fn fetch(
  conversation: ConversationId,
  key: MediaKey,
  format: AudioFormat,
) -> Result<ObjectUrl, FetchError> {
  let bytes = fetch_decrypted(
    conversation,
    key.message,
    key.attachment.to_string(),
    AttachmentPriority::Now,
  )
  .await?;
  url_for(&bytes, format.blob_type().unwrap_or_default(), None).await
}
